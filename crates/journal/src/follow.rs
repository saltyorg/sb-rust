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
    Batch, BootRule, Cursor, Journal, Limits, OpenError, Query, ReadError, ReadErrorKind,
    ReadErrors, Record, Roots,
};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use std::collections::HashMap;
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
}

/// Which of a followed file's entries may be ones already shown, so they
/// are checked against the last entry shown (`seen`): copies a flush
/// wrote, or entries counted between the open and the start. Past them
/// the position alone decides, so an entry a slower file delivers late is
/// still shown.
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
    /// The most payload bytes a poll's cache held after reading a file.
    #[cfg(test)]
    held_peak: usize,
}

impl Follow {
    /// Continue from `files`, read up to the entry counts their headers
    /// gave when they were opened, and `last`, the newest entry shown. The
    /// headers are read again now, and entries up to those counts are
    /// checked against `last`. `pending` are the errors opening them gave.
    fn start(
        files: Vec<JournalFile>,
        last: Option<Last>,
        opener: Opener,
        rule: BootRule,
        pending: Vec<ReadError>,
    ) -> Self {
        let mut follow = Self {
            files: Vec::new(),
            last,
            pending,
            unopened: Vec::new(),
            opener,
            rule,
            #[cfg(test)]
            held_peak: 0,
        };
        for mut file in files {
            let pos = file.header.n_entries;
            let check_until = match file.refresh() {
                Ok(()) => file.header.n_entries,
                Err(_) => pos,
            };
            let mut f = Followed::new(file, pos);
            f.copies = Copies::Until(check_until);
            follow.files.push(f);
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
        for p in paths {
            // The name's own inode, not a symlink's target: a symlinked
            // journal file is opened, and refused, as a page opens it
            // (Difference 4).
            let Ok(meta) = std::fs::symlink_metadata(p) else {
                continue;
            };
            let inode = (meta.dev(), meta.ino());
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
                Ok(file) => {
                    let same = old
                        .iter()
                        .position(|f| f.file.header.file_id == file.header.file_id);
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
    /// a flush, are dropped (`check_until`, `seen`).
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
    /// lost. An entry larger than the whole limit is skipped, reported,
    /// and what it read leaves the cache with it. The errors one poll
    /// reports are capped as a page's are.
    fn poll(&mut self, groups: &[Group], limit: usize) -> (Vec<Record>, ReadErrors) {
        let mut errors = ReadErrors::default();
        for e in self.pending.drain(..) {
            errors.add_open(e);
        }
        // Every header first, so the boot table is the files' as of now.
        let fresh: Vec<bool> = self
            .files
            .iter_mut()
            .map(|f| f.refresh(&mut errors))
            .collect();
        let boots = self.boots();
        let mut streams: Vec<Vec<Candidate>> = Vec::new();
        let mut decoded: HashMap<(usize, u64), Record> = HashMap::new();
        let mut payloads = Payloads::within(limit);
        let mut reached = false;
        for (i, f) in self.files.iter_mut().enumerate() {
            // Each file its own limit: one file's burst can't defer
            // another's entries past a `last` they are older than (where
            // `check_until` would take them for copies), and an entry
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
                    decoded.insert((c.file, c.offset), r);
                    candidates.push(c);
                }
            }
            reached |= budget.reached;
            streams.push(candidates);
        }
        let dirs: Vec<Option<&Path>> = self.files.iter().map(|f| f.file.path.parent()).collect();
        let picked = merge(&streams, |a, b| dirs[a] == dirs[b], &boots);
        let out: Vec<Record> = picked
            .iter()
            .filter_map(|c| decoded.remove(&(c.file, c.offset)))
            .collect();
        if let Some(c) = picked.last() {
            self.last = Some(Last {
                location: c.location,
                dir: dirs[c.file].map(Path::to_path_buf),
            });
        }
        self.files.retain(Followed::keep);
        if reached {
            errors.add(ReadError::of_page(ReadErrorKind::OutputLimitDeferred {
                limit,
            }));
        }
        (out, errors)
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
        // Whether `last` came from this file's directory: if so, an entry
        // here is new by sd-journal's order; if not, by `is_old` (`seen`).
        let same_dir = read.last.is_some_and(|l| {
            l.dir
                .as_deref()
                .is_some_and(|d| self.file.path.parent() == Some(d))
        });
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
        let shown = |l: &Last| seen(&location, &l.location, same_dir, read.boots);
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
/// /var). When `last`'s directory is unknown, `is_old`.
fn seen(l: &Cursor, last: &Cursor, same_dir: bool, boots: &BootOrder) -> bool {
    if same_dir {
        !l.order(last, boots).is_gt()
    } else {
        is_old(l, last, boots)
    }
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
) -> Vec<Candidate> {
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
        let Some(b) = best else { return out };
        let next = streams[b][pos[b]].clone();
        pos[b] += 1;
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
/// directories. `drain` gives what was written since the last call.
///
/// The watching follows rustix's inotify module
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/inotify/index.html>): one
/// descriptor, `add_watch` per directory, and a `Reader` over a buffer.
/// Unlike that module's example and the journal spec (assumption 3: a
/// non-blocking descriptor, drained to `EAGAIN`, waited on through a
/// duplicate in tokio's `AsyncFd`), the descriptor blocks for now, as the
/// prototype's did: `drain` waits in `Reader::next` for the next event
/// (rustix documents that path for a descriptor without `NONBLOCK`), so a
/// caller can't stop a follow between two events. The non-blocking
/// descriptor and `wake_fd` are the plan's next task.
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
    /// files are open waits for a later event, then the files, at their
    /// headers' counts, and the newest matching entry for `last`. That
    /// read's errors are dropped: what it meets lies before the point the
    /// follow starts from, which journalctl `-n 0 -f` doesn't report either,
    /// and a page read before reported it. The errors opening the files are
    /// kept for the first drain.
    pub(crate) fn start(journal: &Journal, q: Query) -> Result<Self, OpenError> {
        let inotify = inotify::init(CreateFlags::CLOEXEC)
            .map_err(|e| OpenError::Inotify { source: e.into() })?;
        let mut dirs = HashMap::new();
        if let Some((path, source)) = watch(&inotify, &journal.roots, &mut dirs)
            .into_iter()
            .next()
        {
            return Err(OpenError::Watch { path, source });
        }
        let mut errors = ReadErrors::default();
        let files = walk::open_listed(|| journal.list(), &journal.opener, &mut errors);
        let mut dropped = ReadErrors::default();
        let lasts = walk::lasts(&files, &mut dropped);
        let boots = crate::boot_order(journal.release.boot_rule(), &files, &lasts);
        let last = walk::newest_match(&files, q.groups(), &boots, &mut dropped).map(|c| Last {
            location: c.location,
            dir: files[c.file].path.parent().map(Path::to_path_buf),
        });
        let follow = Follow::start(
            files,
            last,
            journal.opener.clone(),
            journal.release.boot_rule(),
            errors.shown,
        );
        Ok(Self {
            follow,
            groups: q.into_groups(),
            roots: journal.roots.clone(),
            inotify,
            dirs,
            ignored: Vec::new(),
        })
    }

    /// The matching entries written since the last drain (or the start).
    ///
    /// They come oldest first, with what couldn't be read. It blocks until the next event on the journal's directories (a
    /// write, a rotation, a vacuum, a flush), then takes every event
    /// already queued, so a burst of writes costs one read per file, not
    /// one per event (`Reader::is_buffer_empty`, "useful to avoid further
    /// blocking reads"). Every drain reads every open file; the events only
    /// decide whether to list the directories again first.
    ///
    /// Each file reads at most `limits`' output limit per drain: one that
    /// stops at an entry that doesn't fit goes on from there at the next
    /// drain (`ReadErrorKind::OutputLimitDeferred`), and an entry larger
    /// than the whole limit is skipped and reported (Difference 5).
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
                Err(rustix::io::Errno::INTR) => continue,
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
            if reader.is_buffer_empty() {
                break;
            }
        }
        self.wake(&changed, rescan);
        let (records, errors) = self.follow.poll(&self.groups, limits.output());
        Batch { records, errors }
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
    /// `limit` per file, with every poll's errors kept as text: the
    /// prototype's `Follow`, which its tests drive.
    struct T {
        f: Follow,
        limit: usize,
        errors: Vec<String>,
    }

    impl T {
        fn of(paths: &[PathBuf]) -> Self {
            Self::start(paths, BootRule::Realtime)
        }

        fn start(paths: &[PathBuf], rule: BootRule) -> Self {
            let opener = Opener::default();
            let files = paths
                .iter()
                .map(|p| JournalFile::open_with(p, &opener).expect("opens"))
                .collect();
            Self {
                f: Follow::start(files, None, opener, rule, Vec::new()),
                limit: crate::OUTPUT_DEFAULT,
                errors: Vec::new(),
            }
        }

        /// What the follow's opener counted.
        fn counts(&self) -> crate::open::Counts {
            self.f.opener.counts()
        }

        fn poll(&mut self, groups: &[Group]) -> Vec<Record> {
            let (records, errors) = self.f.poll(groups, self.limit);
            self.errors.extend(errors.shown.iter().map(crate::key));
            if errors.omitted > 0 {
                self.errors.push(format!("{} more errors", errors.omitted));
            }
            records
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
        67 + k * (74 + 2 * std::mem::size_of::<Field>())
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
        assert!(
            f.errors.iter().any(|e| e.contains("output limit")),
            "{:?}",
            f.errors
        );
        assert_eq!(messages(&f.poll(&[])), ["m5", "m6", "m7"]);
        assert_eq!(messages(&f.poll(&[])), ["m8", "m9"]);
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
        );
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
            let out = merge(&streams, |a, b| one_dir || a == b, &none);
            let got: Vec<u64> = out.iter().map(|r| r.location.realtime).collect();
            assert_eq!(got, vec![300, 100], "one directory: {one_dir}");
        }
    }

    /// A journal file (regular, not compact) holding `entries` as
    /// (seqnum, monotonic) ENTRY objects, one boot, all in one entry array,
    /// the header counting `n` of them. Each entry lists one DATA object,
    /// `M=1`, the item an entry needs (sd-journal's `check_object`).
    fn journal(path: &Path, seqnum_id: u8, entries: &[(u64, u64)], n: u64) {
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(crate::file::SIGNATURE);
        // A file id of its own, from its name, as journald gives each file
        // one: a follow tells a copy of a file by it.
        let id = crate::hash::jenkins64(path.as_os_str().as_encoded_bytes()).to_le_bytes();
        b[24..32].copy_from_slice(&id);
        b[32..40].copy_from_slice(&id);
        b[72..88].copy_from_slice(&[seqnum_id; 16]);
        b[88..96].copy_from_slice(&272u64.to_le_bytes());
        b[152..160].copy_from_slice(&n.to_le_bytes());
        let array_size = 24 + 8 * entries.len() as u64;
        let data_at = 272 + array_size;
        let mut data = vec![0u8; 64];
        data[0] = 1;
        data[8..16].copy_from_slice(&67u64.to_le_bytes());
        data.extend_from_slice(b"M=1");
        data.resize(72, 0);
        let mut at = data_at + 72;
        let mut array = vec![0u8; 24];
        array[0] = 6;
        array[8..16].copy_from_slice(&array_size.to_le_bytes());
        let mut objects = Vec::new();
        for &(seqnum, mono) in entries {
            array.extend_from_slice(&at.to_le_bytes());
            let mut e = vec![0u8; 80];
            e[0] = 3;
            e[8..16].copy_from_slice(&80u64.to_le_bytes());
            e[16..24].copy_from_slice(&seqnum.to_le_bytes());
            e[24..32].copy_from_slice(&(1000 + mono).to_le_bytes());
            e[32..40].copy_from_slice(&mono.to_le_bytes());
            e[40..56].copy_from_slice(&[9; 16]);
            e[64..72].copy_from_slice(&data_at.to_le_bytes());
            objects.extend_from_slice(&e);
            at += 80;
        }
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
        272 + 24 + 8 * len + 72 + 80 * k
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
        let rules = [
            (BootRule::NewestTail, [(1, 2), (2, 2), (2, 3)]),
            (BootRule::NewestTailEntryMachine, [(1, 2), (2, 2), (2, 3)]),
            (BootRule::Realtime, [(2, 2), (1, 2), (2, 3)]),
        ];
        for (rule, want) in rules {
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
            let got: Vec<(u8, u64)> = f
                .poll(&[])
                .iter()
                .map(|r| (r.cursor().seqnum_id[0], r.cursor().seqnum))
                .collect();
            assert_eq!(got, want, "{rule:?}");
            assert!(f.errors.is_empty(), "{:?}", f.errors);
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

    /// `f.drain`, failing the test after five seconds rather than hanging
    /// it: the drain blocks until an event, so a watch that is missing
    /// would otherwise stop the whole run. The follower comes back.
    fn drain_within(f: Follower) -> (Follower, crate::Batch) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut f = f;
            let batch = f.drain(&crate::Limits::default());
            // The receiver is gone only once the test failed already.
            let _ = tx.send((f, batch));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the drain woke within five seconds")
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
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        let run_file = run.join("system.journal");
        journal(&run_file, 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("24.04"), &host).expect("opens");
        let f = journal_.follow(crate::Query::every()).expect("follows");
        journal(&run_file, 1, &FOUR[..3], 3);
        let (f, batch) = drain_within(f);
        assert_eq!(seqnums(&batch.records), [3]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        // The flush: /var's directory and file appear.
        std::fs::create_dir(&var).expect("var's directory");
        let flushed = [(4, 10), (5, 20), (6, 30), (7, 40)];
        journal(&var.join("system.journal"), 1, &flushed, 4);
        let (f, batch) = drain_within(f);
        assert_eq!(seqnums(&batch.records), [7]);
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let more = [(4, 10), (5, 20), (6, 30), (7, 40), (8, 50)];
        journal(&var.join("system.journal"), 1, &more, 5);
        assert_eq!(seqnums(&drain_within(f).1.records), [8]);
    }

    /// A file the follow can't open at its start is an error of its first
    /// drain, not dropped: here a symlinked journal file (Difference 4).
    /// The other file is followed.
    #[test]
    fn the_follows_open_errors_come_with_its_first_drain() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        let link = dir.path().join("link.journal");
        journal(&a, 1, &FOUR[..1], 1);
        std::os::unix::fs::symlink(&a, &link).expect("symlink");
        let source = Source::Files(vec![a.clone(), link.clone()]);
        let journal_ = Journal::open(source, &release("22.04")).expect("opens");
        let f = journal_.follow(crate::Query::every()).expect("follows");
        journal(&a, 1, &FOUR[..2], 2);
        let (_, batch) = drain_within(f);
        assert_eq!(seqnums(&batch.records), [2]);
        assert_eq!(batch.errors.shown.len(), 1, "{:?}", batch.errors);
        assert!(batch.errors.shown[0].is_symlink(), "{:?}", batch.errors);
        assert_eq!(batch.errors.shown[0].path(), Some(link.as_path()));
    }

    /// The follow goes on from the newest matching entry when it started
    /// (`Last`), so a flush right after the start shows only what is new:
    /// the copies of the entries there before it, in /var's new file, are
    /// dropped (Difference 2).
    #[test]
    fn the_follow_starts_after_the_newest_match() {
        let dir = tempfile::tempdir().expect("dir");
        let (host, run, var) = local(dir.path());
        journal(&run.join("system.journal"), 1, &FOUR[..2], 2);
        let journal_ = Journal::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let f = journal_.follow(crate::Query::every()).expect("follows");
        std::fs::create_dir(&var).expect("var's directory");
        journal(
            &var.join("system.journal"),
            2,
            &[(1, 10), (2, 20), (3, 30)],
            3,
        );
        let (_, batch) = drain_within(f);
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
    /// creation, a move in, a move out and a deletion. A drain with no
    /// event of its kind would block, and fail the deadline.
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
        let mut f = journal_.follow(crate::Query::every()).expect("follows");
        let mut held = std::fs::OpenOptions::new()
            .append(true)
            .open(&a)
            .expect("open");
        held.write_all(&[0]).expect("modify only");
        (f, _) = drain_within(f);
        drop(held);
        (f, _) = drain_within(f);
        std::fs::create_dir(d.join("sub")).expect("create only");
        (f, _) = drain_within(f);
        std::fs::write(out.join("x"), b"x").expect("outside");
        std::fs::rename(out.join("x"), d.join("x")).expect("move in only");
        (f, _) = drain_within(f);
        std::fs::rename(d.join("x"), out.join("y")).expect("move out only");
        (f, _) = drain_within(f);
        std::fs::remove_dir(d.join("sub")).expect("delete only");
        let (_, batch) = drain_within(f);
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
        let f = journal_.follow(crate::Query::every()).expect("follows");
        let x = d.join("x.journal");
        std::fs::write(&x, b"not yet a journal").expect("created");
        let (f, batch) = drain_within(f);
        assert!(batch.records.is_empty(), "{:?}", batch.records);
        assert!(f.follow.is_unopened(&x));
        // Written in place: write events only, no creation.
        journal(&x, 1, &FOUR, 4);
        let (f, batch) = drain_within(f);
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
        let f = journal_
            .follow(crate::Query::field(&name, b"m3"))
            .expect("follows");
        c.set(H_N_ENTRIES, 4);
        grow(&c, &t);
        let (_, batch) = drain_within(f);
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
        let mut f = journal_.follow(crate::Query::every()).expect("follows");
        // The start's own open error.
        assert_eq!(f.follow.pending.len(), 1);
        f.wake(std::slice::from_ref(&other), false);
        assert_eq!(f.ignored, std::slice::from_ref(&other));
        assert!(f.follow.is_unopened(&link) && !f.follow.is_unopened(&a));
        assert_eq!(f.follow.pending.len(), 1, "a first failure isn't reported");
        f.wake(std::slice::from_ref(&other), false);
        assert_eq!(f.follow.pending.len(), 1, "an ignored file lists nothing");
        f.wake(&[], true);
        assert!(f.ignored.is_empty(), "{:?}", f.ignored);
        assert_eq!(f.follow.pending.len(), 2, "the second failure is");
        f.wake(std::slice::from_ref(&link), false);
        assert_eq!(
            f.follow.pending.len(),
            3,
            "an unopened file's event rescans"
        );
        assert!(f.ignored.is_empty(), "{:?}", f.ignored);
        f.wake(std::slice::from_ref(&a), false);
        assert_eq!(
            f.follow.pending.len(),
            3,
            "an open file's write lists nothing"
        );
        assert!(f.follow.has(&a) && !f.follow.has(&link));
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
