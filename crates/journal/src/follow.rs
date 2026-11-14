//! Following a journal (`Journal::follow`, `Follower::drain`): its files
//! kept open, a position per file, and an inotify watch on the directories
//! they are in, ported from proto-journal's `Follow` (query.rs) and its
//! follow loop (main.rs `watch`, `watch_dirs`, `classify` and the
//! rescans).
//!
//! sd-journal keeps a position per file the same way (`next_beyond_location`,
//! v249 to v259 sd-journal.c); what differs is that a file is held by its
//! inode, not its name, so a rotation's rename doesn't reopen it and a
//! vacuumed file is still read to its end.
//!
//! Every drain reads every open file that isn't sealed and read to its
//! end, not only the one an event named: journald raises each file's
//! `IN_MODIFY` from its own coalesced timer (journal-file.c
//! `schedule_post_change`, 250 ms in journald), so two files written at
//! once notify in either order. A header read per file is the whole cost
//! when nothing was written, plus, on 24.04 and 26.04, a read of the tail
//! entry of each file whose header changed, for the boot table.
//!
//! Its files are read again after a cut the way a page reads them: each
//! drain reads every header first (`JournalFile::refresh`), which caps the
//! reads at the file's length then, so a guessed read near a cut fails
//! with the system's error (`Io`) only when the cut falls inside that
//! drain. Such a failure is waited for once, like any other read
//! (`Wait::Entry`), and the next drain reads the header again first.

use crate::boots::BootOrder;
use crate::file::{ArrayCursor, Entry, JournalFile, Tail};
use crate::open::Opener;
use crate::rules::Group;
use crate::walk::{self, Candidate, Payloads};
use crate::{
    Batch, BootRule, Copied, Cursor, Journal, Limits, OpenError, PageEnd, Query, ReadError,
    ReadErrorKind, ReadErrors, Record, Roots,
};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The newest entry shown, and the directory of the file it came from:
/// what a followed entry is checked against (`seen`).
#[derive(Debug, Clone)]
struct Last {
    location: Cursor,
    dir: Option<PathBuf>,
    /// The entries at `location`'s stamp taken as shown, `location` among
    /// them: those the follow showed, and at its start every entry of the
    /// page's newest's directory at that stamp (`Burst::page_end`).
    burst: Burst,
}

/// The identities of the entries shown at one stamp (boot, monotonic and
/// realtime times): one burst at most, started again when the stamp moves
/// on (`took`). journald stamps every entry of one event-loop pass alike
/// (`sd_event_now`), so a flush's copies of a burst differ from each
/// other only by their XOR hashes, and each is checked against them all,
/// not only against the newest (follow-M1). The stamp once and the
/// hashes, at most `BURST_MAX`.
#[derive(Debug, Clone, Default)]
struct Burst {
    stamp: Option<Cursor>,
    xors: HashSet<u64>,
}

/// The most XOR hashes a follow keeps of one burst, about 512 KiB, and the
/// most entries `Burst::page_end` walks back. journald's stream reads give
/// a few thousand entries at one stamp; past this, a flush's copies of the
/// rest are shown again, which only a crafted file can cause.
const BURST_MAX: usize = 65_536;

/// Whether two entries have one journald stamp: the same boot and the
/// same monotonic and realtime times.
fn same_stamp(a: &Cursor, b: &Cursor) -> bool {
    a.boot_id == b.boot_id && a.monotonic == b.monotonic && a.realtime == b.realtime
}

impl Burst {
    /// One entry's burst.
    fn of(c: Cursor) -> Self {
        let mut b = Self::default();
        b.took(c);
        b
    }

    /// Add `file`'s entries at the burst's stamp: walked back from its
    /// newest, past entries newer than the stamp (written since the page),
    /// to the first one older or of another boot. Unfiltered, as a follow
    /// drops a copy before matching it. An entry that can't be read is
    /// passed over (the follow's read reports it); at most `BURST_MAX`
    /// entries are read.
    fn page_end(&mut self, file: &JournalFile) {
        let Some(stamp) = self.stamp else { return };
        for r in file.all_entries_rev().take(BURST_MAX) {
            let Ok(e) = r.and_then(|off| file.entry(off)) else {
                continue;
            };
            let l = walk::location(file, &e);
            if self.at_stamp(&l) {
                self.add(l.xor_hash);
            } else if l.boot_id != stamp.boot_id || l.monotonic <= stamp.monotonic {
                return;
            }
        }
    }

    /// `c` was just shown: one more of the burst at its stamp, or the
    /// first of a new one.
    fn took(&mut self, c: Cursor) {
        if !self.at_stamp(&c) {
            self.stamp = Some(c);
            self.xors.clear();
        }
        self.add(c.xor_hash);
    }

    fn add(&mut self, xor: u64) {
        if self.xors.len() < BURST_MAX {
            self.xors.insert(xor);
        }
    }

    /// Whether `l` has the burst's stamp.
    fn at_stamp(&self, l: &Cursor) -> bool {
        self.stamp.as_ref().is_some_and(|s| same_stamp(s, l))
    }

    /// Whether `l` has the identity of one of the burst's entries (boot,
    /// monotonic and realtime stamps, XOR hash), as a flush copies it.
    fn has(&self, l: &Cursor) -> bool {
        self.at_stamp(l) && self.xors.contains(&l.xor_hash)
    }
}

impl Last {
    /// Whether `file` is in the directory this entry came from: if so, an
    /// entry there is new by sd-journal's order; if not, by `is_old`
    /// (`seen`).
    fn in_dir_of(&self, file: &JournalFile) -> bool {
        self.dir
            .as_deref()
            .is_some_and(|d| file.path.parent() == Some(d))
    }
}

/// What a followed file waits one drain for before it reports and skips
/// it (or, for the header and the entry list, reports it and stops): each
/// kind its own, so one kind's wait isn't spent by another's at the same
/// position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// The header couldn't be read.
    Header,
    /// The entry list couldn't be read.
    List,
    /// An item that can't be an entry offset: a slot not written yet, or
    /// a bogus value.
    Slot,
    /// The entry, or one of its DATA objects, couldn't be read.
    Entry,
}

/// A followed file's tail for the boot table (`JournalFile::tail`), read
/// again only when its header names another (sd-journal reads a file's
/// tail again before each step when the file changed: v255
/// `next_beyond_location` on a changed mtime, v259 on a new tail offset).
#[derive(Debug, Default)]
struct TailCache {
    /// The header's entry count and tail offsets the tail was read for.
    key: Option<(u64, Option<u64>, u64)>,
    tail: Option<Tail>,
    /// The main list's newest readable entry, when `tail` needed it (a
    /// header whose tail can't be read).
    newest: Option<(u64, Entry)>,
    /// 259.9's machine id from the tail entry.
    machine: Machine,
}

/// A tail entry's machine id (`JournalFile::tail_machine_id`), read only
/// when the boot table asks for it.
#[derive(Debug, Default, Clone, Copy)]
enum Machine {
    #[default]
    Unread,
    Read(Option<[u8; 16]>),
}

impl TailCache {
    /// Read `file`'s tail again if its header names another one.
    fn update(&mut self, file: &JournalFile) {
        let h = &file.header;
        let key = (h.n_entries, h.tail_entry_offset, h.tail_object_offset);
        if self.key == Some(key) {
            return;
        }
        *self = Self {
            key: Some(key),
            tail: file.tail(None),
            ..Self::default()
        };
        // `tail` falls back to the main list's newest entry only when the
        // tail the header names can't be read: walk the list only then.
        if self.tail.is_none() && h.tail_entry_offset.unwrap_or(h.tail_object_offset) != 0 {
            self.newest = file.all_entries_rev().find_map(|r| {
                let off = r.ok()?;
                Some((off, file.entry(off).ok()?))
            });
            self.tail = file.tail(self.newest.as_ref());
        }
    }

    /// The tail entry's machine id (`JournalFile::tail_machine_id`), read
    /// once per tail.
    fn machine(&mut self, file: &JournalFile) -> Option<[u8; 16]> {
        if let Machine::Read(id) = self.machine {
            return id;
        }
        let id = file.tail_machine_id(self.newest.as_ref());
        self.machine = Machine::Read(id);
        id
    }
}

#[derive(Debug)]
struct Followed {
    file: JournalFile,
    /// Entries of the file's entry list read so far.
    pos: u64,
    cursor: Option<ArrayCursor>,
    /// Where entries already shown may be (`Copies`).
    copies: Copies,
    /// Per group, per term, per value: its DATA object here, once it
    /// exists (a value seen for the first time gets one later).
    resolved: Vec<Vec<Vec<Option<u64>>>>,
    /// A lookup here failed and that was reported: one report per file.
    lookup_reported: bool,
    /// Rescans in a row that didn't list this file: 0 while it is still
    /// listed in the journal directories.
    unlisted: u8,
    /// Per kind (`Wait as usize`), the position whose read failed on an
    /// earlier drain: it is retried once, as the file may have been caught
    /// mid-write ("Concurrency"), then reported. One slot per kind, so
    /// failures of two kinds alternating at one position can't keep
    /// erasing each other's mark and wait forever.
    failed: [Option<u64>; 4],
    /// The file couldn't be read twice in a row and that was reported;
    /// cleared by the next good read.
    stuck: bool,
    /// Archived or renamed to `.journal~`, seen by a header read: no
    /// further writes come.
    sealed: bool,
    /// Its tail, for the boot table.
    tail: TailCache,
    /// Whether the page read it (`Origin`).
    origin: Origin,
}

/// Whether the page read a followed file: its inode and file id were in
/// the page's end (`Follow::start`), or the follow found it new (at its
/// start, or by a rescan), where a flush can have copied entries the page
/// showed (`take_copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Page,
    New,
}

/// Which of a followed file's entries may be ones already shown, so they
/// are checked against the last entry shown (`seen`): copies a flush
/// wrote to a file found while following. Past them the position alone
/// decides, so an entry a slower file delivers late is still shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Copies {
    /// Those numbered below this.
    Until(u64),
    /// A file opened while following: its leading entries, up to the
    /// first one that is new (a flush writes all its copies before any new
    /// entry).
    Leading,
}

/// The follow's files and positions, read by `poll`; `Follower` adds the
/// watches.
#[derive(Debug)]
struct Follow {
    files: Vec<Followed>,
    last: Option<Last>,
    /// Errors opening files (at the start and by rescans) and watching,
    /// shown with the next poll's.
    pending: Vec<ReadError>,
    /// Listed files that failed to open at the last rescan.
    unopened: Vec<PathBuf>,
    /// Opens the files a rescan finds, counting as the journal's opener.
    opener: Opener,
    /// The release's order between boots.
    rule: BootRule,
    /// What the page showed past its end's newest entry (`PageEnd`): one
    /// copy of each is dropped from a file the page didn't read. At most
    /// the page's records, and only used up, never cleared: a copy can come
    /// at a later poll (a flush after the follow started).
    shown: Vec<Copied>,
    /// The most payload bytes a poll's cache held after reading a file.
    #[cfg(test)]
    held_peak: usize,
}

impl Follow {
    /// Continue from `from`, a page's end (journal spec, "Starting a
    /// follow"), with its newest entry as the last one shown. A file the
    /// page read (its inode and file id) goes on from the entry count it
    /// recorded; one it didn't from after its newest entry at or below
    /// that newest entry (`first_new`). Every entry from there on was
    /// written after the page, and none is checked against the page's
    /// newest entry, so an entry a slower file delivers late is still
    /// shown; a copy among them of an entry another file gives in the same
    /// poll is the merge's to drop. Flush copies appended to a /var file
    /// the page read, of /run entries the page saw (a page and follow on
    /// either side of a flush), are shown again, as Difference 13 says of
    /// a followed file. From a file the page didn't read (a /var file a
    /// flush created, or one a later rescan finds), one copy of each entry
    /// the page showed past its newest is dropped (`take_copy`), so those
    /// aren't shown twice. `pending` are the errors opening the files gave.
    fn start(
        files: Vec<JournalFile>,
        from: &PageEnd,
        opener: Opener,
        rule: BootRule,
        pending: Vec<ReadError>,
    ) -> Self {
        let mut follow = Self {
            files: files.into_iter().map(|f| Followed::new(f, 0)).collect(),
            last: from.newest.as_ref().map(|(location, dir)| Last {
                location: *location,
                dir: dir.clone(),
                burst: Burst::of(*location),
            }),
            pending,
            shown: from.shown.clone(),
            unopened: Vec::new(),
            opener,
            rule,
            #[cfg(test)]
            held_peak: 0,
        };
        // The burst the page ended at: the entries at its newest's stamp
        // in that entry's directory (follow-M1), read once here rather
        // than by every page.
        if let Some(l) = follow.last.as_mut() {
            for f in &follow.files {
                if l.in_dir_of(&f.file) {
                    l.burst.page_end(&f.file);
                }
            }
        }
        let boots = follow.boots();
        let last = follow.last.as_ref();
        for f in &mut follow.files {
            let h = &f.file.header;
            // The same file, not a new one journald made at the inode once
            // the page's was freed (ext4 reuses inodes): its file id, and no
            // fewer entries than the page counted.
            let read = from.files.iter().find(|e| {
                e.inode == f.file.inode && e.file_id == h.file_id && e.n_entries <= h.n_entries
            });
            if read.is_some() {
                f.origin = Origin::Page;
            }
            f.pos = match read {
                Some(e) => e.n_entries,
                None => first_new(&f.file, last, &boots),
            };
        }
        follow
    }

    /// Match the open files to `paths` (the journal directories' listing,
    /// in search order): a known inode keeps its position under its new
    /// name, a new file is read from its start, and one no longer listed
    /// is read to its end, then closed once deleted. The errors one call
    /// adds are one per listed file at most, so they are bounded by the
    /// file count (a file that keeps failing to open is reported again
    /// at each later rescan).
    fn rescan(&mut self, paths: &[PathBuf]) {
        let mut old = std::mem::take(&mut self.files);
        let unopened = std::mem::take(&mut self.unopened);
        // Each name's own inode, not a symlink's target: a symlinked
        // journal file is opened, and refused, as a page opens it
        // (Difference 4). Read once, so the inodes listed and the ones
        // matched below are the same (a rename over a name between two
        // reads would list one and match another).
        let found: Vec<(&PathBuf, (u64, u64))> = paths
            .iter()
            .filter_map(|p| {
                let meta = std::fs::symlink_metadata(p).ok()?;
                Some((p, (meta.dev(), meta.ino())))
            })
            .collect();
        // The inodes listed now: an open file still among them is no
        // file's copy, whatever ids two files share.
        let listed: HashSet<(u64, u64)> = found.iter().map(|&(_, inode)| inode).collect();
        for (p, inode) in found {
            if let Some(i) = old.iter().position(|f| f.file.inode == inode) {
                let mut f = old.swap_remove(i);
                f.file.path.clone_from(p);
                f.unlisted = 0;
                self.files.push(f);
                continue;
            }
            match JournalFile::open_with(p, &self.opener) {
                // The same file under a new inode: where journald can't
                // clear FS_NOCOW_FL on an archived file (btrfs; overlayfs
                // in follow.sh's containers) it rewrites it as a copy
                // renamed over the name (journal-file-util.c,
                // copy_file_atomic_at_full). The file id is in the copied
                // header, and the offsets are the same, so its position
                // carries over instead of the whole file being read again.
                // Only from a file whose inode is no longer listed, as the
                // copy replaced it: when both inodes are listed they are
                // two files (a journal file copied with `cp`, or crafted
                // files whose ids are zero), as sd-journal tells files
                // apart by path and inode (`add_any_file`, systemd v255
                // sd-journal.c).
                Ok(file) => {
                    let same = old.iter().position(|f| {
                        f.file.header.file_id == file.header.file_id
                            && !listed.contains(&f.file.inode)
                    });
                    if let Some(i) = same {
                        let mut f = old.swap_remove(i);
                        f.file = file;
                        f.unlisted = 0;
                        self.files.push(f);
                    } else {
                        let mut f = Followed::new(file, 0);
                        f.copies = Copies::Leading;
                        self.files.push(f);
                    }
                }
                // A file journald is creating may not have its header yet:
                // it stays unopened, its next modification rescans, and
                // only a second failure is reported.
                Err(e) => {
                    if unopened.contains(p) {
                        self.pending.push(e);
                    }
                    self.unopened.push(p.clone());
                }
            }
        }
        for mut f in old {
            f.unlisted = f.unlisted.saturating_add(1);
            self.files.push(f);
        }
    }

    /// Whether `path` is one of the open files.
    fn has(&self, path: &Path) -> bool {
        self.files.iter().any(|f| f.file.path == path)
    }

    /// Whether `path` failed to open at the last rescan.
    fn is_unopened(&self, path: &Path) -> bool {
        self.unopened.iter().any(|p| p == path)
    }

    /// The order between boots for this poll: on 24.04 and 26.04 the
    /// table of the files' tails as their headers give them now, so it is
    /// kept current as files grow and appear (decision 11).
    fn boots(&mut self) -> BootOrder {
        let table = |files: &mut [Followed]| {
            for f in files.iter_mut() {
                f.tail.update(&f.file);
            }
            BootOrder::of(files.iter().map(|f| f.tail.tail))
        };
        match self.rule {
            BootRule::Realtime => BootOrder::none(),
            BootRule::NewestTail => table(&mut self.files),
            BootRule::NewestTailEntryMachine => {
                let files = &mut self.files;
                let mut boots = table(files);
                boots.name_machines(|i| {
                    let f = &mut files[i];
                    f.tail.machine(&f.file)
                });
                boots
            }
        }
    }

    /// The matching entries appended since the last poll, oldest first,
    /// and what went wrong. Entries already shown, and copies of them from
    /// a flush, are dropped (`Copies`, `seen`).
    ///
    /// Each file's new matches are decoded as they are read, with each
    /// DATA object read once per poll (`walk::record`'s cache, as a page
    /// uses), so a DATA object that fails to read is retried with its
    /// entry; copies of shown entries are dropped before decoding. The
    /// files' candidates are then merged (`merge`), ordered as the
    /// release's journalctl orders them (`boots`).
    ///
    /// Each file reads within the output limit (`limit`) per poll, as a
    /// page does: a file stops at the entry that doesn't fit, without
    /// moving past it, so the next poll goes on from there and nothing is
    /// lost (`Batch::more`). An entry larger than the whole limit is
    /// skipped, reported, and what it read leaves the cache with it. The
    /// errors one poll reports are capped as a page's are.
    fn poll(&mut self, groups: &[Group], limit: usize) -> Batch {
        let mut errors = ReadErrors::default();
        for e in self.pending.drain(..) {
            errors.add_open(e);
        }
        // Every header first, so the boot table is the files' as of now;
        // and every entry read within the limit, as a page bounds them.
        let fresh: Vec<bool> = self
            .files
            .iter_mut()
            .map(|f| {
                f.file.bound_entries(limit);
                f.refresh(&mut errors)
            })
            .collect();
        let boots = self.boots();
        let mut streams: Vec<Vec<Candidate>> = Vec::new();
        let mut decoded: HashMap<(usize, u64), Record> = HashMap::new();
        let mut payloads = Payloads::within(limit);
        let mut reached = false;
        let shown = &mut self.shown;
        for (i, f) in self.files.iter_mut().enumerate() {
            // Each file its own limit: one file's burst can't defer
            // another's entries past a `last` they are older than (where
            // `Copies` would take them for copies), and an entry
            // larger than the limit is first in its file's next poll, so
            // it is skipped rather than deferred again behind a busy file.
            // The poll holds at most the open files' limits together; only
            // the files still written have new entries.
            let mut budget = Budget {
                limit,
                left: limit,
                reached: false,
            };
            let mut candidates = Vec::new();
            if fresh[i] {
                let read = Read {
                    file: i,
                    groups,
                    last: self.last.as_ref(),
                    boots: &boots,
                };
                let stream = f.read_new(&read, &mut payloads, &mut budget, &mut errors);
                #[cfg(test)]
                {
                    self.held_peak = self.held_peak.max(payloads.held());
                }
                for (c, r) in stream {
                    // A copy of an entry the page showed, in a file the page
                    // didn't read (a flush between the page and the follow).
                    let dir = f.file.path.parent();
                    if f.origin == Origin::New && take_copy(shown, &c.location, dir) {
                        continue;
                    }
                    decoded.insert((c.file, c.offset), r);
                    candidates.push(c);
                }
            }
            reached |= budget.reached;
            streams.push(candidates);
        }
        let dirs: Vec<Option<&Path>> = self.files.iter().map(|f| f.file.path.parent()).collect();
        // Taken for the merge, and given back with what it took.
        let burst = self
            .last
            .as_mut()
            .map_or_default(|l| std::mem::take(&mut l.burst));
        let (picked, burst) = merge(&streams, |a, b| dirs[a] == dirs[b], &boots, burst);
        let out: Vec<Record> = picked
            .iter()
            .filter_map(|c| decoded.remove(&(c.file, c.offset)))
            .collect();
        if let Some(c) = picked.last() {
            self.last = Some(Last {
                location: c.location,
                dir: dirs[c.file].map(Path::to_path_buf),
                burst,
            });
        } else if let Some(l) = self.last.as_mut() {
            l.burst = burst;
        }
        self.files.retain(Followed::keep);
        Batch {
            records: out,
            more: reached,
            errors,
        }
    }
}

/// One poll's output limit for one file: what is left, and whether the
/// file stopped at it.
#[derive(Debug)]
struct Budget {
    limit: usize,
    left: usize,
    reached: bool,
}

/// What a file's read in one poll is checked against.
struct Read<'a> {
    /// The file's index, for its candidates.
    file: usize,
    groups: &'a [Group],
    last: Option<&'a Last>,
    boots: &'a BootOrder,
}

