//! Matching, merging and following across a set of journal files.

use crate::file::{Entry, JournalFile, hex_id};
use anyhow::{Context, Result};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One term of a conjunction: the entry carries any of these
/// `FIELD=value` payloads (sd-journal ORs matches on the same field).
pub type Term = Vec<Vec<u8>>;
/// A conjunction of terms; a filter is a disjunction of groups.
pub type Group = Vec<Term>;

/// The supported releases, each with its systemd's `journalctl -u` rule.
/// sb refuses any other release (cli-core startup check), so this list is
/// complete by construction; a new LTS adds its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    /// 22.04, systemd 249.
    Jammy,
    /// 24.04, systemd 255.
    Noble,
    /// 26.04, systemd 259.
    Resolute,
}

impl Release {
    pub fn parse(version_id: &str) -> Option<Self> {
        match version_id {
            "22.04" => Some(Self::Jammy),
            "24.04" => Some(Self::Noble),
            "26.04" => Some(Self::Resolute),
            _ => None,
        }
    }

    /// v255+ prints `__SEQNUM` and `__SEQNUM_ID` in `-o json`.
    pub fn prints_seqnum(self) -> bool {
        !matches!(self, Self::Jammy)
    }
}

const COREDUMP_MESSAGE_ID: &str = "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1";

fn pair(field: &str, value: &str) -> Vec<u8> {
    format!("{field}={value}").into_bytes()
}

