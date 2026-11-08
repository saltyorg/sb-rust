//! Matching, merging and following across a set of journal files.

use crate::file::{ArrayCursor, DataRef, EntriesRev, Entry, JournalFile, OverLimit, hex_id};
use anyhow::{Context, Result};
use itertools::Itertools;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, hash_map};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One term of a conjunction: the entry carries any of these
/// `FIELD=value` payloads (sd-journal ORs matches on the same field).
pub type Term = Vec<Vec<u8>>;
/// A conjunction of terms; a filter is a disjunction of groups.
pub type Group = Vec<Term>;

/// One supported Ubuntu release: what its journalctl does that the
/// reader has to match. sb refuses any other release (cli-core startup
/// check), so [`RELEASES`] lists them all. Get one through
/// [`Release::parse`]; the fields are public for the table and the
/// matrix, so a row built elsewhere is not a supported release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    /// `VERSION_ID` in /etc/os-release, and the `ubuntu:` image tag the
    /// matrix runs.
    pub version_id: &'static str,
    /// The systemd major version the release ships: documentation, and
    /// the matrix's check that the image still ships it. A rule can change
    /// within a major (259.1); the matrix's fixtures catch that, not this.
    pub systemd: u32,
    /// journalctl's `-u` rule.
    pub unit_rule: UnitRule,
    /// `-o json` prints `__SEQNUM` and `__SEQNUM_ID` (v255 and later).
    pub prints_seqnum: bool,
}

/// The supported releases. A new LTS is one row here, checked by the
/// matrix (compare.sh and compat.sh run every row by default); it needs
/// code only if its systemd brings a new `-u` rule, `-o json` shape or
/// incompatible file flag (file.rs `INCOMPAT_KNOWN`).
pub const RELEASES: &[Release] = &[
    Release {
        version_id: "22.04",
        systemd: 249,
        unit_rule: UnitRule::Pid1,
        prints_seqnum: false,
    },
    Release {
        version_id: "24.04",
        systemd: 255,
        unit_rule: UnitRule::Pid1,
        prints_seqnum: true,
    },
    Release {
        version_id: "26.04",
        systemd: 259,
        unit_rule: UnitRule::InitScope,
        prints_seqnum: true,
    },
];

/// journalctl's `-u <unit>` rule, by the systemd versions that have it
/// (src/shared/logs-show.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitRule {
    /// 249 to 259.0, `add_matches_for_unit`: messages from `_PID=1` about
    /// the unit; coredumps only from root.
    Pid1,
    /// 259.1 to main (2026-11), `add_matches_for_unit_full`: messages from
    /// `_SYSTEMD_CGROUP=/init.scope` about the unit, to include PID 1's
    /// helpers; coredumps from root or the `systemd-coredump` user, the
    /// user term dropped for `--file`/`-D` (src/journal/journalctl-filter.c).
    InitScope,
}

impl Release {
    pub fn parse(version_id: &str) -> Option<Self> {
        RELEASES
            .iter()
            .find(|r| r.version_id == version_id)
            .copied()
    }
}

const COREDUMP_MESSAGE_ID: &str = "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1";

fn pair(field: &str, value: &str) -> Vec<u8> {
    format!("{field}={value}").into_bytes()
}

/// journalctl's `-u <unit>` match set, by the release's [`UnitRule`].
/// `external` is `--file`/`-D`, which drops the coredump uid term.
pub fn unit_filter(release: Release, unit: &str, external: bool) -> Vec<Group> {
    let own = vec![vec![pair("_SYSTEMD_UNIT", unit)]];
    let pid1 = vec![vec![b"_PID=1".to_vec()], vec![pair("UNIT", unit)]];
    // UnitRule::InitScope (the v259 tag itself still has _PID=1).
    let init = vec![
        vec![b"_SYSTEMD_CGROUP=/init.scope".to_vec()],
        vec![pair("UNIT", unit)],
    ];
    let daemons = vec![
        vec![b"_UID=0".to_vec()],
        vec![pair("OBJECT_SYSTEMD_UNIT", unit)],
    ];
    match release.unit_rule {
        UnitRule::Pid1 => vec![
            own,
            vec![
                vec![COREDUMP_MESSAGE_ID.as_bytes().to_vec()],
                vec![b"_UID=0".to_vec()],
                vec![pair("COREDUMP_UNIT", unit)],
            ],
            pid1,
            daemons,
        ],
        UnitRule::InitScope => {
            let mut coredump = vec![
                vec![COREDUMP_MESSAGE_ID.as_bytes().to_vec()],
                vec![pair("COREDUMP_UNIT", unit)],
            ];
            if !external && let Some(uid) = coredump_uid() {
                coredump.push(vec![format!("_UID={uid}").into_bytes(), b"_UID=0".to_vec()]);
            }
            vec![own, init, daemons, coredump]
        }
    }
}

/// The `systemd-coredump` user's uid; none (or root) drops the term, as
/// journalctl does.
fn coredump_uid() -> Option<u32> {
    let user = nix::unistd::User::from_name("systemd-coredump").ok()??;
    let uid = user.uid.as_raw();
    (uid != 0).then_some(uid)
}

/// A position in the merged stream: what a cursor names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub seqnum_id: [u8; 16],
    pub seqnum: u64,
    pub boot_id: [u8; 16],
    pub monotonic: u64,
    pub realtime: u64,
    pub xor_hash: u64,
}

impl Location {
    pub fn of(file: &JournalFile, e: &Entry) -> Self {
        Self {
            seqnum_id: file.header.seqnum_id,
            seqnum: e.seqnum,
            boot_id: e.boot_id,
            monotonic: e.monotonic,
            realtime: e.realtime,
            xor_hash: e.xor_hash,
        }
    }

    /// systemd's cursor text (sd_journal_get_cursor).
    pub fn cursor(&self) -> String {
        format!(
            "s={};i={:x};b={};m={:x};t={:x};x={:x}",
            hex_id(&self.seqnum_id),
            self.seqnum,
            hex_id(&self.boot_id),
            self.monotonic,
            self.realtime,
            self.xor_hash
        )
    }

    pub fn parse_cursor(text: &str) -> Option<Self> {
        let mut l = Self {
            seqnum_id: [0; 16],
            seqnum: 0,
            boot_id: [0; 16],
            monotonic: 0,
            realtime: 0,
            xor_hash: 0,
        };
        // One bit per key: a repeated key can't stand in for a missing one.
        let mut seen = 0u8;
        for part in text.split(';') {
            let (k, v) = part.split_once('=')?;
            let bit = match k {
                "s" => {
                    l.seqnum_id = parse_id(v)?;
                    1
                }
                "i" => {
                    l.seqnum = parse_hex(v)?;
                    2
                }
                "b" => {
                    l.boot_id = parse_id(v)?;
                    4
                }
                "m" => {
                    l.monotonic = parse_hex(v)?;
                    8
                }
                "t" => {
                    l.realtime = parse_hex(v)?;
                    16
                }
                "x" => {
                    l.xor_hash = parse_hex(v)?;
                    32
                }
                _ => continue,
            };
            seen |= bit;
        }
        (seen == 63).then_some(l)
    }

    /// sd-journal's order between two entries
    /// (journal_file_compare_locations): the same sequence-number space
    /// orders by seqnum; the same boot by monotonic time; otherwise
    /// realtime; the XOR hash breaks ties.
    pub fn order(&self, other: &Self) -> Ordering {
        if self.seqnum_id == other.seqnum_id {
            let o = self.seqnum.cmp(&other.seqnum);
            if o != Ordering::Equal {
                return o;
            }
        }
        if self.boot_id == other.boot_id {
            let o = self.monotonic.cmp(&other.monotonic);
            if o != Ordering::Equal {
                return o;
            }
        }
        self.realtime
            .cmp(&other.realtime)
            .then(self.xor_hash.cmp(&other.xor_hash))
    }
}

/// A 128-bit id: exactly 32 hex digits
/// (https://docs.rs/hex/0.4.3/hex/fn.decode_to_slice.html).
fn parse_id(v: &str) -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    hex::decode_to_slice(v, &mut out).ok()?;
    Some(out)
}

/// A cursor number: hex digits only. Deliberately stricter than systemd
/// 249's `sscanf("%llx")`, which also takes `+3`, `0x3` and trailing junk:
/// sb only parses cursors it printed itself.
fn parse_hex(v: &str) -> Option<u64> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(v, 16).ok()
}

/// An entry read for output: its location and its fields in item order.
#[derive(Debug, Clone)]
pub struct Record {
    pub location: Location,
    pub fields: Vec<Field>,
    /// The index of the file it was read from, in the files of the
    /// `Journal` (or `Follow` poll) that read it: only that `Journal`'s
    /// `last_of` may use it, as a follow's files change between polls.
    file: usize,
}

/// The newest entry shown, where a follow goes on from: its location, and
/// the directory of the file it came from (`None` when unknown). The
/// directory tells a flush's copies, which always cross journal roots
/// (/run to /var), from entries journald wrote again in the same
/// directory (see `seen`).
#[derive(Debug, Clone)]
pub struct Last {
    pub location: Location,
    pub dir: Option<PathBuf>,
}

/// One `FIELD=value` DATA payload. The entries shown that carry the same
/// DATA object share its bytes: most fields of a unit's entries repeat.
#[derive(Debug, Clone)]
pub struct Field {
    payload: Arc<[u8]>,
    /// Where the first `=` is.
    eq: usize,
}

impl Field {
    /// A payload split at its first `=`; `None` for a payload without one.
    fn new(payload: Vec<u8>) -> Option<Self> {
        let eq = payload.iter().position(|&b| b == b'=')?;
        Some(Self {
            payload: payload.into(),
            eq,
        })
    }

    pub fn name(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.payload[..self.eq])
    }

    pub fn value(&self) -> &[u8] {
        &self.payload[self.eq + 1..]
    }
}