impl Followed {
    /// The query's values are looked up at the first read with entries
    /// (`read_new`), where a failed lookup can be reported.
    fn new(file: JournalFile, pos: u64) -> Self {
        Self {
            file,
            pos,
            cursor: None,
            copies: Copies::Until(0),
            resolved: Vec::new(),
            lookup_reported: false,
            unlisted: 0,
            failed: [None; 4],
            stuck: false,
            sealed: false,
            tail: TailCache::default(),
            origin: Origin::New,
        }
    }

    /// Whether to keep the file open after a poll. One no longer listed
    /// is kept while it still has a name (renamed between a listing and
    /// the stat of its old name, as a rotation can be; the next rescan
    /// finds it), at most two rescans, and once deleted until read to its
    /// end or reported unreadable.
    fn keep(&self) -> bool {
        if self.listed() {
            return true;
        }
        let linked = self.file.metadata().is_ok_and(|m| m.nlink() > 0);
        if linked && self.unlisted < 2 {
            return true;
        }
        !self.stuck && self.pos < self.file.header.n_entries
    }

    /// Still listed in the journal directories at the last rescan.
    fn listed(&self) -> bool {
        self.unlisted == 0
    }

    /// Read the header again, unless the file is sealed and read to its
    /// end; whether to read its entries now. A header that can't be read
    /// is waited for once, then reported.
    fn refresh(&mut self, errors: &mut ReadErrors) -> bool {
        // A file sealed (archived, or renamed to `.journal~` by a journald
        // that found it dirty) and read to its end is never written again.
        // Sealed is only set from a header read after the seal, so its
        // count is final.
        if self.sealed && self.pos >= self.file.header.n_entries {
            return false;
        }
        if let Err(e) = self.file.refresh() {
            self.stuck = self.failure(Wait::Header, e, errors);
            return false;
        }
        self.sealed = self.file.header.archived()
            || self.file.path.extension().is_some_and(|x| x == "journal~");
        true
    }