/// journalctl's `-u <unit>` match set, per release.
///
/// - 249 and 255: `add_matches_for_unit` in src/shared/logs-show.c.
/// - 259.x: `add_matches_for_unit_full`, with init.scope for PID 1. The coredump group also accepts the
///   `systemd-coredump` user's uid, but only for the local journal:
///   journalctl drops the uid term with `--file`/`-D`
///   (src/journal/journalctl-filter.c), so `external` mirrors that.
pub fn unit_filter(release: Release, unit: &str, external: bool) -> Vec<Group> {
    let own = vec![vec![pair("_SYSTEMD_UNIT", unit)]];
    let pid1 = vec![vec![b"_PID=1".to_vec()], vec![pair("UNIT", unit)]];
    // 259.1 and later match init.scope instead of _PID=1, to include PID
    // 1's helper processes (logs-show.c at v259.1..v259.9; the v259 tag
    // itself still has _PID=1). 26.04 ships 259.x.
    let init = vec![
        vec![b"_SYSTEMD_CGROUP=/init.scope".to_vec()],
        vec![pair("UNIT", unit)],
    ];
    let daemons = vec![
        vec![b"_UID=0".to_vec()],
        vec![pair("OBJECT_SYSTEMD_UNIT", unit)],
    ];
    match release {
        Release::Jammy | Release::Noble => vec![
            own,
            vec![
                vec![COREDUMP_MESSAGE_ID.as_bytes().to_vec()],
                vec![b"_UID=0".to_vec()],
                vec![pair("COREDUMP_UNIT", unit)],
            ],
            pid1,
            daemons,
        ],
        Release::Resolute => {
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
    pub fields: Vec<(String, Vec<u8>)>,
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
        let mut found = self.per_file(filter, n, step);
        let skip = found.len().saturating_sub(n);
        found.split_off(skip)
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
        self.per_file(filter, usize::MAX, step)
    }

    /// Each file's matches, merged. A broken file or entry is reported in
    /// `errors` and the rest is still shown, as sd-journal skips an entry
    /// it can't read (output_json on -EBADMSG) and keeps the other files:
    /// `.journal~` files are exactly the ones left dirty.
    fn per_file(
        &mut self,
        filter: &[Group],
        n: usize,
        step: impl Fn(&Location) -> Step + Copy,
    ) -> Vec<Record> {
        let mut streams: Vec<Vec<Record>> = Vec::new();
        for f in &self.files {
            match last_matching(f, filter, n, step) {
                Ok(entries) => {
                    let mut stream = Vec::with_capacity(entries.len());
                    // Newest first from the walk: reverse into file order.
                    for e in entries.iter().rev() {
                        match record(f, e) {
                            Ok(r) => stream.push(r),
                            Err(err) => self.errors.push(format!("{err:#}")),
                        }
                    }
                    streams.push(stream);
                }
                Err(err) => self.errors.push(format!("{err:#}")),
            }
        }
        merge(streams)
    }
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
/// journalctl's `-n` walks backwards (keeping the largest); in the
/// non-transitive cases a forward merge can pick a different last N.
fn merge(streams: Vec<Vec<Record>>) -> Vec<Record> {
    // One position per stream; the head of stream `i` is streams[i][pos[i]].
    let mut pos = vec![0usize; streams.len()];
    let mut out: Vec<Record> = Vec::new();
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

/// The last `n` entries of one file that match `filter` and `step`,
/// newest first. Each group walks the entry list of its rarest term
/// backwards and checks the other terms against the entry's DATA offsets
/// ("Reading": multiple matches). Entry lists are in file order, which is
/// sequence order, so `Stop` ends the walk.
fn last_matching(
    f: &JournalFile,
    filter: &[Group],
    n: usize,
    step: impl Fn(&Location) -> Step,
) -> Result<Vec<Entry>> {
    let mut offsets: BTreeSet<u64> = BTreeSet::new();
    let mut take = |list: &[u64], check: &dyn Fn(&Entry) -> bool| -> Result<()> {
        let mut got = 0;
        for &off in list.iter().rev() {
            if got >= n {
                break;
            }
            let e = f.entry(off)?;
            match step(&Location::of(f, &e)) {
                Step::Stop => break,
                Step::Skip => continue,
                Step::Take => {}
            }
            if check(&e) {
                offsets.insert(off);
                got += 1;
            }
        }
        Ok(())
    };
    if filter.is_empty() {
        take(&f.all_entries()?, &|_| true)?;
    }
    for group in filter {
        // Resolve every term to this file's DATA objects; a term with none
        // means the group can't match here.
        let mut resolved: Vec<Vec<crate::file::DataRef>> = Vec::new();
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
        // Drive from the term with the fewest entries. A term with several
        // values (an OR on one field) drives through the union of its
        // values' entry lists, in file order.
        let Some(driver) = resolved
            .iter()
            .enumerate()
            .min_by_key(|(_, r)| r.iter().map(|d| d.n_entries).sum::<u64>())
            .map(|(i, _)| i)
        else {
            continue;
        };
        let mut list: Vec<u64> = Vec::new();
        for d in &resolved[driver] {
            list.extend(f.entries_of(d)?);
        }
        list.sort_unstable();
        list.dedup();
        let others: Vec<Vec<u64>> = resolved
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != driver)
            .map(|(_, r)| r.iter().map(|d| d.offset).collect())
            .collect();
        take(&list, &|e: &Entry| {
            others
                .iter()
                .all(|alts| alts.iter().any(|o| e.items.contains(o)))
        })?;
    }
    let mut out = Vec::new();
    for off in offsets.iter().rev().take(n) {
        out.push(f.entry(*off)?);
    }
    Ok(out)
}

fn record(f: &JournalFile, e: &Entry) -> Result<Record> {
    let mut fields = Vec::with_capacity(e.items.len());
    for &item in &e.items {
        let payload = f.data_payload(item)?;
        let Some(eq) = payload.iter().position(|&b| b == b'=') else {
            continue;
        };
        let name = String::from_utf8_lossy(&payload[..eq]).into_owned();
        fields.push((name, payload[eq + 1..].to_vec()));
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
    if release.prints_seqnum() {
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
    for (name, v) in &r.fields {
        // The address field comes from the entry header; journalctl skips
        // the data field of the same name (logs-show.c output_json).
        if name == "_BOOT_ID" {
            continue;
        }
        match m.get_mut(name) {
            Some(Value::Array(a)) if a.first().is_some_and(|x| !x.is_number()) => {
                a.push(value(v));
            }
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value(v)]);
            }
            None => {
                m.insert(name.clone(), value(v));
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

    fn rec(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, real: u64) -> Record {
        Record {
            location: Location {
                seqnum_id: [seqnum_id; 16],
                seqnum,
                boot_id: [boot; 16],
                monotonic: mono,
                realtime: real,
                xor_hash: 0,
            },
            fields: Vec::new(),
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

    #[test]
    fn resolute_on_external_files_drops_the_coredump_uid() {
        let g = unit_filter(Release::Resolute, "a.service", true);
        assert_eq!(g[3].len(), 2);
    }
}