/// The files journalctl reads by default: the local machine's directories
/// under /run/log/journal and /var/log/journal (SD_JOURNAL_LOCAL_ONLY), in
/// sd-journal's search order, which decides which copy of a flushed entry
/// is shown.
pub fn local_files() -> Result<Vec<PathBuf>> {
    let mid = std::fs::read_to_string("/etc/machine-id").context("read /etc/machine-id")?;
    let mid = mid.trim();
    let mut out = Vec::new();
    for root in ["/run/log/journal", "/var/log/journal"] {
        out.extend(journal_files_in(&Path::new(root).join(mid))?);
    }
    Ok(out)
}

/// `*.journal` and `*.journal~` files in a directory and its machine-id
/// subdirectories, as `journalctl -D` reads them.
pub fn directory_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = journal_files_in(dir)?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.file_name().is_some_and(|n| n.len() == 32) {
                out.extend(journal_files_in(&p)?);
            }
        }
    }
    Ok(out)
}

fn journal_files_in(dir: &Path) -> Result<Vec<PathBuf>> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".journal") || n.ends_with(".journal~"))
        })
        .collect();
    out.sort();
    Ok(out)
}

pub struct Journal {
    pub files: Vec<JournalFile>,
    pub errors: Vec<String>,
    /// The payload bytes one query may read (QUERY_PAYLOAD_MAX).
    payload_limit: usize,
    /// The bytes of candidates one query may hold before decoding (the
    /// same bound).
    held_limit: usize,
}

/// Our own bound on the payload bytes one query reads and decodes, not a
/// journald value: a corrupt file can give each entry a decompression
/// bomb, or an entry listing one object millions of times, and the
/// records are held for output. Failed reads spend it too, so it bounds
/// work as well as memory. 256 MiB is far above an `sb logs` page (500
/// entries are typically well under a MiB). It is below journald's
/// ENTRY_SIZE_MAX (770 MiB), so it also bounds one entry: only an entry
/// carrying a coredump stored in the journal comes near it, and it is
/// left out with the limit reported, where journalctl would print it.
const QUERY_PAYLOAD_MAX: usize = 256 << 20;

/// The errors met by one query: the first ten distinct ones in full, then
/// a count of the further errors, so a badly broken file can't flood the
/// output or grow the list without bound.
#[derive(Default)]
struct QueryErrors {
    shown: Vec<String>,
    more: usize,
}

impl QueryErrors {
    const SHOWN: usize = 10;

    fn add(&mut self, err: &anyhow::Error) {
        self.add_text(format!("{err:#}"));
    }

    fn add_text(&mut self, text: String) {
        if self.shown.contains(&text) {
            return;
        }
        if self.shown.len() < Self::SHOWN {
            self.shown.push(text);
        } else {
            self.more += 1;
        }
    }

    fn drain_into(self, out: &mut Vec<String>) {
        out.extend(self.shown);
        if self.more > 0 {
            out.push(format!("{} more errors", self.more));
        }
    }
}

