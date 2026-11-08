//! Matching, merging and following across a set of journal files.

use crate::file::{ArrayCursor, DataRef, EntriesRev, Entry, JournalFile, hex_id};
use anyhow::{Context, Result};
use itertools::Itertools;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, hash_map};
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
}

impl Journal {
    /// Open each file. A file that can't be read is reported, never
    /// silently dropped (an unsupported format names the file).
    pub fn open(paths: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        let mut errors = Vec::new();
        for p in paths {
            match JournalFile::open(p) {
                Ok(f) => files.push(f),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        Self { files, errors }
    }

    /// The last `n` matching entries before `before` (all when `None`),
    /// oldest first. `filter` empty means every entry.
    pub fn tail(&mut self, filter: &[Group], n: usize, before: Option<&Location>) -> Vec<Record> {
        let step = |l: &Location| match before {
            Some(b) if !l.order(b).is_lt() => Step::Skip,
            _ => Step::Take,
        };
        let mut errors = Vec::new();
        let picked = newest(self.walks(filter, step, &mut errors), n, &mut errors);
        self.errors.append(&mut errors);
        self.decode(&picked)
    }

    /// One lazy walk per file, newest first. A file whose matches can't be
    /// looked up is reported in `errors` and left out, as sd-journal
    /// leaves out a file it can't iterate and keeps the others:
    /// `.journal~` files are exactly the ones left dirty.
    fn walks<'a, S>(
        &'a self,
        filter: &'a [Group],
        step: S,
        errors: &mut Vec<String>,
    ) -> Vec<impl Iterator<Item = Result<Candidate>> + 'a>
    where
        S: Fn(&Location) -> Step + Copy + 'a,
    {
        let mut out = Vec::new();
        for (file, f) in self.files.iter().enumerate() {
            match file_walk(f, filter, step) {
                Ok(walk) => out.push(walk.map(move |r| {
                    r.map(|(offset, location)| Candidate {
                        location,
                        file,
                        offset,
                    })
                })),
                Err(err) => errors.push(format!("{err:#}")),
            }
        }
        out
    }

    /// Read the chosen entries for output. An entry that can't be read is
    /// reported and skipped, as sd-journal skips an entry it can't read
    /// (output_json on -EBADMSG).
    fn decode(&mut self, picked: &[Candidate]) -> Vec<Record> {
        let mut out = Vec::with_capacity(picked.len());
        // Entries share most DATA objects (a unit's _PID, _COMM, _HOSTNAME
        // and so on): each is read and decompressed once.
        let mut payloads = HashMap::new();
        for c in picked {
            let f = &self.files[c.file];
            match f
                .entry(c.offset)
                .and_then(|e| record(f, &e, &mut payloads, c.file))
            {
                Ok(r) => out.push(r),
                Err(err) => self.errors.push(format!("{err:#}")),
            }
        }
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
    last: Option<Location>,
    pub errors: Vec<String>,
    /// Listed files that failed to open at the last rescan.
    unopened: Vec<PathBuf>,
}

struct Followed {
    file: JournalFile,
    /// Entries of the file's entry list read so far.
    pos: u64,
    cursor: Option<ArrayCursor>,
    /// Entries numbered below this may be ones already shown: copies a
    /// flush wrote, or entries the tail printed. They are checked against
    /// the last entry shown (`is_old`); from here on the position alone
    /// decides, so an entry a slower file delivers late is still shown.
    check_until: u64,
    /// A file opened while following: its leading entries are checked
    /// until the first one that is new (a flush writes all its copies
    /// before any new entry).
    leading: bool,
    /// Per group, per term, per value: its DATA object here, once it
    /// exists (a value seen for the first time gets one later).
    resolved: Vec<Vec<Vec<Option<u64>>>>,
    /// Still listed in the journal directories at the last rescan.
    listed: bool,
    /// Rescans in a row that didn't list this file.
    unlisted: u8,
    /// The position whose read failed on the last wake: it is retried
    /// once, as the file may have been caught mid-write ("Concurrency"),
    /// then reported.
    failed: Option<u64>,
    /// The file couldn't be read twice in a row and that was reported;
    /// cleared by the next good read.
    stuck: bool,
    /// Archived or renamed to `.journal~`, seen by a header read: no
    /// further writes come.
    sealed: bool,
}

impl Follow {
    /// Continue from `journal`'s files, read up to the entry counts of
    /// their headers at open, and `last`, the newest entry shown. The tail
    /// may have shown entries counted after the open: the headers are read
    /// again now, and entries up to those counts are checked against
    /// `last`.
    pub fn new(journal: Journal, filter: &[Group], last: Option<Location>) -> Self {
        let mut follow = Self {
            files: Vec::new(),
            last,
            errors: journal.errors,
            unopened: Vec::new(),
        };
        for mut file in journal.files {
            let pos = file.header.n_entries;
            let check_until = match file.refresh() {
                Ok(()) => file.header.n_entries,
                Err(_) => pos,
            };
            let mut f = Followed::new(file, filter, pos);
            f.check_until = check_until;
            follow.files.push(f);
        }
        follow
    }

    /// Match the open files to `paths` (the journal directories' listing,
    /// in search order): a known inode keeps its position under its new
    /// name, a new file is read from its start, and one no longer listed
    /// is read to its end, then closed once deleted.
    pub fn rescan(&mut self, paths: &[PathBuf], filter: &[Group]) {
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
                        let mut f = Followed::new(file, filter, 0);
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

    /// Whether `path` is one of the open files, or a listed file that
    /// failed to open (its next modification should rescan).
    pub fn has(&self, path: &Path) -> bool {
        self.files.iter().any(|f| f.file.path == path)
    }

    /// Whether `path` failed to open at the last rescan.
    pub fn unopened(&self, path: &Path) -> bool {
        self.unopened.iter().any(|p| p == path)
    }

    /// The matching entries appended since the last poll, oldest first.
    /// Copies of entries already shown are dropped (`check_until`,
    /// `is_old`).
    ///
    /// Each file's new matches are decoded as they are read, with each
    /// DATA object read once per wake (`record`'s cache, as the tail
    /// uses), so a DATA object that fails to read is retried with its
    /// entry; copies of shown entries are dropped before decoding. The
    /// files' `Candidate`s are then merged.
    pub fn poll(&mut self, filter: &[Group]) -> Vec<Record> {
        let mut streams: Vec<Vec<Candidate>> = Vec::new();
        let mut decoded: HashMap<(usize, u64), Record> = HashMap::new();
        let mut payloads = HashMap::new();
        for (i, f) in self.files.iter_mut().enumerate() {
            let last = self.last.as_ref();
            let stream = f.read_new(i, filter, last, &mut payloads, &mut self.errors);
            let mut candidates = Vec::with_capacity(stream.len());
            for (c, r) in stream {
                decoded.insert((c.file, c.offset), r);
                candidates.push(c);
            }
            streams.push(candidates);
        }
        let picked = merge(streams);
        let out: Vec<Record> = picked
            .iter()
            .filter_map(|c| decoded.remove(&(c.file, c.offset)))
            .collect();
        if let Some(c) = picked.last() {
            self.last = Some(c.location.clone());
        }
        self.files.retain(Followed::keep);
        out
    }
}

impl Followed {
    fn new(file: JournalFile, filter: &[Group], pos: u64) -> Self {
        let resolved = filter
            .iter()
            .map(|g| g.iter().map(|t| vec![None; t.len()]).collect())
            .collect();
        let mut f = Self {
            file,
            pos,
            cursor: None,
            check_until: 0,
            leading: false,
            resolved,
            listed: true,
            unlisted: 0,
            failed: None,
            stuck: false,
            sealed: false,
        };
        f.resolve(filter);
        f
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
    /// table each.
    fn resolve(&mut self, filter: &[Group]) {
        for (g, group) in filter.iter().enumerate() {
            for (t, term) in group.iter().enumerate() {
                for (v, payload) in term.iter().enumerate() {
                    if self.resolved[g][t][v].is_none()
                        && let Ok(Some(d)) = self.file.find_data(payload)
                    {
                        self.resolved[g][t][v] = Some(d.offset);
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

    /// A read of the file failed at `pos`: the first time it is retried on
    /// the next wake; the second time it is reported (unless the file is
    /// deleted and was already read to its end: vacuum deallocates a file
    /// before it unlinks it, so it can read as zeros) and true is returned.
    fn failure(&mut self, e: &anyhow::Error, errors: &mut Vec<String>) -> bool {
        if self.failed != Some(self.pos) {
            self.failed = Some(self.pos);
            return false;
        }
        let drained = !self.listed && self.pos >= self.file.header.n_entries;
        if !self.stuck && !drained {
            errors.push(format!("{e:#}"));
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
        last: Option<&Location>,
        payloads: &mut HashMap<(usize, u64), Option<Field>>,
        errors: &mut Vec<String>,
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
            self.stuck = self.failure(&e, errors);
            return out;
        }
        self.sealed = self.file.header.archived()
            || self.file.path.extension().is_some_and(|x| x == "journal~");
        let mut resolved = false;
        // Rounds: a zero item ends a read (`entries_since`); once it is
        // skipped, the entries after it are read in the same wake.
        loop {
            let offsets = match self.file.entries_since(&mut self.cursor, self.pos) {
                Ok(o) => o,
                Err(e) => {
                    self.stuck = self.failure(&e, errors);
                    return out;
                }
            };
            self.stuck = false;
            if offsets.is_empty() {
                // A failed header read on an earlier wake is behind us.
                self.failed = None;
                return out;
            }
            if !resolved && !filter.is_empty() {
                self.resolve(filter);
                resolved = true;
            }
            let mut skipped_zero = false;
            for off in offsets {
                // A slot not written below n_entries: waited for once (it
                // may be mid-write), then skipped, as `EntriesRev` skips it.
                if off == 0 {
                    if self.failed == Some(self.pos) {
                        self.failed = None;
                        self.pos += 1;
                        skipped_zero = true;
                        continue;
                    }
                    self.failed = Some(self.pos);
                    return out;
                }
                let read = self.file.entry(off).and_then(|e| {
                    let location = Location::of(&self.file, &e);
                    // The first entry newer than the last shown ends a new
                    // file's leading copies, whether it matches or not.
                    if self.leading && last.is_none_or(|l| !is_old(&location, l)) {
                        self.check_until = self.pos;
                        self.leading = false;
                    }
                    // Where an entry already shown can be (`check_until`),
                    // one not newer than the last shown is a copy: dropped
                    // before it is decoded.
                    let copy =
                        self.pos < self.check_until && last.is_some_and(|l| is_old(&location, l));
                    if copy || !self.matches(filter, &e) {
                        return Ok(None);
                    }
                    // Decoded here, so a DATA object that fails to read is
                    // retried on the next wake like the entry itself. Unlike
                    // the tail's decode of only what is shown, the merge
                    // can still drop one of these (a same-wake flush copy);
                    // that is around a flush only.
                    let r = record(&self.file, &e, payloads, file)?;
                    let c = Candidate {
                        location,
                        file,
                        offset: off,
                    };
                    Ok(Some((c, r)))
                });
                match read {
                    Ok(c) => out.extend(c),
                    // Skipped once reported.
                    Err(e) if self.failure(&e, errors) => {}
                    Err(_) => return out,
                }
                self.failed = None;
                self.pos += 1;
            }
            if !skipped_zero {
                return out;
            }
        }
    }
}

/// Whether a followed entry is one already shown, or older than it: by
/// time within the boot of `last`, not by sequence number. Flushing to
/// /var copies the runtime journal's entries with the same boot,
/// monotonic and realtime timestamps and XOR hash; systemd 255 gives the
/// copies the runtime file's sequence-number id and new, higher seqnums,
/// so sd-journal's order calls them newer and `journalctl -f` 255 prints
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
/// A walk that fails is reported and dropped from then on, as
/// sd-journal's `real_journal_next` drops a file it can't iterate; what it
/// gave before stands.
fn newest<I>(walks: Vec<I>, n: usize, errors: &mut Vec<String>) -> Vec<Candidate>
where
    I: Iterator<Item = Result<Candidate>>,
{
    let mut walks: Vec<Option<I>> = walks.into_iter().map(Some).collect();
    let mut heads: Vec<Option<Candidate>> = walks.iter().map(|_| None).collect();
    let mut out: Vec<Candidate> = Vec::new();
    while out.len() < n {
        for (walk, head) in walks.iter_mut().zip(heads.iter_mut()) {
            loop {
                if head.is_none() {
                    match walk.as_mut().and_then(Iterator::next) {
                        Some(Ok(c)) => *head = Some(c),
                        Some(Err(err)) => {
                            errors.push(format!("{err:#}"));
                            *walk = None;
                            break;
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

/// Merge per-file streams, each already in file order, the way
/// sd-journal's `next_beyond_location` does (v249 sd-journal.c): every
/// file first moves past heads that aren't strictly beyond the last entry
/// taken, then the smallest head by `journal_file_compare_locations` is
/// taken, the earlier file on a tie. Sorting with that comparator instead
/// could panic: it isn't a total order across sequence-number ids and
/// boots (realtime fallback, clock jumps), and a head ordered before the
/// last one taken is dropped, as sd-journal drops it, not shown out of
/// order.
///
/// The not-beyond skip is also what hides flushed copies: after a flush
/// the archived runtime file in /run holds the same entries as /var under
/// another sequence-number id, with the same boot, monotonic time,
/// realtime and XOR hash. Files are read in sd-journal's search order
/// (/run before /var), so the copy kept is the one journalctl shows.
///
/// Following uses this forward merge, as `journalctl -f` goes on with
/// sd_journal_next. Its skip is `is_old`, sd-journal's order except
/// within one boot, where monotonic time decides: systemd 255's flushed
/// copies have the originals' times but higher seqnums under the same
/// seqnum id, so sd-journal's order would call them newer. The last N
/// (`tail`) come from `newest`, which walks backwards as journalctl's
/// `-n` does: in the non-transitive cases a forward merge could pick a
/// different last N.
fn merge(streams: Vec<Vec<Candidate>>) -> Vec<Candidate> {
    // One position per stream; the head of stream `i` is streams[i][pos[i]].
    let mut pos = vec![0usize; streams.len()];
    let mut out: Vec<Candidate> = Vec::new();
    loop {
        if let Some(last) = out.last() {
            for (i, s) in streams.iter().enumerate() {
                while s
                    .get(pos[i])
                    .is_some_and(|h| is_old(&h.location, &last.location))
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
/// yielded as soon as a stream meets one; the callers stop there.
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
    done: bool,
}

impl<S: Fn(&Location) -> Step> Iterator for Walk<'_, S> {
    type Item = Result<(u64, Location)>;

    /// The next older match. After an error it ends: one unreadable entry
    /// ends this file's walk, and the error is reported.
    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            let found = self.driver.next()?.and_then(|(off, ())| {
                let e = self.f.entry(off)?;
                Ok((off, Location::of(self.f, &e), e))
            });
            let (off, location, e) = match found {
                Ok(x) => x,
                Err(err) => {
                    self.done = true;
                    return Some(Err(err));
                }
            };
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
        None
    }
}

/// One file's matches for `filter` and `step`, newest first, read lazily:
/// one walk per group, driven from the term with the fewest entries, the
/// groups' matches merged. A term that ORs several values of one field
/// drives through the union of their entry lists. Every term is looked up
/// in the file's hash table first; a term with no DATA object here means
/// its group can't match in this file.
fn file_walk<'a, S>(f: &'a JournalFile, filter: &[Group], step: S) -> Result<Stream<'a, Location>>
where
    S: Fn(&Location) -> Step + Copy + 'a,
{
    fn offsets(it: EntriesRev<'_>) -> Stream<'_, ()> {
        Box::new(it.map(|r| r.map(|off| (off, ()))))
    }
    let mut groups: Vec<Stream<'a, Location>> = Vec::new();
    if filter.is_empty() {
        groups.push(Box::new(Walk {
            f,
            driver: offsets(f.all_entries_rev()?),
            others: Vec::new(),
            step,
            done: false,
        }));
    }
    for group in filter {
        let mut resolved: Vec<Vec<DataRef>> = Vec::new();
        for term in group {
            let mut refs = Vec::new();
            for payload in term {
                if let Some(d) = f.find_data(payload)? {
                    refs.push(d);
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
            .min_by_key(|(_, r)| r.iter().map(|d| d.n_entries).sum::<u64>())
            .map(|(i, _)| i)
        else {
            continue;
        };
        let mut lists = Vec::new();
        for d in &resolved[driver] {
            lists.push(offsets(f.entries_of_rev(d)?));
        }
        let others = resolved
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != driver)
            .map(|(_, r)| r.iter().map(|d| d.offset).collect())
            .collect();
        groups.push(Box::new(Walk {
            f,
            driver: union(lists),
            others,
            step,
            done: false,
        }));
    }
    Ok(union(groups))
}

/// An entry's fields, each DATA object read once through `payloads`,
/// keyed by (file index, offset). A payload without `=` is skipped.
fn record(
    f: &JournalFile,
    e: &Entry,
    payloads: &mut HashMap<(usize, u64), Option<Field>>,
    file: usize,
) -> Result<Record> {
    let mut fields = Vec::with_capacity(e.items.len());
    for &item in &e.items {
        let field = match payloads.entry((file, item)) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => v.insert(Field::new(f.data_payload(item)?)),
        };
        fields.extend(field.clone());
    }
    Ok(Record {
        location: Location::of(f, e),
        fields,
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

    /// A flushed copy (another seqnum id, same boot, times and hash)
    /// appears once, from the earlier stream; the streams interleave.
    #[test]
    fn merge_interleaves_and_drops_flushed_copies() {
        let run = vec![rec(1, 1, 9, 10, 100), rec(1, 2, 9, 20, 200)];
        let var = vec![
            rec(2, 1, 9, 10, 100),
            rec(2, 2, 9, 20, 200),
            rec(2, 3, 9, 30, 300),
        ];
        let other = vec![rec(3, 7, 8, 5, 150)];
        let out = merge(vec![run, var, other]);
        let got: Vec<(u8, u64)> = out
            .iter()
            .map(|r| (r.location.seqnum_id[0], r.location.realtime))
            .collect();
        assert_eq!(got, vec![(1, 100), (3, 150), (1, 200), (2, 300)]);
    }

    /// The comparator isn't transitive: x < y (same seqnum id), y < z (same
    /// boot), z < x (realtime). Once x is taken, z is not beyond it and is
    /// dropped, as sd-journal's next_beyond_location drops it, instead of
    /// being printed out of order after y.
    #[test]
    fn merge_drops_a_head_not_beyond_the_last_taken() {
        let x = rec(1, 1, 0xa, 5, 300);
        let y = rec(1, 2, 0xb, 1, 100);
        let z = rec(2, 1, 0xb, 9, 200);
        assert!(x.location.order(&y.location).is_lt());
        assert!(y.location.order(&z.location).is_lt());
        assert!(z.location.order(&x.location).is_lt());
        let out = merge(vec![vec![y], vec![z], vec![x]]);
        let got: Vec<u64> = out.iter().map(|r| r.location.realtime).collect();
        assert_eq!(got, vec![300, 100]);
    }

    /// `newest` over walks given newest first, as realtimes oldest first.
    fn newest_of(walks: Vec<Vec<Candidate>>, n: usize) -> Vec<u64> {
        let mut errors = Vec::new();
        let walks = walks.into_iter().map(|w| w.into_iter().map(Ok)).collect();
        let out = newest(walks, n, &mut errors);
        assert!(errors.is_empty());
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
        let mut errors = Vec::new();
        let out = newest(walks, 10, &mut errors);
        assert_eq!(out.iter().map(|c| c.file).collect::<Vec<_>>(), vec![0]);
    }

    /// A walk that fails is reported and dropped; what it gave stands.
    #[test]
    fn newest_drops_a_failing_walk_and_keeps_the_rest() {
        let good = vec![Ok(rec(1, 2, 9, 20, 200)), Ok(rec(1, 1, 9, 10, 100))];
        let bad = vec![Ok(rec(2, 5, 8, 50, 250)), Err(anyhow::anyhow!("broken"))];
        let mut errors = Vec::new();
        let out = newest(vec![good.into_iter(), bad.into_iter()], 10, &mut errors);
        let got: Vec<u64> = out.iter().map(|c| c.location.realtime).collect();
        assert_eq!(got, vec![100, 200, 250]);
        assert_eq!(errors, vec!["broken".to_string()]);
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
    /// (seqnum, monotonic) ENTRY objects without items, one boot, all in
    /// one entry array, the header counting `n` of them.
    fn journal(path: &Path, seqnum_id: u8, entries: &[(u64, u64)], n: u64) {
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(crate::file::SIGNATURE);
        b[72..88].copy_from_slice(&[seqnum_id; 16]);
        b[88..96].copy_from_slice(&272u64.to_le_bytes());
        b[152..160].copy_from_slice(&n.to_le_bytes());
        let array_size = 24 + 8 * entries.len() as u64;
        let mut at = 272 + array_size;
        let mut array = vec![0u8; 24];
        array[0] = 6;
        array[8..16].copy_from_slice(&array_size.to_le_bytes());
        let mut objects = Vec::new();
        for &(seqnum, mono) in entries {
            array.extend_from_slice(&at.to_le_bytes());
            let mut e = vec![0u8; 64];
            e[0] = 3;
            e[8..16].copy_from_slice(&64u64.to_le_bytes());
            e[16..24].copy_from_slice(&seqnum.to_le_bytes());
            e[24..32].copy_from_slice(&(1000 + mono).to_le_bytes());
            e[32..40].copy_from_slice(&mono.to_le_bytes());
            e[40..56].copy_from_slice(&[9; 16]);
            objects.extend_from_slice(&e);
            at += 64;
        }
        b[176..184].copy_from_slice(&272u64.to_le_bytes());
        b.extend_from_slice(&array);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        set_n(&a, 3);
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(std::slice::from_ref(&b), &[]);
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        set_n(&b, 4);
        std::fs::remove_file(&b).expect("remove");
        f.rescan(&[], &[]);
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
        let mut f = Follow::new(Journal::open(&paths), &[], None);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 2 * 64;
        bytes[third] = 1;
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        std::fs::remove_file(&a).expect("remove");
        f.rescan(&[], &[]);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        std::fs::rename(&a, &b).expect("rename");
        f.rescan(&[], &[]);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(f.files.len(), 1);
        set_n(&b, 3);
        f.rescan(std::slice::from_ref(&b), &[]);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        journal(&b, 1, &FOUR[1..], 0);
        let mut bytes = std::fs::read(&b).expect("read");
        bytes[176..184].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&b, &bytes).expect("write");
        f.rescan(&[a.clone(), b.clone()], &[]);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        set_n(&a, 3);
        let tmp = dir.path().join("copy.tmp");
        std::fs::copy(&a, &tmp).expect("copy");
        std::fs::rename(&tmp, &a).expect("rename");
        f.rescan(std::slice::from_ref(&a), &[]);
        assert_eq!(seqnums(&f.poll(&[])), vec![3]);
        assert_eq!(f.files.len(), 1);
        std::fs::write(&a, vec![0u8; 4096]).expect("zero");
        std::fs::remove_file(&a).expect("remove");
        f.rescan(&[], &[]);
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// A file appearing mid-follow is read from its start; the copies a
    /// flush put there (the shown entries' times) are dropped, the new
    /// entries after them kept.
    #[test]
    fn follow_drops_flushed_copies_in_a_new_file() {
        let dir = tempfile::tempdir().expect("dir");
        let run = dir.path().join("run.journal");
        let var = dir.path().join("var.journal");
        journal(&run, 1, &FOUR[..2], 1);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&run)), &[], None);
        set_n(&run, 2);
        assert_eq!(seqnums(&f.poll(&[])), vec![2]);
        // Copies of 1 and 2 under higher seqnums of the same id, then 5.
        journal(&var, 1, &[(7, 10), (8, 20), (9, 50)], 3);
        f.rescan(&[run.clone(), var.clone()], &[]);
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        // Entries 3 and 4 again at the end, in order (the array must not
        // decrease); 3 with one item, an unaligned offset.
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 2 * 64;
        let at3 = bytes.len() as u64;
        let mut e = bytes[third..third + 64].to_vec();
        e[8..16].copy_from_slice(&80u64.to_le_bytes());
        e.extend_from_slice(&9u64.to_le_bytes());
        e.extend_from_slice(&0u64.to_le_bytes());
        let fourth = bytes[third + 64..third + 128].to_vec();
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
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        // Zero the array's second item (entry 2).
        let mut bytes = std::fs::read(&a).expect("read");
        bytes[272 + 24 + 8..272 + 24 + 16].copy_from_slice(&0u64.to_le_bytes());
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        assert!(f.poll(&[]).is_empty());
        assert_eq!(seqnums(&f.poll(&[])), vec![3, 4]);
        assert!(f.errors.is_empty(), "{:?}", f.errors);
    }

    /// An entry that can't be read is retried on the next wake (journald
    /// may be mid-write), then reported and skipped.
    #[test]
    fn follow_retries_an_unreadable_entry_once() {
        let dir = tempfile::tempdir().expect("dir");
        let a = dir.path().join("a.journal");
        journal(&a, 1, &FOUR, 2);
        let mut f = Follow::new(Journal::open(std::slice::from_ref(&a)), &[], None);
        // Break entry 3's object type; the header counts all four.
        let mut bytes = std::fs::read(&a).expect("read");
        let third = 272 + 24 + 8 * 4 + 2 * 64;
        bytes[third] = 1;
        std::fs::write(&a, &bytes).expect("write");
        set_n(&a, 4);
        assert!(f.poll(&[]).is_empty());
        assert!(f.errors.is_empty());
        assert_eq!(seqnums(&f.poll(&[])), vec![4]);
        assert_eq!(f.errors.len(), 1);
    }
}
