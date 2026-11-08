//! Matching, merging and following across a set of journal files.

use crate::file::{DataRef, EntriesRev, Entry, JournalFile, hex_id};
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

    /// Every matching entry after `after`, oldest first.
    pub fn newer(&mut self, filter: &[Group], after: &Location) -> Vec<Record> {
        let step = |l: &Location| {
            if l.order(after).is_gt() {
                Step::Take
            } else {
                Step::Stop
            }
        };
        let mut errors = Vec::new();
        let mut streams = Vec::new();
        for walk in self.walks(filter, step, &mut errors) {
            // Up to the first error, which is reported: what the walk gave
            // before stands, as in `tail`.
            let mut found = Vec::new();
            for c in walk {
                match c {
                    Ok(c) => found.push(c),
                    Err(err) => {
                        errors.push(format!("{err:#}"));
                        break;
                    }
                }
            }
            // Newest first from the walk: reverse into file order.
            found.reverse();
            streams.push(found);
        }
        self.errors.append(&mut errors);
        self.decode(&merge(streams))
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
/// sd_journal_next. The last N (`tail`) come from `newest`, which walks
/// backwards as journalctl's `-n` does: in the non-transitive cases a
/// forward merge could pick a different last N.
fn merge(streams: Vec<Vec<Candidate>>) -> Vec<Candidate> {
    // One position per stream; the head of stream `i` is streams[i][pos[i]].
    let mut pos = vec![0usize; streams.len()];
    let mut out: Vec<Candidate> = Vec::new();
    loop {
        if let Some(last) = out.last() {
            for (i, s) in streams.iter().enumerate() {
                while s
                    .get(pos[i])
                    .is_some_and(|h| !h.location.order(&last.location).is_gt())
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
    /// Older than the range wanted: nothing further back qualifies.
    Stop,
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
/// other terms' DATA offsets ("Reading": multiple matches). Entry lists
/// are in file order, which is sequence order, so `Stop` ends the walk.
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
                Step::Stop => self.done = true,
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
}