impl Journal {
    /// Open each file. A file that can't be read is reported, never
    /// silently dropped (an unsupported format names the file). The
    /// errors are one per file at most, so they are bounded by the file
    /// count, not capped as a query's are.
    pub fn open(paths: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for p in paths {
            match JournalFile::open(p) {
                Ok(f) => files.push(f),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        Self {
            files,
            errors,
            payload_limit: QUERY_PAYLOAD_MAX,
            held_limit: QUERY_PAYLOAD_MAX,
        }
    }

    /// Open the files `list` names, then list again. A rotation between
    /// listing and opening renames the active file away and a new file
    /// takes its name, so the archived file's entries would be missing
    /// without an error; when the list changed, open again, up to three
    /// times. (journalctl lists once and has the same race.) A rotation
    /// after the second listing is harmless: an open file is read
    /// through its descriptor under its new name.
    pub fn open_listed(mut list: impl FnMut() -> Result<Vec<PathBuf>>) -> Result<Self> {
        let mut paths = list()?;
        for _ in 0..3 {
            let journal = Self::open(&paths);
            let again = list()?;
            if again == paths {
                return Ok(journal);
            }
            paths = again;
        }
        let mut journal = Self::open(&paths);
        journal.errors.push(
            "the journal's file list kept changing while it was opened; entries may be missing"
                .to_string(),
        );
        Ok(journal)
    }

    /// Where a follow goes on from after `r`, a record this journal read.
    pub fn last_of(&self, r: &Record) -> Last {
        Last {
            location: r.location.clone(),
            dir: self
                .files
                .get(r.file)
                .and_then(|f| f.path.parent())
                .map(Path::to_path_buf),
        }
    }

    /// The last `n` matching entries before `before` (all when `None`),
    /// oldest first. `filter` empty means every entry.
    pub fn tail(&mut self, filter: &[Group], n: usize, before: Option<&Location>) -> Vec<Record> {
        let step = |l: &Location| match before {
            Some(b) if !l.order(b).is_lt() => Step::Skip,
            _ => Step::Take,
        };
        let mut errors = QueryErrors::default();
        let walks = self.walks(filter, step, &mut errors);
        let held = self.held_limit / std::mem::size_of::<Candidate>().max(1);
        let picked = newest(walks, n, held, &mut errors);
        let out = self.decode(&picked, &mut errors);
        errors.drain_into(&mut self.errors);
        out
    }

    /// One lazy walk per file, newest first. A group whose terms can't be
    /// looked up is reported in `errors` and left out, the other groups
    /// still match, as sd-journal leaves out what it can't iterate and
    /// keeps the rest: `.journal~` files are exactly the ones left dirty.
    fn walks<'a, S>(
        &'a self,
        filter: &'a [Group],
        step: S,
        errors: &mut QueryErrors,
    ) -> Vec<impl Iterator<Item = Result<Candidate>> + 'a>
    where
        S: Fn(&Location) -> Step + Copy + 'a,
    {
        let mut out = Vec::new();
        for (file, f) in self.files.iter().enumerate() {
            let walk = file_walk(f, filter, step, errors);
            out.push(walk.map(move |r| {
                r.map(|(offset, location)| Candidate {
                    location,
                    file,
                    offset,
                })
            }));
        }
        out
    }

    /// Read the chosen entries for output, newest first, within one output
    /// limit for the query (QUERY_PAYLOAD_MAX), which every read spends,
    /// failed ones too. Entries share most DATA objects (a unit's _PID,
    /// _COMM, _HOSTNAME and so on): each is read and decompressed once, and
    /// one that failed isn't decoded again for the next entry listing it.
    /// An entry that can't be read is reported and skipped, as sd-journal
    /// skips an entry it can't read (output_json on -EBADMSG). The first
    /// entry that doesn't fit ends the output, so it has no gap: a page
    /// back from the first entry shown goes on from there. The entries left
    /// out are reported. An entry that doesn't fit the whole limit, read
    /// before anything else, is reported and skipped instead, as a follow
    /// skips it: stopping there would show nothing at all.
    fn decode(&self, picked: &[Candidate], errors: &mut QueryErrors) -> Vec<Record> {
        let mut out = Vec::with_capacity(picked.len());
        let mut payloads = Payloads::default();
        let mut left = self.payload_limit;
        for (i, c) in picked.iter().rev().enumerate() {
            let f = &self.files[c.file];
            let whole = left == self.payload_limit;
            let read = f
                .entry(c.offset)
                .and_then(|e| record(f, &e, &mut payloads, c.file, &mut left));
            match read {
                Ok(r) => out.push(r),
                Err(err) if err.is::<OverLimit>() && whole => {
                    errors.add_text(format!(
                        "{err:#}: an entry larger than the output limit of {} bytes, left out",
                        self.payload_limit
                    ));
                    // Nothing was read before it, so all the cache holds is
                    // its own fields: drop them with it.
                    payloads.fields.clear();
                    left = self.payload_limit;
                }
                Err(err) if err.is::<OverLimit>() => {
                    errors.add_text(format!(
                        "output limit of {} bytes reached; {} older entries left out",
                        self.payload_limit,
                        picked.len() - i
                    ));
                    break;
                }
                Err(err) => errors.add(&err),
            }
        }
        out.reverse();
        out
    }
}

/// Following: the files stay open, each with the number of entries of its
/// entry list already read, so a wake reads only what was appended since.
/// sd-journal keeps a position per file the same way
/// (next_beyond_location); what differs is that a file is held by its
/// inode, not its name, so a rotation's rename doesn't reopen it and a
/// vacuumed file is still read to its end.
///
/// Every wake reads every open file that isn't archived and read to its
/// end, not only the one an event named: journald raises each file's
/// IN_MODIFY from its own coalesced timer (journal-file.c
/// schedule_post_change, 250 ms in journald), so two files written at once
/// notify in either order. A header read per file is the whole cost.
pub struct Follow {
    files: Vec<Followed>,
    last: Option<Last>,
    pub errors: Vec<String>,
    /// Listed files that failed to open at the last rescan.
    unopened: Vec<PathBuf>,
    /// The payload bytes each followed file may read per poll
    /// (QUERY_PAYLOAD_MAX).
    payload_limit: usize,
    /// The most payload bytes a poll's cache held after reading a file.
    #[cfg(test)]
    held_peak: usize,
}

/// What a followed file waits one wake for before it reports and skips
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

struct Followed {
    file: JournalFile,
    /// Entries of the file's entry list read so far.
    pos: u64,
    cursor: Option<ArrayCursor>,
    /// Entries numbered below this may be ones already shown: copies a
    /// flush wrote, or entries the tail printed. They are checked against
    /// the last entry shown (`seen`); from here on the position alone
    /// decides, so an entry a slower file delivers late is still shown.
    check_until: u64,
    /// A file opened while following: its leading entries are checked
    /// until the first one that is new (a flush writes all its copies
    /// before any new entry).
    leading: bool,
    /// Per group, per term, per value: its DATA object here, once it
    /// exists (a value seen for the first time gets one later).
    resolved: Vec<Vec<Vec<Option<u64>>>>,
    /// A lookup here failed and that was reported: one report per file.
    lookup_reported: bool,
    /// Still listed in the journal directories at the last rescan.
    listed: bool,
    /// Rescans in a row that didn't list this file.
    unlisted: u8,
    /// Per kind (`Wait as usize`), the position whose read failed on an
    /// earlier wake: it is retried once, as the file may have been caught
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
}

impl Follow {
    /// Continue from `journal`'s files, read up to the entry counts of
    /// their headers at open, and `last`, the newest entry shown
    /// (`Journal::last_of`). The tail may have shown entries counted after
    /// the open: the headers are read again now, and entries up to those
    /// counts are checked against `last`.
    pub fn new(journal: Journal, last: Option<Last>) -> Self {
        let mut follow = Self {
            files: Vec::new(),
            last,
            errors: journal.errors,
            unopened: Vec::new(),
            payload_limit: journal.payload_limit,
            #[cfg(test)]
            held_peak: 0,
        };
        for mut file in journal.files {
            let pos = file.header.n_entries;
            let check_until = match file.refresh() {
                Ok(()) => file.header.n_entries,
                Err(_) => pos,
            };
            let mut f = Followed::new(file, pos);
            f.check_until = check_until;
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
    pub fn rescan(&mut self, paths: &[PathBuf]) {
        use std::os::unix::fs::MetadataExt;
        let mut old = std::mem::take(&mut self.files);
        let unopened = std::mem::take(&mut self.unopened);
        for p in paths {
            let Ok(meta) = std::fs::metadata(p) else {
                continue;
            };
            let inode = (meta.dev(), meta.ino());
            if let Some(i) = old.iter().position(|f| f.file.inode == inode) {
                let mut f = old.swap_remove(i);
                f.file.path.clone_from(p);
                f.listed = true;
                f.unlisted = 0;
                self.files.push(f);
                continue;
            }
            match JournalFile::open(p) {
                // The same file under a new inode: where journald can't
                // clear FS_NOCOW_FL on an archived file (btrfs; overlayfs
                // in follow.sh's containers) it rewrites it as a copy
                // renamed over the name (journal-file-util.c,
                // copy_file_atomic_at_full). The file id is in the copied
                // header, and the offsets are the same, so its position
                // carries over instead of the whole file being read again.
                Ok(file) => match old
                    .iter()
                    .position(|f| f.file.header.file_id == file.header.file_id)
                {
                    Some(i) => {
                        let mut f = old.swap_remove(i);
                        f.file = file;
                        f.listed = true;
                        f.unlisted = 0;
                        self.files.push(f);
                    }
                    None => {
                        let mut f = Followed::new(file, 0);
                        f.check_until = u64::MAX;
                        f.leading = true;
                        self.files.push(f);
                    }
                },
                // A file journald is creating may not have its header yet:
                // it stays unopened, its next modification rescans, and
                // only a second failure is reported.
                Err(e) => {
                    if unopened.contains(p) {
                        self.errors.push(format!("{e:#}"));
                    }
                    self.unopened.push(p.clone());
                }
            }
        }
        for mut f in old {
            f.listed = false;
            f.unlisted = f.unlisted.saturating_add(1);
            self.files.push(f);
        }
    }

    /// Whether `path` is one of the open files.
    pub fn has(&self, path: &Path) -> bool {
        self.files.iter().any(|f| f.file.path == path)
    }

    /// Whether `path` failed to open at the last rescan.
    pub fn unopened(&self, path: &Path) -> bool {
        self.unopened.iter().any(|p| p == path)
    }

    /// The matching entries appended since the last poll, oldest first.
    /// Entries already shown, and copies of them from a flush, are dropped
    /// (`check_until`, `seen`).
    ///
    /// Each file's new matches are decoded as they are read, with each
    /// DATA object read once per wake (`record`'s cache, as the tail
    /// uses), so a DATA object that fails to read is retried with its
    /// entry; copies of shown entries are dropped before decoding. The
    /// files' `Candidate`s are then merged.
    ///
    /// Each file reads within the output limit (QUERY_PAYLOAD_MAX) per
    /// poll, as a tail does: a file stops at the entry that doesn't fit,
    /// without moving past it, so the next poll goes on from there and
    /// nothing is lost. An entry larger than the whole limit is skipped,
    /// reported, and what it read leaves the cache with it. The errors one
    /// poll reports are capped as a query's are.
    pub fn poll(&mut self, filter: &[Group]) -> Vec<Record> {
        let mut streams: Vec<Vec<Candidate>> = Vec::new();
        let mut decoded: HashMap<(usize, u64), Record> = HashMap::new();
        let mut payloads = Payloads::default();
        let mut errors = QueryErrors::default();
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
                limit: self.payload_limit,
                left: self.payload_limit,
                reached: false,
            };
            let last = self.last.as_ref();
            let stream = f.read_new(i, filter, last, &mut payloads, &mut budget, &mut errors);
            #[cfg(test)]
            {
                self.held_peak = self.held_peak.max(payloads.held());
            }
            reached |= budget.reached;
            let mut candidates = Vec::with_capacity(stream.len());
            for (c, r) in stream {
                decoded.insert((c.file, c.offset), r);
                candidates.push(c);
            }
            streams.push(candidates);
        }
        let dirs: Vec<Option<&Path>> = self.files.iter().map(|f| f.file.path.parent()).collect();
        let picked = merge(streams, |a, b| dirs[a] == dirs[b]);
        let out: Vec<Record> = picked
            .iter()
            .filter_map(|c| decoded.remove(&(c.file, c.offset)))
            .collect();
        if let Some(c) = picked.last() {
            self.last = Some(Last {
                location: c.location.clone(),
                dir: dirs[c.file].map(Path::to_path_buf),
            });
        }
        self.files.retain(Followed::keep);
        if reached {
            errors.add_text(format!(
                "output limit of {} bytes reached; the rest follows on the next wake",
                self.payload_limit
            ));
        }
        errors.drain_into(&mut self.errors);
        out
    }
}

/// One poll's output limit: what is left, and whether a file stopped at it.
struct Budget {
    limit: usize,
    left: usize,
    reached: bool,
}

impl Followed {
    /// The filter's values are looked up at the first read with entries
    /// (`read_new`), where a failed lookup can be reported.
    fn new(file: JournalFile, pos: u64) -> Self {
        Self {
            file,
            pos,
            cursor: None,
            check_until: 0,
            leading: false,
            resolved: Vec::new(),
            lookup_reported: false,
            listed: true,
            unlisted: 0,
            failed: [None; 4],
            stuck: false,
            sealed: false,
        }
    }

    /// Whether to keep the file open after a poll. One no longer listed
    /// is kept while it still has a name (renamed between a listing and
    /// the stat of its old name, as a rotation can be; the next rescan
    /// finds it), at most two rescans, and once deleted until read to its
    /// end or reported unreadable.
    fn keep(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        if self.listed {
            return true;
        }
        let linked = self.file.metadata().is_ok_and(|m| m.nlink() > 0);
        if linked && self.unlisted < 2 {
            return true;
        }
        !self.stuck && self.pos < self.file.header.n_entries
    }