    /// Look up the values not found here yet: a few reads of the hash
    /// table each. A failed lookup leaves its value unresolved, looked up
    /// again on the next poll (an entry carrying it doesn't match until
    /// then: Difference 16); the first failure in this file is reported,
    /// once.
    fn resolve(&mut self, groups: &[Group], errors: &mut ReadErrors) {
        if self.resolved.is_empty() {
            self.resolved = groups
                .iter()
                .map(|g| g.iter().map(|t| vec![None; t.len()]).collect())
                .collect();
        }
        for (g, group) in groups.iter().enumerate() {
            for (t, term) in group.iter().enumerate() {
                for (v, payload) in term.iter().enumerate() {
                    if self.resolved[g][t][v].is_some() {
                        continue;
                    }
                    match self.file.find_data(payload) {
                        Ok(Some(d)) => self.resolved[g][t][v] = Some(d.offset),
                        Ok(None) => {}
                        Err(e) => {
                            if !self.lookup_reported {
                                self.lookup_reported = true;
                                errors.add(ReadError {
                                    kind: crate::file::in_context(
                                        "a lookup failed; entries with that value are left out until it reads",
                                        e.kind,
                                    ),
                                    path: e.path,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    /// An empty query matches all; otherwise some group whose every term
    /// has a value among the entry's items. Matching by the items, not by
    /// the values' entry lists as a page walks them, is why the follow
    /// shows an entry journald was cut off from linking (Difference 12).
    fn matches(&self, groups: &[Group], e: &Entry) -> bool {
        groups.is_empty()
            || self.resolved.iter().any(|group| {
                group.iter().all(|term| {
                    term.iter()
                        .any(|v| v.is_some_and(|off| e.items.contains(&off)))
                })
            })
    }

    /// Whether `kind` already had its wait at this position: if not, it
    /// is marked and the caller waits for the next poll.
    fn waited(&mut self, kind: Wait) -> bool {
        if self.failed[kind as usize] == Some(self.pos) {
            return true;
        }
        self.failed[kind as usize] = Some(self.pos);
        false
    }

    /// A read of the file failed at `pos`: the first time it is retried on
    /// the next poll; the second time it is reported (unless the file is
    /// deleted and was already read to its end: vacuum deallocates a file
    /// before it unlinks it, so it can read as zeros) and true is returned.
    fn failure(&mut self, kind: Wait, e: ReadError, errors: &mut ReadErrors) -> bool {
        if !self.waited(kind) {
            return false;
        }
        let drained = !self.listed() && self.pos >= self.file.header.n_entries;
        if !self.stuck && !drained {
            errors.add(e);
        }
        true
    }

    /// The matching entries appended since the last read, with their
    /// records, in file order. The header was read just before
    /// (`refresh`).
    fn read_new(
        &mut self,
        read: &Read<'_>,
        payloads: &mut Payloads,
        budget: &mut Budget,
        errors: &mut ReadErrors,
    ) -> Vec<(Candidate, Record)> {
        let mut out = Vec::new();
        let same_dir = read.last.is_some_and(|l| l.in_dir_of(&self.file));
        let mut resolved = false;
        // Rounds: an item that can't be an entry ends a read
        // (`entries_since`); once it is skipped, the entries after it are
        // read in the same poll.
        loop {
            let offsets = match self.file.entries_since(&mut self.cursor, self.pos) {
                Ok(o) => o,
                // An array out of order (a stale slot, Difference 17), or
                // one that can't be read: waited for once, then reported,
                // and the file read no further while it stays so.
                Err(e) => {
                    self.stuck = self.failure(Wait::List, e, errors);
                    return out;
                }
            };
            self.stuck = false;
            if offsets.is_empty() {
                // A failed header read on an earlier poll is behind us.
                self.failed = [None; 4];
                return out;
            }
            if !resolved && !read.groups.is_empty() {
                self.resolve(read.groups, errors);
                resolved = true;
            }
            let mut skipped_slot = false;
            for (k, &off) in offsets.iter().enumerate() {
                // Entries never overlap (journald appends): one must end
                // before the next, checked before its body is read.
                let end = offsets
                    .get(k + 1)
                    .copied()
                    .filter(|&o| self.file.plausible_entry(o))
                    .unwrap_or(u64::MAX);
                // A slot not written below n_entries, or an item that can't
                // be an entry offset: waited for once (it may be
                // mid-write), then skipped, as `EntriesRev` skips it, and a
                // bogus one reported.
                if !self.file.plausible_entry(off) {
                    if self.waited(Wait::Slot) {
                        if off != 0 {
                            errors.add(self.file.error(ReadErrorKind::Corrupt(format!(
                                "entry {} of the entry list is {off:#x}, not an entry offset; skipped",
                                self.pos
                            ))));
                        }
                        self.failed = [None; 4];
                        self.pos += 1;
                        skipped_slot = true;
                        continue;
                    }
                    return out;
                }
                let left_before = budget.left;
                match self.read_entry(read, (off, end), same_dir, payloads, &mut budget.left) {
                    Ok(c) => out.extend(c),
                    // At the limit: stop here, unmoved, for the next poll.
                    // An entry larger than the whole limit is skipped,
                    // reported.
                    Err(e) if e.is_over_limit() => {
                        if left_before < budget.limit {
                            budget.reached = true;
                            return out;
                        }
                        errors.add(ReadError {
                            kind: ReadErrorKind::TooLarge {
                                context: e.kind.to_string(),
                                limit: budget.limit,
                            },
                            path: e.path,
                        });
                        // Nothing of this file was read before it, so its
                        // part of the cache is this entry's alone: dropped
                        // with it, or each entry skipped would hold up to
                        // a limit more.
                        payloads.forget(read.file);
                        budget.left = budget.limit;
                    }
                    // A DATA object this poll already found unreadable,
                    // after its wait: skipped at once.
                    Err(e) if e.is_known_bad() => errors.add(e),
                    // Skipped once reported.
                    Err(e) => {
                        if !self.failure(Wait::Entry, e, errors) {
                            return out;
                        }
                    }
                }
                self.failed = [None; 4];
                self.pos += 1;
            }
            if !skipped_slot {
                return out;
            }
        }
    }

    /// The entry at `at` (its offset, and where the next one starts) as
    /// a candidate with its record, `None` when it doesn't match or is a
    /// copy of one shown. Its fields are read within `left` bytes.
    fn read_entry(
        &mut self,
        read: &Read<'_>,
        (off, end): (u64, u64),
        same_dir: bool,
        payloads: &mut Payloads,
        left: &mut usize,
    ) -> Result<Option<(Candidate, Record)>, ReadError> {
        let e = self.file.entry_before(off, end)?;
        let location = walk::location(&self.file, &e);
        let shown = |l: &Last| seen(&location, &l.location, &l.burst, same_dir, read.boots);
        // The first entry newer than the last shown ends a new file's
        // leading copies, whether it matches or not.
        if self.copies == Copies::Leading && !read.last.is_some_and(shown) {
            self.copies = Copies::Until(self.pos);
        }
        // Where an entry already shown can be (`Copies`), one not newer than
        // the last shown is a copy: dropped before it is decoded.
        let until = match self.copies {
            Copies::Until(n) => n,
            Copies::Leading => u64::MAX,
        };
        let copy = self.pos < until && read.last.is_some_and(shown);
        if copy || !self.matches(read.groups, &e) {
            return Ok(None);
        }
        // Decoded here, so a DATA object that fails to read is retried on
        // the next poll like the entry itself. Unlike a page's decode of
        // only what is shown, the merge can still drop one of these (a
        // same-poll flush copy); that is around a flush only.
        let r = walk::record(&self.file, &e, payloads, read.file, left)?;
        let c = Candidate {
            location,
            file: read.file,
            offset: off,
        };
        Ok(Some((c, r)))
    }
}

/// Whether a followed entry `l` is one already shown, or older than it,
/// against `last`. Within the directory `last` came from (`same_dir`):
/// sd-journal's order, so entries journald wrote again there (systemd 249
/// repeats an entry as the next file's first when a rotation interrupts
/// it, and splits a long line into entries with equal times and hash) are
/// all shown, as journalctl shows them. From another directory: `is_old`,
/// which drops a flush's copies; a flush always crosses roots (/run to
/// /var). When `last`'s directory is unknown, `is_old`, and a copy of
/// any entry of `last`'s `burst` is old too: journald stamps a burst
/// alike, so `is_old` alone would take only `last`'s own copy for one
/// (follow-M1). Not within the directory, where entries with equal times
/// and hash are all journalctl's to show.
fn seen(l: &Cursor, last: &Cursor, burst: &Burst, same_dir: bool, boots: &BootOrder) -> bool {
    if same_dir {
        !l.order(last, boots).is_gt()
    } else {
        is_old(l, last, boots) || burst.has(l)
    }
}

/// Whether `l`, an entry of a file in `dir`, is a copy of an entry the page
/// showed past its end's newest entry (`shown`, `PageEnd`), in another
/// directory (`Copied::copied_as`): if so, that one is used up, so a second
/// copy, or a later entry that happens to match, is shown. The order of
/// `shown` doesn't matter: each is matched on its own.
fn take_copy(shown: &mut Vec<Copied>, l: &Cursor, dir: Option<&Path>) -> bool {
    match shown.iter().position(|s| s.copied_as(l, dir)) {
        Some(i) => {
            shown.swap_remove(i);
            true
        }
        None => false,
    }
}

/// Where a followed file the page didn't read goes on (journal spec,
/// "Starting a follow"): after its newest entry that `seen` takes for
/// `last` or older, walked back from its newest entry; from its start
/// without `last` or such an entry. `seen` is monotone within a file, so
/// the first such entry met ends the walk, which reads only what was
/// written after `last` and that one entry. An entry or array that can't
/// be read is passed over, so the follow's own read meets it (waited for,
/// then reported) rather than skip what lies after it.
fn first_new(file: &JournalFile, last: Option<&Last>, boots: &BootOrder) -> u64 {
    let Some(last) = last else { return 0 };
    let same_dir = last.in_dir_of(file);
    let mut entries = file.all_entries_rev();
    while let Some(r) = entries.next() {
        let Some(e) = r.ok().and_then(|off| file.entry(off).ok()) else {
            continue;
        };
        if seen(
            &walk::location(file, &e),
            &last.location,
            &last.burst,
            same_dir,
            boots,
        ) {
            return entries.index() + 1;
        }
    }
    0
}

/// Whether an entry is one already shown, or older than it: by time
/// within the boot of `last`, not by sequence number. Flushing to /var
/// copies the runtime journal's entries with the same boot, monotonic and
/// realtime timestamps and XOR hash; systemd 255 gives the copies the
/// runtime file's sequence-number id and new, higher seqnums, so
/// sd-journal's order calls them newer and `journalctl -f` 255 prints
/// them twice (follow.sh, flush-var; Difference 2). Monotonic time within
/// a boot only grows, so a copy is older than the last entry shown, or
/// equal to it. Across boots this falls back to sd-journal's order.
fn is_old(l: &Cursor, last: &Cursor, boots: &BootOrder) -> bool {
    if l.boot_id == last.boot_id {
        l.monotonic < last.monotonic
            || (l.monotonic == last.monotonic
                && l.realtime == last.realtime
                && l.xor_hash == last.xor_hash)
    } else {
        !l.order(last, boots).is_gt()
    }
}

/// Merge a poll's per-file streams, each already in file order, the way
/// sd-journal's `next_beyond_location` does (v249 to v259 sd-journal.c):
/// every file first moves past heads already shown (`seen` against the
/// last entry taken), then the smallest head by sd-journal's order with
/// the release's `boots` is taken, the earlier file on a tie. Sorting with
/// that comparator instead could panic: it isn't a total order across
/// sequence-number ids and boots (realtime fallback, clock jumps, the boot
/// table), and a head ordered before the last one taken is dropped, as
/// sd-journal drops it, not shown out of order. Why not itertools'
/// `kmerge_by`: its heap needs a transitive order and has no tie rule
/// (`walk::Merge` spells out the same loop for the page).
///
/// `burst` is the last poll's (`Last::burst`), extended by each entry
/// taken and returned; it is checked from the second head on, as the
/// first is checked against the last poll's entry in `read_entry`.
///
/// `same_dir` tells whether two candidates' files (by index) share a
/// directory. Within one, the skip is sd-journal's not-beyond rule, as
/// `journalctl -f` goes on with `sd_journal_next`. Across directories it is
/// `is_old`, which also hides a flush's copies: systemd 255's have the
/// originals' times but higher seqnums under the same seqnum id, so
/// sd-journal's order would call them newer; 249's (and the archived
/// runtime file left in /run) are under another seqnum id. Files are read
/// in sd-journal's search order (/run before /var), so the copy kept is
/// the one journalctl shows.
fn merge(
    streams: &[Vec<Candidate>],
    same_dir: impl Fn(usize, usize) -> bool,
    boots: &BootOrder,
    mut burst: Burst,
) -> (Vec<Candidate>, Burst) {
    // One position per stream; the head of stream `i` is streams[i][pos[i]].
    let mut pos = vec![0usize; streams.len()];
    let mut out: Vec<Candidate> = Vec::new();
    loop {
        if let Some(last) = out.last() {
            for (i, s) in streams.iter().enumerate() {
                while s.get(pos[i]).is_some_and(|h| {
                    seen(
                        &h.location,
                        &last.location,
                        &burst,
                        same_dir(h.file, last.file),
                        boots,
                    )
                }) {
                    pos[i] += 1;
                }
            }
        }
        let mut best: Option<usize> = None;
        for (i, s) in streams.iter().enumerate() {
            let Some(head) = s.get(pos[i]) else { continue };
            let better = best.is_none_or(|b| {
                head.location
                    .order(&streams[b][pos[b]].location, boots)
                    .is_lt()
            });
            if better {
                best = Some(i);
            }
        }
        let Some(b) = best else { return (out, burst) };
        let next = streams[b][pos[b]].clone();
        pos[b] += 1;
        burst.took(next.location);
        out.push(next);
    }
}

/// What one inotify event asks of the follow.
#[derive(Debug, PartialEq, Eq)]
enum Wake {
    /// A journal file was written to.
    Modified(PathBuf),
    /// The file set may have changed: list the directories again.
    Rescan,
    /// Nothing journal related (another file in the directory).
    Ignore,
}

/// Classify an event the way sd-journal's `process_inotify_event` does: a
/// queue overflow rescans (`process_q_overflow`); an event on a journal
/// file is a modification or, for a create, rename or delete, a rescan;
/// a directory event, a watch dropped (`IN_IGNORED`) or an unknown watch
/// rescans; other names are ignored. Positions live in the open files,
/// not in the events, so an overflow loses nothing.
fn classify(dirs: &HashMap<i32, PathBuf>, wd: i32, flags: ReadFlags, name: Option<&CStr>) -> Wake {
    if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
        return Wake::Rescan;
    }
    let (Some(dir), Some(name)) = (dirs.get(&wd), name) else {
        return Wake::Rescan;
    };
    if flags.contains(ReadFlags::ISDIR) {
        return Wake::Rescan;
    }
    let name = name.to_bytes();
    // Names as bytes, as the listing takes them (`walk::scan`).
    if !walk::is_journal_name(name) {
        return Wake::Ignore;
    }
    let p = dir.join(OsStr::from_bytes(name));
    let modified = ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE;
    if !flags.is_empty() && modified.contains(flags) {
        Wake::Modified(p)
    } else {
        Wake::Rescan
    }
}

/// The directories to watch while following: those holding the files
/// `listing` names, a `Source::Directory`'s own, and for the local journal
/// both machine-id directories, or their root when one doesn't exist yet
/// (journald creates it on its first write or flush), so following doesn't
/// wait on no watch while a root exists. journalctl 259 doesn't watch a machine-id
/// directory that didn't exist when it started, and loses what is written
/// after a flush creates it (Difference 2); sb finds it through its
/// root's watch.
fn watch_dirs(roots: &Roots, listing: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = listing
        .iter()
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .collect();
    match roots {
        Roots::Local(ids) => {
            for dir in ids {
                if dir.is_dir() {
                    dirs.push(dir.clone());
                } else if let Some(root) = dir.parent().filter(|r| r.is_dir()) {
                    dirs.push(root.to_path_buf());
                }
            }
        }
        Roots::Directory(d) => dirs.push(d.clone()),
        Roots::Files(_) => {}
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Watch every directory `roots` reads from now (`watch_dirs`), recording
/// which directory each watch is; the directories that couldn't be
/// watched, with the system's error. Adding a watch twice returns the same
/// one (inotify(7)). A directory gone between listing and watching (the
/// runtime journal after a flush) is no error: its root's watch stays,
/// so following goes on.
fn watch(
    fd: &OwnedFd,
    roots: &Roots,
    dirs: &mut HashMap<i32, PathBuf>,
) -> Vec<(PathBuf, std::io::Error)> {
    let mut failed = Vec::new();
    for d in watch_dirs(roots, &roots.list().paths) {
        let watched = inotify::add_watch(
            fd,
            &d,
            WatchFlags::MODIFY
                | WatchFlags::CREATE
                | WatchFlags::MOVED_TO
                | WatchFlags::MOVED_FROM
                | WatchFlags::DELETE
                | WatchFlags::CLOSE_WRITE,
        );
        match watched {
            Ok(wd) => {
                dirs.insert(wd, d);
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(e) => failed.push((d, e.into())),
        }
    }
    failed
}

/// The inotify read buffer: room for many events (each at most 16 bytes
/// and a `NAME_MAX` name).
const EVENTS_BUF: usize = 8192;

/// A follow of a journal (`Journal::follow`): its files kept open by
/// inode with a position each, and one inotify descriptor watching their
/// directories. `drain` gives what was written since the last call, and
/// never blocks; `wake_fd` is what to wait on between drains.
///
/// The watching follows rustix's inotify module and its example
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/inotify/index.html>): one
/// descriptor created `NONBLOCK`, `add_watch` per directory, and a
/// `Reader` over a buffer read until it fails with `WOULDBLOCK`.
///
/// Waiting for new entries (journal spec, "Waiting for new entries" and
/// decision 12): the caller's wait (sb-host's `journal::FollowWait`)
/// polls `wake_fd()` beside a stop `eventfd` under `spawn_blocking`, and
/// sb-host's module docs give the loop:
/// 1. wait until `wake_fd()` is readable (or the wait is stopped);
/// 2. run `drain` under `spawn_blocking` (the `Follower` is `Send`); it
///    reads the inotify descriptor to `EAGAIN`, which empties what the
///    duplicate shows, as both share one open file description;
/// 3. while the `Batch` has `more`, drain again;
/// 4. after a batch with errors, drain once more: the drain may not have
///    read the events to `EAGAIN`;
/// 5. wait again. `poll` is level-triggered, so there is no readiness to
///    clear.
///
/// Not tokio's `AsyncFd`: tokio 1.54.1 deprecates `AsyncFd::new` as
/// unsound (<https://github.com/tokio-rs/tokio/pull/8587>), and its
/// replacement, `AsyncFd::register`, is an `unsafe fn`, which the
/// workspace forbids.
///
/// The crate itself spawns nothing and doesn't depend on tokio (spec,
/// assumption 3); its tests wait with rustix's `poll`.
///
/// Why not the `notify` crate (8.2), which keeps the same wd-to-path map
/// and turns an overflow into a rescan flag: it runs its own thread and
/// channel, where the crate spawns no thread (journal spec, assumption 3),
/// its path bookkeeping would duplicate `watch_dirs`, and it isn't in sb's
/// tree; the map and the event classification are about 30 lines.
#[derive(Debug)]
pub struct Follower {
    follow: Follow,
    groups: Vec<Group>,
    roots: Roots,
    inotify: OwnedFd,
    /// Each watch's directory.
    dirs: HashMap<i32, PathBuf>,
    /// Journal files modified that a rescan didn't take (another file in a
    /// watched directory with `Source::Files`): they don't rescan again
    /// until the directory itself changes.
    ignored: Vec<PathBuf>,
}

impl Follower {
    /// `Journal::follow`: the watches first, so nothing written once the
    /// files are open waits for a later event, then the files, positioned
    /// from `from` (`Follow::start`), and the first batch, read as a drain
    /// reads. The walk back of a file the page didn't read drops its read
    /// errors: what it can't read there it passes over, and the batch's own
    /// read meets and reports it. The errors opening the files come with
    /// the first batch.
    pub(crate) fn start(
        journal: &Journal,
        q: Query,
        from: &PageEnd,
        limits: Limits,
    ) -> Result<(Self, Batch), OpenError> {
        // Non-blocking, so `drain` reads it to `EAGAIN` and never waits
        // (rustix's inotify example); close-on-exec, so no child of the
        // caller inherits it.
        let inotify = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
            .map_err(|e| OpenError::Inotify { source: e.into() })?;
        let mut dirs = HashMap::new();
        if let Some((path, source)) = watch(&inotify, &journal.roots, &mut dirs)
            .into_iter()
            .next()
        {
            return Err(OpenError::Watch { path, source });
        }
        // With no watch, no event could ever make the wake descriptor
        // readable: refused rather than waited on for ever.
        if dirs.is_empty() {
            return Err(OpenError::NothingToWatch);
        }
        let mut errors = ReadErrors::default();
        let files = walk::open_listed(
            || journal.list(),
            &journal.opener,
            limits.output(),
            &mut errors,
        );
        let mut follow = Follow::start(
            files,
            from,
            journal.opener.clone(),
            journal.release.boot_rule(),
            errors.shown,
        );
        let groups = q.into_groups();
        let batch = follow.poll(&groups, limits.output());
        let follower = Self {
            follow,
            groups,
            roots: journal.roots.clone(),
            inotify,
            dirs,
            ignored: Vec::new(),
        };
        Ok((follower, batch))
    }

    /// A duplicate of the follow's inotify descriptor, for the caller's
    /// readiness wait (see `Follower`): readable once a watched directory
    /// has an event `drain` hasn't read. It is close-on-exec (rustix's
    /// `fcntl_dupfd_cloexec`, `F_DUPFD_CLOEXEC`,
    /// <https://docs.rs/rustix/1.1.6/rustix/io/fn.fcntl_dupfd_cloexec.html>),
    /// and non-blocking as the follow's own is: `O_NONBLOCK` belongs to the
    /// open file description the two share (fcntl(2)), and so does the
    /// event queue `drain` empties.
    ///
    /// # Errors
    ///
    /// The system's, when the descriptor can't be duplicated (`EMFILE`
    /// past the process's descriptor limit).
    pub fn wake_fd(&self) -> std::io::Result<OwnedFd> {
        Ok(rustix::io::fcntl_dupfd_cloexec(&self.inotify, 0)?)
    }

    /// The matching entries written since the last drain (or the start).
    ///
    /// They come oldest first, with what couldn't be read. It never
    /// blocks: it reads every event queued on the journal's directories (a
    /// write, a rotation, a vacuum, a flush) until the descriptor has none
    /// left (`EAGAIN`), so a burst of writes costs one read per file, not
    /// one per event, and the wake descriptor is no longer readable after
    /// it. Every drain reads every open file; the events only decide
    /// whether to list the directories again first. With no event, a drain
    /// is a header read per file and gives an empty batch.
    ///
    /// Each file reads at most `limits`' output limit per drain: one that
    /// stops at an entry that doesn't fit goes on from there at the next
    /// drain (`Batch::more`), and an entry larger than the whole limit is
    /// skipped and reported (Difference 5).
    /// Unreadable entries, slots and headers are waited for one drain
    /// (journald may be writing them), then reported once and skipped; a
    /// file whose entry list is out of order stops there (Difference 17).
    #[must_use]
    pub fn drain(&mut self, limits: &Limits) -> Batch {
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut rescan = false;
        let mut buf = [MaybeUninit::<u8>::uninit(); EVENTS_BUF];
        let mut reader = inotify::Reader::new(&self.inotify, &mut buf);
        loop {
            match reader.next() {
                Ok(e) => match classify(&self.dirs, e.wd(), e.events(), e.file_name()) {
                    Wake::Modified(p) => {
                        if !changed.contains(&p) {
                            changed.push(p);
                        }
                    }
                    Wake::Rescan => rescan = true,
                    Wake::Ignore => {}
                },
                // Every queued event read (rustix's example stops here).
                Err(rustix::io::Errno::WOULDBLOCK) => break,
                Err(rustix::io::Errno::INTR) => {}
                // The events are lost: list the directories again, as for
                // an overflow, and report it.
                Err(e) => {
                    self.follow
                        .pending
                        .push(ReadError::of_page(ReadErrorKind::Io {
                            context: "read the follow's inotify events".to_owned(),
                            source: e.into(),
                        }));
                    rescan = true;
                    break;
                }
            }
        }
        self.wake(&changed, rescan);
        self.follow.poll(&self.groups, limits.output())
    }

    /// Act on one drain's events: list the directories again when the file
    /// set may have changed, or when a modified journal file isn't open
    /// (created before its event was read, or not openable yet when it
    /// was).
    fn wake(&mut self, changed: &[PathBuf], mut rescan: bool) {
        if rescan {
            self.ignored.clear();
        }
        let follow = &self.follow;
        let ignored = &self.ignored;
        let unknown =
            |p: &PathBuf| !follow.has(p) && (follow.is_unopened(p) || !ignored.contains(p));
        if changed.iter().any(unknown) {
            rescan = true;
        }
        if !rescan {
            return;
        }
        // A directory may have appeared (journald creates the machine-id
        // directory on its first write or flush): watched from now on.
        for (path, source) in watch(&self.inotify, &self.roots, &mut self.dirs) {
            self.follow.pending.push(ReadError {
                path: Some(path),
                kind: ReadErrorKind::Io {
                    context: "watch the directory".to_owned(),
                    source,
                },
            });
        }
        let listing = self.roots.list();
        self.follow.pending.extend(listing.errors);
        self.follow.rescan(&listing.paths);
        for p in changed {
            if !self.follow.has(p) && !self.follow.is_unopened(p) && !self.ignored.contains(p) {
                self.ignored.push(p.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted::{self, *};
    use crate::{Field, Release, Source};

    /// A follow over `paths`, opened as a page opens them, polled under
    /// `limit` per file, with every poll's errors kept as text and its
    /// last `Batch::more`: the prototype's `Follow`, which its tests drive.
    struct T {
        f: Follow,
        limit: usize,
        errors: Vec<String>,
        more: bool,
    }

    impl T {
        fn of(paths: &[PathBuf]) -> Self {
            Self::start(paths, BootRule::Realtime)
        }

        fn start(paths: &[PathBuf], rule: BootRule) -> Self {
            let opener = Opener::default();
            let files: Vec<JournalFile> = paths
                .iter()
                .map(|p| JournalFile::open_with(p, &opener).expect("opens"))
                .collect();
            // Each file from its header's count, and no entry shown yet:
            // the prototype's start, which its tests assume.
            let from = PageEnd {
                files: files
                    .iter()
                    .map(|f| crate::FileEnd {
                        inode: f.inode,
                        file_id: f.header.file_id,
                        n_entries: f.header.n_entries,
                    })
                    .collect(),
                newest: None,
                shown: Vec::new(),
            };
            Self {
                f: Follow::start(files, &from, opener, rule, Vec::new()),
                limit: crate::OUTPUT_DEFAULT,
                errors: Vec::new(),
                more: false,
            }
        }

        /// What the follow's opener counted.
        fn counts(&self) -> crate::open::Counts {
            self.f.opener.counts()
        }

        fn poll(&mut self, groups: &[Group]) -> Vec<Record> {
            let batch = self.f.poll(groups, self.limit);
            let errors = batch.errors;
            self.errors.extend(errors.shown.iter().map(crate::key));
            if errors.omitted > 0 {
                self.errors.push(format!("{} more errors", errors.omitted));
            }
            self.more = batch.more;
            batch.records
        }

        fn rescan(&mut self, paths: &[PathBuf]) {
            self.f.rescan(paths);
        }

        fn has(&self, path: &Path) -> bool {
            self.f.has(path)
        }

        fn files(&self) -> usize {
            self.f.files.len()
        }
    }

    fn messages(records: &[Record]) -> Vec<String> {
        records
            .iter()
            .filter_map(Record::message)
            .map(|m| String::from_utf8_lossy(m).into_owned())
            .collect()
    }

    fn u_is_x() -> Vec<Group> {
        vec![vec![vec![b"U=x".to_vec()]]]
    }

    /// The output limit `k` entries of `crafted::journal` take: the shared
    /// `U=x` object once (16 + 48 + 3 bytes), then per entry its MESSAGE
    /// object (16 + 48 + 10) and two fields.
    fn per_record(k: usize) -> usize {
        per_record_in(Format::Regular, k)
    }

    /// `per_record` in `format`: a compact DATA object's header is 8 bytes
    /// longer.
    fn per_record_in(format: Format, k: usize) -> usize {
        let header = usize::try_from(format.payload_at()).expect("small");
        header + 3 + k * (header + 10 + 2 * std::mem::size_of::<Field>())
    }

    /// A crafted journal of `n` entries whose header counts only `shown`,
    /// followed from there: `grow` then counts them all, as journald
    /// appending would.
    fn followed(c: &mut Crafted, shown: u64) -> (tempfile::NamedTempFile, T) {
        let n = c.get(H_N_ENTRIES);
        c.set(H_N_ENTRIES, shown);
        let t = c.file();
        let f = T::of(&[t.path().to_path_buf()]);
        c.set(H_N_ENTRIES, n);
        (t, f)
    }

    fn grow(c: &Crafted, t: &tempfile::NamedTempFile) {
        std::fs::write(t.path(), &c.b).expect("grow");
    }

    /// Following past an unreadable entry: it is waited for once (it may be
    /// mid-write), then reported and skipped; the entries around it are
    /// shown.
    #[test]
    fn follow_skips_an_unreadable_entry_after_one_wait() {
        let (mut c, l) = crafted::journal(6);
        c.set(l.entries[4] + O_SIZE, 0);
        let (t, mut f) = followed(&mut c, 2);
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&u_is_x())), ["m2", "m3"]);
        // Waiting, not yet reported.
        assert!(f.errors.is_empty(), "{:?}", f.errors);
        assert_eq!(messages(&f.poll(&u_is_x())), ["m5"]);
        assert_eq!(f.errors.len(), 1, "{:?}", f.errors);
    }

    /// Following within the output limit: a poll stops at the entry that
    /// doesn't fit, unmoved, and the next poll goes on from it: nothing is
    /// lost or doubled.
    #[test]
    fn follow_over_the_output_limit_goes_on_at_the_next_poll() {
        let (mut c, _l) = crafted::journal(10);
        let (t, mut f) = followed(&mut c, 2);
        f.limit = per_record(3) + 5;
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&[])), ["m2", "m3", "m4"]);
        assert!(f.more, "the limit stopped the poll");
        assert!(f.errors.is_empty(), "nothing lost: {:?}", f.errors);
        assert_eq!(messages(&f.poll(&[])), ["m5", "m6", "m7"]);
        assert!(f.more, "the limit stopped the poll");
        assert_eq!(messages(&f.poll(&[])), ["m8", "m9"]);
        assert!(!f.more, "read to the end");
    }

    /// An entry larger than the whole limit is skipped, reported, and the
    /// follow goes on past it.
    #[test]
    fn follow_skips_an_entry_larger_than_the_limit() {
        let (mut c, l) = crafted::journal(4);
        let big = c.data(&[b'B'; 4000]);
        c.set(l.entries[2] + E_ITEMS, big);
        let (t, mut f) = followed(&mut c, 1);
        f.limit = 1000;
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&[])), ["m1"]);
        assert_eq!(messages(&f.poll(&[])), ["m3"]);
        assert!(
            f.errors
                .iter()
                .any(|e| e.contains("larger than the output limit")),
            "{:?}",
            f.errors
        );
    }

    /// Security audit (Task 11, M2): a follow bounds each entry it reads
    /// by its limit, as a page does: an entry listing more items than the
    /// limit could show is refused before its body is read, reported once
    /// the limit is whole, and the follow goes on past it.
    #[test]
    fn follow_refuses_an_entry_with_more_items_than_its_limit_shows() {
        // The third entry lists one object again and again: with its
        // message, one item more than a 1000-byte limit could show.
        let items = 1000 / std::mem::size_of::<Field>() + 1;
        let repeated: Vec<&[u8]> = vec![b"S=1"; items - 1];
        let (mut c, _e) = crafted::journal_of(&[&[], &[], &repeated, &[]]);
        let (t, mut f) = followed(&mut c, 1);
        f.limit = 1000;
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&[])), ["m1"]);
        assert_eq!(messages(&f.poll(&[])), ["m3"]);
        assert!(
            f.errors
                .iter()
                .any(|e| e.contains(&format!("lists {items} items"))),
            "{:?}",
            f.errors
        );
    }

    /// Each followed file has its own limit per poll: a busy file can't
    /// defer another's entries (where they could be taken for copies of
    /// ones shown), and an entry larger than the limit in the second file
    /// is skipped at once, not deferred behind the first file again.
    #[test]
    fn a_follow_limit_is_per_file() {
        let (mut a, _la) = crafted::journal(6);
        let (mut b, lb) = crafted::journal(6);
        // Another file: its own seqnum id, its entries later in the boot.
        b.b[72..88].copy_from_slice(&[2; 16]);
        for (i, &e) in lb.entries.iter().enumerate() {
            b.set(e + 16, 100 + i as u64);
            b.set(e + 32, 100 + i as u64);
        }
        let big = b.data(&[b'B'; 4000]);
        b.set(lb.entries[2] + E_ITEMS, big);
        a.set(H_N_ENTRIES, 2);
        b.set(H_N_ENTRIES, 2);
        let ta = a.file();
        let tb = b.file();
        let mut f = T::of(&[ta.path().to_path_buf(), tb.path().to_path_buf()]);
        f.limit = per_record(3) + 5;
        a.set(H_N_ENTRIES, 6);
        b.set(H_N_ENTRIES, 6);
        grow(&a, &ta);
        grow(&b, &tb);
        let got = f.poll(&[]);
        assert_eq!(messages(&got), ["m2", "m3", "m4", "m3", "m4", "m5"]);
        assert!(
            f.errors
                .iter()
                .any(|e| e.contains("larger than the output limit")),
            "{:?}",
            f.errors
        );
        // The first file's last entry, older than the second file's shown,
        // still comes: not taken for a copy.
        assert_eq!(messages(&f.poll(&[])), ["m5"]);
    }

    fn loc(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, xor: u64) -> Cursor {
        Cursor {
            seqnum_id: [seqnum_id; 16],
            seqnum,
            boot_id: [boot; 16],
            monotonic: mono,
            realtime: 1000 + mono,
            xor_hash: xor,
        }
    }

    /// systemd 255's flushed copies: same seqnum id, higher seqnums, the
    /// originals' times and hash. sd-journal's order calls them newer;
    /// follow drops them. A different entry at the same microsecond stays.
    #[test]
    fn is_old_drops_flushed_copies_by_time() {
        let none = BootOrder::none();
        let last = loc(1, 5, 9, 50, 7);
        let copy_of_earlier = loc(1, 8, 9, 30, 3);
        let copy_of_last = loc(1, 10, 9, 50, 7);
        assert!(copy_of_earlier.order(&last, &none).is_gt());
        assert!(is_old(&copy_of_earlier, &last, &none));
        assert!(is_old(&copy_of_last, &last, &none));
        assert!(!is_old(&loc(1, 11, 9, 50, 8), &last, &none));
        assert!(!is_old(&loc(2, 1, 9, 51, 0), &last, &none));
        // Another boot: sd-journal's order (realtime here).
        assert!(is_old(&loc(3, 1, 4, 10, 0), &last, &none));
        assert!(!is_old(&loc(3, 1, 4, 90, 0), &last, &none));
    }

    fn rec(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, real: u64) -> Candidate {
        Candidate {
            location: Cursor {
                seqnum_id: [seqnum_id; 16],
                seqnum,
                boot_id: [boot; 16],
                monotonic: mono,
                realtime: real,
                xor_hash: 0,
            },
            file: 0,
            offset: 0,
        }
    }

    /// Streams as candidates of files 0, 1, 2 and so on, in order.
    fn in_files(streams: Vec<Vec<Candidate>>) -> Vec<Vec<Candidate>> {
        streams
            .into_iter()
            .enumerate()
            .map(|(i, s)| s.into_iter().map(|c| Candidate { file: i, ..c }).collect())
            .collect()
    }

    /// A flushed copy (another seqnum id, same boot, times and hash) in a
    /// file of another directory is seen by `is_old` and appears once,
    /// from the earlier stream; the streams interleave.
    #[test]
    fn merge_interleaves_and_drops_flushed_copies() {
        let run = vec![rec(1, 1, 9, 10, 100), rec(1, 2, 9, 20, 200)];
        let var = vec![
            rec(2, 1, 9, 10, 100),
            rec(2, 2, 9, 20, 200),
            rec(2, 3, 9, 30, 300),
        ];
        let other = vec![rec(3, 7, 8, 5, 150)];
        let out = merge(
            &in_files(vec![run, var, other]),
            |a, b| a == b,
            &BootOrder::none(),
            Burst::default(),
        )
        .0;
        let got: Vec<(u8, u64)> = out
            .iter()
            .map(|r| (r.location.seqnum_id[0], r.location.realtime))
            .collect();
        assert_eq!(got, vec![(1, 100), (3, 150), (1, 200), (2, 300)]);
    }

    /// The comparator isn't transitive: x < y (same seqnum id), y < z (same
    /// boot), z < x (realtime). Once x is taken, z is seen (`seen`: another
    /// boot, so sd-journal's order, by either rule) and dropped, as
    /// sd-journal's `next_beyond_location` drops it, instead of being printed
    /// out of order after y.
    #[test]
    fn merge_drops_a_head_not_beyond_the_last_taken() {
        let none = BootOrder::none();
        let x = rec(1, 1, 0xa, 5, 300);
        let y = rec(1, 2, 0xb, 1, 100);
        let z = rec(2, 1, 0xb, 9, 200);
        assert!(x.location.order(&y.location, &none).is_lt());
        assert!(y.location.order(&z.location, &none).is_lt());
        assert!(z.location.order(&x.location, &none).is_lt());
        for one_dir in [false, true] {
            let streams = in_files(vec![vec![y.clone()], vec![z.clone()], vec![x.clone()]]);
            let (out, _) = merge(&streams, |a, b| one_dir || a == b, &none, Burst::default());
            let got: Vec<u64> = out.iter().map(|r| r.location.realtime).collect();
            assert_eq!(got, vec![300, 100], "one directory: {one_dir}");
        }
    }

    /// A journal file (regular, not compact) holding `entries` as
    /// (seqnum, monotonic) ENTRY objects, one boot, all in one entry array,
    /// the header counting `n` of them. Each entry lists one DATA object,
    /// `M=1`, the item an entry needs (sd-journal's `check_object`).
    fn journal(path: &Path, seqnum_id: u8, entries: &[(u64, u64)], n: u64) {
        journal_in(Format::Regular, path, seqnum_id, entries, n);
    }

    /// The sizes of `journal_in`'s objects in `format`, each padded to 8
    /// bytes as journald places objects: the entry array of `len` items,
    /// the DATA object and one ENTRY.
    fn sizes(format: Format, len: usize) -> (usize, usize, usize) {
        let width = usize::try_from(format.array_item_width()).expect("small");
        let entry_item: usize = if format == Format::Compact { 4 } else { 16 };
        let data = usize::try_from(format.payload_at()).expect("small") + 3;
        (
            (24 + width * len).next_multiple_of(8),
            data.next_multiple_of(8),
            (64 + entry_item).next_multiple_of(8),
        )
    }

    /// `journal`, in `format`.
    fn journal_in(format: Format, path: &Path, seqnum_id: u8, entries: &[(u64, u64)], n: u64) {
        let compact = format == Format::Compact;
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(crate::file::SIGNATURE);
        if compact {
            b[12..16].copy_from_slice(&crate::file::INCOMPAT_COMPACT.to_le_bytes());
        }
        // A file id of its own, from its name, as journald gives each file
        // one: a follow tells a copy of a file by it.
        let id = crate::hash::jenkins64(path.as_os_str().as_encoded_bytes()).to_le_bytes();
        b[24..32].copy_from_slice(&id);
        b[32..40].copy_from_slice(&id);
        b[72..88].copy_from_slice(&[seqnum_id; 16]);
        b[88..96].copy_from_slice(&272u64.to_le_bytes());
        b[152..160].copy_from_slice(&n.to_le_bytes());
        let (array_room, data_room, entry_room) = sizes(format, entries.len());
        let array_size = 24 + format.array_item_width() * entries.len() as u64;
        let data_at = 272 + array_room as u64;
        let payload_at = usize::try_from(format.payload_at()).expect("small");
        let mut data = vec![0u8; payload_at];
        data[0] = 1;
        data[8..16].copy_from_slice(&(format.payload_at() + 3).to_le_bytes());
        data.extend_from_slice(b"M=1");
        data.resize(data_room, 0);
        let mut at = data_at + data_room as u64;
        let mut array = vec![0u8; 24];
        array[0] = 6;
        array[8..16].copy_from_slice(&array_size.to_le_bytes());
        let mut objects = Vec::new();
        let small = |v: u64| u32::try_from(v).expect("fits 32 bits").to_le_bytes();
        for &(seqnum, mono) in entries {
            if compact {
                array.extend_from_slice(&small(at));
            } else {
                array.extend_from_slice(&at.to_le_bytes());
            }
            let entry_size = if compact { 68u64 } else { 80 };
            let mut e = vec![0u8; entry_room];
            e[0] = 3;
            e[8..16].copy_from_slice(&entry_size.to_le_bytes());
            e[16..24].copy_from_slice(&seqnum.to_le_bytes());
            e[24..32].copy_from_slice(&(1000 + mono).to_le_bytes());
            e[32..40].copy_from_slice(&mono.to_le_bytes());
            e[40..56].copy_from_slice(&[9; 16]);
            if compact {
                e[64..68].copy_from_slice(&small(data_at));
            } else {
                e[64..72].copy_from_slice(&data_at.to_le_bytes());
            }
            objects.extend_from_slice(&e);
            at += entry_room as u64;
        }
        array.resize(array_room, 0);
        b[176..184].copy_from_slice(&272u64.to_le_bytes());
        b.extend_from_slice(&array);
        b.extend_from_slice(&data);
        b.extend_from_slice(&objects);
        let arena = b.len() as u64 - 272;
        b[96..104].copy_from_slice(&arena.to_le_bytes());
        std::fs::write(path, &b).expect("write");
    }

    /// Rewrite the header's entry count in place, as journald does.
    fn set_n(path: &Path, n: u64) {
        let mut b = std::fs::read(path).expect("read");
        b[152..160].copy_from_slice(&n.to_le_bytes());
        std::fs::write(path, &b).expect("write");
    }

    fn seqnums(r: &[Record]) -> Vec<u64> {
        r.iter().map(|r| r.cursor().seqnum).collect()
    }

    const FOUR: [(u64, u64); 4] = [(1, 10), (2, 20), (3, 30), (4, 40)];

    #[test]
    fn follow_reads_only_what_was_appended() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        assert!(f.poll(&[]).is_empty());
        set_n(&a, 3);
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        set_n(&a, 4);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert!(f.poll(&[]).is_empty());
    }

    /// Rotation renames the file: it keeps its position under the new
    /// name. Vacuum deletes it: what it still had is read, then it closes.
    #[test]
    fn follow_keeps_a_renamed_file_and_reads_a_deleted_one_to_its_end() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        let b = dir.path().join("system-old.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        set_n(&a, 3);
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(std::slice::from_ref(&b));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        set_n(&b, 4);
        std::fs::remove_file(&b).expect("remove");
        f.rescan(&[]);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert!(!f.has(&b));
        assert_eq!(f.files(), 0);
    }

    /// Two files written at once (system and user journal): journald raises
    /// each file's `IN_MODIFY` from its own coalesced timer, so the newer
    /// entry in one file can be shown before the older entry in the other
    /// is read. The older one is still shown, not dropped as old.
    #[test]
    fn follow_keeps_an_older_entry_of_a_file_read_later() {
        let dir = tempfile::tempdir().expect("dir");
        let sys = dir.path().join("system.journal");
        let user = dir.path().join("user-1000.journal");
        journal(&sys, 1, &[(1, 10), (3, 30), (4, 40)], 1);
        journal(&user, 1, &[(2, 20), (5, 50)], 0);
        let mut f = T::of(&[sys.clone(), user.clone()]);
        set_n(&sys, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        // The user file's entry 2 (monotonic 20) was linked before 3 but
        // counted only now.
        set_n(&user, 1);
        set_n(&sys, 3);
        assert_eq!(seqnums(&f.poll(&[])), vec![2, 4]);
        set_n(&user, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![5]);
    }

    /// A deleted file whose read failed keeps being read: it closes only
    /// once read to its end, or after its error was reported.
    #[test]
    fn follow_keeps_a_deleted_file_until_it_was_read() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 72 + 2 * 80;
        bytes[third] = 1;
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        std::fs::remove_file(&a).expect("remove");
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files(), 1);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert_eq!(f.errors.len(), 1);
        assert_eq!(f.files(), 0);
    }

