//! The fuzz harness's logic (journal spec, Testing Strategy, "Fuzzing on
//! stable"): real journal files as seeds, mutated, cut and spliced, then
//! read the way hash-check, decode-all, the pages and a follow read them,
//! and the rules that fail a run. The example (`examples/fuzz.rs`) is a
//! thin wrapper: it loads the seeds, sets the address-space limit, runs
//! the watchdog, reads and resets the peak memory around each case and
//! hands what it measured to [`Tally`], so coverage and cargo-mutants reach
//! the rules (plan, "The `testing` feature").
//!
//! Why not cargo-fuzz (libFuzzer): it needs a nightly compiler, and the
//! toolchain is pinned to stable. Why not proptest or bolero: on stable
//! neither is coverage-guided either, and their strategies generate values
//! rather than mutate a large binary seed; what finds bugs here is a valid
//! journal with one field wrong, which a mutator over real files produces
//! directly. afl.rs (cargo-afl), which mutates seed files with coverage
//! feedback, is the next step (journal spec, decision 7): it needs the
//! AFL++ toolchain on the host and runs as its own long-lived process.
//! Randomness comes from fastrand, seeded per case ([`case_rng`]), so case
//! `i` of run seed `s` is the same file on every run.

use crate::testing::tools;
use crate::{
    FieldName, Journal, Limits, OpenError, PageEnd, Position, Query, QueryError, RELEASES,
    ReadErrors, Release, Source, UnitName,
};
use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

/// A run with fewer cases fails (journal spec, Testing Strategy).
pub const CASES_MIN: u64 = 9_000;
/// A run over fewer distinct seeds fails: journal-remote 249's and 255's
/// output, four writer variants each.
pub const SEEDS_MIN: usize = 8;
/// A case whose peak resident memory is more than this above the seeds'
/// base fails, in KiB (16 MiB).
pub const MEMORY_OVER_BASE_MAX_KIB: u64 = 16 << 10;
/// A case running longer is a hang: the run stops there.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The run's address-space limit (`RLIMIT_AS`), 3 GiB: an allocation past
/// it aborts the run, so the case that made it is named by the wrapper's
/// `current` file.
pub const ADDRESS_SPACE: u64 = 3 << 30;

/// A journal file's header size (`header_size`) and arena size
/// (`arena_size`) fields, and its entry count (`n_entries`).
const H_HEADER_SIZE: usize = 88;
const H_ARENA_SIZE: usize = 96;
const H_N_ENTRIES: usize = 152;
/// The smallest header journald writes; a mutation needs one to work on.
const HEADER_MIN: usize = 272;
/// A cut keeps at least this many bytes.
const CUT_MIN: usize = 200;

/// A seed file: its bytes up to its last object, and each object's offset
/// and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    bytes: Vec<u8>,
    objects: Vec<(usize, usize)>,
}

impl Seed {
    /// The seed in `bytes`, a journal file: cut to its arena, then to its
    /// last object (the arena size is set to match). journal-remote
    /// preallocates its files (8 MiB) and counts it all as arena, so the
    /// cut keeps the zeros past the last object from slowing each case
    /// down, and the bytes are shrunk so the base memory the cases are
    /// judged against holds only the seeds. `None` when no object follows
    /// the header: such a file gives nothing to mutate.
    #[must_use]
    pub fn parse(mut bytes: Vec<u8>) -> Option<Self> {
        let header = at(get(&bytes, H_HEADER_SIZE));
        let end = at(get(&bytes, H_HEADER_SIZE).saturating_add(get(&bytes, H_ARENA_SIZE)));
        bytes.truncate(end);
        // Objects follow each other, 8-byte aligned, from the header's end.
        let mut objects = Vec::new();
        let mut next = header;
        while next.saturating_add(16) <= bytes.len() {
            let size = at(get(&bytes, next + 8));
            if size < 16 {
                break;
            }
            objects.push((next, size));
            // The size comes from the file: a corrupt seed ends the walk,
            // and one past memory runs past the file, which keeps its bytes.
            next = next
                .checked_add(size)
                .and_then(|e| e.checked_next_multiple_of(8))
                .unwrap_or(usize::MAX);
        }
        if objects.is_empty() {
            return None;
        }
        if next < bytes.len() {
            bytes.truncate(next);
            put(&mut bytes, H_ARENA_SIZE, (next - header) as u64);
        }
        bytes.shrink_to_fit();
        Some(Self { bytes, objects })
    }

    /// The bytes cases are made from.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Each object's offset and size, in file order.
    #[must_use]
    pub fn objects(&self) -> &[(usize, usize)] {
        &self.objects
    }
}

/// `seeds` without repeats: the same bytes twice count as one seed
/// (`SEEDS_MIN`). The first of each is kept, in order.
#[must_use]
pub fn distinct(seeds: Vec<Seed>) -> Vec<Seed> {
    // Compared with those kept, not hashed into a set of copies: a run has
    // a handful of seeds, and copies would raise the base memory.
    let mut kept: Vec<Seed> = Vec::new();
    for s in seeds {
        if !kept.iter().any(|k| k.bytes == s.bytes) {
            kept.push(s);
        }
    }
    kept
}