    /// Look up the values not found here yet: a few reads of the hash
    /// table each. A failed lookup leaves its value unresolved, looked up
    /// again on the next wake (an entry carrying it doesn't match until
    /// then); the first failure in this file is reported, once.
    fn resolve(&mut self, filter: &[Group], errors: &mut QueryErrors) {
        if self.resolved.is_empty() {
            self.resolved = filter
                .iter()
                .map(|g| g.iter().map(|t| vec![None; t.len()]).collect())
                .collect();
        }
        for (g, group) in filter.iter().enumerate() {
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
                                errors.add(&e.context(
                                    "a lookup failed; entries with that value are left out until it reads",
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    /// An empty filter matches all; otherwise some group whose every term
    /// has a value among the entry's items.
    fn matches(&self, filter: &[Group], e: &Entry) -> bool {
        filter.is_empty()
            || self.resolved.iter().any(|group| {
                group.iter().all(|term| {
                    term.iter()
                        .any(|v| v.is_some_and(|off| e.items.contains(&off)))
                })
            })
    }

    /// Whether `kind` already had its wait at this position: if not, it
    /// is marked and the caller waits for the next wake.
    fn waited(&mut self, kind: Wait) -> bool {
        if self.failed[kind as usize] == Some(self.pos) {
            return true;
        }
        self.failed[kind as usize] = Some(self.pos);
        false
    }

    /// A read of the file failed at `pos`: the first time it is retried on
    /// the next wake; the second time it is reported (unless the file is
    /// deleted and was already read to its end: vacuum deallocates a file
    /// before it unlinks it, so it can read as zeros) and true is returned.
    fn failure(&mut self, kind: Wait, e: &anyhow::Error, errors: &mut QueryErrors) -> bool {
        if !self.waited(kind) {
            return false;
        }
        let drained = !self.listed && self.pos >= self.file.header.n_entries;
        if !self.stuck && !drained {
            errors.add(e);
        }
        true
    }

    /// The matching entries appended since the last read, with their
    /// numbers in the file's entry list. `file` is this file's index, for
    /// the candidates.
    fn read_new(
        &mut self,
        file: usize,
        filter: &[Group],
        last: Option<&Last>,
        payloads: &mut Payloads,
        budget: &mut Budget,
        errors: &mut QueryErrors,
    ) -> Vec<(Candidate, Record)> {
        let mut out = Vec::new();
        // A file sealed (archived, or renamed to `.journal~` by a journald
        // that found it dirty) and read to its end is never written again.
        // Sealed is only set from a header read after the seal, so its
        // count is final.
        if self.sealed && self.pos >= self.file.header.n_entries {
            return out;
        }
        if let Err(e) = self.file.refresh() {
            self.stuck = self.failure(Wait::Header, &e, errors);
            return out;
        }
        self.sealed = self.file.header.archived()
            || self.file.path.extension().is_some_and(|x| x == "journal~");
        // Whether `last` came from this file's directory: if so, an entry
        // here is new by sd-journal's order; if not, by `is_old` (`seen`).
        let same_dir = last.is_some_and(|l| {
            l.dir
                .as_deref()
                .is_some_and(|d| self.file.path.parent() == Some(d))
        });
        let mut resolved = false;
        // Rounds: an item that can't be an entry ends a read
        // (`entries_since`); once it is skipped, the entries after it are
        // read in the same wake.
        loop {
            let offsets = match self.file.entries_since(&mut self.cursor, self.pos) {
                Ok(o) => o,
                Err(e) => {
                    self.stuck = self.failure(Wait::List, &e, errors);
                    return out;
                }
            };
            self.stuck = false;
            if offsets.is_empty() {
                // A failed header read on an earlier wake is behind us.
                self.failed = [None; 4];
                return out;
            }
            if !resolved && !filter.is_empty() {
                self.resolve(filter, errors);
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
                            errors.add_text(format!(
                                "{}: entry {} of the entry list is {off:#x}, not an entry offset; skipped",
                                self.file.path.display(),
                                self.pos
                            ));
                        }
                        self.failed = [None; 4];
                        self.pos += 1;
                        skipped_slot = true;
                        continue;
                    }
                    return out;
                }
                let left_before = budget.left;
                let read = self.file.entry_before(off, end).and_then(|e| {
                    let location = Location::of(&self.file, &e);
                    let shown = |l: &Last| seen(&location, &l.location, same_dir);
                    // The first entry newer than the last shown ends a new
                    // file's leading copies, whether it matches or not.
                    if self.leading && !last.is_some_and(shown) {
                        self.check_until = self.pos;
                        self.leading = false;
                    }
                    // Where an entry already shown can be (`check_until`),
                    // one not newer than the last shown is a copy: dropped
                    // before it is decoded.
                    let copy = self.pos < self.check_until && last.is_some_and(shown);
                    if copy || !self.matches(filter, &e) {
                        return Ok(None);
                    }
                    // Decoded here, so a DATA object that fails to read is
                    // retried on the next wake like the entry itself. Unlike
                    // the tail's decode of only what is shown, the merge
                    // can still drop one of these (a same-wake flush copy);
                    // that is around a flush only.
                    let r = record(&self.file, &e, payloads, file, &mut budget.left)?;
                    let c = Candidate {
                        location,
                        file,
                        offset: off,
                    };
                    Ok(Some((c, r)))
                });
                match read {
                    Ok(c) => out.extend(c),
                    // At the limit: stop here, unmoved, for the next poll.
                    // An entry larger than the whole limit is skipped,
                    // reported.
                    Err(e) if e.is::<OverLimit>() => {
                        if left_before < budget.limit {
                            budget.reached = true;
                            return out;
                        }
                        errors.add_text(format!(
                            "{e:#}: an entry larger than the output limit of {} bytes, left out",
                            budget.limit
                        ));
                        // Nothing of this file was read before it, so its
                        // part of the cache is this entry's alone: dropped
                        // with it, or each entry skipped would hold up to
                        // a limit more.
                        payloads.fields.retain(|&(f, _), _| f != file);
                        budget.left = budget.limit;
                    }
                    // A DATA object this poll already found unreadable,
                    // after its wait: skipped at once.
                    Err(e) if e.is::<KnownBad>() => errors.add(&e),
                    // Skipped once reported.
                    Err(e) if self.failure(Wait::Entry, &e, errors) => {}
                    Err(_) => return out,
                }
                self.failed = [None; 4];
                self.pos += 1;
            }
            if !skipped_slot {
                return out;
            }
        }
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
fn seen(l: &Location, last: &Location, same_dir: bool) -> bool {
    if same_dir {
        !l.order(last).is_gt()
    } else {
        is_old(l, last)
    }
}

/// Whether an entry is one already shown, or older than it: by time
/// within the boot of `last`, not by sequence number. Flushing to /var
/// copies the runtime journal's entries with the same boot, monotonic and
/// realtime timestamps and XOR hash; systemd 255 gives the copies the
/// runtime file's sequence-number id and new, higher seqnums, so
/// sd-journal's order calls them newer and `journalctl -f` 255 prints
/// them twice (follow.sh, flush-var). Monotonic time within a boot only
/// grows, so a copy is older than the last entry shown, or equal to it.
/// Across boots this falls back to sd-journal's order.
fn is_old(l: &Location, last: &Location) -> bool {
    if l.boot_id == last.boot_id {
        l.monotonic < last.monotonic
            || (l.monotonic == last.monotonic
                && l.realtime == last.realtime
                && l.xor_hash == last.xor_hash)
    } else {
        !l.order(last).is_gt()
    }
}

/// An entry a file's walk chose: its location for the merge, and where to
/// read it once it is among the entries shown.
#[derive(Debug, Clone)]
struct Candidate {
    location: Location,
    file: usize,
    offset: u64,
}

/// The newest `n` entries across the files' walks (each newest first),
/// returned oldest first. This is how `journalctl -n` finds them:
/// sd_journal_seek_tail, then sd_journal_previous_skip, which is
/// `real_journal_next` going up (v249 sd-journal.c). Every file first
/// moves past heads that aren't strictly before the last entry taken,
/// then the largest head by `journal_file_compare_locations` is taken, the
/// earlier file on a tie. So a flushed copy (see `merge`) is shown once,
/// from the earlier file in search order, and only the heads are read: a
/// file's walk goes no further back than the entries it contributes.
///
/// An error a walk yields is reported and the walk goes on: the walks
/// pass over what they can't read (an entry, an array) and yield the
/// error in its place, so only what it hides is lost.
///
/// At most `held` candidates are kept (the query's held limit): past it
/// the older entries are left out, reported.
fn newest<I>(walks: Vec<I>, n: usize, held: usize, errors: &mut QueryErrors) -> Vec<Candidate>
where
    I: Iterator<Item = Result<Candidate>>,
{
    let mut walks: Vec<Option<I>> = walks.into_iter().map(Some).collect();
    let mut heads: Vec<Option<Candidate>> = walks.iter().map(|_| None).collect();
    let mut out: Vec<Candidate> = Vec::new();
    while out.len() < n {
        if out.len() >= held {
            errors.add_text("limit of held entries reached; older entries left out".to_string());
            break;
        }
        for (walk, head) in walks.iter_mut().zip(heads.iter_mut()) {
            loop {
                if head.is_none() {
                    match walk.as_mut().and_then(Iterator::next) {
                        Some(Ok(c)) => *head = Some(c),
                        Some(Err(err)) => {
                            errors.add(&err);
                            continue;
                        }
                        None => {
                            *walk = None;
                            break;
                        }
                    }
                }
                match (head.as_ref(), out.last()) {
                    (Some(h), Some(last)) if !h.location.order(&last.location).is_lt() => {
                        *head = None;
                    }
                    _ => break,
                }
            }
        }
        // The largest head, compared in file order against the running
        // best, which keeps the earlier file on a tie: real_journal_next's
        // loop, spelled out because the order isn't transitive (std's
        // `min_by`/`max_by` document results for a total order only). Why
        // not itertools' `kmerge_by`: its heap needs a transitive order and
        // has no tie rule.
        let mut best: Option<usize> = None;
        for (i, head) in heads.iter().enumerate() {
            let Some(h) = head else { continue };
            let better = best.is_none_or(|b| {
                heads[b]
                    .as_ref()
                    .is_some_and(|bh| h.location.order(&bh.location).is_gt())
            });
            if better {
                best = Some(i);
            }
        }
        let Some(best) = best else { break };
        out.extend(heads[best].take());
    }
    out.reverse();
    out
}

/// Merge a follow's per-file streams, each already in file order, the
/// way sd-journal's `next_beyond_location` does (v249 sd-journal.c):
/// every file first moves past heads already shown (`seen` against the
/// last entry taken), then the smallest head by
/// `journal_file_compare_locations` is taken, the earlier file on a tie.
/// Sorting with that comparator instead could panic: it isn't a total
/// order across sequence-number ids and boots (realtime fallback, clock
/// jumps), and a head ordered before the last one taken is dropped, as
/// sd-journal drops it, not shown out of order.
///
/// `same_dir` tells whether two candidates' files (by index) share a
/// directory. Within one, the skip is sd-journal's not-beyond rule, as
/// `journalctl -f` goes on with sd_journal_next. Across directories it is
/// `is_old`, which also hides a flush's copies: systemd 255's have the
/// originals' times but higher seqnums under the same seqnum id, so
/// sd-journal's order would call them newer; 249's (and the archived
/// runtime file left in /run) are under another seqnum id. Files are read
/// in sd-journal's search order (/run before /var), so the copy kept is
/// the one journalctl shows. The last N (`tail`) come from `newest`, which
/// walks backwards as journalctl's `-n` does: in the non-transitive cases
/// a forward merge could pick a different last N.
fn merge(streams: Vec<Vec<Candidate>>, same_dir: impl Fn(usize, usize) -> bool) -> Vec<Candidate> {
    // One position per stream; the head of stream `i` is streams[i][pos[i]].
    let mut pos = vec![0usize; streams.len()];
    let mut out: Vec<Candidate> = Vec::new();
    loop {
        if let Some(last) = out.last() {
            for (i, s) in streams.iter().enumerate() {
                while s
                    .get(pos[i])
                    .is_some_and(|h| seen(&h.location, &last.location, same_dir(h.file, last.file)))
                {
                    pos[i] += 1;
                }
            }
        }
        let mut best: Option<usize> = None;
        for (i, s) in streams.iter().enumerate() {
            let Some(head) = s.get(pos[i]) else { continue };
            let better =
                best.is_none_or(|b| head.location.order(&streams[b][pos[b]].location).is_lt());
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

/// What to do with an entry met while walking backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Take,
    /// Newer than the range wanted: keep walking.
    Skip,
}

/// A newest-first stream of entry offsets from one file, each with what
/// the stream knows about it.
type Stream<'a, T> = Box<dyn Iterator<Item = Result<(u64, T)>> + 'a>;

/// Several newest-first streams of one file merged into one, newest
/// first; an offset in several streams comes out once. Offsets are file
/// order, so the largest is the newest. An error sorts first, so it is
/// yielded as soon as a stream meets one; the callers report it and go
/// on.
/// https://docs.rs/itertools/0.14.0/itertools/trait.Itertools.html#method.kmerge_by
fn union<'a, T: 'a>(streams: Vec<Stream<'a, T>>) -> Stream<'a, T> {
    fn key<T>(r: &Result<(u64, T)>) -> u64 {
        r.as_ref().map_or(u64::MAX, |x| x.0)
    }
    Box::new(
        streams
            .into_iter()
            .kmerge_by(|a, b| key(a) > key(b))
            .dedup_by(|a, b| matches!((a, b), (Ok(x), Ok(y)) if x.0 == y.0)),
    )
}

/// One group's matches in one file, newest first: its driver term's entry
/// list walked backwards, each entry read once and checked against the
/// other terms' DATA offsets ("Reading": multiple matches).
struct Walk<'a, S> {
    f: &'a JournalFile,
    driver: Stream<'a, ()>,
    /// Per other term, the DATA offsets of its values; the entry must
    /// carry one of each.
    others: Vec<Vec<u64>>,
    step: S,
    /// The last entry read: an older one must end before it.
    end: u64,
}

impl<S: Fn(&Location) -> Step> Iterator for Walk<'_, S> {
    type Item = Result<(u64, Location)>;

    /// The next older match. An entry that can't be read is yielded as
    /// its error and the walk goes on, as sd-journal's generic_array_get
    /// skips a bad entry ("Entry item is bad, skipping over it"). Skipping
    /// is sound for `step`: the next readable entry is checked on its own.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (off, ()) = match self.driver.next()? {
                Ok(x) => x,
                Err(err) => return Some(Err(err)),
            };
            // Entries never overlap (journald appends): checked before the
            // body is read, so a corrupt size can't make each entry read
            // the rest of the file.
            let e = match self.f.entry_before(off, self.end) {
                Ok(e) => e,
                Err(err) => return Some(Err(err)),
            };
            self.end = off;
            let location = Location::of(self.f, &e);
            match (self.step)(&location) {
                Step::Skip => {}
                Step::Take => {
                    let matches = self
                        .others
                        .iter()
                        .all(|alts| alts.iter().any(|o| e.items.contains(o)));
                    if matches {
                        return Some(Ok((off, location)));
                    }
                }
            }
        }
    }
}

/// One file's matches for `filter` and `step`, newest first, read lazily:
/// one walk per group, driven from the term with the fewest entries, the
/// groups' matches merged. A term that ORs several values of one field
/// drives through the union of their entry lists. Every term is looked up
/// in the file's hash table first; a term with no DATA object here means
/// its group can't match in this file, and one that can't be looked up
/// leaves its group out, reported in `errors`: the other groups still
/// match.
fn file_walk<'a, S>(
    f: &'a JournalFile,
    filter: &[Group],
    step: S,
    errors: &mut QueryErrors,
) -> Stream<'a, Location>
where
    S: Fn(&Location) -> Step + Copy + 'a,
{
    fn offsets(it: EntriesRev<'_>) -> Stream<'_, ()> {
        Box::new(it.map(|r| r.map(|off| (off, ()))))
    }
    let walk = |driver, others| -> Stream<'a, Location> {
        Box::new(Walk {
            f,
            driver,
            others,
            step,
            end: u64::MAX,
        })
    };
    let mut groups: Vec<Stream<'a, Location>> = Vec::new();
    if filter.is_empty() {
        groups.push(walk(offsets(f.all_entries_rev()), Vec::new()));
    }
    'groups: for group in filter {
        let mut resolved: Vec<Vec<DataRef>> = Vec::new();
        for term in group {
            let mut refs = Vec::new();
            for payload in term {
                match f.find_data(payload) {
                    Ok(Some(d)) => refs.push(d),
                    Ok(None) => {}
                    Err(err) => {
                        errors.add(&err);
                        continue 'groups;
                    }
                }
            }
            resolved.push(refs);
        }
        if resolved.iter().any(Vec::is_empty) {
            continue;
        }
        let Some(driver) = resolved
            .iter()
            .enumerate()
            // The counts come from the file: saturate, don't overflow.
            .min_by_key(|(_, r)| r.iter().fold(0u64, |a, d| a.saturating_add(d.n_entries)))
            .map(|(i, _)| i)
        else {
            continue;
        };
        let lists = resolved[driver]
            .iter()
            .map(|d| offsets(f.entries_of_rev(d)))
            .collect();
        let others = resolved
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != driver)
            .map(|(_, r)| r.iter().map(|d| d.offset).collect())
            .collect();
        groups.push(walk(union(lists), others));
    }
    union(groups)
}