    /// A file renamed between the listing and the stat of its old name
    /// (rotation racing a rescan) isn't closed: it still has a name.
    #[test]
    fn follow_keeps_a_renamed_file_missing_from_one_listing() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        let b = dir.path().join("system-old.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files(), 1);
        set_n(&b, 3);
        f.rescan(std::slice::from_ref(&b));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
    }

    /// A file created mid-follow (rotation, restart) is first read before
    /// journald linked its entry array; its entries are still shown.
    #[test]
    fn follow_reads_a_new_file_first_seen_empty() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system-old.journal");
        let b = dir.path().join("system.journal");
        journal(&a, 1, &FOUR[..1], 1);
        let mut f = T::of(std::slice::from_ref(&a));
        journal(&b, 1, &FOUR[1..], 0);
        let mut bytes = std::fs::read(&b).expect("read");
        bytes[176..184].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&b, &bytes).expect("write");
        f.rescan(&[a.clone(), b.clone()]);
        assert!(f.poll(&[]).is_empty());
        bytes[176..184].copy_from_slice(&272u64.to_le_bytes());
        bytes[152..160].copy_from_slice(&3u64.to_le_bytes());
        std::fs::write(&b, &bytes).expect("write");
        assert_eq!(seqnums(&f.poll(&[])), vec![2, 3, 4]);
    }

    /// An archived file rewritten as a copy (new inode, same file id) keeps
    /// its position: nothing is read twice, nothing skipped. A deleted
    /// file that reads as zeros (vacuum deallocates) after it was read to
    /// its end is no error.
    #[test]
    fn follow_keeps_the_position_of_a_rewritten_copy() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system-old.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        set_n(&a, 3);
        let tmp = dir.path().join("copy.tmp");
        std::fs::copy(&a, &tmp).expect("copy");
        std::fs::rename(&tmp, &a).expect("rename");
        f.rescan(std::slice::from_ref(&a));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        assert_eq!(f.files(), 1);
        std::fs::write(&a, vec![0u8; 4096]).expect("zero");
        std::fs::remove_file(&a).expect("remove");
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// A new file with the id of a file still listed under its own inode
    /// (a journal file copied into the directory, or crafted files that
    /// all leave the id zero) is a file of its own, read from its start,
    /// as journalctl reads it: sd-journal knows its files by path and
    /// inode (`add_any_file`, systemd v255 sd-journal.c). Only a file whose
    /// inode is gone carries its position over to a copy with its id. The
    /// new file is listed first here, as `next.journal` is before
    /// `system.journal`.
    #[test]
    fn a_new_file_with_a_listed_files_id_is_read_as_new() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        let b = dir.path().join("next.journal");
        // a's second entry is written after the follow starts.
        journal(&a, 1, &FOUR[..2], 1);
        let mut f = T::of(std::slice::from_ref(&a));
        journal(&b, 1, &FOUR[2..], 2);
        let id = std::fs::read(&a).expect("read")[24..40].to_vec();
        let mut bytes = std::fs::read(&b).expect("read");
        bytes[24..40].copy_from_slice(&id);
        std::fs::write(&b, &bytes).expect("write");
        f.rescan(&[b.clone(), a.clone()]);
        assert_eq!(seqnums(&f.poll(&[])), vec![3, 4]);
        assert_eq!(f.files(), 2);
        // a goes on from where it was: its next entry, once.
        set_n(&a, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![2]);
        assert!(f.poll(&[]).is_empty());
    }

    /// A file appearing mid-follow in the other journal root (a flush,
    /// /run to /var) is read from its start; the copies the flush put
    /// there (the shown entries' times) are dropped, the new entries after
    /// them kept.
    #[test]
    fn follow_drops_flushed_copies_in_a_new_file() {
        let dir = tempfile::tempdir().expect("dir");
        for root in ["run", "var"] {
            std::fs::create_dir(dir.path().join(root)).expect("root");
        }
        let run = dir.path().join("run/system.journal");
        let var = dir.path().join("var/system.journal");
        journal(&run, 1, &FOUR[..2], 1);
        let mut f = T::of(std::slice::from_ref(&run));
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![2]);
        // Copies of 1 and 2 under higher seqnums of the same id, then 5.
        journal(&var, 1, &[(7, 10), (8, 20), (9, 50)], 3);
        f.rescan(&[run.clone(), var.clone()]);
        assert_eq!(seqnums(&f.poll(&[])), vec![9]);
    }

    /// An entry whose DATA can't be read (here an item pointing nowhere)
    /// is retried on the next poll like an unreadable entry, then reported
    /// and skipped; the entries after it still come.
    #[test]
    fn follow_retries_an_entry_whose_data_fails_once() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        // Entries 3 and 4 again at the end, in order (the array must not
        // decrease); 3 with one item, an unaligned offset.
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 72 + 2 * 80;
        let at3 = bytes.len() as u64;
        let mut e = bytes[third..third + 64].to_vec();
        e[8..16].copy_from_slice(&80u64.to_le_bytes());
        e.extend_from_slice(&9u64.to_le_bytes());
        e.extend_from_slice(&0u64.to_le_bytes());
        let fourth = bytes[third + 80..third + 160].to_vec();
        bytes.extend_from_slice(&e);
        let at4 = bytes.len() as u64;
        bytes.extend_from_slice(&fourth);
        bytes[272 + 24 + 16..272 + 24 + 24].copy_from_slice(&at3.to_le_bytes());
        bytes[272 + 24 + 24..272 + 24 + 32].copy_from_slice(&at4.to_le_bytes());
        let arena = bytes.len() as u64 - 272;
        bytes[96..104].copy_from_slice(&arena.to_le_bytes());
        bytes[152..160].copy_from_slice(&4u64.to_le_bytes());
        std::fs::write(&a, &bytes).expect("write");
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty(), "{:?}", f.errors);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert_eq!(f.errors.len(), 1);
    }

    /// A slot never written below `n_entries` (an unclean shutdown) is
    /// waited for once, then skipped without an error: the entries after
    /// it still come, and a dirty file can't stall the follow.
    #[test]
    fn follow_skips_a_zero_item_after_one_wait() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 1);
        let mut f = T::of(std::slice::from_ref(&a));
        // Zero the array's second item (entry 2).
        let mut bytes = std::fs::read(&a).expect("read");
        bytes[272 + 24 + 8..272 + 24 + 16].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(seqnums(&f.poll(&[])), vec![3, 4]);
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// A crafted journal: `crafted::journal(n)`'s entries, then one entry
    /// per element of `extra` listing those DATA objects, all in the main
    /// entry array.
    fn with_entries(n: u64, extra: impl Fn(&mut Crafted) -> Vec<Vec<u64>>) -> Crafted {
        let (mut c, l) = crafted::journal(n);
        let mut all = l.entries.clone();
        for (i, items) in extra(&mut c).into_iter().enumerate() {
            all.push(c.entry(100 + i as u64, &items));
        }
        let main = c.array(&all, all.len(), 0);
        c.set(H_N_ENTRIES, all.len() as u64);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        c
    }

    /// An entry of two 600-byte fields, over a 1000-byte limit: the first
    /// field is read (and cached) before the second runs out.
    fn oversized(c: &mut Crafted, i: u64) -> Vec<u64> {
        let a = c.data(format!("A{i}={}", "a".repeat(600)).as_bytes());
        let b = c.data(format!("B{i}={}", "b".repeat(600)).as_bytes());
        vec![a, b]
    }

    /// Oversized entries skipped in one poll leave nothing in its cache:
    /// the poll holds at most the file's limit, not one limit per entry
    /// skipped.
    #[test]
    fn follow_skipping_oversized_entries_holds_at_most_the_limit() {
        let mut c = with_entries(1, |c| {
            let after = c.data(b"MESSAGE=after");
            vec![
                oversized(c, 0),
                oversized(c, 1),
                oversized(c, 2),
                vec![after],
            ]
        });
        let (t, mut f) = followed(&mut c, 1);
        f.limit = 1000;
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&[])), ["after"]);
        let skipped = f
            .errors
            .iter()
            .filter(|e| e.contains("larger than the output limit"))
            .count();
        assert_eq!(skipped, 3, "{:?}", f.errors);
        assert!(f.f.held_peak <= 1000, "held {}", f.f.held_peak);
    }

    /// Entries sharing a DATA object that can't be read: the object gets
    /// its one wait, then every entry listing it is skipped at once, so
    /// the good entries after them come by the second poll.
    #[test]
    fn follow_skips_entries_sharing_a_bad_object_together() {
        let mut c = with_entries(1, |c| {
            let bad = c.data(b"BAD=object");
            c.set(bad + O_SIZE, 8);
            let mut extra: Vec<Vec<u64>> = (0..5).map(|_| vec![bad]).collect();
            for i in 0..3 {
                extra.push(vec![c.data(format!("MESSAGE=good{i}").as_bytes())]);
            }
            extra
        });
        let (t, mut f) = followed(&mut c, 1);
        grow(&c, &t);
        let mut got = messages(&f.poll(&[]));
        got.extend(messages(&f.poll(&[])));
        assert_eq!(got, ["good0", "good1", "good2"], "{:?}", f.errors);
    }

    /// An array item that can't be an entry offset (here past the arena)
    /// is treated like a zero item: waited for once, then skipped and
    /// reported; the good items after it come by the second poll.
    #[test]
    fn follow_skips_an_implausible_item_after_one_wait() {
        let (mut c, l) = crafted::journal(3);
        let items = [l.entries[0], 0xFFFF_FFF8, l.entries[1], l.entries[2]];
        let main = c.array(&items, 4, 0);
        c.set(H_N_ENTRIES, 4);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let (t, mut f) = followed(&mut c, 0);
        grow(&c, &t);
        let mut got = messages(&f.poll(&[]));
        got.extend(messages(&f.poll(&[])));
        assert_eq!(got, ["m0", "m1", "m2"], "{:?}", f.errors);
        assert!(
            f.errors.iter().any(|e| e.contains("not an entry offset")),
            "{:?}",
            f.errors
        );
    }

    /// journald 249 writes an entry again as the first of the next file
    /// when a rotation interrupts it: the same times and hash, the next
    /// seqnum, the same directory. journalctl shows both; so does the
    /// follow, whether the copy is read in a later poll or the same one.
    #[test]
    fn follow_keeps_a_rotation_double_write() {
        for same_poll in [false, true] {
            let dir = tempfile::tempdir().expect("dir");
            let old = dir.path().join("system-old.journal");
            let new = dir.path().join("system.journal");
            journal(&old, 1, &[(1, 10), (2, 20)], 1);
            let mut f = T::of(std::slice::from_ref(&old));
            set_n(&old, 2);
            let mut got = Vec::new();
            if !same_poll {
                got.extend(seqnums(&f.poll(&[])));
            }
            journal(&new, 1, &[(3, 20), (4, 30)], 2);
            f.rescan(&[old.clone(), new.clone()]);
            got.extend(seqnums(&f.poll(&[])));
            assert_eq!(got, vec![2, 3, 4], "same poll: {same_poll}");
        }
    }

    /// A lookup that fails while following is reported, once per file
    /// however many polls look again; nothing matches it meanwhile.
    #[test]
    fn follow_reports_a_failed_lookup_once() {
        let (mut c, _l) = crafted::journal(4);
        // A table size that isn't whole items: every lookup fails.
        c.set(H_HASH_TABLE_SIZE, c.get(H_HASH_TABLE_SIZE) + 8);
        let (t, mut f) = followed(&mut c, 1);
        for n in [2, 4] {
            c.set(H_N_ENTRIES, n);
            grow(&c, &t);
            assert!(f.poll(&u_is_x()).is_empty());
        }
        let lookups = f
            .errors
            .iter()
            .filter(|e| e.contains("lookup failed"))
            .count();
        assert_eq!(lookups, 1, "{:?}", f.errors);
    }

    /// Each kind of failure has its own wait at a position: a header read
    /// that failed doesn't spend the wait of the entry read there next.
    #[test]
    fn a_wait_is_per_kind() {
        let (mut c, _l) = crafted::journal(1);
        let (_t, file) = c.write();
        let mut f = Followed::new(file, 0);
        assert!(!f.waited(Wait::Header));
        assert!(!f.waited(Wait::Entry));
        assert!(f.waited(Wait::Entry));
        // Alternating kinds don't erase each other's mark.
        assert!(f.waited(Wait::Header));
    }

    /// journald 249 splits a long line into entries with the same times
    /// and hash; within one file they are distinct entries, all shown.
    #[test]
    fn follow_keeps_entries_with_equal_times_in_one_file() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &[(1, 5), (2, 10), (3, 10), (4, 10)], 1);
        let mut f = T::of(std::slice::from_ref(&a));
        set_n(&a, 4);
        assert_eq!(seqnums(&f.poll(&[])), vec![2, 3, 4]);
    }

    // New in the crate: the Differences on the follow's side, the boot
    // table kept current, and the follow's cut file.

    /// Difference 12: an entry journald was cut off from linking into its
    /// DATA objects' lists, then wrote again whole, carries `U=x` among its
    /// items. The follow matches by the items, so it shows both copies,
    /// where the tail, walking `U=x`'s list, shows one
    /// (`walk::tests::a_tail_shows_one_copy_of_an_entry_cut_off_while_linking`).
    #[test]
    fn difference_12_the_follow_shows_both_copies_of_an_entry_cut_off_while_linking() {
        let (mut c, l) = crafted::journal(2);
        let again = c.data(b"MESSAGE=again");
        // The cut-off copy: in the file's list, in no DATA object's.
        let cut = c.entry(3, &[l.shared, again]);
        let whole = c.entry(4, &[l.shared, again]);
        let main = c.array(&[l.entries[0], l.entries[1], cut, whole], 4, 0);
        let shared = c.array(&[l.entries[1], whole], 2, 0);
        c.set(l.shared + D_ENTRY_ARRAY, shared);
        c.set(l.shared + D_N_ENTRIES, 3);
        c.set(again + D_ENTRY_OFFSET, whole);
        c.set(again + D_N_ENTRIES, 1);
        c.set(H_N_ENTRIES, 4);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let (t, mut f) = followed(&mut c, 2);
        grow(&c, &t);
        let got = f.poll(&u_is_x());
        assert_eq!(messages(&got), ["again", "again"]);
        assert_eq!(seqnums(&got), [3, 4]);
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// Difference 13: copies a flush appends to a /var file the follow
    /// already reads past its start are shown again: only a file opened
    /// while following has its leading copies checked (`Copies`).
    #[test]
    fn difference_13_flush_copies_appended_to_a_followed_file_are_shown_again() {
        let dir = tempfile::tempdir().expect("dir");
        for root in ["run", "var"] {
            std::fs::create_dir(dir.path().join(root)).expect("root");
        }
        let run = dir.path().join("run/system.journal");
        let var = dir.path().join("var/system.journal");
        journal(&run, 1, &FOUR[..2], 1);
        // /var's own seqnum id; its first entry older than /run's.
        journal(&var, 2, &[(1, 5), (2, 10), (3, 20)], 1);
        let mut f = T::of(&[run.clone(), var.clone()]);
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![2]);
        // /run's entries 1 and 2 copied into /var (their times, /var's
        // seqnums), after the follow read it.
        set_n(&var, 3);
        let got = f.poll(&[]);
        assert_eq!(seqnums(&got), vec![2, 3]);
        assert!(
            got.iter().all(|r| r.cursor().seqnum_id == [2; 16]),
            "{got:?}"
        );
    }

    /// Difference 16: a lookup that fails while following is reported once
    /// per file, and the entries carrying the value don't match until a
    /// later poll's lookup succeeds; from then on they do. An entry read
    /// while it failed isn't shown later.
    #[test]
    fn difference_16_a_failed_lookup_matches_again_once_it_reads() {
        let (mut c, _l) = crafted::journal(4);
        let size = c.get(H_HASH_TABLE_SIZE);
        // A table size that isn't whole items: every lookup fails.
        c.set(H_HASH_TABLE_SIZE, size + 8);
        let (t, mut f) = followed(&mut c, 1);
        c.set(H_N_ENTRIES, 2);
        grow(&c, &t);
        assert!(f.poll(&u_is_x()).is_empty());
        c.set(H_HASH_TABLE_SIZE, size);
        c.set(H_N_ENTRIES, 4);
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&u_is_x())), ["m2", "m3"]);
        let lookups = f
            .errors
            .iter()
            .filter(|e| e.contains("lookup failed"))
            .count();
        assert_eq!(lookups, 1, "{:?}", f.errors);
    }

    /// Difference 17: a stale slot naming an older entry leaves the newer
    /// array out of order. The follow stops the file there: it waits a
    /// poll, reports it once, and reads nothing more from the file while it
    /// stays so, not even the good entries before the slot in that array.
    /// The tail walks on past it
    /// (`walk::tests::a_tail_walks_past_an_out_of_order_item`).
    // Regular only: Difference 17 stays as is; Task 38 changes forward
    // walks in file.rs, not this stop.
    #[test]
    fn difference_17_an_out_of_order_item_stops_a_followed_file() {
        let mut c = Crafted::new();
        c.hash_table(7);
        let older = c.array(&[], 2, 0);
        let newer = c.array(&[], 4, 0);
        let mut e = Vec::new();
        for i in 0..5u64 {
            let m = c.data(format!("MESSAGE=m{i}").as_bytes());
            e.push(c.entry(i + 1, &[m]));
        }
        c.set(older + A_NEXT, newer);
        for (k, &item) in [e[0], e[1]].iter().enumerate() {
            c.set(older + A_ITEMS + 8 * k as u64, item);
        }
        // e[2]'s slot written again with a stale e[2]: e[3] is lost.
        for (k, &item) in [e[2], e[3], e[2], e[4]].iter().enumerate() {
            c.set(newer + A_ITEMS + 8 * k as u64, item);
        }
        c.set(H_N_ENTRIES, 6);
        c.set(H_ENTRY_ARRAY_OFFSET, older);
        let (t, mut f) = followed(&mut c, 2);
        grow(&c, &t);
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty(), "waits first: {:?}", f.errors);
        assert!(f.poll(&[]).is_empty());
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.errors.len(), 1, "{:?}", f.errors);
        assert!(f.errors[0].contains("is out of order"), "{:?}", f.errors);
    }

    /// A file cut short while it is followed (vacuum deallocating it, a
    /// copy cut off) shows the entries before the cut and no system error:
    /// the poll reads the header first, which caps the reads at the file's
    /// new length, so an entry's guessed read near the cut doesn't run
    /// past it (the follow's side of a page's `Io` on a cut, file.rs
    /// `a_file_truncated_after_open_is_an_io_error`). The entry at the cut
    /// is corrupt now, waited for once.
    #[test]
    fn a_file_cut_while_followed_reads_up_to_the_cut() {
        let (mut c, l) = crafted::journal(6);
        let (t, mut f) = followed(&mut c, 2);
        grow(&c, &t);
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(l.entries[4] + 8)
            .expect("truncate");
        assert_eq!(messages(&f.poll(&[])), ["m2", "m3"]);
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// `journal`'s file with `boot` and these realtimes (in order with the
    /// entries) in place of its own, and a header naming its tail entry
    /// (`tail_entry_offset`) for its first `n` entries, as journald 254 and
    /// later keep it.
    fn boot_journal(path: &Path, seqnum_id: u8, boot: u8, entries: &[(u64, u64, u64)], n: u64) {
        let plain: Vec<(u64, u64)> = entries.iter().map(|&(s, m, _)| (s, m)).collect();
        journal(path, seqnum_id, &plain, n);
        let mut b = std::fs::read(path).expect("read");
        for (k, &(_, _, real)) in entries.iter().enumerate() {
            let at = entry_at(entries.len(), k);
            b[at + 24..at + 32].copy_from_slice(&real.to_le_bytes());
            b[at + 40..at + 56].copy_from_slice(&[boot; 16]);
        }
        std::fs::write(path, &b).expect("write");
        set_tail(path, entries.len(), n);
    }

    /// Where `journal`'s entry `k` of `len` lies.
    fn entry_at(len: usize, k: usize) -> usize {
        entry_at_in(Format::Regular, len, k)
    }

    /// Where `journal_in`'s entry `k` of `len` lies in `format`.
    fn entry_at_in(format: Format, len: usize, k: usize) -> usize {
        let (array, data, entry) = sizes(format, len);
        272 + array + data + entry * k
    }

    /// `set_n`, with the header's tail entry the `n`-th's.
    fn set_tail(path: &Path, len: usize, n: u64) {
        set_n(path, n);
        let mut b = std::fs::read(path).expect("read");
        let tail = match usize::try_from(n).expect("small") {
            0 => 0,
            n => entry_at(len, n - 1) as u64,
        };
        b[264..272].copy_from_slice(&tail.to_le_bytes());
        std::fs::write(path, &b).expect("write");
    }

    /// The follow orders boots as the page does (decision 11), with its
    /// boot table read again as the files grow: two boots of one machine,
    /// a and b, each in its own file. When the follow starts, b's tail
    /// (realtime 50) is older than a's (100), so the table puts b first;
    /// then a gains a2 (300) and b gains b2 (200) and b3 (900), and b's
    /// tail is the newer. 24.04 and 26.04 then show all of a's new entries
    /// before b's, as journalctl's `compare_boot_ids` reads the tails again;
    /// a table kept from the start would show b's first, and 22.04 orders
    /// them by realtime.
    #[test]
    fn the_follow_keeps_its_boot_table_current_as_files_grow() {
        // The preads of the poll after the headers change: 11 for the
        // headers and the new entries, which is all `Realtime` reads (it
        // keeps no boot table); one more per file for its tail, read from
        // the entry its header names (`TailCache`), and with
        // `NewestTailEntryMachine` two more per file for the machine id.
        // A tail found by walking the list instead reads more.
        let rules = [
            (BootRule::NewestTail, [(1, 2), (2, 2), (2, 3)], 13),
            (
                BootRule::NewestTailEntryMachine,
                [(1, 2), (2, 2), (2, 3)],
                17,
            ),
            (BootRule::Realtime, [(2, 2), (1, 2), (2, 3)], 11),
        ];
        for (rule, want, preads) in rules {
            let dir = tempfile::tempdir().expect("dir");
            let a = dir.path().join("a.journal");
            let b = dir.path().join("b.journal");
            let a_entries = [(1, 10, 100), (2, 20, 300)];
            let b_entries = [(1, 10, 50), (2, 20, 200), (3, 30, 900)];
            boot_journal(&a, 1, 0xa, &a_entries, 1);
            boot_journal(&b, 2, 0xb, &b_entries, 1);
            let mut f = T::start(&[a.clone(), b.clone()], rule);
            assert!(f.poll(&[]).is_empty());
            set_tail(&a, a_entries.len(), 2);
            set_tail(&b, b_entries.len(), 3);
            let read = f.counts().preads;
            let got: Vec<(u8, u64)> = f
                .poll(&[])
                .iter()
                .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
                .collect();
            assert_eq!(got, want, "{rule:?}");
            assert!(f.errors.is_empty(), "{:?}", f.errors);
            // Headers changed: each tail read from the entry its header
            // names (above).
            assert_eq!(f.counts().preads - read, preads, "{rule:?}");
            // Headers unchanged: one header read per file, no tail read
            // again (`TailCache`).
            let read = f.counts().preads;
            assert!(f.poll(&[]).is_empty());
            assert_eq!(f.counts().preads - read, 2, "{rule:?}");
        }
    }

    /// The directories a follow watches: the files' own, a directory
    /// source's, and for the local journal each machine-id directory, or
    /// its root while the directory doesn't exist yet (journald creates it
    /// on its first write or flush, Difference 2); neither when the root is
    /// missing too.
    #[test]
    fn a_follow_watches_the_files_directories_and_the_missing_ones_roots() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        let run = root.join("run/m");
        std::fs::create_dir_all(&run).expect("run");
        std::fs::create_dir(root.join("var")).expect("var");
        let var = root.join("var/m");
        let local = Roots::Local([run.clone(), var.clone()]);
        assert_eq!(watch_dirs(&local, &[]), [run.clone(), root.join("var")]);
        std::fs::create_dir(&var).expect("var/m");
        assert_eq!(watch_dirs(&local, &[]), [run.clone(), var.clone()]);
        let gone = Roots::Local([root.join("none/m"), var.clone()]);
        assert_eq!(watch_dirs(&gone, &[]), std::slice::from_ref(&var));
        let sub = root.join("d/s");
        let directory = Roots::Directory(root.join("d"));
        assert_eq!(
            watch_dirs(&directory, &[sub.join("x.journal")]),
            [root.join("d"), sub]
        );
        let files = Roots::Files(vec![run.join("a.journal"), var.join("b.journal")]);
        assert_eq!(
            watch_dirs(&files, &[run.join("a.journal"), run.join("c.journal")]),
            [run]
        );
    }

    /// Whether `fd` is readable within `ms` milliseconds, as a caller's
    /// readiness wait on the wake descriptor sees it (rustix's `poll`).
    fn readable(fd: &OwnedFd, ms: u64) -> bool {
        use std::os::fd::AsFd as _;
        let timeout = std::time::Duration::from_millis(ms);
        crate::testing::tools::wait_readable(fd.as_fd(), Some(timeout)).expect("poll")
    }

    /// `f.drain` once its wake descriptor is readable, as a caller waits:
    /// failing the test after five seconds without an event, so a watch
    /// that is missing can't pass.
    fn woken(f: &mut Follower) -> crate::Batch {
        let wake = f.wake_fd().expect("a wake descriptor");
        assert!(readable(&wake, 5000), "woken within five seconds");
        f.drain(&crate::Limits::default())
    }

    /// `journal`'s follow of `q` from the end of an empty page (`-n 0`),
    /// as the example's `follow` starts one: from now on.
    fn follow_now(journal: &Journal, q: crate::Query) -> (Follower, crate::Batch) {
        let limits = crate::Limits::default();
        let page = journal.page(&q, &crate::Position::Tail, 0, &limits);
        journal.follow(q, &page.end, &limits).expect("follows")
    }

    /// A local journal for `Journal::open_on`: a machine-id file and the
    /// two roots, /run's machine-id directory there and /var's not yet.
    fn local(root: &Path) -> (crate::open::Host, PathBuf, PathBuf) {
        let id = "0123456789abcdef0123456789abcdef";
        std::fs::write(root.join("machine-id"), format!("{id}\n")).expect("machine id");
        let roots = [root.join("run"), root.join("var")];
        for r in &roots {
            std::fs::create_dir(r).expect("root");
        }
        std::fs::create_dir(roots[0].join(id)).expect("run's directory");
        let (run, var) = (roots[0].join(id), roots[1].join(id));
        let host = crate::open::Host {
            machine_id: root.join("machine-id"),
            roots,
        };
        (host, run, var)
    }

    fn release(version_id: &str) -> Release {
        Release::parse(version_id).expect("a supported release")
    }

    /// Difference 2: the follow loses and doubles nothing across a flush,
    /// even one that creates /var's machine-id directory after the follow
    /// started (journalctl 259 loses what comes after it; 255 shows the
    /// flushed copies twice). The new file in /var holds copies of /run's
    /// entries under higher seqnums of /run's seqnum id (255's flush), then
    /// a new one; only the new ones are shown, and later writes to /var
    /// come too. Through the follower's own watches: the root's, then the
    /// directory's once it appears.
    #[test]
    fn difference_2_the_follow_loses_and_doubles_nothing_across_a_flush() {
        each_format(difference_2_the_follow_loses_and_doubles_nothing_across_a_flush_in);
    }

    fn difference_2_the_follow_loses_and_doubles_nothing_across_a_flush_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        let run_file = run.join("system.journal");
        journal_in(format, &run_file, 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let (mut f, first) = follow_now(&journal_, crate::Query::every());
        assert!(first.records.is_empty(), "{:?}", first.records);
        journal_in(format, &run_file, 1, &FOUR[..3], 3);
        let batch = woken(&mut f);
        assert_eq!(seqnums(&batch.records), [3]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        // The flush: /var's directory and file appear.
        std::fs::create_dir(&var).expect("var's directory");
        let flushed = [(4, 10), (5, 20), (6, 30), (7, 40)];
        journal_in(format, &var.join("system.journal"), 1, &flushed, 4);
        let batch = woken(&mut f);
        assert_eq!(seqnums(&batch.records), [7]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let more = [(4, 10), (5, 20), (6, 30), (7, 40), (8, 50)];
        journal_in(format, &var.join("system.journal"), 1, &more, 5);
        assert_eq!(seqnums(&woken(&mut f).records), [8]);
    }

    /// Give `journal_in`'s entries of `path` (`len` of them) the XOR hashes
    /// `xors`, in order: journald's one stamp for a burst leaves them, and
    /// the boot, as what tells the burst's entries apart.
    fn set_xors(format: Format, path: &Path, len: usize, xors: &[u64]) {
        let mut b = std::fs::read(path).expect("read");
        for (k, x) in xors.iter().enumerate() {
            let at = entry_at_in(format, len, k) + 56;
            b[at..at + 8].copy_from_slice(&x.to_le_bytes());
        }
        std::fs::write(path, &b).expect("write");
    }

    /// The cursors' seqnum ids and seqnums, telling /run's from /var's.
    fn ids(r: &[Record]) -> Vec<(u8, u64)> {
        r.iter()
            .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
            .collect()
    }

    /// follow-M1: journald stamps every entry of one event-loop pass with
    /// the same times, so a burst's entries differ only by their XOR
    /// hashes. /run holds four such entries, all shown; the flush writes
    /// their copies (another seqnum id, as 249 writes them) and a new
    /// entry to /var. Only the new one is shown, not the burst again.
    #[test]
    fn a_flushed_burst_the_follow_showed_is_not_shown_again() {
        each_format(a_flushed_burst_the_follow_showed_is_not_shown_again_in);
    }

    fn a_flushed_burst_the_follow_showed_is_not_shown_again_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        for root in ["run", "var"] {
            std::fs::create_dir(dir.path().join(root)).expect("root");
        }
        let run = dir.path().join("run/system.journal");
        let var = dir.path().join("var/system.journal");
        let burst = [(1, 10), (2, 10), (3, 10), (4, 10)];
        journal_in(format, &run, 1, &burst, 0);
        set_xors(format, &run, 4, &[1, 2, 3, 4]);
        let mut f = T::of(std::slice::from_ref(&run));
        set_n(&run, 4);
        assert_eq!(ids(&f.poll(&[])), [(1, 1), (1, 2), (1, 3), (1, 4)]);
        let copies = [(11, 10), (12, 10), (13, 10), (14, 10), (15, 20)];
        journal_in(format, &var, 2, &copies, 5);
        set_xors(format, &var, 5, &[1, 2, 3, 4, 5]);
        f.rescan(&[run.clone(), var.clone()]);
        let first = f.poll(&[]);
        let second = f.poll(&[]);
        assert_eq!((ids(&first), ids(&second)), (vec![(2, 15)], vec![]));
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// follow-M1 from a page: the page read a journal whose newest entries
    /// are one burst (the page's end `newest` the last of them), and its
    /// follow has shown nothing yet when the flush writes their copies and
    /// a new entry to /var. Only the new one is shown.
    #[test]
    fn a_flushed_burst_the_page_ended_at_is_not_shown_again() {
        each_format(a_flushed_burst_the_page_ended_at_is_not_shown_again_in);
    }

    fn a_flushed_burst_the_page_ended_at_is_not_shown_again_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        let run_file = run.join("system.journal");
        let burst = [(1, 10), (2, 10), (3, 10), (4, 10)];
        journal_in(format, &run_file, 1, &burst, 4);
        set_xors(format, &run_file, 4, &[1, 2, 3, 4]);
        let journal_ = Journal::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let limits = crate::Limits::default();
        let q = crate::Query::every();
        let page = journal_.page(&q, &crate::Position::Tail, 10, &limits);
        assert_eq!(seqnums(&page.records), [1, 2, 3, 4]);
        let (mut f, first) = journal_.follow(q, &page.end, &limits).expect("follows");
        assert!(first.records.is_empty(), "{:?}", first.records);
        std::fs::create_dir(&var).expect("var's directory");
        let var_file = var.join("system.journal");
        let copies = [(11, 10), (12, 10), (13, 10), (14, 10), (15, 20)];
        journal_in(format, &var_file, 2, &copies, 5);
        set_xors(format, &var_file, 5, &[1, 2, 3, 4, 5]);
        let batch = woken(&mut f);
        assert_eq!(ids(&batch.records), [(2, 15)]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
    }

    /// follow-M1 across a poll: the follow has shown half a burst when the
    /// rest of it and the flush's copies (under /run's seqnum id with
    /// higher seqnums, 255's flush) are read in one poll. The copies of the
    /// half already shown, and of the rest, are each dropped: every entry
    /// is shown once.
    #[test]
    fn a_burst_half_shown_before_its_flush_shows_once() {
        each_format(a_burst_half_shown_before_its_flush_shows_once_in);
    }

    fn a_burst_half_shown_before_its_flush_shows_once_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        for root in ["run", "var"] {
            std::fs::create_dir(dir.path().join(root)).expect("root");
        }
        let run = dir.path().join("run/system.journal");
        let var = dir.path().join("var/system.journal");
        let burst = [(1, 10), (2, 10), (3, 10), (4, 10)];
        journal_in(format, &run, 1, &burst, 0);
        set_xors(format, &run, 4, &[1, 2, 3, 4]);
        let mut f = T::of(std::slice::from_ref(&run));
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), [1, 2]);
        set_n(&run, 4);
        let copies = [(5, 10), (6, 10), (7, 10), (8, 10), (9, 20)];
        journal_in(format, &var, 1, &copies, 5);
        set_xors(format, &var, 5, &[1, 2, 3, 4, 5]);
        f.rescan(&[run.clone(), var.clone()]);
        let first = f.poll(&[]);
        let second = f.poll(&[]);
        assert_eq!((seqnums(&first), seqnums(&second)), (vec![3, 4, 9], vec![]));
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// A file the follow can't open at its start is an error of its first
    /// batch, not dropped: here a symlinked journal file (Difference 4).
    /// The other file is followed.
    #[test]
    fn the_follows_open_errors_come_with_its_first_batch() {
        each_format(the_follows_open_errors_come_with_its_first_batch_in);
    }

    fn the_follows_open_errors_come_with_its_first_batch_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        let link = dir.path().join("link.journal");
        journal_in(format, &a, 1, &FOUR[..1], 1);
        std::os::unix::fs::symlink(&a, &link).expect("symlink");
        let source = Source::Files(vec![a.clone(), link.clone()]);
        let journal_ = Journal::open(source, &release("22.04")).expect("opens");
        let (mut f, first) = follow_now(&journal_, crate::Query::every());
        assert!(first.records.is_empty(), "{:?}", first.records);
        assert_eq!(first.errors.shown.len(), 1, "{:?}", first.errors);
        assert!(first.errors.shown[0].is_symlink(), "{:?}", first.errors);
        assert_eq!(first.errors.shown[0].path(), Some(link.as_path()));
        journal_in(format, &a, 1, &FOUR[..2], 2);
        let batch = woken(&mut f);
        assert_eq!(seqnums(&batch.records), [2]);
        assert!(batch.errors.is_empty(), "reported once: {:?}", batch.errors);
    }

    /// The follow goes on from its page's newest entry (`Last`), so a
    /// flush right after the start shows only what is new: the copies of
    /// the entries there before it, in /var's new file, are dropped
    /// (Difference 2).
    #[test]
    fn the_follow_starts_after_its_pages_newest_entry() {
        each_format(the_follow_starts_after_its_pages_newest_entry_in);
    }

    fn the_follow_starts_after_its_pages_newest_entry_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        journal_in(format, &run.join("system.journal"), 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        std::fs::create_dir(&var).expect("var's directory");
        journal_in(
            format,
            &var.join("system.journal"),
            2,
            &[(1, 10), (2, 20), (3, 30)],
            3,
        );
        let batch = woken(&mut f);
        assert_eq!(seqnums(&batch.records), [3]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
    }

    /// A renamed file (a rotation) is kept by its inode: the rescan finds
    /// it under its new name without opening it again.
    #[test]
    fn a_renamed_file_is_kept_by_its_inode_not_opened_again() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        let b = dir.path().join("system-1.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = T::of(std::slice::from_ref(&a));
        let opened = f.counts().opens;
        std::fs::rename(&a, &b).expect("rename");
        set_n(&b, 3);
        f.rescan(std::slice::from_ref(&b));
        assert_eq!(f.counts().opens, opened);
        assert!(f.has(&b) && !f.has(&a));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
    }

    /// A file renamed away and missing from the listings (still linked) is
    /// kept through one rescan that doesn't list it, then closed at the
    /// second once it was read to its end.
    #[test]
    fn an_unlisted_file_read_to_its_end_closes_at_the_second_rescan() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        journal(&a, 1, &FOUR, 4);
        let mut f = T::of(std::slice::from_ref(&a));
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files(), 1);
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files(), 0);
    }

    /// A listed file whose header can't be read any more is waited for one
    /// poll, then reported, once, even when it was read to its end (only a
    /// deleted file read to its end, which vacuum zeroes, is exempt).
    #[test]
    fn a_header_that_cant_be_read_is_reported_after_one_wait() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 4);
        let mut f = T::of(std::slice::from_ref(&a));
        let mut bytes = std::fs::read(&a).expect("read");
        bytes[0..8].copy_from_slice(b"NOTAJRNL");
        std::fs::write(&a, &bytes).expect("write");
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty(), "waits first: {:?}", f.errors);
        assert!(f.poll(&[]).is_empty());
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.errors.len(), 1, "{:?}", f.errors);
        assert!(f.errors[0].contains("not a journal file"), "{:?}", f.errors);
    }

    /// A sealed file (renamed to `.journal~`, or archived) read to its end
    /// is never read again, not even its header: no more writes come.
    #[test]
    fn a_sealed_file_read_to_its_end_isnt_read_again() {
        let dir = tempfile::tempdir().expect("dir");
        let dirty = dir.path().join("system-x.journal~");
        let archived = dir.path().join("system-y.journal");
        journal(&dirty, 1, &FOUR, 4);
        journal(&archived, 2, &FOUR, 4);
        let mut bytes = std::fs::read(&archived).expect("read");
        bytes[16] = 2; // STATE_ARCHIVED
        std::fs::write(&archived, &bytes).expect("write");
        for p in [dirty, archived] {
            let mut f = T::of(std::slice::from_ref(&p));
            assert!(f.poll(&[]).is_empty());
            let read = f.counts().preads;
            assert!(f.poll(&[]).is_empty());
            assert_eq!(f.counts().preads, read, "{}", p.display());
        }
    }

    /// A file opened while following has only its leading entries checked
    /// for copies: once one is new, a later entry older than the last one
    /// shown (from another file) is still new, and shown.
    #[test]
    fn a_new_files_copies_end_at_its_first_new_entry() {
        let dir = tempfile::tempdir().expect("dir");
        for root in ["run", "var"] {
            std::fs::create_dir(dir.path().join(root)).expect("root");
        }
        let run = dir.path().join("run/system.journal");
        let system = dir.path().join("var/system.journal");
        let user = dir.path().join("var/user-1000.journal");
        journal(&run, 1, &FOUR[..2], 1);
        let mut f = T::of(std::slice::from_ref(&run));
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), [2]);
        // A flush: copies of 1 and 2, then 3 at 30; the user file's 50.
        let var_entries = [(1, 10), (2, 20), (3, 30), (4, 40)];
        journal(&system, 2, &var_entries, 3);
        journal(&user, 3, &[(1, 50)], 1);
        f.rescan(&[run.clone(), system.clone(), user.clone()]);
        let got: Vec<(u8, u64)> = f
            .poll(&[])
            .iter()
            .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
            .collect();
        assert_eq!(got, [(2, 3), (3, 1)]);
        // /var's 4, at 40, older than the user file's 50 shown.
        set_n(&system, 4);
        assert_eq!(seqnums(&f.poll(&[])), [4]);
    }

    /// A crafted file of one boot and one seqnum id, its entries at these
    /// (monotonic, realtime) times, each carrying `_MACHINE_ID=` of
    /// `machine` when given, and its header naming the `n`-th entry as its
    /// tail, or with `bogus_tail` an object that isn't an entry. Returns the
    /// builder and the entries' offsets, for `machine_grow`.
    fn machine_journal(
        path: &Path,
        (seqnum_id, boot, machine): (u8, u8, Option<u8>),
        entries: &[(u64, u64)],
        n: u64,
        bogus_tail: bool,
    ) -> (Crafted, Vec<u64>) {
        let mut c = Crafted::new();
        c.hash_table(7);
        let main = c.array(&[], entries.len(), 0);
        let id = machine.map(|m| {
            let text = format!("_MACHINE_ID={}", crate::file::hex_id(&[m; 16]));
            c.data(text.as_bytes())
        });
        let mut offsets = Vec::new();
        for (k, &(mono, real)) in entries.iter().enumerate() {
            let message = c.data(format!("MESSAGE=m{k}").as_bytes());
            let items: Vec<u64> = id.into_iter().chain([message]).collect();
            let e = c.entry(k as u64 + 1, &items);
            c.set(e + 24, real);
            c.set(e + 32, mono);
            let at = usize::try_from(e).expect("small") + 40;
            c.b[at..at + 16].copy_from_slice(&[boot; 16]);
            c.set(main + A_ITEMS + 8 * k as u64, e);
            offsets.push(e);
        }
        if bogus_tail {
            // The main array: not an entry, so the tail is the list's newest.
            offsets = vec![main; offsets.len()];
        }
        c.b[72..88].copy_from_slice(&[seqnum_id; 16]);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        machine_grow(path, &mut c, &offsets, n);
        (c, offsets)
    }

    /// Write `c` to `path` counting its first `n` entries, its header's
    /// tail the `n`-th.
    fn machine_grow(path: &Path, c: &mut Crafted, offsets: &[u64], n: u64) {
        c.set(H_N_ENTRIES, n);
        c.set(264, offsets[usize::try_from(n).expect("small") - 1]);
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        std::fs::write(path, &c.b).expect("write");
    }

    /// The boot table of a follow on 26.04 takes each boot's machine from
    /// its tail entry's `_MACHINE_ID=` (decision 11, issue #34169): two
    /// boots of two source machines (one receiving machine in the headers)
    /// then compare equal and realtime interleaves them, where 24.04 orders
    /// them by the table, as the boot-table test's files are. Tail entries
    /// without one keep the header's, and the table orders them on 26.04
    /// too. A header naming a tail that isn't an entry falls back to the
    /// main list's newest entry, which the table then uses.
    #[test]
    fn the_follows_boot_table_takes_26_04s_tail_entry_machine() {
        let table = [(1, 2), (2, 2), (2, 3)];
        let by_realtime = [(2, 2), (1, 2), (2, 3)];
        let cases = [
            (BootRule::NewestTail, Some((0x11, 0x22)), false, table),
            (
                BootRule::NewestTailEntryMachine,
                Some((0x11, 0x22)),
                false,
                by_realtime,
            ),
            (BootRule::NewestTailEntryMachine, None, false, table),
            (BootRule::NewestTail, None, true, table),
            (BootRule::Realtime, Some((0x11, 0x22)), false, by_realtime),
        ];
        for (rule, machines, bogus_tail, want) in cases {
            let dir = tempfile::tempdir().expect("dir");
            let a = dir.path().join("a.journal");
            let b = dir.path().join("b.journal");
            let (ma, mb) = machines.map_or((None, None), |(x, y)| (Some(x), Some(y)));
            let a_entries = [(10, 100), (20, 300)];
            let b_entries = [(10, 50), (20, 200), (30, 900)];
            let (mut ca, oa) = machine_journal(&a, (1, 0xa, ma), &a_entries, 1, bogus_tail);
            let (mut cb, ob) = machine_journal(&b, (2, 0xb, mb), &b_entries, 1, bogus_tail);
            let mut f = T::start(&[a.clone(), b.clone()], rule);
            assert!(f.poll(&[]).is_empty());
            machine_grow(&a, &mut ca, &oa, 2);
            machine_grow(&b, &mut cb, &ob, 3);
            let got: Vec<(u8, u64)> = f
                .poll(&[])
                .iter()
                .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
                .collect();
            assert_eq!(got, want, "{rule:?} {machines:?} bogus tail {bogus_tail}");
            assert!(f.errors.is_empty(), "{:?}", f.errors);
        }
    }

    /// Each kind of event the follow watches for wakes a drain on its own:
    /// a write (`IN_MODIFY`), a close after writing (`IN_CLOSE_WRITE`), a
    /// creation, a move in, a move out and a deletion. Without an event of
    /// its kind the wake descriptor would stay unreadable, and fail the
    /// deadline.
    #[test]
    fn each_watched_event_wakes_a_drain() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path().join("d");
        let out = dir.path().join("out");
        std::fs::create_dir(&d).expect("d");
        std::fs::create_dir(&out).expect("out");
        let a = d.join("a.journal");
        journal(&a, 1, &FOUR, 4);
        let journal_ =
            Journal::open(Source::Directory(d.clone()), &release("24.04")).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        let mut held = std::fs::OpenOptions::new()
            .append(true)
            .open(&a)
            .expect("open");
        held.write_all(&[0]).expect("modify only");
        assert!(woken(&mut f).records.is_empty());
        drop(held);
        assert!(woken(&mut f).records.is_empty());
        std::fs::create_dir(d.join("sub")).expect("create only");
        assert!(woken(&mut f).records.is_empty());
        std::fs::write(out.join("x"), b"x").expect("outside");
        std::fs::rename(out.join("x"), d.join("x")).expect("move in only");
        assert!(woken(&mut f).records.is_empty());
        std::fs::rename(d.join("x"), out.join("y")).expect("move out only");
        assert!(woken(&mut f).records.is_empty());
        std::fs::remove_dir(d.join("sub")).expect("delete only");
        let batch = woken(&mut f);
        assert!(batch.records.is_empty(), "{:?}", batch.records);
    }

    /// A journal file that can't be opened when it appears (journald may
    /// not have written its header yet) is opened at a later write to it:
    /// the drain's write event on a file that isn't open lists the
    /// directory again, and its entries come.
    #[test]
    fn a_file_unopened_at_its_creation_is_opened_at_its_next_write() {
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path().join("d");
        std::fs::create_dir(&d).expect("d");
        let journal_ =
            Journal::open(Source::Directory(d.clone()), &release("24.04")).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        let x = d.join("x.journal");
        std::fs::write(&x, b"not yet a journal").expect("created");
        let batch = woken(&mut f);
        assert!(batch.records.is_empty(), "{:?}", batch.records);
        assert!(f.follow.is_unopened(&x));
        // Written in place: write events only, no creation.
        journal(&x, 1, &FOUR, 4);
        let batch = woken(&mut f);
        assert_eq!(seqnums(&batch.records), [1, 2, 3, 4]);
        assert!(f.follow.has(&x));
    }

    /// The follower matches its query: only the entries it names come.
    #[test]
    fn a_follower_shows_only_what_its_query_matches() {
        let (mut c, _l) = crafted::journal(4);
        c.set(H_N_ENTRIES, 2);
        let t = c.file();
        let source = Source::Files(vec![t.path().to_path_buf()]);
        let journal_ = Journal::open(source, &release("26.04")).expect("opens");
        let name = crate::FieldName::new(b"MESSAGE").expect("a field name");
        let (mut f, _) = follow_now(&journal_, crate::Query::field(&name, b"m3"));
        c.set(H_N_ENTRIES, 4);
        grow(&c, &t);
        let batch = woken(&mut f);
        assert_eq!(messages(&batch.records), ["m3"]);
    }

    /// What a drain's events make it do (`Follower::wake`): a modified
    /// journal file that isn't open lists the directories again, and one
    /// the listing doesn't take (another file beside `Source::Files`') is
    /// then ignored until the directory changes; a file that failed to open
    /// is retried at each rescan, reported from its second failure on; an
    /// open file's write lists nothing.
    #[test]
    fn a_drains_events_decide_its_rescans() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        let link = dir.path().join("link.journal");
        let other = dir.path().join("other.journal");
        journal(&a, 1, &FOUR, 4);
        std::os::unix::fs::symlink(&a, &link).expect("symlink");
        let source = Source::Files(vec![a.clone(), link.clone()]);
        let journal_ = Journal::open(source, &release("22.04")).expect("opens");
        let (mut f, first) = follow_now(&journal_, crate::Query::every());
        // The start's own open error, given with the first batch.
        assert_eq!(first.errors.shown.len(), 1, "{:?}", first.errors);
        assert!(f.follow.pending.is_empty());
        f.wake(std::slice::from_ref(&other), false);
        assert_eq!(f.ignored, std::slice::from_ref(&other));
        assert!(f.follow.is_unopened(&link) && !f.follow.is_unopened(&a));
        assert_eq!(f.follow.pending.len(), 0, "a first failure isn't reported");
        f.wake(std::slice::from_ref(&other), false);
        assert_eq!(f.follow.pending.len(), 0, "an ignored file lists nothing");
        f.wake(&[], true);
        assert!(f.ignored.is_empty(), "{:?}", f.ignored);
        assert_eq!(f.follow.pending.len(), 1, "the second failure is");
        f.wake(std::slice::from_ref(&link), false);
        assert_eq!(
            f.follow.pending.len(),
            2,
            "an unopened file's event rescans"
        );
        assert!(f.ignored.is_empty(), "{:?}", f.ignored);
        f.wake(std::slice::from_ref(&a), false);
        assert_eq!(
            f.follow.pending.len(),
            2,
            "an open file's write lists nothing"
        );
        assert!(f.follow.has(&a) && !f.follow.has(&link));
    }

    // The start from a page's end, `Batch::more` and the wake fd.

    /// What a page of `n` at the tail shows, and the follow from its end:
    /// the page's records, the follower and its first batch.
    fn page_then_follow(
        journal: &Journal,
        q: &crate::Query,
        n: usize,
        between: impl FnOnce(),
    ) -> (Vec<Record>, Follower, crate::Batch) {
        let limits = crate::Limits::default();
        let page = journal.page(q, &crate::Position::Tail, n, &limits);
        between();
        let (f, first) = journal
            .follow(q.clone(), &page.end, &limits)
            .expect("follows");
        (page.records, f, first)
    }

    /// Writes between a full page and its follow come as the follow's
    /// first batch, with no event read, and later writes with the next
    /// drain: none lost, none shown twice (journal spec, "Starting a
    /// follow").
    #[test]
    fn writes_between_a_full_page_and_its_follow_come_first() {
        each_format(writes_between_a_full_page_and_its_follow_come_first_in);
    }

    fn writes_between_a_full_page_and_its_follow_come_first_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal_in(format, &a, 1, &FOUR, 2);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("24.04")).expect("opens");
        let (page, mut f, first) =
            page_then_follow(&journal_, &crate::Query::every(), 2, || set_n(&a, 3));
        assert_eq!(seqnums(&page), [1, 2]);
        assert_eq!(seqnums(&first.records), [3]);
        assert!(!first.more && first.errors.is_empty(), "{first:?}");
        set_n(&a, 4);
        assert_eq!(seqnums(&woken(&mut f).records), [4]);
    }

    /// The same after a page that matched nothing: its end still names
    /// the files and their counts, so the entries written between it and
    /// the follow come first, the query honoured.
    #[test]
    fn writes_between_an_empty_page_and_its_follow_come_first() {
        each_format(writes_between_an_empty_page_and_its_follow_come_first_in);
    }

    fn writes_between_an_empty_page_and_its_follow_come_first_in(format: Format) {
        // Two entries, m0 and m1, then the same file with four.
        let t = crafted::journal_in(format, 2).0.file();
        let (mut c, _l) = crafted::journal_in(format, 4);
        let source = Source::Files(vec![t.path().to_path_buf()]);
        let journal_ = Journal::open(source, &release("26.04")).expect("opens");
        let name = crate::FieldName::new(b"MESSAGE").expect("a field name");
        let q = crate::Query::field(&name, b"m3");
        let (page, _f, first) = page_then_follow(&journal_, &q, 10, || {
            c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
            grow(&c, &t);
        });
        assert!(page.is_empty(), "{page:?}");
        assert_eq!(messages(&first.records), ["m3"]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
    }

    /// A file the page didn't read (new since) is walked back to the
    /// page's newest entry: here /var's file from a flush after the page,
    /// its copies of /run's entries the page saw left out by the flush-copy
    /// rule (another directory), its new entry shown. A new user file whose
    /// every entry is newer is read from its start.
    #[test]
    fn a_file_new_since_the_page_is_read_from_after_the_pages_newest_entry() {
        each_format(a_file_new_since_the_page_is_read_from_after_the_pages_newest_entry_in);
    }

    fn a_file_new_since_the_page_is_read_from_after_the_pages_newest_entry_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        journal_in(format, &run.join("system.journal"), 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let (page, _f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            std::fs::create_dir(&var).expect("var's directory");
            // 255's flush: copies under /run's seqnum id, higher seqnums.
            journal_in(
                format,
                &var.join("system.journal"),
                1,
                &[(4, 10), (5, 20), (6, 30)],
                3,
            );
            journal_in(format, &var.join("user-1000.journal"), 3, &[(1, 50)], 1);
        });
        assert_eq!(seqnums(&page), [1, 2]);
        let got: Vec<(u8, u64)> = first
            .records
            .iter()
            .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
            .collect();
        assert_eq!(got, [(1, 6), (3, 1)]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
    }

    /// An entry written to /run after the page and flushed to /var before
    /// the follow shows once: /run's file goes on from its count, /var's
    /// new file from after the page's newest entry, and the merge drops
    /// the copy.
    #[test]
    fn an_entry_flushed_between_a_page_and_its_follow_shows_once() {
        each_format(an_entry_flushed_between_a_page_and_its_follow_shows_once_in);
    }

    fn an_entry_flushed_between_a_page_and_its_follow_shows_once_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        let run_file = run.join("system.journal");
        journal_in(format, &run_file, 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("22.04"), &host).expect("opens");
        let (_page, _f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            journal_in(format, &run_file, 1, &FOUR[..3], 3);
            std::fs::create_dir(&var).expect("var's directory");
            let flushed = [(4, 10), (5, 20), (6, 30), (7, 40)];
            journal_in(format, &var.join("system.journal"), 1, &flushed, 4);
        });
        assert_eq!(seqnums(&first.records), [3, 7]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
    }

    /// The walk back of a new file passes over an entry it can't read, so
    /// the follow's own read meets it (waited for, then reported) and
    /// nothing after it is lost; with no entry at or below the page's
    /// newest, the file is read from its start.
    #[test]
    fn the_walk_back_passes_over_an_unreadable_entry() {
        each_format(the_walk_back_passes_over_an_unreadable_entry_in);
    }

    fn the_walk_back_passes_over_an_unreadable_entry_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        let b = dir.path().join("b.journal");
        journal_in(format, &a, 1, &FOUR[..1], 1);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("24.04")).expect("opens");
        let (_page, mut f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            // b's entries, all newer than a's 1 at 10, the second unreadable.
            journal_in(format, &b, 2, &[(1, 20), (2, 30), (3, 40)], 3);
            let mut bytes = std::fs::read(&b).expect("read");
            let second = entry_at_in(format, 3, 1);
            bytes[second + 8..second + 16].copy_from_slice(&0u64.to_le_bytes());
            std::fs::write(&b, &bytes).expect("write");
        });
        assert_eq!(seqnums(&first.records), [1]);
        assert!(first.errors.is_empty(), "waits first: {:?}", first.errors);
        let next = f.drain(&crate::Limits::default());
        assert_eq!(seqnums(&next.records), [3]);
        assert_eq!(next.errors.shown.len(), 1, "{:?}", next.errors);
    }

    /// The walk back of a new file in the page's newest entry's directory
    /// stops by sd-journal's order, not the flush-copy rule: here an entry
    /// numbered below the page's newest, under its seqnum id, though later
    /// by monotonic time, is taken as shown, and only the one after it is
    /// read.
    #[test]
    fn the_walk_back_in_the_pages_directory_stops_by_sd_journals_order() {
        each_format(the_walk_back_in_the_pages_directory_stops_by_sd_journals_order_in);
    }

    fn the_walk_back_in_the_pages_directory_stops_by_sd_journals_order_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal_in(format, &a, 1, &FOUR[..2], 2);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("22.04")).expect("opens");
        let (page, _f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            journal_in(
                format,
                &dir.path().join("b.journal"),
                1,
                &[(1, 25), (3, 30)],
                2,
            );
        });
        assert_eq!(seqnums(&page), [1, 2]);
        assert_eq!(seqnums(&first.records), [3]);
    }

    /// A file the page read is known by its inode and its header's file
    /// id: a new file journald made at a freed inode (a vacuum, then a
    /// rotation) is read as new, from after the page's newest entry, not
    /// from the old file's count; so is a file with fewer entries than the
    /// page counted.
    #[test]
    fn a_new_file_at_an_inode_the_page_read_is_read_as_new() {
        each_format(a_new_file_at_an_inode_the_page_read_is_read_as_new_in);
    }

    fn a_new_file_at_an_inode_the_page_read_is_read_as_new_in(format: Format) {
        let five = [(1, 50), (2, 60), (3, 70), (4, 80), (5, 90)];
        for (id, n) in [(0x77, 5), (0, 2)] {
            let dir = tempfile::tempdir().expect("dir");
            let a = dir.path().join("a.journal");
            journal_in(format, &a, 1, &FOUR, 4);
            let journal_ = Journal::open(Source::Directory(dir.path().into()), &release("24.04"))
                .expect("opens");
            let (_page, _f, first) =
                page_then_follow(&journal_, &crate::Query::every(), 10, || {
                    // Written in place: the same inode.
                    let old = std::fs::read(&a).expect("read");
                    journal_in(format, &a, 2, &five, n);
                    let mut bytes = std::fs::read(&a).expect("read");
                    if id == 0 {
                        bytes[24..40].copy_from_slice(&old[24..40]);
                    } else {
                        bytes[24..40].copy_from_slice(&[id; 16]);
                    }
                    std::fs::write(&a, &bytes).expect("write");
                });
            let want: Vec<u64> = (1..=n).collect();
            assert_eq!(
                seqnums(&first.records),
                want,
                "file id {id:#x}, {n} entries"
            );
        }
    }

    /// The output limit stops the first batch as it stops a drain: `more`
    /// is set, and drains go on from there with no event, until a batch
    /// reads to the end.
    #[test]
    fn the_limit_stops_a_batch_with_more_set() {
        each_format(the_limit_stops_a_batch_with_more_set_in);
    }

    fn the_limit_stops_a_batch_with_more_set_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 10);
        c.set(H_N_ENTRIES, 2);
        let t = c.file();
        let journal_ =
            Journal::open(Source::Files(vec![t.path().into()]), &release("24.04")).expect("opens");
        let limits = crate::Limits::with_output(per_record_in(format, 3) + 5);
        let page = journal_.page(&crate::Query::every(), &crate::Position::Tail, 2, &limits);
        c.set(H_N_ENTRIES, 10);
        grow(&c, &t);
        let (mut f, first) = journal_
            .follow(crate::Query::every(), &page.end, &limits)
            .expect("follows");
        assert_eq!(messages(&first.records), ["m2", "m3", "m4"]);
        assert!(first.more && first.errors.is_empty(), "{first:?}");
        let next = f.drain(&limits);
        assert_eq!(messages(&next.records), ["m5", "m6", "m7"]);
        assert!(next.more, "{next:?}");
        let last = f.drain(&limits);
        assert_eq!(messages(&last.records), ["m8", "m9"]);
        assert!(!last.more && last.errors.is_empty(), "{last:?}");
    }

    /// One crafted entry's payloads.
    type Row = &'static [&'static [u8]];

    // A filtered page and its follow give every entry once (sb-journal
    // hardening, Task 33: follow-H1, page-H1, fwd-M2; journal spec decision
    // 13). The fixtures are what a file holds at each of the page's reads:
    // the test seam between its two header reads, and what journald has
    // written by the time the follow starts.

    /// The DATA object of `payload` in `c`, found by its bytes.
    fn data_of(c: &Crafted, payload: &[u8]) -> u64 {
        let at = c.b.windows(payload.len()).position(|w| w == payload);
        at.expect("the payload") as u64 - c.format().payload_at()
    }

    fn query(name: &[u8], value: &[u8]) -> crate::Query {
        crate::Query::field(&crate::FieldName::new(name).expect("a field name"), value)
    }

    /// A cursor in a crafted file's seqnum space at `seqnum`, of its boot.
    fn at_seqnum(seqnum: u64) -> Cursor {
        Cursor {
            seqnum_id: [0; 16],
            seqnum,
            boot_id: [7; 16],
            monotonic: seqnum,
            realtime: 1_000_000 + seqnum,
            xor_hash: 0,
        }
    }

    /// `c`'s bytes written in place at `path` (the same inode), the header
    /// counting `n` entries.
    fn counted(c: &Crafted, path: &Path, n: u64) {
        let mut c = c.clone();
        c.set(H_N_ENTRIES, n);
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        std::fs::write(path, &c.b).expect("write");
    }

    /// A page of `q` at `at`, at most `n`, over `paths`; then a follow from
    /// its end under `follow_limits`.
    struct Scenario<'a> {
        paths: &'a [PathBuf],
        q: crate::Query,
        at: crate::Position,
        n: usize,
        follow_limits: crate::Limits,
        /// How many distinct read errors the page reports.
        page_errors: usize,
    }

    impl<'a> Scenario<'a> {
        fn tail(paths: &'a [PathBuf], q: crate::Query) -> Self {
            Self {
                paths,
                q,
                at: crate::Position::Tail,
                n: 10,
                follow_limits: crate::Limits::default(),
                page_errors: 0,
            }
        }
    }

    /// `s`'s page, with `between` run at the seam between the page's two
    /// header reads of a file, then `after`, then the follow from the
    /// page's end, drained until a drain reads to the end and shows
    /// nothing. Returns what the page showed and what every batch showed,
    /// in order (each record as `show` names it), and whether a batch
    /// stopped at the limit.
    fn seamed(
        s: &Scenario<'_>,
        between: impl FnMut(&Path) + 'static,
        after: impl FnOnce(),
        show: impl Fn(&Record) -> String,
    ) -> (Vec<String>, Vec<String>, bool) {
        let follow_limits = &s.follow_limits;
        let journal_ =
            Journal::open(Source::Files(s.paths.to_vec()), &release("24.04")).expect("opens");
        let limits = crate::Limits::default();
        let page = crate::testing::seam::between_header_reads(between, || {
            journal_.page(&s.q, &s.at, s.n, &limits)
        });
        assert_eq!(page.errors.shown.len(), s.page_errors, "{:?}", page.errors);
        after();
        let (mut f, first) = journal_
            .follow(s.q.clone(), &page.end, follow_limits)
            .expect("follows");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        let mut followed: Vec<String> = first.records.iter().map(&show).collect();
        let mut stopped = first.more;
        let mut last_more = first.more;
        for _ in 0..20 {
            let b = f.drain(follow_limits);
            assert!(b.errors.is_empty(), "{:?}", b.errors);
            let empty = b.records.is_empty();
            followed.extend(b.records.iter().map(&show));
            stopped |= b.more;
            if empty && !b.more && !last_more {
                break;
            }
            last_more = b.more;
        }
        let shown = page.records.iter().map(&show).collect();
        (shown, followed, stopped)
    }

    fn message(r: &Record) -> String {
        String::from_utf8_lossy(r.message().unwrap_or_default()).into_owned()
    }

    /// An entry appended and linked during the page: the header counts 2
    /// at the page's first read, the value's list holds 3 at the lookup,
    /// and the header counts 3 after it. The page shows the third entry,
    /// and the follow, drained again, doesn't show it a second time. For
    /// a Tail page and for a forward page that reaches the tail.
    #[test]
    fn an_entry_linked_during_the_page_is_delivered_once() {
        each_format(an_entry_linked_during_the_page_is_delivered_once_in);
    }

    fn an_entry_linked_during_the_page_is_delivered_once_in(format: Format) {
        let a: &[&[u8]] = &[b"A=1"];
        let (c, _e) = crafted::journal_of_in(format, &[a, a, a]);
        for at in [crate::Position::Tail, crate::Position::After(at_seqnum(0))] {
            let t = tempfile::NamedTempFile::new().expect("temp file");
            counted(&c, t.path(), 2);
            let (raise, done) = (c.clone(), c.clone());
            let path = t.path().to_path_buf();
            let paths = [path.clone()];
            let s = Scenario {
                at: at.clone(),
                ..Scenario::tail(&paths, query(b"A", b"1"))
            };
            let (page, followed, _) = seamed(
                &s,
                move |p| counted(&raise, p, 3),
                || counted(&done, &path, 3),
                message,
            );
            let all: Vec<String> = page.iter().chain(&followed).cloned().collect();
            assert_eq!(all, ["m0", "m1", "m2"], "{at:?}: page {page:?}");
        }
    }

    /// Two lists read at different moments: a query of two values, `A=1`
    /// (m0, m2) and `B=1` (m1, m3). At `A=1`'s lookup m2 isn't linked into
    /// its list yet; at `B=1`'s, m3 is. The header counts both only after
    /// the page's first read. Each entry is delivered once, though m2 is
    /// older than m3, which the page showed.
    #[test]
    fn two_lists_read_at_different_moments_give_each_entry_once() {
        each_format(two_lists_read_at_different_moments_give_each_entry_once_in);
    }

    fn two_lists_read_at_different_moments_give_each_entry_once_in(format: Format) {
        let (a, b): (&[&[u8]], &[&[u8]]) = (&[b"A=1"], &[b"B=1"]);
        let (whole, _e) = crafted::journal_of_in(format, &[a, b, a, b]);
        let mut early = whole.clone();
        let list_a = data_of(&early, b"A=1");
        early.set(list_a + D_N_ENTRIES, 1);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 2);
        let (raise, done) = (whole.clone(), whole.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let q = query(b"A", b"1").or(query(b"B", b"1"));
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, q),
            move |p| counted(&raise, p, 4),
            || counted(&done, &path, 4),
            message,
        );
        let all: Vec<String> = page.iter().chain(&followed).cloned().collect();
        assert_eq!(all, ["m0", "m1", "m2", "m3"], "page {page:?}");
    }

    /// The mirror case (follow-N1): the header counts 3 while the value's
    /// list holds 2, journald not yet done linking the third. It is
    /// delivered once. Where journald linked it by the page's second header
    /// read, by the page, as journalctl would show it then; where it hadn't
    /// yet, by the follow, which the page hands over at its index and which
    /// reads it by its items.
    #[test]
    fn the_newest_counted_entry_not_yet_listed_is_delivered_once() {
        each_format(the_newest_counted_entry_not_yet_listed_is_delivered_once_in);
    }

    fn the_newest_counted_entry_not_yet_listed_is_delivered_once_in(format: Format) {
        let a: Row = &[b"A=1", b"B=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        let mut early = whole.clone();
        early.set(data_of(&early, b"A=1") + D_N_ENTRIES, 2);
        let both = query(b"A", b"1").and(query(b"B", b"1")).expect("an AND");
        for (linked_by_the_second_read, q) in [
            (true, query(b"A", b"1")),
            (false, query(b"A", b"1")),
            (true, both.clone()),
            (false, both),
        ] {
            let t = tempfile::NamedTempFile::new().expect("temp file");
            counted(&early, t.path(), 3);
            let path = t.path().to_path_buf();
            let paths = [path.clone()];
            let (link, done) = (whole.clone(), whole.clone());
            let (page, followed, _) = seamed(
                &Scenario::tail(&paths, q),
                move |p| {
                    if linked_by_the_second_read {
                        counted(&link, p, 3);
                    }
                },
                || counted(&done, &path, 3),
                message,
            );
            let (want_page, want_followed): (&[&str], &[&str]) = if linked_by_the_second_read {
                (&["m0", "m1", "m2"], &[])
            } else {
                (&["m0", "m1"], &["m2"])
            };
            assert_eq!(page, want_page, "linked: {linked_by_the_second_read}");
            assert_eq!(
                followed, want_followed,
                "linked: {linked_by_the_second_read}"
            );
        }
    }

    /// An entry journald counted and never linked (it died mid-link, and
    /// nothing writes the file again): a filtered page doesn't show it, as
    /// journalctl's filtered view doesn't, and the follow, which reads
    /// entries by their items, does (Difference 12).
    #[test]
    fn an_entry_journald_never_linked_is_the_follows_to_show() {
        each_format(an_entry_journald_never_linked_is_the_follows_to_show_in);
    }

    fn an_entry_journald_never_linked_is_the_follows_to_show_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (mut c, _e) = crafted::journal_of_in(format, &[a, a, a]);
        c.set(data_of(&c, b"A=1") + D_N_ENTRIES, 2);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&c, t.path(), 3);
        let paths = [t.path().to_path_buf()];
        for at in [crate::Position::Tail, crate::Position::After(at_seqnum(1))] {
            let s = Scenario {
                at: at.clone(),
                ..Scenario::tail(&paths, query(b"A", b"1"))
            };
            let (page, followed, _) = seamed(&s, |_| {}, || {}, message);
            let want: &[&str] = match at {
                crate::Position::Tail => &["m0", "m1"],
                _ => &["m1"],
            };
            assert_eq!(page, want, "{at:?}");
            assert_eq!(followed, ["m2"], "{at:?}");
        }
    }

    /// An AND query whose walk drives through `A=1`'s list (the smaller):
    /// m3 is in it, and `B=1`'s list doesn't hold m3 yet. The walk shows m3,
    /// checking `B=1` by its items, so the second header read takes it as
    /// shown, through the list the walk walks, and doesn't hand it to the
    /// follow again. For a Tail page and a forward page.
    #[test]
    fn an_and_querys_entry_in_its_driving_list_is_delivered_once() {
        each_format(an_and_querys_entry_in_its_driving_list_is_delivered_once_in);
    }

    fn an_and_querys_entry_in_its_driving_list_is_delivered_once_in(format: Format) {
        let (ab, b): (Row, Row) = (&[b"A=1", b"B=1"], &[b"B=1"]);
        let (whole, _e) = crafted::journal_of_in(format, &[ab, b, b, ab]);
        let mut early = whole.clone();
        early.set(data_of(&early, b"B=1") + D_N_ENTRIES, 3);
        let both = query(b"A", b"1").and(query(b"B", b"1")).expect("an AND");
        for (at, want) in [
            (crate::Position::Tail, vec!["m0", "m3"]),
            (crate::Position::After(at_seqnum(1)), vec!["m3"]),
        ] {
            let t = tempfile::NamedTempFile::new().expect("temp file");
            counted(&early, t.path(), 4);
            let path = t.path().to_path_buf();
            let paths = [path.clone()];
            let done = whole.clone();
            let s = Scenario {
                at: at.clone(),
                ..Scenario::tail(&paths, both.clone())
            };
            let (page, followed, _) = seamed(&s, |_| {}, || counted(&done, &path, 4), message);
            assert_eq!(page, want, "{at:?}");
            assert!(followed.is_empty(), "{at:?}: {followed:?}");
        }
    }

    /// An older entry the second header read finds carrying the value but
    /// not in its list, while a newer one is: journald moved on from it, so
    /// its link was cut off for good, and neither the page nor the follow
    /// shows it (as journalctl's filtered view doesn't). Here m1 at the
    /// first read's newest index, m2 listed by the second read.
    #[test]
    fn an_older_entry_cut_off_for_good_is_shown_by_neither() {
        each_format(an_older_entry_cut_off_for_good_is_shown_by_neither_in);
    }

    fn an_older_entry_cut_off_for_good_is_shown_by_neither_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        // `A=1`'s list: m0 only at the lookup; m0 and m2 by the second read
        // (m1's slot left out of the array: the list is m0, then m2).
        let list = data_of(&whole, b"A=1");
        let mut early = whole.clone();
        early.set(list + D_N_ENTRIES, 1);
        let mut later = whole.clone();
        let rest = later.get(list + D_ENTRY_ARRAY);
        let m2 = later.array_item(rest, 1);
        later.set_array_item(rest, 0, m2);
        later.set(list + D_N_ENTRIES, 2);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 2);
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let (raise, done) = (later.clone(), later);
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| counted(&raise, p, 3),
            || counted(&done, &path, 3),
            message,
        );
        assert_eq!(page, ["m0", "m2"]);
        assert!(followed.is_empty(), "{followed:?}");
    }

    /// A file journald archived between the page's two header reads, its
    /// last entry counted but cut off from `A=1`'s list: the page, which
    /// judged the file live at its first read, doesn't show that entry,
    /// and the follow, reading it by its items, does (Difference 12: where
    /// journald writes it again in its next file, the follow shows both).
    #[test]
    fn an_entry_cut_off_by_an_archive_during_the_page_isnt_shown_by_it() {
        each_format(an_entry_cut_off_by_an_archive_during_the_page_isnt_shown_by_it_in);
    }

    fn an_entry_cut_off_by_an_archive_during_the_page_isnt_shown_by_it_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        let mut early = whole.clone();
        early.set(data_of(&early, b"A=1") + D_N_ENTRIES, 2);
        let mut archived = early.clone();
        archived.b[16] = 2; // STATE_ARCHIVED
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 2);
        let paths = [t.path().to_path_buf()];
        let (cut, done) = (archived.clone(), archived);
        let path = t.path().to_path_buf();
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| counted(&cut, p, 3),
            || counted(&done, &path, 3),
            message,
        );
        assert_eq!(page, ["m0", "m1"]);
        assert_eq!(followed, ["m2"]);
    }

    /// A page stopped by its count, and a Before page, over a file written
    /// during the page: the follow starts at the journal's end, repeating
    /// nothing the page showed (and, after the Before page, skipping m2,
    /// as decision 10 says of a follow from a page back).
    #[test]
    fn a_limited_or_before_page_hands_over_at_the_journals_end() {
        each_format(a_limited_or_before_page_hands_over_at_the_journals_end_in);
    }

    fn a_limited_or_before_page_hands_over_at_the_journals_end_in(format: Format) {
        let a: &[&[u8]] = &[b"A=1"];
        let (c, _e) = crafted::journal_of_in(format, &[a, a, a]);
        let cases = [
            (crate::Position::Tail, 1, vec!["m2"]),
            (crate::Position::Before(at_seqnum(3)), 10, vec!["m0", "m1"]),
        ];
        for (at, n, want) in cases {
            let t = tempfile::NamedTempFile::new().expect("temp file");
            counted(&c, t.path(), 2);
            let (raise, done) = (c.clone(), c.clone());
            let path = t.path().to_path_buf();
            let paths = [path.clone()];
            let s = Scenario {
                at: at.clone(),
                n,
                ..Scenario::tail(&paths, query(b"A", b"1"))
            };
            let (page, followed, _) = seamed(
                &s,
                move |p| counted(&raise, p, 3),
                || counted(&done, &path, 3),
                message,
            );
            assert_eq!(page, want, "{at:?}");
            assert!(followed.is_empty(), "{at:?}: {followed:?}");
        }
    }

    /// A Tail page over an archived file and an active one, unfiltered and
    /// filtered, then a follow: its first batch, and every drain, is empty.
    /// The archived file's last entry, which journald was cut off from
    /// linking into `A=1`'s list (Difference 12), isn't shown by the
    /// filtered page and doesn't come again: an archived file isn't read
    /// again.
    #[test]
    fn an_archived_files_cut_off_entry_doesnt_come_again() {
        each_format(an_archived_files_cut_off_entry_doesnt_come_again_in);
    }

    fn an_archived_files_cut_off_entry_doesnt_come_again_in(format: Format) {
        let (old_a, new_a, new): (Row, Row, Row) =
            (&[b"A=1", b"F=old"], &[b"A=1", b"F=new"], &[b"F=new"]);
        let (mut archived, _e) = crafted::journal_of_in(format, &[old_a, old_a]);
        archived.set(data_of(&archived, b"A=1") + D_N_ENTRIES, 1);
        archived.b[16] = 2; // STATE_ARCHIVED
        let (mut active, entries) = crafted::journal_of_in(format, &[new, new_a]);
        active.b[72..88].copy_from_slice(&[2; 16]); // its own seqnum id
        for (i, &e) in entries.iter().enumerate() {
            active.set(e + 24, 5_000_000 + i as u64); // realtime
            active.set(e + 32, 500 + i as u64); // monotonic
        }
        let (old, cur) = (archived.file(), active.file());
        let paths = [old.path().to_path_buf(), cur.path().to_path_buf()];
        let field = crate::FieldName::new(b"F").expect("a field name");
        let show = move |r: &Record| {
            let f = String::from_utf8_lossy(r.field(&field).unwrap_or_default()).into_owned();
            format!("{f}/{}", message(r))
        };
        let cases = [
            (query(b"A", b"1"), vec!["old/m0", "new/m1"]),
            (
                crate::Query::every(),
                vec!["old/m0", "old/m1", "new/m0", "new/m1"],
            ),
        ];
        for (q, want) in cases {
            let (page, followed, _) =
                seamed(&Scenario::tail(&paths, q), |_| {}, || {}, show.clone());
            assert_eq!(page, want);
            assert!(followed.is_empty(), "{followed:?}");
        }
    }

    /// A first batch cut by a small output limit (`Batch::more`), after a
    /// page over a file written during it: the drains repeat nothing the
    /// page showed and lose nothing written after it.
    #[test]
    fn a_follow_cut_by_its_limit_repeats_and_loses_nothing() {
        each_format(a_follow_cut_by_its_limit_repeats_and_loses_nothing_in);
    }

    fn a_follow_cut_by_its_limit_repeats_and_loses_nothing_in(format: Format) {
        let a: &[&[u8]] = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a; 8]);
        // At the lookup `A=1`'s list holds 4, the header counting 3; the
        // seam raises it to 4; then journald writes the other four.
        let mut early = whole.clone();
        early.set(data_of(&early, b"A=1") + D_N_ENTRIES, 4);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 3);
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        // One record a batch: the shared object and one MESSAGE, as
        // `per_record` counts `crafted::journal`'s.
        let s = Scenario {
            follow_limits: crate::Limits::with_output(per_record_in(format, 1) + 5),
            ..Scenario::tail(&paths, query(b"A", b"1"))
        };
        let (page, followed, stopped) = seamed(
            &s,
            move |p| counted(&early, p, 4),
            || counted(&whole, &path, 8),
            message,
        );
        assert!(stopped, "a batch stopped at the limit");
        let all: Vec<String> = page.iter().chain(&followed).cloned().collect();
        let want: Vec<String> = (0..8).map(|i| format!("m{i}")).collect();
        assert_eq!(all, want, "page {page:?}");
    }

    /// A value first written during the page has no DATA object at the
    /// lookup: the page looks it up again for its second read, shows its
    /// entry, and the follow doesn't show it again.
    #[test]
    fn a_value_first_written_during_the_page_is_delivered_once() {
        each_format(a_value_first_written_during_the_page_is_delivered_once_in);
    }

    fn a_value_first_written_during_the_page_is_delivered_once_in(format: Format) {
        let (c, b): (&[&[u8]], &[&[u8]]) = (&[b"C=1"], &[b"B=1"]);
        let (whole, _e) = crafted::journal_of_in(format, &[c, c, b]);
        // Before it: `B=1` unlinked from its hash bucket, as if not yet
        // written. It is alone in its bucket here.
        let mut early = whole.clone();
        let d = data_of(&early, b"B=1");
        let buckets = early.get(H_HASH_TABLE_SIZE) / 16;
        let bucket = early.get(H_HASH_TABLE_OFFSET) + early.get(d + D_HASH) % buckets * 16;
        assert_eq!(
            (early.get(bucket), early.get(bucket + 8)),
            (d, d),
            "B=1 alone in its bucket"
        );
        early.set(bucket, 0);
        early.set(bucket + 8, 0);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 2);
        let (raise, done) = (whole.clone(), whole.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"B", b"1")),
            move |p| counted(&raise, p, 3),
            || counted(&done, &path, 3),
            message,
        );
        assert_eq!(page, ["m2"]);
        assert!(followed.is_empty(), "{followed:?}");
    }

    /// A file journald just created: at the page's first header read it
    /// counts nothing and has no entry array yet; at the lookup `A=1`'s
    /// list holds m0; at the second read the header counts m0 and m1 and
    /// names the array. The second read follows the list from the header
    /// it read, so each entry is delivered once.
    #[test]
    fn a_file_empty_at_the_first_read_gives_each_entry_once() {
        each_format(a_file_empty_at_the_first_read_gives_each_entry_once_in);
    }

    fn a_file_empty_at_the_first_read_gives_each_entry_once_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a]);
        let mut early = whole.clone();
        early.set(data_of(&early, b"A=1") + D_N_ENTRIES, 1);
        early.set(H_ENTRY_ARRAY_OFFSET, 0);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 0);
        let (raise, done) = (whole.clone(), whole.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| counted(&raise, p, 2),
            || counted(&done, &path, 2),
            message,
        );
        let all: Vec<String> = page.iter().chain(&followed).cloned().collect();
        assert_eq!(all, ["m0", "m1"], "page {page:?}");
    }

    /// A forward page whose seek meets an error (`B=1`'s list names a DATA
    /// object as its array) leaves out what the second header read added,
    /// and hands over the first read's count, so the follow delivers m4,
    /// which `A=1`'s list didn't hold yet at the lookup.
    #[test]
    fn a_forward_seek_error_hands_over_the_first_count() {
        each_format(a_forward_seek_error_hands_over_the_first_count_in);
    }

    fn a_forward_seek_error_hands_over_the_first_count_in(format: Format) {
        let (a, b): (Row, Row) = (&[b"A=1"], &[b"B=1"]);
        let (mut whole, _e) = crafted::journal_of_in(format, &[a, b, a, a, a]);
        let (list_a, list_b) = (data_of(&whole, b"A=1"), data_of(&whole, b"B=1"));
        whole.set(list_b + D_N_ENTRIES, 2);
        whole.set(list_b + D_ENTRY_ARRAY, list_a);
        let mut early = whole.clone();
        early.set(list_a + D_N_ENTRIES, 3);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 4);
        let (raise, done) = (whole.clone(), whole.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let s = Scenario {
            at: crate::Position::After(at_seqnum(3)),
            page_errors: 1,
            ..Scenario::tail(&paths, query(b"A", b"1").or(query(b"B", b"1")))
        };
        let (page, followed, _) = seamed(
            &s,
            move |p| counted(&raise, p, 5),
            || counted(&done, &path, 5),
            message,
        );
        assert_eq!(page, ["m3"]);
        assert_eq!(followed, ["m4"]);
    }

    /// A `.journal~` file (renamed by a journald that found it dirty) is
    /// sealed: its newest counted entry, missing from `A=1`'s list, is one
    /// journald was cut off from linking for good, so the filtered page
    /// doesn't read it again and doesn't show it (Difference 12).
    #[test]
    fn a_dirty_files_unlisted_last_entry_isnt_shown() {
        each_format(a_dirty_files_unlisted_last_entry_isnt_shown_in);
    }

    fn a_dirty_files_unlisted_last_entry_isnt_shown_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (mut c, _e) = crafted::journal_of_in(format, &[a, a, a]);
        c.set(data_of(&c, b"A=1") + D_N_ENTRIES, 2);
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("system@0000-1111.journal~");
        counted(&c, &path, 3);
        let paths = [path];
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            |_| {},
            || {},
            message,
        );
        assert_eq!(page, ["m0", "m1"]);
        assert!(followed.is_empty(), "{followed:?}");
    }

    /// A count that falls between the page's two header reads isn't a file
    /// journald appends to: the page hands over the first count, so the
    /// follow repeats nothing the page showed.
    #[test]
    fn a_count_that_falls_between_the_reads_hands_over_the_first() {
        each_format(a_count_that_falls_between_the_reads_hands_over_the_first_in);
    }

    fn a_count_that_falls_between_the_reads_hands_over_the_first_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (c, _e) = crafted::journal_of_in(format, &[a, a]);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&c, t.path(), 2);
        let (fall, back) = (c.clone(), c.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| counted(&fall, p, 1),
            || counted(&back, &path, 2),
            message,
        );
        assert_eq!(page, ["m0", "m1"]);
        assert!(followed.is_empty(), "{followed:?}");
    }

    /// A file that held only its hash table at the page's first read (no
    /// entries, an arena that ends there) and three entries of a new value
    /// at the second: the page looks the value up again, reads the entries
    /// past the arena it opened with, and shows them; the follow shows
    /// none again.
    #[test]
    fn entries_past_the_first_reads_arena_are_shown_once() {
        each_format(entries_past_the_first_reads_arena_are_shown_once_in);
    }

    fn entries_past_the_first_reads_arena_are_shown_once_in(format: Format) {
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        // The header, then the hash table alone, its buckets empty.
        let table = whole.get(H_HASH_TABLE_OFFSET);
        let size = whole.get(H_HASH_TABLE_SIZE);
        let mut early = Crafted {
            b: whole.b[..usize::try_from(table + size).expect("small")].to_vec(),
        };
        early.b[usize::try_from(table).expect("small")..].fill(0);
        early.set(H_ENTRY_ARRAY_OFFSET, 0);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 0);
        let (raise, done) = (whole.clone(), whole.clone());
        let path = t.path().to_path_buf();
        let paths = [path.clone()];
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| counted(&raise, p, 3),
            || counted(&done, &path, 3),
            message,
        );
        assert_eq!(page, ["m0", "m1", "m2"]);
        assert!(followed.is_empty(), "{followed:?}");
    }

    /// The seam's hook lasts for its `between_header_reads` only: a page
    /// after it doesn't call it.
    #[test]
    fn the_seam_is_gone_after_its_run() {
        let a: Row = &[b"A=1"];
        let (c, _e) = crafted::journal_of(&[a, a]);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&c, t.path(), 2);
        let journal_ =
            Journal::open(Source::Files(vec![t.path().into()]), &release("24.04")).expect("opens");
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = std::rc::Rc::clone(&calls);
        let limits = crate::Limits::default();
        let q = query(b"A", b"1");
        let page = || journal_.page(&q, &crate::Position::Tail, 10, &limits);
        crate::testing::seam::between_header_reads(move |_| seen.set(seen.get() + 1), page);
        assert_eq!(calls.get(), 1);
        page();
        assert_eq!(calls.get(), 1, "called after its run");
    }

    // The handover across files (sb-journal hardening, Task 34; journal spec
    // decision 13, the plan's decision 26).

    /// `c` with its own seqnum id `id` and its entries renumbered from
    /// `first`, their boot, times and XOR hash kept: the copies a flush
    /// writes to /var (255 numbers them anew under the runtime file's
    /// seqnum id, 249 under the new file's).
    fn flushed_copy(c: &Crafted, entries: &[u64], id: u8, first: u64) -> Crafted {
        let mut copy = c.clone();
        copy.b[72..88].copy_from_slice(&[id; 16]);
        copy.b[24..40].copy_from_slice(&[id; 16]); // its own file id
        for (k, &e) in entries.iter().enumerate() {
            copy.set(e + 16, first + k as u64);
        }
        copy
    }

    /// A flush between a filtered page and its follow, the /var file new to
    /// the follow: the page read /run and showed m2, written during the
    /// page, past its end's newest entry (m1, from the first header read).
    /// The flush copies m0 to m2 to /var, adds m3 there and removes /run.
    /// The follow walks /var's file back to m1's copy and drops m2's copy,
    /// whose identity the page's end carries: each entry once (the plan's
    /// decision 26), the end's newest not raised.
    #[test]
    fn a_flush_to_a_new_var_file_doesnt_repeat_what_the_page_showed() {
        each_format(a_flush_to_a_new_var_file_doesnt_repeat_what_the_page_showed_in);
    }

    fn a_flush_to_a_new_var_file_doesnt_repeat_what_the_page_showed_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        let run_file = run.join("system.journal");
        counted(&whole, &run_file, 2);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let q = query(b"A", b"1");
        let limits = crate::Limits::default();
        let raise = whole.clone();
        let page = crate::testing::seam::between_header_reads(
            move |p| counted(&raise, p, 3),
            || journal_.page(&q, &crate::Position::Tail, 10, &limits),
        );
        assert_eq!(messages(&page.records), ["m0", "m1", "m2"]);
        let newest = page.end.newest.as_ref().map(|(c, _)| c.seqnum);
        assert_eq!(newest, Some(2), "the end's newest is the first read's");
        // The flush: /var's new file holds the copies and m3; /run goes.
        let (four, entries) = crafted::journal_of_in(format, &[a, a, a, a]);
        std::fs::create_dir(&var).expect("var's directory");
        counted(
            &flushed_copy(&four, &entries, 5, 11),
            &var.join("system.journal"),
            4,
        );
        std::fs::remove_file(&run_file).expect("the flush removes /run");
        // The end's equality includes what it carries for the flush.
        let mut without = page.end.clone();
        without.shown.clear();
        assert_ne!(without, page.end);
        let (_f, first) = journal_.follow(q, &page.end, &limits).expect("follows");
        assert_eq!(messages(&first.records), ["m3"]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
    }

    /// 249's rotation repeat (platform note, "Writing"): journald writes
    /// the entry a rotation interrupted again as the next file's first, in
    /// the same directory, with the same boot, times and XOR hash, and
    /// journalctl shows both. A filtered page showed m2 (written during
    /// it); the new system.journal starts with m2 again, then m3: the
    /// follow shows the repeat, as only a copy in another directory (a
    /// flush, from /run to /var) is dropped.
    #[test]
    fn a_rotation_repeat_in_the_same_directory_is_shown_again() {
        each_format(a_rotation_repeat_in_the_same_directory_is_shown_again_in);
    }

    fn a_rotation_repeat_in_the_same_directory_is_shown_again_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let active = dir.path().join("system.journal");
        let a: Row = &[b"A=1"];
        let (whole, _e) = crafted::journal_of_in(format, &[a, a, a]);
        counted(&whole, &active, 2);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("22.04")).expect("opens");
        let q = query(b"A", b"1");
        let limits = crate::Limits::default();
        let raise = whole.clone();
        let page = crate::testing::seam::between_header_reads(
            move |p| counted(&raise, p, 3),
            || journal_.page(&q, &crate::Position::Tail, 10, &limits),
        );
        let real = |r: &[Record]| -> Vec<u64> { r.iter().map(|r| r.cursor().realtime).collect() };
        assert_eq!(real(&page.records), [1_000_001, 1_000_002, 1_000_003]);
        // The rotation: the file archived and renamed; the next one holds
        // m2 again (seqnum 4, m2's times) and m3.
        let mut bytes = std::fs::read(&active).expect("read");
        bytes[16] = 2; // STATE_ARCHIVED
        std::fs::write(&active, &bytes).expect("write");
        std::fs::rename(&active, dir.path().join("system@1-2-3.journal")).expect("rename");
        let (mut next, entries) = crafted::journal_of_in(format, &[a, a]);
        next.b[24..40].copy_from_slice(&[9; 16]); // its own file id
        for (k, (&e, t)) in entries.iter().zip([3u64, 4]).enumerate() {
            next.set(e + 16, 4 + k as u64); // seqnum
            next.set(e + 24, 1_000_000 + t); // realtime
            next.set(e + 32, t); // monotonic
        }
        counted(&next, &active, 2);
        let (_f, first) = journal_.follow(q, &page.end, &limits).expect("follows");
        assert_eq!(real(&first.records), [1_000_003, 1_000_004]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
    }

    /// A rotation between the page and the follow: the archived file keeps
    /// its position by inode and file id (renamed, with the entry journald
    /// wrote before archiving it), and every entry of the new
    /// system.journal is delivered. Nothing lost or doubled.
    #[test]
    fn a_rotation_between_the_page_and_the_follow_loses_and_doubles_nothing() {
        each_format(a_rotation_between_the_page_and_the_follow_loses_and_doubles_nothing_in);
    }

    fn a_rotation_between_the_page_and_the_follow_loses_and_doubles_nothing_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let active = dir.path().join("system.journal");
        journal_in(format, &active, 1, &FOUR, 2);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("24.04")).expect("opens");
        let (page, mut f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            // m3 written, then the file archived under its new name...
            set_n(&active, 3);
            let mut bytes = std::fs::read(&active).expect("read");
            bytes[16] = 2; // STATE_ARCHIVED
            std::fs::write(&active, &bytes).expect("write");
            std::fs::rename(&active, dir.path().join("system@1-2-3.journal")).expect("rename");
            // ...and a new system.journal with 5 and 6.
            journal_in(format, &active, 1, &[(5, 50), (6, 60)], 2);
        });
        assert_eq!(seqnums(&page), [1, 2]);
        assert_eq!(seqnums(&first.records), [3, 5, 6]);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert!(f.drain(&crate::Limits::default()).records.is_empty());
    }

    /// A flush between the page and the follow where the page read the
    /// /var file: the copies appended to it, of /run entries the page
    /// showed, are shown again, as Difference 13 says of a followed file.
    #[test]
    fn a_flush_into_a_var_file_the_page_read_shows_the_copies_again() {
        each_format(a_flush_into_a_var_file_the_page_read_shows_the_copies_again_in);
    }

    fn a_flush_into_a_var_file_the_page_read_shows_the_copies_again_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        std::fs::create_dir(&var).expect("var's directory");
        let (run_file, var_file) = (run.join("system.journal"), var.join("system.journal"));
        journal_in(format, &run_file, 1, &FOUR[..2], 2);
        // /var's own entry, of another seqnum id, before /run's.
        journal_in(format, &var_file, 2, &[(1, 5), (2, 10), (3, 20)], 1);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let (page, _f, first) = page_then_follow(&journal_, &crate::Query::every(), 10, || {
            // The flush appends copies of /run's two entries to /var's file
            // (its seqnum id, its next seqnums, /run's times).
            set_n(&var_file, 3);
            std::fs::remove_file(&run_file).expect("the flush removes /run");
        });
        let ids = |r: &[Record]| -> Vec<(u8, u64)> {
            r.iter()
                .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
                .collect()
        };
        assert_eq!(ids(&page), [(2, 1), (1, 1), (1, 2)]);
        assert_eq!(ids(&first.records), [(2, 2), (2, 3)], "shown again");
    }

    /// A file the page couldn't open (journald creating it: no header yet)
    /// is read as a new file by the follow, after the page's newest entry;
    /// its open error is the page's, reported once.
    #[test]
    fn a_file_the_page_couldnt_open_is_read_as_new() {
        each_format(a_file_the_page_couldnt_open_is_read_as_new_in);
    }

    fn a_file_the_page_couldnt_open_is_read_as_new_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let (a, b) = (dir.path().join("a.journal"), dir.path().join("b.journal"));
        journal_in(format, &a, 1, &FOUR[..2], 2);
        std::fs::write(&b, b"").expect("an empty file");
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("24.04")).expect("opens");
        let limits = crate::Limits::default();
        let page = journal_.page(&crate::Query::every(), &crate::Position::Tail, 10, &limits);
        assert_eq!(seqnums(&page.records), [1, 2]);
        assert_eq!(page.errors.shown.len(), 1, "{:?}", page.errors);
        journal_in(format, &b, 2, &[(1, 30), (2, 40)], 2);
        let (mut f, first) = journal_
            .follow(crate::Query::every(), &page.end, &limits)
            .expect("follows");
        let got: Vec<(u8, u64)> = first
            .records
            .iter()
            .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
            .collect();
        assert_eq!(got, [(2, 1), (2, 2)]);
        assert!(first.errors.is_empty(), "reported once: {:?}", first.errors);
        assert!(f.drain(&limits).errors.is_empty());
    }

    /// A merged page over two files hands each over on its own: `a`,
    /// written during the page (its list holds an entry its first header
    /// read didn't count), at its second read's count; `b`, whose newest
    /// counted entry isn't linked yet, at that entry's index. Each entry
    /// of both is delivered once.
    #[test]
    fn a_merged_page_hands_each_file_over_on_its_own() {
        each_format(a_merged_page_hands_each_file_over_on_its_own_in);
    }

    fn a_merged_page_hands_each_file_over_on_its_own_in(format: Format) {
        let (in_a, in_b): (Row, Row) = (&[b"A=1", b"F=a"], &[b"A=1", b"F=b"]);
        let (a_whole, _e) = crafted::journal_of_in(format, &[in_a, in_a, in_a]);
        let (mut b_whole, b_entries) = crafted::journal_of_in(format, &[in_b, in_b, in_b]);
        b_whole.b[72..88].copy_from_slice(&[2; 16]); // its own seqnum id
        for (i, &e) in b_entries.iter().enumerate() {
            b_whole.set(e + 24, 2_000_000 + i as u64); // realtime, after a's
        }
        let mut b_early = b_whole.clone();
        b_early.set(data_of(&b_early, b"A=1") + D_N_ENTRIES, 2);
        let (ta, tb) = (
            tempfile::NamedTempFile::new().expect("temp file"),
            tempfile::NamedTempFile::new().expect("temp file"),
        );
        counted(&a_whole, ta.path(), 2);
        counted(&b_early, tb.path(), 3);
        let paths = [ta.path().to_path_buf(), tb.path().to_path_buf()];
        let (a_path, b_path) = (paths[0].clone(), paths[1].clone());
        let (raise, done_a, done_b) = (a_whole.clone(), a_whole, b_whole);
        let field = crate::FieldName::new(b"F").expect("a field name");
        let show = move |r: &Record| {
            let f = String::from_utf8_lossy(r.field(&field).unwrap_or_default()).into_owned();
            format!("{f}/{}", message(r))
        };
        let a_seam = a_path.clone();
        let (page, followed, _) = seamed(
            &Scenario::tail(&paths, query(b"A", b"1")),
            move |p| {
                if p == a_seam {
                    counted(&raise, p, 3);
                }
            },
            || {
                counted(&done_a, &a_path, 3);
                counted(&done_b, &b_path, 3);
            },
            show,
        );
        // One boot, two seqnum ids: interleaved by monotonic time.
        assert_eq!(page, ["a/m0", "b/m0", "a/m1", "b/m1", "a/m2"]);
        assert_eq!(followed, ["b/m2"]);
    }

    /// The wake protocol (journal spec, "Waiting for new entries"): the
    /// duplicate is close-on-exec and non-blocking, unreadable while
    /// nothing happened, readable after a write, and a drain clears it.
    #[test]
    fn a_write_makes_the_wake_fd_readable_and_a_drain_clears_it() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("26.04")).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        let wake = f.wake_fd().expect("a wake descriptor");
        let fd_flags = rustix::io::fcntl_getfd(&wake).expect("getfd");
        assert!(
            fd_flags.contains(rustix::io::FdFlags::CLOEXEC),
            "{fd_flags:?}"
        );
        let status = rustix::fs::fcntl_getfl(&wake).expect("getfl");
        assert!(status.contains(rustix::fs::OFlags::NONBLOCK), "{status:?}");
        assert!(!readable(&wake, 0), "nothing happened yet");
        set_n(&a, 3);
        assert!(readable(&wake, 5000), "a write wakes it");
        assert_eq!(seqnums(&f.drain(&crate::Limits::default()).records), [3]);
        assert!(!readable(&wake, 0), "the drain read every event");
    }

    /// A drain with no event doesn't wait for one: it returns an empty
    /// batch at once. Run on a thread under a deadline, so a drain that
    /// blocks fails the test rather than hang it.
    #[test]
    fn a_drain_without_an_event_returns_at_once() {
        let dir = tempfile::tempdir().expect("dir");
        journal(&dir.path().join("a.journal"), 1, &FOUR, 4);
        let journal_ =
            Journal::open(Source::Directory(dir.path().into()), &release("22.04")).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let batch = f.drain(&crate::Limits::default());
            // The receiver is gone only once the test failed already.
            let _ = tx.send(batch);
        });
        let batch = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the drain returned");
        assert!(batch.records.is_empty() && !batch.more, "{batch:?}");
    }

    /// The parent-directory watch, through the injected roots: with /var's
    /// machine-id directory missing, its root is watched, so the directory
    /// appearing wakes the follow, the drain watches it and clears the
    /// descriptor, and a write in it wakes the follow again.
    #[test]
    fn the_root_of_a_missing_directory_wakes_the_follow() {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        journal(&run.join("system.journal"), 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let (mut f, _) = follow_now(&journal_, crate::Query::every());
        let wake = f.wake_fd().expect("a wake descriptor");
        assert!(!readable(&wake, 0));
        std::fs::create_dir(&var).expect("var's directory");
        assert!(readable(&wake, 5000), "the root's watch wakes it");
        assert!(f.drain(&crate::Limits::default()).records.is_empty());
        assert!(!readable(&wake, 0), "the drain read every event");
        journal(&var.join("system.journal"), 2, &[(1, 30)], 1);
        assert!(readable(&wake, 5000), "the new directory's watch wakes it");
        assert_eq!(seqnums(&f.drain(&crate::Limits::default()).records), [1]);
    }

    /// A follow with no directory to watch would never wake: refused, for
    /// the local journal with both roots missing and for files whose
    /// directories don't exist.
    #[test]
    fn a_follow_with_nothing_to_watch_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut host, _run, _var) = local(dir.path());
        host.roots = [dir.path().join("no-run"), dir.path().join("no-var")];
        let local_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let files = Source::Files(vec![dir.path().join("gone/a.journal")]);
        let files_ = Journal::open(files, &release("24.04")).expect("opens");
        for j in [local_, files_] {
            let limits = crate::Limits::default();
            let refused = j.follow(crate::Query::every(), &PageEnd::default(), &limits);
            assert!(
                matches!(refused, Err(OpenError::NothingToWatch)),
                "{refused:?}"
            );
        }
    }

    // main.rs's `classify` tests.

    fn dirs() -> HashMap<i32, PathBuf> {
        HashMap::from([(1, PathBuf::from("/j"))])
    }

    #[test]
    fn a_modified_journal_file_is_named() {
        let name = c"system.journal";
        assert_eq!(
            classify(&dirs(), 1, ReadFlags::MODIFY, Some(name)),
            Wake::Modified(PathBuf::from("/j/system.journal"))
        );
        assert_eq!(
            classify(&dirs(), 1, ReadFlags::CLOSE_WRITE, Some(c"x.journal~")),
            Wake::Modified(PathBuf::from("/j/x.journal~"))
        );
    }

    #[test]
    fn structural_events_rescan() {
        let j = Some(c"system-old.journal");
        for flags in [
            ReadFlags::CREATE,
            ReadFlags::MOVED_TO,
            ReadFlags::MOVED_FROM,
            ReadFlags::DELETE,
            ReadFlags::MODIFY | ReadFlags::DELETE,
        ] {
            assert_eq!(classify(&dirs(), 1, flags, j), Wake::Rescan, "{flags:?}");
        }
        // A machine-id directory appearing in a root directory.
        let dir = Some(c"0123456789abcdef0123456789abcdef");
        assert_eq!(
            classify(&dirs(), 1, ReadFlags::CREATE | ReadFlags::ISDIR, dir),
            Wake::Rescan
        );
        // The watch dropped (its directory removed), and an unknown watch.
        assert_eq!(classify(&dirs(), 1, ReadFlags::IGNORED, None), Wake::Rescan);
        assert_eq!(classify(&dirs(), 7, ReadFlags::MODIFY, j), Wake::Rescan);
    }

    #[test]
    fn an_overflow_rescans() {
        assert_eq!(
            classify(&dirs(), -1, ReadFlags::QUEUE_OVERFLOW, None),
            Wake::Rescan
        );
    }

    #[test]
    fn other_files_are_ignored() {
        for flags in [ReadFlags::CREATE, ReadFlags::DELETE, ReadFlags::MODIFY] {
            assert_eq!(classify(&dirs(), 1, flags, Some(c"junk.1")), Wake::Ignore);
        }
    }
}