/// A file offset or size as an index, saturating: a value past memory is
/// past every file.
fn at(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

fn put(b: &mut [u8], offset: usize, v: u64) {
    if let Some(s) = offset.checked_add(8).and_then(|end| b.get_mut(offset..end)) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

fn get(b: &[u8], offset: usize) -> u64 {
    offset
        .checked_add(8)
        .and_then(|end| b.get(offset..end))
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// Case `case`'s randomness in the run seeded `run`: the same pair gives
/// the same case on every run built from the same `Cargo.lock` (fastrand
/// doesn't promise the same sequence across its versions). The case number
/// is spread by 2^64 divided by the golden ratio, Fibonacci hashing's
/// multiplier, so neighbouring cases get unrelated seeds.
#[must_use]
pub fn case_rng(run: u64, case: u64) -> fastrand::Rng {
    fastrand::Rng::with_seed(run ^ case.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

/// Case `case` of the run seeded `run`: a mutated seed and the release
/// whose rules read it.
///
/// # Panics
///
/// `seeds` is empty.
#[must_use]
pub fn case(seeds: &[Seed], run: u64, case: u64) -> (Vec<u8>, Release) {
    let mut rng = case_rng(run, case);
    let bytes = mutate(&mut rng, seeds);
    let release = RELEASES[rng.usize(..RELEASES.len())];
    (bytes, release)
}

/// Values a corrupt or half-written file holds, by `kind` (0 to 15): zero,
/// sizes and offsets just off the real one (`near`), values near overflow,
/// the file's length and around it, another object's offset.
fn interesting(kind: u8, rng: &mut fastrand::Rng, seed: &Seed, near: u64) -> u64 {
    let len = seed.bytes.len() as u64;
    match kind {
        0 => 0,
        1 => 1,
        2 => near.wrapping_add(8),
        3 => near.wrapping_sub(8),
        4 => u64::MAX,
        5 => !7u64,
        6 => 1 << 63,
        7 => len,
        8 => len.wrapping_sub(8),
        9 => len + 8,
        // Another object, which may be of the wrong type.
        10 => seed.objects[rng.usize(..seed.objects.len())].0 as u64,
        11 => near,
        12 => u64::from(rng.u32(..)),
        13 => near.wrapping_mul(2),
        // journald's ENTRY_SIZE_MAX, 770 MiB.
        14 => 770 << 20,
        _ => rng.u64(..),
    }
}

/// One to four mutations of a seed chosen by `rng`.
///
/// # Panics
///
/// `seeds` is empty.
#[must_use]
pub fn mutate(rng: &mut fastrand::Rng, seeds: &[Seed]) -> Vec<u8> {
    let seed = &seeds[rng.usize(..seeds.len())];
    let mut b = seed.bytes.clone();
    for _ in 0..rng.usize(1..=4) {
        let kind = rng.u8(..8);
        mutate_once(kind, rng, seeds, seed, &mut b);
    }
    b
}

/// One mutation of `b`, a copy of `seed` mutated so far, by `kind` (0 to
/// 7). A file cut below a header is left alone.
fn mutate_once(kind: u8, rng: &mut fastrand::Rng, seeds: &[Seed], seed: &Seed, b: &mut Vec<u8>) {
    if b.len() < HEADER_MIN {
        return;
    }
    match kind {
        // A field of an object: its header (type, size, links, counts) or
        // one of its first items, 8 bytes or, for compact files' items, 4.
        0..=2 => {
            let (o, size) = seed.objects[rng.usize(..seed.objects.len())];
            let offset = o + 8 * rng.usize(..(size / 8).clamp(1, 64));
            let v = interesting(rng.u8(..16), rng, seed, get(b, offset));
            if rng.bool() {
                put(b, offset, v);
            } else if let Some(s) = b
                .get_mut(offset + 4 * rng.usize(..2)..)
                .and_then(|s| s.get_mut(..4))
            {
                s.copy_from_slice(&v.to_le_bytes()[..4]);
            }
        }
        // An object's type or flags byte (compression).
        3 => {
            let (o, _) = seed.objects[rng.usize(..seed.objects.len())];
            if let Some(x) = b.get_mut(o + rng.usize(..2)) {
                *x = rng.u8(..8);
            }
        }
        // A header field.
        4 => {
            let offset = H_HEADER_SIZE + 8 * rng.usize(..23);
            let v = interesting(rng.u8(..16), rng, seed, get(b, offset));
            put(b, offset, v);
        }
        // Cut the file short.
        5 => {
            let len = rng.usize(CUT_MIN..=b.len());
            b.truncate(len);
        }
        // Splice a run of bytes from any seed into this one, 8-byte
        // aligned where it lands.
        6 => {
            let from = &seeds[rng.usize(..seeds.len())].bytes;
            let n = rng.usize(8..=4096).min(from.len()).min(b.len());
            let src = rng.usize(..=from.len() - n);
            let dst = rng.usize(..=b.len() - n) & !7;
            b[dst..dst + n].copy_from_slice(&from[src..src + n]);
        }
        // Random bytes anywhere, payloads included.
        _ => {
            for _ in 0..rng.usize(1..=16) {
                let i = rng.usize(..b.len());
                b[i] ^= rng.u8(1..);
            }
        }
    }
}

/// Why a case couldn't be read at all: the harness's own setup, not the
/// file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExerciseError {
    /// Writing the case's file.
    #[error("write the case file")]
    Write(#[source] io::Error),
    /// Opening the journal or starting a follow (inotify).
    #[error(transparent)]
    Open(#[from] OpenError),
    /// A query that can't be built.
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// What a case's reads gave: the errors reported, and the entries read
/// (decoded, paged or followed), which show the reads reached the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reads {
    /// Errors reported, shown and omitted.
    pub errors: usize,
    /// Entries decoded, shown on a page or given by a follow.
    pub records: usize,
}

impl std::ops::AddAssign for Reads {
    fn add_assign(&mut self, other: Self) {
        self.errors += other.errors;
        self.records += other.records;
    }
}

impl Reads {
    /// `records` entries read, with `errors`.
    fn of(records: usize, errors: &ReadErrors) -> Self {
        Self {
            errors: count(errors),
            records,
        }
    }
}

/// The errors in `errors`, shown and omitted.
fn count(errors: &ReadErrors) -> usize {
    errors.shown.len() + errors.omitted
}

/// What `sb logs` and the tools do with a file, written to `path` from
/// `bytes` and read under `release`'s rules, and what they read:
/// - every DATA object through the hash table, as hash-check;
/// - every entry and its fields in file order, as decode-all, but each
///   DATA object once: a bomb listed by every entry would otherwise be a
///   false hang of the harness;
/// - for three queries (every entry, `-u demo` with the release's unit
///   rule, and an AND of an OR), the tail, the page before its oldest
///   record (paging back, as a viewer does) and the page after it
///   (`Before` and `After`), each
///   building the release's boot table;
/// - a follow of each query from an empty `PageEnd` (every entry in its
///   first batch) and two drains;
/// - a follow from a page of the file with its entry count halved, the
///   whole file written back over it (the same inode, as journald appends)
///   and three drains, so the drains read entries the start didn't.
///
/// # Errors
///
/// The case file can't be written, or a follow can't start (inotify).
pub fn exercise(path: &Path, bytes: &[u8], release: &Release) -> Result<Reads, ExerciseError> {
    std::fs::write(path, bytes).map_err(ExerciseError::Write)?;
    let source = Source::Files(vec![path.to_path_buf()]);
    let journal = Journal::open(source.clone(), release)?;
    let mut reads = read_files(&journal);
    let limits = Limits::default();
    for q in queries(release, &source)? {
        reads += pages(&journal, &q, limits);
        reads += followed(&journal, q, limits)?;
    }
    reads += grown(path, bytes, &journal, limits)?;
    Ok(reads)
}

/// hash-check and decode-all over the journal's files; the records are
/// the entries decoded.
fn read_files(journal: &Journal) -> Reads {
    let (files, opened) = tools::files(journal);
    let hashes = tools::hash_check(&files, &opened);
    let mut reads = Reads::of(0, &opened);
    reads.errors += at(hashes.errors) + at(hashes.differ);
    for f in &files {
        let mut offsets = Vec::new();
        for r in f.all_entries_rev() {
            match r {
                Ok(offset) => offsets.push(offset),
                Err(_) => reads.errors += 1,
            }
        }
        offsets.reverse();
        let mut seen = HashSet::new();
        for (k, &offset) in offsets.iter().enumerate() {
            let end = offsets.get(k + 1).copied().unwrap_or(u64::MAX);
            match f.entry_before(offset, end) {
                Ok(e) => {
                    reads.records += 1;
                    for item in e.items {
                        if seen.insert(item) {
                            reads.errors += usize::from(f.data_payload(item).is_err());
                        }
                    }
                }
                Err(_) => reads.errors += 1,
            }
        }
    }
    reads
}

/// Every entry; `-u demo` under the release's unit rule; and `_UID=0` with
/// `PRIORITY=3` or `PRIORITY=5`.
fn queries(release: &Release, source: &Source) -> Result<[Query; 3], QueryError> {
    let field = |name: &[u8], value: &[u8]| -> Result<Query, QueryError> {
        Ok(Query::field(&FieldName::new(name)?, value))
    };
    let priority = field(b"PRIORITY", b"3")?.or(field(b"PRIORITY", b"5")?);
    Ok([
        tools::every_entry(),
        Query::unit(release, &UnitName::new("demo")?, None, source),
        field(b"_UID", b"0")?.and(priority)?,
    ])
}

/// The tail, and the pages before and after its oldest record: the page
/// back reads entries the tail didn't.
fn pages(journal: &Journal, q: &Query, limits: Limits) -> Reads {
    let page = |at: &Position, n| {
        let p = journal.page(q, at, n, &limits);
        Reads::of(p.records.len(), &p.errors)
    };
    let tail = journal.page(q, &Position::Tail, 50, &limits);
    let mut reads = Reads::of(tail.records.len(), &tail.errors);
    if let Some(oldest) = tail.records.first() {
        reads += page(&Position::Before(oldest.cursor()), 20);
        reads += page(&Position::After(oldest.cursor()), 20);
    }
    reads
}

/// A follow from an empty `PageEnd`: every matching entry in its first
/// batch, then two drains.
fn followed(journal: &Journal, q: Query, limits: Limits) -> Result<Reads, ExerciseError> {
    let (mut follower, first) = journal.follow(q, &PageEnd::default(), &limits)?;
    let mut reads = Reads::of(first.records.len(), &first.errors);
    for _ in 0..2 {
        let batch = follower.drain(&limits);
        reads += Reads::of(batch.records.len(), &batch.errors);
    }
    Ok(reads)
}

/// The follow of a file that grows after its page: the page reads `bytes`
/// with its entry count halved, then the whole of `bytes` is written back
/// in place and the follow drains what the page didn't count. The records
/// are the drains' only.
fn grown(
    path: &Path,
    bytes: &[u8],
    journal: &Journal,
    limits: Limits,
) -> Result<Reads, ExerciseError> {
    let mut half = bytes.to_vec();
    put(&mut half, H_N_ENTRIES, get(bytes, H_N_ENTRIES) / 2);
    std::fs::write(path, &half).map_err(ExerciseError::Write)?;
    let q = tools::every_entry();
    let page = journal.page(&q, &Position::Tail, 50, &limits);
    let (mut follower, first) = journal.follow(q, &page.end, &limits)?;
    let mut reads = Reads::of(0, &page.errors);
    reads += Reads::of(first.records.len(), &first.errors);
    std::fs::write(path, bytes).map_err(ExerciseError::Write)?;
    for _ in 0..3 {
        let batch = follower.drain(&limits);
        reads += Reads::of(batch.records.len(), &batch.errors);
    }
    Ok(reads)
}

/// Why a run fails (journal spec, "The fuzz harness enforcing its own
/// bounds").
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// The case panicked.
    Panic {
        /// The case.
        case: u64,
    },
    /// The case ran past the timeout.
    Hang {
        /// The case.
        case: u64,
        /// How long it had run.
        after: Duration,
    },
    /// The case's peak resident memory was more than
    /// `MEMORY_OVER_BASE_MAX_KIB` above the seeds' base.
    Memory {
        /// The case.
        case: u64,
        /// Its peak above the base, in KiB.
        over_kib: u64,
    },
    /// The peak couldn't be read: after a case, or (`None`) for the base
    /// before the cases.
    PeakUnreadable {
        /// The case.
        case: Option<u64>,
    },
    /// The peak couldn't be reset (`/proc/self/clear_refs`): before a
    /// case, or (`None`) before the base was read.
    ClearRefs {
        /// The case.
        case: Option<u64>,
        /// The system's error.
        error: String,
    },
    /// The run had fewer than `CASES_MIN` cases.
    TooFewCases {
        /// The cases run.
        cases: u64,
    },
    /// The run had fewer than `SEEDS_MIN` distinct seeds.
    TooFewSeeds {
        /// The distinct seeds.
        seeds: usize,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panic { case } => write!(f, "case {case} panicked"),
            Self::Hang { case, after } => {
                write!(f, "case {case} hung: still running after {after:?}")
            }
            Self::Memory { case, over_kib } => write!(
                f,
                "case {case} peaked {over_kib} KiB above the seeds' base, over \
                 {MEMORY_OVER_BASE_MAX_KIB} KiB"
            ),
            Self::PeakUnreadable { case: Some(case) } => {
                write!(f, "case {case}: no peak memory (VmHWM) to read")
            }
            Self::PeakUnreadable { case: None } => {
                write!(f, "no base memory (VmHWM) to read before the cases")
            }
            Self::ClearRefs {
                case: Some(case),
                error,
            } => write!(
                f,
                "case {case}: the peak wasn't reset (clear_refs: {error})"
            ),
            Self::ClearRefs { case: None, error } => write!(
                f,
                "the peak wasn't reset before the cases (clear_refs: {error})"
            ),
            Self::TooFewCases { cases } => {
                write!(f, "{cases} cases run, fewer than {CASES_MIN}")
            }
            Self::TooFewSeeds { seeds } => {
                write!(f, "{seeds} distinct seeds, fewer than {SEEDS_MIN}")
            }
        }
    }
}

/// The peak resident memory in KiB (`VmHWM`) in the text of
/// `/proc/self/status`; `None` without a readable one. The line's form is
/// proc(5)'s, `VmHWM:` and the size in kB, as procfs-core 0.18's
/// `Status::vmhwm` parses it.
#[must_use]
pub fn peak_kib(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse().ok())
}

/// The watchdog's rule: case `case`, running for `running`, is a hang past
/// `timeout`.
#[must_use]
pub fn hang(case: u64, running: Duration, timeout: Duration) -> Option<Failure> {
    (running > timeout).then_some(Failure::Hang {
        case,
        after: running,
    })
}

/// What the wrapper measured of one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseRun {
    /// The case.
    pub case: u64,
    /// `exercise` panicked.
    pub panicked: bool,
    /// The errors it reported, when it didn't panic.
    pub errors: usize,
    /// How long it ran.
    pub took: Duration,
    /// Resetting the peak before the case (`clear_refs`), with the system's
    /// error.
    pub reset: Result<(), String>,
    /// The peak after it, in KiB.
    pub peak_kib: Option<u64>,
}

/// A run's cases so far, judged against the rules.
#[derive(Debug, Clone)]
pub struct Tally {
    seeds: usize,
    base_kib: Option<u64>,
    cases: u64,
    with_errors: u64,
    failures: Vec<Failure>,
    largest: Vec<(u64, u64)>,
    slowest: Vec<(Duration, u64)>,
}

impl Tally {
    /// A run over `seeds` distinct seeds. Once they were loaded the peak
    /// was reset (`reset`; an error, with the system's text, is a failure)
    /// and read back as the base the cases are judged against, `base_kib`
    /// (`None`: unreadable, a failure).
    #[must_use]
    pub fn new(seeds: usize, reset: Result<(), String>, base_kib: Option<u64>) -> Self {
        let failures = match reset {
            Ok(()) => Vec::new(),
            Err(error) => vec![Failure::ClearRefs { case: None, error }],
        };
        Self {
            seeds,
            base_kib,
            cases: 0,
            with_errors: 0,
            failures,
            largest: Vec::new(),
            slowest: Vec::new(),
        }
    }

    /// One case's measurements, and any failure they show.
    pub fn record(&mut self, run: CaseRun) {
        let CaseRun {
            case,
            panicked,
            errors,
            took,
            reset,
            peak_kib,
        } = run;
        self.cases += 1;
        self.with_errors += u64::from(errors > 0);
        if panicked {
            self.failures.push(Failure::Panic { case });
        }
        if let Err(error) = reset {
            self.failures.push(Failure::ClearRefs {
                case: Some(case),
                error,
            });
        }
        match (peak_kib, self.base_kib) {
            (None, _) => self
                .failures
                .push(Failure::PeakUnreadable { case: Some(case) }),
            (Some(peak), Some(base)) => {
                let over_kib = peak.saturating_sub(base);
                if over_kib > MEMORY_OVER_BASE_MAX_KIB {
                    self.failures.push(Failure::Memory { case, over_kib });
                }
                top5(&mut self.largest, over_kib, case);
            }
            // No base to judge against: `finish` fails the run for it.
            (Some(_), None) => {}
        }
        // The watchdog stops a case that never returns; one that returns
        // just past the timeout, between two of its looks, fails here.
        if let Some(hung) = hang(case, took, TIMEOUT) {
            self.failures.push(hung);
        }
        top5(&mut self.slowest, took, case);
    }

    /// The run's verdict: every case's failures, then the run's own (no
    /// base, too few cases or seeds).
    #[must_use]
    pub fn finish(mut self) -> Report {
        if self.base_kib.is_none() {
            self.failures
                .insert(0, Failure::PeakUnreadable { case: None });
        }
        if self.cases < CASES_MIN {
            self.failures
                .push(Failure::TooFewCases { cases: self.cases });
        }
        if self.seeds < SEEDS_MIN {
            self.failures
                .push(Failure::TooFewSeeds { seeds: self.seeds });
        }
        Report {
            seeds: self.seeds,
            base_kib: self.base_kib,
            cases: self.cases,
            with_errors: self.with_errors,
            failures: self.failures,
            largest: self.largest,
            slowest: self.slowest,
        }
    }
}

/// `list` joined by commas, or "none".
fn or_none(list: &[String]) -> String {
    if list.is_empty() {
        "none".to_owned()
    } else {
        list.join(", ")
    }
}

/// Keep the five largest `(value, case)` pairs, largest first.
fn top5<T: Ord>(list: &mut Vec<(T, u64)>, value: T, case: u64) {
    list.push((value, case));
    list.sort_by(|a, b| b.cmp(a));
    list.truncate(5);
}

/// A run's verdict and figures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Report {
    /// Distinct seeds.
    pub seeds: usize,
    /// The seeds' base memory, in KiB.
    pub base_kib: Option<u64>,
    /// Cases run.
    pub cases: u64,
    /// Cases that reported an error: how often the mutations reach the
    /// reader's checks.
    pub with_errors: u64,
    /// Why the run fails, in the order found; empty when it passes.
    pub failures: Vec<Failure>,
    /// The five highest peaks above the base, in KiB, with their cases.
    pub largest: Vec<(u64, u64)>,
    /// The five slowest cases.
    pub slowest: Vec<(Duration, u64)>,
}

impl Report {
    /// Failures named in full; the rest are counted.
    pub const SHOWN: usize = 20;

    /// No rule failed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for failure in self.failures.iter().take(Self::SHOWN) {
            writeln!(f, "fuzz: FAIL {failure}")?;
        }
        if self.failures.len() > Self::SHOWN {
            writeln!(
                f,
                "fuzz: {} more failures",
                self.failures.len() - Self::SHOWN
            )?;
        }
        let base = self
            .base_kib
            .map_or_else(|| "unreadable".to_owned(), |k| format!("{k} KiB"));
        writeln!(
            f,
            "fuzz: {} cases over {} seeds, {} with errors reported, base memory {base}",
            self.cases, self.seeds, self.with_errors
        )?;
        let largest: Vec<String> = self
            .largest
            .iter()
            .map(|(k, i)| format!("case {i} {k} KiB"))
            .collect();
        writeln!(
            f,
            "fuzz: highest peaks above the base: {}",
            or_none(&largest)
        )?;
        let slowest: Vec<String> = self
            .slowest
            .iter()
            .map(|(d, i)| format!("case {i} {d:?}"))
            .collect();
        writeln!(f, "fuzz: slowest: {}", or_none(&slowest))?;
        write!(
            f,
            "fuzz: {}",
            if self.passed() { "passed" } else { "FAILED" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted;

    /// A case that went well: reset, read, no error.
    fn fine(case: u64, peak_kib: u64) -> CaseRun {
        CaseRun {
            case,
            panicked: false,
            errors: 0,
            took: Duration::from_millis(5),
            reset: Ok(()),
            peak_kib: Some(peak_kib),
        }
    }

    /// A tally of enough cases over enough seeds, all fine, from a base of
    /// 1000 KiB.
    fn enough() -> Tally {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), Some(1000));
        for i in 0..CASES_MIN {
            t.record(fine(i, 1000));
        }
        t
    }

    #[test]
    fn enough_fine_cases_over_enough_seeds_pass() {
        let report = enough().finish();
        assert_eq!(report.failures, []);
        assert!(report.passed());
        assert_eq!((report.cases, report.seeds), (CASES_MIN, SEEDS_MIN));
    }

    #[test]
    fn a_panic_fails_the_run() {
        let mut t = enough();
        t.record(CaseRun {
            panicked: true,
            ..fine(CASES_MIN, 1000)
        });
        assert_eq!(t.finish().failures, [Failure::Panic { case: CASES_MIN }]);
    }

    #[test]
    fn a_case_past_the_timeout_is_a_hang() {
        assert_eq!(hang(4, TIMEOUT, TIMEOUT), None);
        let after = TIMEOUT + Duration::from_millis(1);
        assert_eq!(
            hang(4, after, TIMEOUT),
            Some(Failure::Hang { case: 4, after })
        );
    }

    /// A case that returns past the timeout, between two of the
    /// watchdog's looks, still fails the run as a hang.
    #[test]
    fn a_case_that_returned_past_the_timeout_fails_the_run() {
        let mut t = enough();
        t.record(CaseRun {
            took: TIMEOUT,
            ..fine(1, 1000)
        });
        let after = TIMEOUT + Duration::from_millis(1);
        t.record(CaseRun {
            took: after,
            ..fine(2, 1000)
        });
        assert_eq!(t.finish().failures, [Failure::Hang { case: 2, after }]);
    }

    /// The bounds are the journal spec's (Testing Strategy).
    #[test]
    fn the_bounds_are_the_specs() {
        assert_eq!(MEMORY_OVER_BASE_MAX_KIB * 1024, 16 * 1024 * 1024);
        assert_eq!(ADDRESS_SPACE, 3 * 1024 * 1024 * 1024);
        assert_eq!(TIMEOUT, Duration::from_secs(10));
        assert_eq!((CASES_MIN, SEEDS_MIN), (9000, 8));
    }

    /// The bound is on the peak above the base: at it passes, a KiB more
    /// fails, and a peak below the base (memory given back) passes.
    #[test]
    fn a_case_over_the_memory_bound_fails_the_run() {
        let mut t = enough();
        t.record(fine(1, 1000 + MEMORY_OVER_BASE_MAX_KIB));
        t.record(fine(2, 500));
        t.record(fine(3, 1001 + MEMORY_OVER_BASE_MAX_KIB));
        assert_eq!(
            t.finish().failures,
            [Failure::Memory {
                case: 3,
                over_kib: MEMORY_OVER_BASE_MAX_KIB + 1
            }]
        );
    }

    #[test]
    fn an_unreadable_peak_fails_the_run() {
        let mut t = enough();
        t.record(CaseRun {
            peak_kib: None,
            ..fine(7, 1000)
        });
        assert_eq!(
            t.finish().failures,
            [Failure::PeakUnreadable { case: Some(7) }]
        );
    }

    #[test]
    fn an_unreadable_base_fails_the_run() {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), None);
        for i in 0..CASES_MIN {
            t.record(fine(i, 1000));
        }
        assert_eq!(
            t.finish().failures,
            [Failure::PeakUnreadable { case: None }]
        );
    }

    /// The base is the resident size once the seeds are loaded: the peak
    /// is reset first, and a failed reset fails the run.
    #[test]
    fn a_failed_clear_refs_write_before_the_cases_fails_the_run() {
        let mut t = Tally::new(SEEDS_MIN, Err("denied".into()), Some(1000));
        for i in 0..CASES_MIN {
            t.record(fine(i, 1000));
        }
        assert_eq!(
            t.finish().failures,
            [Failure::ClearRefs {
                case: None,
                error: "denied".into()
            }]
        );
    }

    #[test]
    fn a_failed_clear_refs_write_fails_the_run() {
        let mut t = enough();
        t.record(CaseRun {
            reset: Err("denied".into()),
            ..fine(8, 1000)
        });
        assert_eq!(
            t.finish().failures,
            [Failure::ClearRefs {
                case: Some(8),
                error: "denied".into()
            }]
        );
    }

    #[test]
    fn fewer_than_the_minimum_cases_fail_the_run() {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), Some(1000));
        for i in 0..CASES_MIN - 1 {
            t.record(fine(i, 1000));
        }
        assert_eq!(
            t.finish().failures,
            [Failure::TooFewCases {
                cases: CASES_MIN - 1
            }]
        );
    }

    #[test]
    fn fewer_than_the_minimum_seeds_fail_the_run() {
        let mut t = Tally::new(SEEDS_MIN - 1, Ok(()), Some(1000));
        for i in 0..CASES_MIN {
            t.record(fine(i, 1000));
        }
        assert_eq!(
            t.finish().failures,
            [Failure::TooFewSeeds {
                seeds: SEEDS_MIN - 1
            }]
        );
    }

    #[test]
    fn the_peak_is_vm_hwm_in_kib() {
        let status = "Name:\tfuzz\nVmPeak:\t  99999 kB\nVmHWM:\t   12345 kB\nVmRSS:\t 1 kB\n";
        assert_eq!(peak_kib(status), Some(12345));
        assert_eq!(peak_kib("VmRSS:\t 1 kB\n"), None);
        assert_eq!(peak_kib("VmHWM:\t lots kB\n"), None);
        assert_eq!(peak_kib(""), None);
    }
    /// The largest and slowest five, largest first; peaks above the base.
    #[test]
    fn the_report_keeps_the_five_largest_and_slowest() {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), Some(100));
        for i in 0..7 {
            t.record(CaseRun {
                took: Duration::from_millis(10 * i),
                errors: usize::from(i % 2 == 0),
                ..fine(i, 100 + 3 * i)
            });
        }
        let r = t.finish();
        assert_eq!(r.largest, [(18, 6), (15, 5), (12, 4), (9, 3), (6, 2)]);
        let slowest: Vec<u64> = r.slowest.iter().map(|&(_, i)| i).collect();
        assert_eq!(slowest, [6, 5, 4, 3, 2]);
        assert_eq!(r.slowest[0].0, Duration::from_millis(60));
        assert_eq!((r.cases, r.with_errors), (7, 4));
    }

    #[test]
    fn each_failure_says_what_failed() {
        let after = Duration::from_secs(11);
        let shown: Vec<String> = [
            Failure::Panic { case: 1 },
            Failure::Hang { case: 2, after },
            Failure::Memory {
                case: 3,
                over_kib: 20000,
            },
            Failure::PeakUnreadable { case: Some(4) },
            Failure::PeakUnreadable { case: None },
            Failure::ClearRefs {
                case: Some(5),
                error: "denied".into(),
            },
            Failure::ClearRefs {
                case: None,
                error: "denied".into(),
            },
            Failure::TooFewCases { cases: 6 },
            Failure::TooFewSeeds { seeds: 7 },
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            shown,
            [
                "case 1 panicked",
                "case 2 hung: still running after 11s",
                "case 3 peaked 20000 KiB above the seeds' base, over 16384 KiB",
                "case 4: no peak memory (VmHWM) to read",
                "no base memory (VmHWM) to read before the cases",
                "case 5: the peak wasn't reset (clear_refs: denied)",
                "the peak wasn't reset before the cases (clear_refs: denied)",
                "6 cases run, fewer than 9000",
                "7 distinct seeds, fewer than 8",
            ]
        );
    }

    #[test]
    fn the_report_names_its_failures_and_its_verdict() {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), Some(100));
        t.record(fine(3, 150));
        let text = t.finish().to_string();
        assert_eq!(
            text,
            "fuzz: FAIL 1 cases run, fewer than 9000\n\
             fuzz: 1 cases over 8 seeds, 0 with errors reported, base memory 100 KiB\n\
             fuzz: highest peaks above the base: case 3 50 KiB\n\
             fuzz: slowest: case 3 5ms\n\
             fuzz: FAILED"
        );
        let passed = enough().finish().to_string();
        assert!(passed.ends_with("\nfuzz: passed"), "{passed}");
        assert!(!passed.contains("FAIL"), "{passed}");
    }

    /// Exactly `Report::SHOWN` failures are all shown, with no count.
    #[test]
    fn the_report_shows_up_to_its_limit_without_a_count() {
        let mut t = enough();
        for i in 0..Report::SHOWN as u64 - 1 {
            t.record(CaseRun {
                panicked: true,
                ..fine(i, 1000)
            });
        }
        t.record(CaseRun {
            reset: Err("denied".into()),
            ..fine(99, 1000)
        });
        let r = t.finish();
        assert_eq!(r.failures.len(), Report::SHOWN);
        let text = r.to_string();
        assert_eq!(text.matches("fuzz: FAIL ").count(), Report::SHOWN);
        assert!(!text.contains("more failures"), "{text}");
        let none = Tally::new(SEEDS_MIN, Ok(()), None).finish().to_string();
        assert!(none.contains("above the base: none\n"), "{none}");
        assert!(none.contains("slowest: none\n"), "{none}");
    }

    /// Past `Report::SHOWN` failures the rest are counted, and a missing
    /// base is named.
    #[test]
    fn the_report_counts_the_failures_past_those_shown() {
        let mut t = Tally::new(SEEDS_MIN, Ok(()), None);
        for i in 0..CASES_MIN {
            t.record(CaseRun {
                panicked: true,
                ..fine(i, 1)
            });
        }
        let r = t.finish();
        assert_eq!(r.failures.len(), 1 + 9000);
        let text = r.to_string();
        assert_eq!(text.matches("fuzz: FAIL ").count(), Report::SHOWN);
        assert!(text.contains("\nfuzz: 8981 more failures\n"), "{text}");
        assert!(text.contains("base memory unreadable"), "{text}");
    }

    /// The crafted file's bytes with its arena covering every object, as
    /// `Crafted::file` writes them.
    fn written(c: &mut crafted::Crafted) -> Vec<u8> {
        c.set(crafted::H_ARENA_SIZE, c.b.len() as u64 - 272);
        c.b.clone()
    }

    /// A valid journal of `n` entries with its arena set, as journald
    /// leaves it: then `extra` preallocated zeros, inside the arena.
    fn journal_bytes(n: u64, extra: usize) -> Vec<u8> {
        let (mut c, _) = crafted::journal(n);
        let mut b = written(&mut c);
        b.resize(b.len() + extra, 0);
        let arena = b.len() as u64 - 272;
        b[H_ARENA_SIZE..H_ARENA_SIZE + 8].copy_from_slice(&arena.to_le_bytes());
        b
    }

    /// The objects, in order, end to end from the header, and the zeros
    /// past the last one cut with the arena.
    #[test]
    fn a_seed_is_its_objects_without_the_zeros_past_them() {
        let valid = journal_bytes(4, 0);
        let seed = Seed::parse(journal_bytes(4, 4096)).expect("a seed");
        assert_eq!(seed.bytes(), valid);
        assert_eq!(get(seed.bytes(), H_ARENA_SIZE), valid.len() as u64 - 272);
        // The hash table, the two arrays, the shared DATA object, and a
        // DATA object and an ENTRY per entry.
        assert_eq!(seed.objects().len(), 4 + 2 * 4);
        let mut next = 272;
        for &(o, size) in seed.objects() {
            assert_eq!(o, next);
            assert_eq!(get(&valid, o + 8), size as u64);
            next = (o + size).next_multiple_of(8);
        }
        assert_eq!(next, valid.len());
    }

    /// Bytes past the arena are cut even when they aren't zeros; a file
    /// whose arena ends at its last object keeps its arena size.
    #[test]
    fn a_seed_ends_at_its_arena() {
        let valid = journal_bytes(2, 0);
        let mut longer = valid.clone();
        longer.extend_from_slice(&[0xff; 64]);
        let seed = Seed::parse(longer).expect("a seed");
        assert_eq!(seed.bytes(), valid);
    }

    /// An object with a size below a header's ends the walk there.
    #[test]
    fn a_bad_object_size_ends_a_seed() {
        let mut b = journal_bytes(2, 0);
        let second = 272
            + usize::try_from(get(&b, 272 + 8))
                .expect("small")
                .next_multiple_of(8);
        put(&mut b, second + 8, 8);
        let seed = Seed::parse(b).expect("a seed");
        assert_eq!(seed.objects().len(), 1);
        assert_eq!(seed.bytes().len(), second);
        assert_eq!(get(seed.bytes(), H_ARENA_SIZE), second as u64 - 272);
    }

    /// An object size past memory ends the walk after that object.
    #[test]
    fn an_object_size_past_memory_ends_a_seed() {
        let mut b = journal_bytes(2, 0);
        put(&mut b, 272 + 8, u64::MAX);
        let seed = Seed::parse(b.clone()).expect("a seed");
        assert_eq!(seed.objects(), [(272, usize::MAX)]);
        assert_eq!(seed.bytes(), b);
    }

    /// An object of 16 bytes, a bare object header, is still an object;
    /// one whose objects end at the file's end keeps an arena claiming
    /// more, as it came.
    #[test]
    fn a_seed_keeps_its_smallest_objects_and_a_long_arena() {
        let mut c = crafted::Crafted::new();
        c.object(1, 0, &[]);
        c.object(1, 0, &[0; 8]);
        let mut b = written(&mut c);
        let seed = Seed::parse(b.clone()).expect("a seed");
        assert_eq!(seed.objects(), [(272, 16), (288, 24)]);
        put(&mut b, H_ARENA_SIZE, 4096);
        let seed = Seed::parse(b.clone()).expect("a seed");
        assert_eq!(seed.bytes(), b);
    }

    #[test]
    fn a_file_without_objects_is_no_seed() {
        assert_eq!(Seed::parse(crafted::Crafted::new().b), None);
        assert_eq!(Seed::parse(Vec::new()), None);
        let mut short = journal_bytes(1, 0);
        short.truncate(272 + 15);
        assert_eq!(Seed::parse(short), None);
    }

    #[test]
    fn repeated_seeds_count_once() {
        let a = Seed::parse(journal_bytes(2, 0)).expect("a seed");
        let b = Seed::parse(journal_bytes(3, 0)).expect("a seed");
        let kept = distinct(vec![a.clone(), b.clone(), a.clone(), b.clone()]);
        assert_eq!(kept, [a, b]);
    }

    #[test]
    fn a_case_is_the_same_on_every_run() {
        let seeds = [
            Seed::parse(journal_bytes(5, 0)).expect("a seed"),
            Seed::parse(journal_bytes(9, 0)).expect("a seed"),
        ];
        assert_eq!(case(&seeds, 1, 5), case(&seeds, 1, 5));
        assert_ne!(case(&seeds, 1, 5).0, case(&seeds, 1, 6).0);
        assert_ne!(case(&seeds, 1, 5).0, case(&seeds, 2, 5).0);
        // Every release reads some case, and both seeds are mutated.
        let releases: HashSet<&str> = (0..100)
            .map(|i| case(&seeds, 1, i).1.version_id())
            .collect();
        assert_eq!(releases.len(), RELEASES.len());
        let lengths: HashSet<bool> = (0..100)
            .map(|i| case(&seeds, 1, i).0.len() > seeds[0].bytes().len())
            .collect();
        assert_eq!(lengths.len(), 2);
    }

    /// Each kind of value, as documented.
    #[test]
    fn interesting_values_are_those_corrupt_files_hold() {
        let seed = Seed::parse(journal_bytes(3, 0)).expect("a seed");
        let len = seed.bytes().len() as u64;
        let mut rng = fastrand::Rng::with_seed(7);
        let near = 1000;
        let fixed: Vec<u64> = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 11, 13, 14]
            .iter()
            .map(|&k| interesting(k, &mut rng, &seed, near))
            .collect();
        assert_eq!(
            fixed,
            [
                0,
                1,
                1008,
                992,
                u64::MAX,
                u64::MAX - 7,
                1 << 63,
                len,
                len - 8,
                len + 8,
                1000,
                2000,
                770 << 20
            ]
        );
        assert_eq!(interesting(2, &mut rng, &seed, u64::MAX), 7);
        assert_eq!(interesting(3, &mut rng, &seed, 0), u64::MAX - 7);
        let offsets: HashSet<u64> = seed.objects().iter().map(|&(o, _)| o as u64).collect();
        let picked: HashSet<u64> = (0..200)
            .map(|_| interesting(10, &mut rng, &seed, near))
            .collect();
        assert_eq!(picked, offsets);
        assert!((0..200).all(|_| u32::try_from(interesting(12, &mut rng, &seed, near)).is_ok()));
        let wide =
            (0..200).filter(|_| interesting(15, &mut rng, &seed, near) > u64::from(u32::MAX));
        assert!(wide.count() > 150);
    }

    /// Where `a` and `b`, of one length, differ: the first and last byte.
    fn changed(a: &[u8], b: &[u8]) -> Option<(usize, usize)> {
        assert_eq!(a.len(), b.len());
        let first = a.iter().zip(b).position(|(x, y)| x != y)?;
        let last = a.iter().zip(b).rposition(|(x, y)| x != y)?;
        Some((first, last))
    }

    /// Two seeds of different files, the first of eight entries, and kind
    /// `kind` applied once to the first for each of 300 rng seeds: the
    /// mutated bytes.
    fn mutated_once(kind: u8) -> (Seed, Vec<Vec<u8>>) {
        let seeds = [
            Seed::parse(journal_bytes(8, 0)).expect("a seed"),
            Seed::parse(journal_bytes(3, 0)).expect("a seed"),
        ];
        let out = (0..300)
            .map(|i| {
                let mut rng = fastrand::Rng::with_seed(i);
                let mut b = seeds[0].bytes().to_vec();
                mutate_once(kind, &mut rng, &seeds, &seeds[0], &mut b);
                b
            })
            .collect();
        (seeds[0].clone(), out)
    }

    /// An object field: at most 8 bytes, 8-byte aligned or one half of
    /// that, within one of an object's first 64 words.
    #[test]
    fn an_object_field_mutation_changes_one_field_of_an_object() {
        for kind in 0..=2 {
            let (seed, cases) = mutated_once(kind);
            let (mut halves, mut hits, mut deepest) = (0, 0, 0);
            for b in &cases {
                let Some((first, last)) = changed(seed.bytes(), b) else {
                    continue;
                };
                hits += 1;
                let word = first & !7;
                assert!(last < word + 8, "{first}..{last}");
                halves += usize::from(last < word + 4 || first >= word + 4);
                let (o, _) = seed
                    .objects()
                    .iter()
                    .copied()
                    .find(|&(o, size)| word >= o && word < o + 8 * (size / 8).clamp(1, 64))
                    .unwrap_or_else(|| panic!("{word} isn't in an object's first words"));
                deepest = deepest.max((word - o) / 8);
            }
            assert!(hits > 200, "{hits}");
            // The hash table and the arrays reach past their first words.
            assert!(deepest > 8, "{deepest}");
            assert!(halves > 20, "{halves}");
        }
    }

    /// An object's type or flags byte, set below 8.
    #[test]
    fn a_type_mutation_changes_an_objects_type_or_flags() {
        let (seed, cases) = mutated_once(3);
        let mut positions = HashSet::new();
        for b in &cases {
            let Some((first, last)) = changed(seed.bytes(), b) else {
                continue;
            };
            assert_eq!(first, last);
            assert!(b[first] < 8);
            let o = seed
                .objects()
                .iter()
                .find(|&&(o, _)| o == first || o + 1 == first);
            assert!(o.is_some(), "{first}");
            positions.insert(first - o.expect("an object").0);
        }
        assert_eq!(positions, HashSet::from([0, 1]));
    }

    /// A header field after the signature's 88 bytes, as a whole word.
    #[test]
    fn a_header_mutation_changes_one_header_field() {
        let (seed, cases) = mutated_once(4);
        let mut fields = HashSet::new();
        for b in &cases {
            let Some((first, last)) = changed(seed.bytes(), b) else {
                continue;
            };
            let word = first & !7;
            assert!(word >= H_HEADER_SIZE && last < word + 8 && last < HEADER_MIN);
            fields.insert(word);
        }
        assert!(fields.len() > 15, "{fields:?}");
    }

    /// A cut keeps a prefix of at least 200 bytes.
    #[test]
    fn a_cut_keeps_a_prefix() {
        let (seed, cases) = mutated_once(5);
        let lengths: HashSet<usize> = cases.iter().map(Vec::len).collect();
        for b in &cases {
            assert!(b.len() >= CUT_MIN);
            assert_eq!(b[..], seed.bytes()[..b.len()]);
        }
        assert!(lengths.len() > 200);
        assert!(lengths.iter().any(|&l| l < 272));
    }

    /// A splice copies a run of some seed's bytes over an 8-aligned place.
    #[test]
    fn a_splice_copies_a_run_of_a_seed() {
        let (seed, cases) = mutated_once(6);
        let other = Seed::parse(journal_bytes(3, 0)).expect("a seed");
        let (mut hits, mut furthest) = (0, 0);
        for b in &cases {
            let Some((first, last)) = changed(seed.bytes(), b) else {
                continue;
            };
            hits += 1;
            furthest = furthest.max(first);
            assert!(last - first < 4096);
            let run = &b[first..=last];
            assert!(
                [seed.bytes(), other.bytes()]
                    .iter()
                    .any(|s| s.windows(run.len()).any(|w| w == run)),
                "{first}..{last}"
            );
        }
        assert!(hits > 100, "{hits}");
        // It lands anywhere in the file, not only near its start.
        assert!(furthest > seed.bytes().len() / 2, "{furthest}");
    }

    /// Random bytes: at most 16 changed, anywhere.
    #[test]
    fn random_bytes_change_at_most_sixteen() {
        let (seed, cases) = mutated_once(7);
        let mut most = 0;
        for b in &cases {
            let n = seed.bytes().iter().zip(b).filter(|(x, y)| x != y).count();
            assert!((1..=16).contains(&n), "{n}");
            most = most.max(n);
        }
        assert!(most > 8, "{most}");
        // Flipped, not set: a byte of all ones changes too.
        let mut ones = vec![0xff; 4096];
        mutate_once(
            7,
            &mut fastrand::Rng::with_seed(3),
            std::slice::from_ref(&seed),
            &seed,
            &mut ones,
        );
        assert!(ones.iter().any(|&x| x != 0xff));
    }

    /// A file already cut below a header isn't mutated again; one of a
    /// header's length still is.
    #[test]
    fn a_file_shorter_than_a_header_is_left_alone() {
        let seed = Seed::parse(journal_bytes(3, 0)).expect("a seed");
        let seeds = [seed.clone()];
        for kind in 0..8 {
            let mut b = seed.bytes()[..271].to_vec();
            for i in 0..50 {
                mutate_once(
                    kind,
                    &mut fastrand::Rng::with_seed(i),
                    &seeds,
                    &seed,
                    &mut b,
                );
            }
            assert_eq!(b, seed.bytes()[..271], "kind {kind}");
        }
        let mut b = seed.bytes()[..272].to_vec();
        for i in 0..50 {
            mutate_once(4, &mut fastrand::Rng::with_seed(i), &seeds, &seed, &mut b);
        }
        assert_ne!(b, seed.bytes()[..272]);
    }

    /// One to four mutations of one of the seeds.
    #[test]
    fn a_case_is_one_to_four_mutations_of_a_seed() {
        let seeds = [
            Seed::parse(journal_bytes(8, 0)).expect("a seed"),
            Seed::parse(journal_bytes(20, 0)).expect("a seed"),
        ];
        // A mutation can write a value a field holds already (`near`), so
        // a few cases equal their seed.
        let (mut from, mut same) = ([0, 0], 0);
        for i in 0..200 {
            let b = mutate(&mut fastrand::Rng::with_seed(i), &seeds);
            let k = usize::from(b.len() > seeds[0].bytes().len());
            from[k] += 1;
            same += usize::from(b == seeds[k].bytes());
        }
        assert!(from.iter().all(|&n| n > 50), "{from:?}");
        assert!(same < 10, "{same}");
    }

    /// What an 80-entry file gives each read: decode-all its 80 entries;
    /// every entry's tail (50), the 20 before its oldest and the 20 after
    /// it; a follow from an empty end (80); the other two queries match
    /// nothing; the grown follow's drains read the 40 the halved count
    /// left out.
    #[test]
    fn a_valid_file_reads_every_entry_without_errors() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("case.journal");
        let bytes = journal_bytes(80, 0);
        let source = Source::Files(vec![path.clone()]);
        std::fs::write(&path, &bytes).expect("write");
        let release = &RELEASES[0];
        let journal = Journal::open(source.clone(), release).expect("opens");
        let limits = Limits::default();
        let ok = |records| Reads { errors: 0, records };
        assert_eq!(read_files(&journal), ok(80));
        let [every, unit, fields] = queries(release, &source).expect("queries");
        assert_eq!(pages(&journal, &every, limits), ok(90));
        assert_eq!(pages(&journal, &unit, limits), ok(0));
        assert_eq!(pages(&journal, &fields, limits), ok(0));
        assert_eq!(followed(&journal, every, limits).expect("follows"), ok(80));
        assert_eq!(followed(&journal, unit, limits).expect("follows"), ok(0));
        assert_eq!(
            grown(&path, &bytes, &journal, limits).expect("grows"),
            ok(40)
        );
        for release in RELEASES {
            assert_eq!(
                exercise(&path, &bytes, release).expect("exercised"),
                ok(80 + 90 + 80 + 40)
            );
        }
        assert_eq!(std::fs::read(&path).expect("read"), bytes);
    }

    #[test]
    fn reads_add_up() {
        let mut r = Reads {
            errors: 1,
            records: 2,
        };
        r += Reads {
            errors: 3,
            records: 4,
        };
        assert_eq!(
            r,
            Reads {
                errors: 4,
                records: 6
            }
        );
        let errors = ReadErrors {
            omitted: 3,
            ..ReadErrors::default()
        };
        assert_eq!(
            Reads::of(5, &errors),
            Reads {
                errors: 3,
                records: 5
            }
        );
    }

    /// A corrupt file's errors are counted by each read that meets them,
    /// and the reads go on.
    #[test]
    fn a_corrupt_files_errors_are_counted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("case.journal");
        let release = &RELEASES[0];
        let limits = Limits::default();
        let journal_of = |bytes: &[u8]| {
            std::fs::write(&path, bytes).expect("write");
            Journal::open(Source::Files(vec![path.clone()]), release).expect("opens")
        };
        // A message's stored hash wrong: hash-check alone reports it.
        let (mut c, layout) = crafted::journal(30);
        c.set(layout.messages[3] + crafted::D_HASH, 1);
        let bad_hash = written(&mut c);
        let j = journal_of(&bad_hash);
        assert_eq!(
            read_files(&j),
            Reads {
                errors: 1,
                records: 30
            }
        );
        assert_eq!(pages(&j, &tools::every_entry(), limits).errors, 0);
        // A message's DATA object unreadable: decode-all, the pages and
        // the follows report it.
        let (mut c, layout) = crafted::journal(30);
        c.set(layout.messages[29] + crafted::O_SIZE, 1 << 40);
        let bad_data = written(&mut c);
        let j = journal_of(&bad_data);
        let decoded = read_files(&j);
        assert_eq!(decoded.records, 30);
        assert!(decoded.errors >= 1, "{decoded:?}");
        let paged = pages(&j, &tools::every_entry(), limits);
        assert!(paged.errors >= 1, "{paged:?}");
        let follow = followed(&j, tools::every_entry(), limits).expect("follows");
        assert!(follow.errors >= 1, "{follow:?}");
        let whole = exercise(&path, &bad_data, release).expect("exercised");
        assert!(
            whole.errors >= decoded.errors + paged.errors + follow.errors,
            "{whole:?}"
        );
        // An entry's slot in the main array zeroed, and an entry's object
        // cut: decode-all counts each.
        let (mut c, layout) = crafted::journal(30);
        c.set(layout.main_array + crafted::A_ITEMS + 8 * 5, 1);
        c.set(layout.entries[9] + crafted::O_SIZE, 8);
        let bad_list = written(&mut c);
        let j = journal_of(&bad_list);
        let listed = read_files(&j);
        assert!(listed.errors >= 2 && listed.records < 30, "{listed:?}");
    }

    /// A case file that can't be written is the harness's error.
    #[test]
    fn an_unwritable_case_file_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("gone").join("case.journal");
        let failed = exercise(&missing, &journal_bytes(2, 0), &RELEASES[0]);
        assert!(matches!(failed, Err(ExerciseError::Write(_))), "{failed:?}");
    }
}