/// The DATA payloads one query has read, keyed by (file index, offset):
/// a field, `None` for a payload without `=` (skipped), and the objects
/// that failed, so another entry listing one isn't decoded again.
#[derive(Default)]
struct Payloads {
    fields: HashMap<(usize, u64), Option<Field>>,
    failed: HashSet<(usize, u64)>,
}

/// An entry lists a DATA object this query already failed to read
/// (`Payloads::failed`): a definitive error, not one to wait for, since
/// the object had its read (and, following, its wait) already.
/// Why not thiserror's derive (the workspace's error convention): the
/// prototype doesn't depend on it, and file.rs's `OverLimit` is written
/// the same way; the `sb-journal` crate derives both.
#[derive(Debug)]
struct KnownBad {
    path: PathBuf,
    item: u64,
}

impl std::fmt::Display for KnownBad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: more entries list data {:#x}, which can't be read; left out too",
            self.path.display(),
            self.item
        )
    }
}

impl std::error::Error for KnownBad {}

#[cfg(test)]
impl Payloads {
    /// The payload bytes the cache holds.
    fn held(&self) -> usize {
        self.fields
            .values()
            .flatten()
            .map(|f| f.payload.len())
            .sum()
    }
}

/// An entry's fields, each DATA object read once through `payloads`
/// within the query's `left` bytes, which every read spends
/// (`OverLimit` when they run out). Each field held spends its own size
/// too, so an entry listing one cached object millions of times is
/// bounded as well.
fn record(
    f: &JournalFile,
    e: &Entry,
    payloads: &mut Payloads,
    file: usize,
    left: &mut usize,
) -> Result<Record> {
    let mut fields = Vec::with_capacity(e.items.len());
    for &item in &e.items {
        if payloads.failed.contains(&(file, item)) {
            return Err(KnownBad {
                path: f.path.clone(),
                item,
            }
            .into());
        }
        let field = match payloads.fields.entry((file, item)) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => match f.data_payload_spending(item, left) {
                Ok(p) => v.insert(Field::new(p)),
                Err(err) => {
                    if !err.is::<OverLimit>() {
                        payloads.failed.insert((file, item));
                    }
                    return Err(err).with_context(|| format!("entry {}'s fields", e.seqnum));
                }
            },
        };
        let Some(rest) = left.checked_sub(std::mem::size_of::<Field>()) else {
            let allowed = *left;
            *left = 0;
            return Err(OverLimit { limit: allowed })
                .with_context(|| format!("entry {}'s fields", e.seqnum));
        };
        *left = rest;
        fields.extend(field.clone());
    }
    Ok(Record {
        location: Location::of(f, e),
        fields,
        file,
    })
}

/// systemd's `utf8_is_printable`: valid UTF-8 without C0 controls other
/// than tab and newline, DEL or C1 controls, and without noncharacters,
/// which `unichar_is_valid` refuses (src/basic/utf8.c): U+FDD0..U+FDEF and
/// every code point ending in FFFE or FFFF.
pub fn printable(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|s| {
        s.chars().all(|c| {
            let c = u32::from(c);
            !((c < 0x20 && c != 0x09 && c != 0x0a)
                || (0x7f..=0x9f).contains(&c)
                || (0xfdd0..=0xfdef).contains(&c)
                || c & 0xfffe == 0xfffe)
        })
    })
}

/// One entry as `journalctl -o json --all` prints it (output_json in
/// src/shared/logs-show.c): the address fields first, then every field; a
/// value that isn't printable UTF-8 is an array of byte values; a field
/// carried more than once is an array of its values.
pub fn to_json(r: &Record, release: Release) -> serde_json::Value {
    use serde_json::{Map, Value};
    let l = &r.location;
    let mut m = Map::new();
    m.insert("__CURSOR".into(), Value::String(l.cursor()));
    m.insert(
        "__REALTIME_TIMESTAMP".into(),
        Value::String(l.realtime.to_string()),
    );
    m.insert(
        "__MONOTONIC_TIMESTAMP".into(),
        Value::String(l.monotonic.to_string()),
    );
    m.insert("_BOOT_ID".into(), Value::String(hex_id(&l.boot_id)));
    if release.prints_seqnum {
        m.insert("__SEQNUM".into(), Value::String(l.seqnum.to_string()));
        m.insert("__SEQNUM_ID".into(), Value::String(hex_id(&l.seqnum_id)));
    }
    let value = |b: &[u8]| {
        if printable(b) {
            Value::String(String::from_utf8_lossy(b).into_owned())
        } else {
            Value::Array(b.iter().map(|&x| Value::from(x)).collect())
        }
    };
    for field in &r.fields {
        let (name, v) = (field.name(), field.value());
        // The address field comes from the entry header; journalctl skips
        // the data field of the same name (logs-show.c output_json).
        if name == "_BOOT_ID" {
            continue;
        }
        match m.get_mut(name.as_ref()) {
            Some(Value::Array(a)) if a.first().is_some_and(|x| !x.is_number()) => {
                a.push(value(v));
            }
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value(v)]);
            }
            None => {
                m.insert(name.into_owned(), value(v));
            }
        }
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic ids: a cursor built here, not copied from a journal.
    const CURSOR: &str = "s=00112233445566778899aabbccddeeff;i=3;b=ffeeddccbbaa99887766554433221100;m=1f4;t=5f5e100;x=abc";

    #[test]
    fn cursor_round_trips() {
        assert_eq!(
            Location::parse_cursor(CURSOR).expect("parses").cursor(),
            CURSOR
        );
    }

    #[test]
    fn cursor_numbers_are_hex_digits_only() {
        assert!(Location::parse_cursor(&CURSOR.replace("i=3", "i=+3")).is_none());
        assert!(Location::parse_cursor(&CURSOR.replace("s=00", "s=+0")).is_none());
        assert!(Location::parse_cursor(&CURSOR.replace(";x=abc", "")).is_none());
        // A repeated key doesn't make up for the missing one.
        assert!(Location::parse_cursor(&CURSOR.replace(";x=abc", ";i=4")).is_none());
    }

    #[test]
    fn printable_follows_systemd() {
        assert!(printable(b"plain\ttext\n"));
        assert!(!printable(b"\x1b[31mred\x1b[0m"));
        assert!(!printable(&[0xff, 0xfe]));
        assert!(!printable("\u{85}".as_bytes()));
        // Noncharacters: valid UTF-8, refused by systemd's unichar_is_valid.
        assert!(!printable("\u{fdd0}".as_bytes()));
        assert!(!printable("\u{fffe}".as_bytes()));
        assert!(!printable("\u{1ffff}".as_bytes()));
        assert!(printable("\u{fdcf}\u{fff0}".as_bytes()));
    }

    fn rec(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, real: u64) -> Candidate {
        Candidate {
            location: Location {
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
        let out = merge(in_files(vec![run, var, other]), |a, b| a == b);
        let got: Vec<(u8, u64)> = out
            .iter()
            .map(|r| (r.location.seqnum_id[0], r.location.realtime))
            .collect();
        assert_eq!(got, vec![(1, 100), (3, 150), (1, 200), (2, 300)]);
    }

    /// The comparator isn't transitive: x < y (same seqnum id), y < z (same
    /// boot), z < x (realtime). Once x is taken, z is seen (`seen`: another
    /// boot, so sd-journal's order, by either rule) and dropped, as
    /// sd-journal's next_beyond_location drops it, instead of being printed
    /// out of order after y.
    #[test]
    fn merge_drops_a_head_not_beyond_the_last_taken() {
        let x = rec(1, 1, 0xa, 5, 300);
        let y = rec(1, 2, 0xb, 1, 100);
        let z = rec(2, 1, 0xb, 9, 200);
        assert!(x.location.order(&y.location).is_lt());
        assert!(y.location.order(&z.location).is_lt());
        assert!(z.location.order(&x.location).is_lt());
        for one_dir in [false, true] {
            let streams = in_files(vec![vec![y.clone()], vec![z.clone()], vec![x.clone()]]);
            let out = merge(streams, |a, b| one_dir || a == b);
            let got: Vec<u64> = out.iter().map(|r| r.location.realtime).collect();
            assert_eq!(got, vec![300, 100], "one directory: {one_dir}");
        }
    }

    /// `newest` over walks given newest first, as realtimes oldest first.
    fn newest_of(walks: Vec<Vec<Candidate>>, n: usize) -> Vec<u64> {
        let mut errors = QueryErrors::default();
        let walks = walks.into_iter().map(|w| w.into_iter().map(Ok)).collect();
        let out = newest(walks, n, usize::MAX, &mut errors);
        assert!(errors.shown.is_empty());
        out.iter().map(|c| c.location.realtime).collect()
    }

    /// Newest first across files, a flushed copy once (from the earlier
    /// file), and no more than `n`.
    #[test]
    fn newest_takes_the_last_n_and_drops_flushed_copies() {
        let run = vec![rec(1, 2, 9, 20, 200), rec(1, 1, 9, 10, 100)];
        let var = vec![
            rec(2, 3, 9, 30, 300),
            rec(2, 2, 9, 20, 200),
            rec(2, 1, 9, 10, 100),
        ];
        let other = vec![rec(3, 7, 8, 5, 150)];
        let all = newest_of(vec![run.clone(), var.clone(), other.clone()], 10);
        assert_eq!(all, vec![100, 150, 200, 300]);
        assert_eq!(newest_of(vec![run, var, other], 2), vec![200, 300]);
    }

    /// The same non-transitive x, y, z as above: going backwards, as
    /// journalctl's `-n` does (seek_tail, previous_skip), x is taken, then
    /// y isn't strictly before it and is dropped, then z is. journalctl
    /// then prints forward from z: z, then x (its forward seek from z finds
    /// nothing in y's file by monotonic time, and x by realtime).
    #[test]
    fn newest_walks_backwards_like_journalctl() {
        let x = rec(1, 1, 0xa, 5, 300);
        let y = rec(1, 2, 0xb, 1, 100);
        let z = rec(2, 1, 0xb, 9, 200);
        assert_eq!(
            newest_of(vec![vec![y], vec![z], vec![x]], 2),
            vec![200, 300]
        );
    }

    /// Equal heads (a flushed copy): the earlier file's is kept.
    #[test]
    fn newest_keeps_the_earlier_file_on_a_tie() {
        let mut copies: Vec<Candidate> = (0..3).map(|_| rec(1, 1, 9, 10, 100)).collect();
        for (i, c) in copies.iter_mut().enumerate() {
            c.file = i;
            c.location.seqnum_id = [u8::try_from(i).expect("small"); 16];
        }
        let walks = copies
            .into_iter()
            .map(|c| vec![Ok(c)].into_iter())
            .collect();
        let mut errors = QueryErrors::default();
        let out = newest(walks, 10, usize::MAX, &mut errors);
        assert_eq!(out.iter().map(|c| c.file).collect::<Vec<_>>(), vec![0]);
    }

    /// A walk's error is reported and the walk goes on: what it gives
    /// before and after stands.
    #[test]
    fn newest_reports_a_walk_error_and_goes_on() {
        let good = vec![Ok(rec(1, 2, 9, 20, 200)), Ok(rec(1, 1, 9, 10, 100))];
        let bad = vec![
            Ok(rec(2, 5, 8, 50, 250)),
            Err(anyhow::anyhow!("broken")),
            Ok(rec(2, 4, 8, 40, 150)),
        ];
        let mut errors = QueryErrors::default();
        let out = newest(
            vec![good.into_iter(), bad.into_iter()],
            10,
            usize::MAX,
            &mut errors,
        );
        let got: Vec<u64> = out.iter().map(|c| c.location.realtime).collect();
        assert_eq!(got, vec![100, 150, 200, 250]);
        assert_eq!(errors.shown, vec!["broken".to_string()]);
    }

    /// The output limit `k` entries of `journal` take, read newest or
    /// oldest first: the shared `U=x` object once (16 + 48 + 3 bytes),
    /// then per entry its MESSAGE object (16 + 48 + 10) and two fields.
    fn per_record(k: usize) -> usize {
        67 + k * (74 + 2 * std::mem::size_of::<Field>())
    }

    /// An entry listing one DATA object over and over reads it once (the
    /// query's cache), and spends a field's size on every listing: it is
    /// refused once the limit runs out.
    #[test]
    fn an_entry_over_the_output_limit_is_refused() {
        let (mut c, l) = crafted::journal(1);
        let items = vec![l.messages[0]; 50];
        let big = c.entry(9, &items);
        let (_t, f) = c.write();
        let e = f.entry(big).expect("entry");
        let field = std::mem::size_of::<Field>();
        let mut left = 74 + 49 * field;
        let err = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect_err("over");
        assert!(err.is::<OverLimit>(), "{err:#}");
        let mut left = 74 + 50 * field;
        let r = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect("fits exactly");
        assert_eq!((r.fields.len(), left), (50, 0));
    }

    /// Empty payloads cost their object and their fields too, so an entry
    /// listing one over and over can't read without end under the limit.
    #[test]
    fn empty_payloads_spend_the_output_limit() {
        let (mut c, _l) = crafted::journal(1);
        let empty = c.data(b"");
        let big = c.entry(9, &vec![empty; 1000]);
        let (_t, f) = c.write();
        let e = f.entry(big).expect("entry");
        let mut left = 64 + 999 * std::mem::size_of::<Field>();
        let err = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect_err("over");
        assert!(err.is::<OverLimit>(), "{err:#}");
    }
    use crate::file::crafted::{self, *};

    fn messages(records: &[Record]) -> Vec<String> {
        records
            .iter()
            .flat_map(|r| &r.fields)
            .filter(|f| f.name() == "MESSAGE")
            .map(|f| String::from_utf8_lossy(f.value()).into_owned())
            .collect()
    }

    fn u_is_x() -> Vec<Group> {
        vec![vec![vec![b"U=x".to_vec()]]]
    }

    /// One unreadable entry is skipped and reported; the entries on both
    /// sides of it are still shown, unfiltered and through a match.
    #[test]
    fn an_unreadable_entry_is_skipped_not_the_file() {
        let (mut c, l) = crafted::journal(5);
        c.set(l.entries[2] + O_SIZE, 0);
        let (t, _f) = c.write();
        for filter in [Vec::new(), u_is_x()] {
            let mut j = Journal::open(&[t.path().to_path_buf()]);
            let got = j.tail(&filter, 10, None);
            assert_eq!(messages(&got), ["m0", "m1", "m3", "m4"], "{filter:?}");
            assert_eq!(j.errors.len(), 1, "{:?}", j.errors);
        }
    }

    /// The last N are still N when one in the way can't be read.
    #[test]
    fn an_unreadable_entry_does_not_count_against_n() {
        let (mut c, l) = crafted::journal(5);
        c.set(l.entries[3] + O_SIZE, 0);
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        assert_eq!(messages(&j.tail(&u_is_x(), 2, None)), ["m2", "m4"]);
    }

    /// An entry whose fields can't be read is dropped whole, as
    /// journalctl drops it (output_json on -EBADMSG), and reported.
    #[test]
    fn an_entry_with_an_unreadable_field_is_dropped_and_reported() {
        let (mut c, l) = crafted::journal(4);
        c.set(l.messages[1] + O_SIZE, 8);
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        let got = j.tail(&[], 10, None);
        assert_eq!(messages(&got), ["m0", "m2", "m3"]);
        assert_eq!(j.errors.len(), 1, "{:?}", j.errors);
    }

    /// A crafted journal of `n` entries whose header counts only `shown`,
    /// followed from there: `grow` then counts them all, as journald
    /// appending would.
    fn followed(c: &mut crafted::Crafted, shown: u64) -> (tempfile::NamedTempFile, Journal) {
        let n = c.get(H_N_ENTRIES);
        c.set(H_N_ENTRIES, shown);
        let (t, _f) = c.write();
        let j = Journal::open(&[t.path().to_path_buf()]);
        c.set(H_N_ENTRIES, n);
        (t, j)
    }

    fn grow(c: &crafted::Crafted, t: &tempfile::NamedTempFile) {
        std::fs::write(t.path(), &c.b).expect("grow");
    }

    /// Following past an unreadable entry: it is waited for once (it may be
    /// mid-write), then reported and skipped; the entries around it are
    /// shown.
    #[test]
    fn follow_skips_an_unreadable_entry_after_one_wait() {
        let (mut c, l) = crafted::journal(6);
        c.set(l.entries[4] + O_SIZE, 0);
        let (t, j) = followed(&mut c, 2);
        let mut f = Follow::new(j, None);
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&u_is_x())), ["m2", "m3"]);
        // Waiting, not yet reported.
        assert!(f.errors.is_empty(), "{:?}", f.errors);
        assert_eq!(messages(&f.poll(&u_is_x())), ["m5"]);
        assert_eq!(f.errors.len(), 1, "{:?}", f.errors);
    }
    /// A cut-off file shows what lies before the cut.
    #[test]
    fn a_truncated_file_shows_the_entries_before_the_cut() {
        let (mut c, l) = crafted::journal(6);
        let (t, _f) = c.write();
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(l.entries[4] + 8)
            .expect("truncate");
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        let got = j.tail(&u_is_x(), 10, None);
        assert_eq!(messages(&got), ["m0", "m1", "m2", "m3"]);
        assert_eq!(j.errors.len(), 2, "{:?}", j.errors);
    }

    /// A broken lookup leaves out its group only: the other group of the
    /// filter still matches.
    #[test]
    fn a_failed_lookup_leaves_the_other_groups() {
        let (mut c, l) = crafted::journal(3);
        // The bucket chain U=x sits in now loops back on itself.
        c.set(l.shared + D_NEXT_HASH, l.shared);
        let (t, _f) = c.write();
        let filter = vec![
            vec![vec![b"U=x".to_vec()]],
            vec![vec![b"MESSAGE=m1".to_vec()]],
        ];
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        let got = j.tail(&filter, 10, None);
        assert!(messages(&got).contains(&"m1".to_string()));
        assert!(!j.errors.is_empty());
    }

    /// A file list that changed while the files were opened (a
    /// rotation) is listed and opened again.
    #[test]
    fn a_list_that_changed_while_opening_is_opened_again() {
        let (mut c, _l) = crafted::journal(2);
        let (a, _f) = c.write();
        let (b, _g) = c.write();
        let mut lists = vec![
            vec![a.path().to_path_buf(), b.path().to_path_buf()],
            vec![a.path().to_path_buf(), b.path().to_path_buf()],
            vec![a.path().to_path_buf()],
        ];
        let j = Journal::open_listed(|| Ok(lists.pop().expect("listed at most 3 times")))
            .expect("opens");
        assert_eq!(j.files.len(), 2);
    }

    /// Entry counts come from the file: an OR term whose values claim
    /// counts that overflow when added still matches (found by the fuzz
    /// harness: the sum overflowed, a panic with overflow checks on).
    #[test]
    fn entry_counts_that_overflow_when_added_still_match() {
        let (mut c, l) = crafted::journal(3);
        c.set(l.messages[0] + D_N_ENTRIES, u64::MAX);
        c.set(l.messages[1] + D_N_ENTRIES, u64::MAX);
        let (t, _f) = c.write();
        let filter = vec![vec![
            vec![b"MESSAGE=m0".to_vec(), b"MESSAGE=m1".to_vec()],
            vec![b"U=x".to_vec()],
        ]];
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        assert_eq!(messages(&j.tail(&filter, 10, None)), ["m0", "m1"]);
    }

    /// Many bad entries: each distinct error once, ten in full, then a
    /// count, so a ruined file can't flood the output.
    #[test]
    fn errors_per_query_are_capped() {
        let (mut c, l) = crafted::journal(30);
        for (i, &e) in l.entries.iter().enumerate() {
            if i % 2 == 1 {
                c.set(e + O_SIZE, 0);
            }
        }
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        assert_eq!(j.tail(&[], 100, None).len(), 15);
        assert_eq!(j.errors.len(), QueryErrors::SHOWN + 1, "{:?}", j.errors);
        assert!(j.errors.last().expect("count").ends_with("5 more errors"));
    }

    /// A shared DATA object that decodes a long way and then fails is
    /// decoded once, not once per entry listing it: the failure spends
    /// the limit and is remembered. The entries that don't list it are
    /// still shown.
    #[test]
    fn a_failing_shared_object_is_decoded_once() {
        let text = b"MESSAGE="
            .iter()
            .chain(&[b'z'; 99_992])
            .copied()
            .collect::<Vec<u8>>();
        // A zstd frame (a 128 KiB window, a 4-byte content size): a raw block
        // of the 100,000 bytes, then a block of the reserved type, so the
        // decoder gives part of the text and then fails.
        let mut zstd = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x38];
        zstd.extend_from_slice(&100_000u32.to_le_bytes());
        zstd.extend_from_slice(&(100_000u32 << 3).to_le_bytes()[..3]);
        zstd.extend_from_slice(&text);
        zstd.extend_from_slice(&[0x07, 0x00, 0x00]);
        let (mut c, l) = crafted::journal(1);
        let bomb = c.data_raw(4, &zstd, crate::hash::jenkins64(&text));
        let mut odd = Vec::new();
        for i in 0..20u64 {
            let items = if i % 2 == 0 {
                vec![l.messages[0]]
            } else {
                vec![l.messages[0], bomb]
            };
            odd.push(c.entry(10 + i, &items));
        }
        c.set(H_N_ENTRIES, 21);
        let main = c.array(&[&l.entries[..], &odd[..]].concat(), 21, 0);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let (t, f) = c.write();
        let mut left = usize::MAX;
        assert!(f.data_payload_spending(bomb, &mut left).is_err());
        // The raw bytes read, and what was decoded before the failure.
        assert!(
            usize::MAX - left > zstd.len() + 32 * 1024,
            "spent {}",
            usize::MAX - left
        );
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        assert_eq!(j.tail(&[], 100, None).len(), 11);
        let decoded = j.errors.iter().filter(|e| e.contains("zstd")).count();
        let remembered = j
            .errors
            .iter()
            .filter(|e| e.contains("can't be read"))
            .count();
        assert_eq!((decoded, remembered), (1, 1), "{:?}", j.errors);
    }

    /// One query holds at most its payload limit: the newest entries that
    /// fit are shown, the limit is reported, and the older ones are left
    /// out. (A corrupt file can grow one DATA object shared by every
    /// entry; without the limit, each record would hold a copy.)
    #[test]
    fn a_query_over_its_payload_limit_keeps_the_newest() {
        let (mut c, _l) = crafted::journal(10);
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        // U=x is read once, each MESSAGE once (see per_record).
        j.payload_limit = per_record(3) + 5;
        let got = j.tail(&[], 10, None);
        assert_eq!(messages(&got), ["m7", "m8", "m9"]);
        assert_eq!(j.errors.len(), 1, "{:?}", j.errors);
        assert!(j.errors[0].contains("output limit"), "{:?}", j.errors);
    }

    /// Fields are read after the merge, newest first across files, so the
    /// limit leaves out the oldest entries of the whole journal, not the
    /// files listed last.
    #[test]
    fn the_output_limit_keeps_the_newest_across_files() {
        let (mut c, _l) = crafted::journal(4);
        let (older, _f) = c.write();
        let (mut c, l) = crafted::journal(4);
        for (i, &e) in l.entries.iter().enumerate() {
            c.set(e + 16, 100 + i as u64); // seqnum
            c.set(e + 32, 100 + i as u64); // monotonic, same boot
        }
        let (newer, _g) = c.write();
        let mut j = Journal::open(&[newer.path().to_path_buf(), older.path().to_path_buf()]);
        j.payload_limit = per_record(3);
        let got = j.tail(&[], 10, None);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|r| r.location.seqnum >= 100), "{got:?}");
    }

    /// Following within the output limit: a poll stops at the entry that
    /// doesn't fit, unmoved, and the next poll goes on from it: nothing is
    /// lost or doubled.
    #[test]
    fn follow_over_the_output_limit_goes_on_at_the_next_poll() {
        let (mut c, _l) = crafted::journal(10);
        let (t, mut j) = followed(&mut c, 2);
        j.payload_limit = per_record(3) + 5;
        let mut f = Follow::new(j, None);
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
        let (t, mut j) = followed(&mut c, 1);
        j.payload_limit = 1000;
        let mut f = Follow::new(j, None);
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
        let (ta, _fa) = a.write();
        let (tb, _fb) = b.write();
        let mut j = Journal::open(&[ta.path().to_path_buf(), tb.path().to_path_buf()]);
        j.payload_limit = per_record(3) + 5;
        let mut f = Follow::new(j, None);
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

    /// The entries held before the merge are bounded too.
    #[test]
    fn held_entries_are_bounded() {
        let (mut c, _l) = crafted::journal(10);
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        j.held_limit = std::mem::size_of::<Candidate>() * 4;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m6", "m7", "m8", "m9"]);
        assert!(
            j.errors.iter().any(|e| e.contains("held entries")),
            "{:?}",
            j.errors
        );
    }

    fn release(version_id: &str) -> Release {
        Release::parse(version_id).expect("a supported release")
    }

    #[test]
    fn init_scope_rule_on_external_files_drops_the_coredump_uid() {
        let g = unit_filter(release("26.04"), "a.service", true);
        assert_eq!(g[3].len(), 2);
    }

    /// Each row is found by its own VERSION_ID, once; anything else is
    /// refused.
    #[test]
    fn releases_parse_by_version_id() {
        for r in RELEASES {
            assert_eq!(Release::parse(r.version_id), Some(*r));
            assert_eq!(
                RELEASES
                    .iter()
                    .filter(|o| o.version_id == r.version_id)
                    .count(),
                1
            );
        }
        for other in ["20.04", "26.10", "", "24.04.1"] {
            assert!(Release::parse(other).is_none(), "{other}");
        }
    }

    /// The rows keep the rules the matrix checked: `_PID=1` before
    /// 259.1, init.scope after.
    #[test]
    fn unit_rule_per_release() {
        let has = |v: &str, term: &[u8]| {
            unit_filter(release(v), "a.service", true)
                .iter()
                .flatten()
                .flatten()
                .any(|p| p.as_slice() == term)
        };
        for v in ["22.04", "24.04"] {
            assert!(has(v, b"_PID=1") && !has(v, b"_SYSTEMD_CGROUP=/init.scope"));
        }
        assert!(has("26.04", b"_SYSTEMD_CGROUP=/init.scope") && !has("26.04", b"_PID=1"));
    }

    fn loc(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, xor: u64) -> Location {
        Location {
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
        let last = loc(1, 5, 9, 50, 7);
        let copy_of_earlier = loc(1, 8, 9, 30, 3);
        let copy_of_last = loc(1, 10, 9, 50, 7);
        assert!(copy_of_earlier.order(&last).is_gt());
        assert!(is_old(&copy_of_earlier, &last));
        assert!(is_old(&copy_of_last, &last));
        assert!(!is_old(&loc(1, 11, 9, 50, 8), &last));
        assert!(!is_old(&loc(2, 1, 9, 51, 0), &last));
        // Another boot: sd-journal's order (realtime here).
        assert!(is_old(&loc(3, 1, 4, 10, 0), &last));
        assert!(!is_old(&loc(3, 1, 4, 90, 0), &last));
    }

    /// A journal file (regular, not compact) holding `entries` as
    /// (seqnum, monotonic) ENTRY objects, one boot, all in one entry array,
    /// the header counting `n` of them. Each entry lists one DATA object,
    /// `M=1`, the item an entry needs (sd-journal's check_object).
    fn journal(path: &Path, seqnum_id: u8, entries: &[(u64, u64)], n: u64) {
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(crate::file::SIGNATURE);
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
        r.iter().map(|r| r.location.seqnum).collect()
    }

    const FOUR: [(u64, u64); 4] = [(1, 10), (2, 20), (3, 30), (4, 40)];

    #[test]
    fn follow_reads_only_what_was_appended() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
        set_n(&a, 3);
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(std::slice::from_ref(&b));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        set_n(&b, 4);
        std::fs::remove_file(&b).expect("remove");
        f.rescan(&[]);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert!(!f.has(&b));
        assert!(f.files.is_empty());
    }

    /// Two files written at once (system and user journal): journald raises
    /// each file's IN_MODIFY from its own coalesced timer, so the newer
    /// entry in one file can be shown before the older entry in the other
    /// is read. The older one is still shown, not dropped as old.
    #[test]
    fn follow_keeps_an_older_entry_of_a_file_read_later() {
        let dir = tempfile::tempdir().expect("dir");
        let sys = dir.path().join("system.journal");
        let user = dir.path().join("user-1000.journal");
        journal(&sys, 1, &[(1, 10), (3, 30), (4, 40)], 1);
        journal(&user, 1, &[(2, 20), (5, 50)], 0);
        let paths = [sys.clone(), user.clone()];
        let mut f = Follow::new(Journal::open(&paths), None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 72 + 2 * 80;
        bytes[third] = 1;
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        std::fs::remove_file(&a).expect("remove");
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files.len(), 1);
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert_eq!(f.errors.len(), 1);
        assert!(f.files.is_empty());
    }

    /// A file renamed between the listing and the stat of its old name
    /// (rotation racing a rescan) isn't closed: it still has a name.
    #[test]
    fn follow_keeps_a_renamed_file_missing_from_one_listing() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("system.journal");
        let b = dir.path().join("system-old.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(&[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files.len(), 1);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
        set_n(&a, 3);
        let tmp = dir.path().join("copy.tmp");
        std::fs::copy(&a, &tmp).expect("copy");
        std::fs::rename(&tmp, &a).expect("rename");
        f.rescan(std::slice::from_ref(&a));
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        assert_eq!(f.files.len(), 1);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&run)), None);
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![2]);
        // Copies of 1 and 2 under higher seqnums of the same id, then 5.
        journal(&var, 1, &[(7, 10), (8, 20), (9, 50)], 3);
        f.rescan(&[run.clone(), var.clone()]);
        assert_eq!(seqnums(&f.poll(&[])), vec![9]);
    }

    /// An entry whose DATA can't be read (here an item pointing nowhere)
    /// is retried on the next wake like an unreadable entry, then reported
    /// and skipped; the entries after it still come.
    #[test]
    fn follow_retries_an_entry_whose_data_fails_once() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
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

    /// A slot never written below n_entries (an unclean shutdown) is
    /// waited for once, then skipped without an error: the entries after
    /// it still come, and a dirty file can't stall the follow.
    #[test]
    fn follow_skips_a_zero_item_after_one_wait() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 1);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
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
    fn with_entries(
        n: u64,
        extra: impl Fn(&mut crafted::Crafted) -> Vec<Vec<u64>>,
    ) -> crafted::Crafted {
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
    fn oversized(c: &mut crafted::Crafted, i: u64) -> Vec<u64> {
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
        let (t, mut j) = followed(&mut c, 1);
        j.payload_limit = 1000;
        let mut f = Follow::new(j, None);
        grow(&c, &t);
        assert_eq!(messages(&f.poll(&[])), ["after"]);
        let skipped = f
            .errors
            .iter()
            .filter(|e| e.contains("larger than the output limit"))
            .count();
        assert_eq!(skipped, 3, "{:?}", f.errors);
        assert!(f.held_peak <= 1000, "held {}", f.held_peak);
    }

    /// The tail's newest entry alone over the limit is reported and
    /// skipped, and the older entries are shown, as a follow skips it.
    #[test]
    fn a_tail_skips_a_newest_entry_larger_than_the_limit() {
        let mut c = with_entries(3, |c| vec![oversized(c, 0)]);
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        j.payload_limit = 1000;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m0", "m1", "m2"]);
        assert!(
            j.errors
                .iter()
                .any(|e| e.contains("larger than the output limit")),
            "{:?}",
            j.errors
        );
    }

    /// An entry over the limit after others were read still ends the tail
    /// there, with no gap: only an entry read with the limit whole is
    /// skipped.
    #[test]
    fn a_tail_still_stops_at_a_later_entry_over_the_limit() {
        let mut c = with_entries(3, |c| {
            let m = c.data(b"MESSAGE=newest");
            vec![oversized(c, 0), vec![m]]
        });
        let (t, _f) = c.write();
        let mut j = Journal::open(&[t.path().to_path_buf()]);
        j.payload_limit = 1000;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["newest"]);
        assert!(
            j.errors
                .iter()
                .any(|e| e.contains("older entries left out")),
            "{:?}",
            j.errors
        );
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
        let (t, j) = followed(&mut c, 1);
        let mut f = Follow::new(j, None);
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
        let (t, j) = followed(&mut c, 0);
        let mut f = Follow::new(j, None);
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
            let mut f = Follow::new(Journal::open(std::slice::from_ref(&old)), None);
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
    /// however many wakes look again; nothing matches it meanwhile.
    #[test]
    fn follow_reports_a_failed_lookup_once() {
        let (mut c, _l) = crafted::journal(4);
        // A table size that isn't whole items: every lookup fails.
        c.set(H_HASH_TABLE_SIZE, c.get(H_HASH_TABLE_SIZE) + 8);
        let (t, j) = followed(&mut c, 1);
        let mut f = Follow::new(j, None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), None);
        set_n(&a, 4);
        assert_eq!(seqnums(&f.poll(&[])), vec![2, 3, 4]);
    }
}
