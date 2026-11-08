//! A mutation harness for the reader that runs on the pinned stable
//! toolchain: real journal files as seeds, mutated, cut and spliced, then
//! read the way hash-check, decode-all and tail read them, each case under
//! a time bound and the whole run under an address-space bound.
//!
//! Why not cargo-fuzz (libFuzzer): it needs a nightly compiler, and the
//! toolchain is pinned to stable. Why not proptest or bolero: on stable
//! neither is coverage-guided either, and their strategies generate
//! values rather than mutate a large binary seed; what finds bugs here is
//! a valid journal with one field wrong, which a mutator over real files
//! produces directly. Why not afl.rs (cargo-afl), which mutates seed
//! files with coverage feedback and is the natural next step: it needs
//! the AFL++ toolchain built on the host and runs as its own long-lived
//! process outside `cargo test`; this harness is self-contained and each
//! case reproduces from two numbers. Not tried; FINDINGS lists it.
//! Randomness comes from fastrand (already in the tree
//! through tempfile), seeded per case, so case `i` of seed `s` is the same
//! file on every run: `fuzz --seed s --start i --cases 1 --keep DIR`
//! writes it out.

use crate::query::{self, Follow, Group, Journal, Release};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    pub seeds: Vec<PathBuf>,
    pub seed: u64,
    pub start: u64,
    pub cases: u64,
    /// A case running longer is a hang: the case is saved and the run
    /// stops.
    pub timeout: Duration,
    /// RLIMIT_AS for the whole run: an allocation past it aborts the run,
    /// and `<work>/current` names the case.
    pub memory_limit: u64,
    pub work: PathBuf,
    /// Also write each case to this directory.
    pub keep: Option<PathBuf>,
}

/// A seed file: its bytes up to its last object, and each object's offset
/// and size.
struct Seed {
    bytes: Vec<u8>,
    objects: Vec<(usize, usize)>,
}

impl Seed {
    fn load(path: &Path) -> Result<Self> {
        let mut bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let header = usize::try_from(get(&bytes, 88))?;
        let end = usize::try_from(get(&bytes, 88).saturating_add(get(&bytes, 96)))?;
        bytes.truncate(end);
        // Objects follow each other, 8-byte aligned, from the header's end.
        let mut objects = Vec::new();
        let mut at = header;
        while at + 16 <= bytes.len() {
            let size = usize::try_from(get(&bytes, at + 8))?;
            if size < 16 {
                break;
            }
            objects.push((at, size));
            // The size comes from the file: a corrupt seed ends the walk.
            match at
                .checked_add(size)
                .and_then(|e| e.checked_next_multiple_of(8))
            {
                Some(next) => at = next,
                None => break,
            }
        }
        // The preallocated zeros past the last object only slow each case
        // down: cut them, and the arena with them.
        if at < bytes.len() {
            bytes.truncate(at);
            put(&mut bytes, 96, (at - header) as u64);
        }
        Ok(Self { bytes, objects })
    }
}

/// Values a corrupt or half-written file holds: zero, sizes and offsets
/// just off the real ones, and values near overflow.
fn interesting(rng: &mut fastrand::Rng, seed: &Seed, near: u64) -> u64 {
    let len = seed.bytes.len() as u64;
    match rng.u8(..16) {
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
        10 => seed
            .objects
            .get(rng.usize(..seed.objects.len().max(1)))
            .map_or(0, |&(o, _)| o as u64),
        11 => near,
        12 => u64::from(rng.u32(..)),
        13 => near.wrapping_mul(2),
        14 => 770 << 20,
        _ => rng.u64(..),
    }
}

fn put(b: &mut [u8], at: usize, v: u64) {
    if let Some(s) = b.get_mut(at..at + 8) {
        s.copy_from_slice(&v.to_le_bytes());
    }
}

fn get(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// One to four mutations of a seed, chosen by `rng`.
fn mutate(rng: &mut fastrand::Rng, seeds: &[Seed]) -> Vec<u8> {
    let seed = &seeds[rng.usize(..seeds.len())];
    let mut b = seed.bytes.clone();
    for _ in 0..rng.usize(1..=4) {
        if b.len() < 272 {
            break;
        }
        match rng.u8(..8) {
            // A field of an object: its header (type, size, links,
            // counts) or one of its first items, 8 bytes or, for compact
            // files' items, 4.
            0..=2 if !seed.objects.is_empty() => {
                let (o, size) = seed.objects[rng.usize(..seed.objects.len())];
                let at = o + 8 * rng.usize(..(size / 8).clamp(1, 64));
                let v = interesting(rng, seed, get(&b, at));
                if rng.bool() {
                    put(&mut b, at, v);
                } else if let Some(s) = b
                    .get_mut(at + 4 * rng.usize(..2)..)
                    .and_then(|s| s.get_mut(..4))
                {
                    s.copy_from_slice(&v.to_le_bytes()[..4]);
                }
            }
            // An object's type or flags byte (compression).
            3 if !seed.objects.is_empty() => {
                let (o, _) = seed.objects[rng.usize(..seed.objects.len())];
                let at = o + rng.usize(..2);
                if let Some(x) = b.get_mut(at) {
                    *x = rng.u8(..8);
                }
            }
            // A header field.
            4 => {
                let at = 88 + 8 * rng.usize(..23);
                let v = interesting(rng, seed, get(&b, at));
                put(&mut b, at, v);
            }
            // Cut the file short.
            5 => {
                let len = rng.usize(200..=b.len());
                b.truncate(len);
            }
            // Splice a run of bytes from any seed into this one.
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
                    let at = rng.usize(..b.len());
                    b[at] ^= rng.u8(1..);
                }
            }
        }
    }
    b
}

/// What `sb logs` and the test tools do with a file: every DATA object
/// through the hash table, every entry and its fields in file order, and
/// tail, a page back and newer for three filters. Returns the errors
/// reported.
fn exercise(path: &Path, release: Release) -> usize {
    let mut j = Journal::open(&[path.to_path_buf()]);
    let mut errors = j.errors.len();
    for f in &j.files {
        match f.all_data() {
            Ok(all) => {
                for (off, stored) in all {
                    match f.data_payload(off) {
                        Ok(p) => errors += usize::from(f.hash(&p) != stored),
                        Err(_) => errors += 1,
                    }
                }
            }
            Err(_) => errors += 1,
        }
        let mut offsets = Vec::new();
        for r in f.all_entries_rev() {
            match r {
                Ok(off) => offsets.push(off),
                Err(_) => errors += 1,
            }
        }
        // As decode-all reads, but each DATA object once: a bomb listed
        // by every entry would otherwise be a false hang of the harness.
        offsets.reverse();
        let mut seen = std::collections::HashSet::new();
        for (k, &off) in offsets.iter().enumerate() {
            let end = offsets.get(k + 1).copied().unwrap_or(u64::MAX);
            match f.entry_before(off, end) {
                Ok(e) => {
                    for item in e.items {
                        if seen.insert(item) {
                            errors += usize::from(f.data_payload(item).is_err());
                        }
                    }
                }
                Err(_) => errors += 1,
            }
        }
    }
    let filters: [Vec<Group>; 3] = [
        Vec::new(),
        query::unit_filter(release, "demo.service", true),
        vec![vec![
            vec![b"_UID=0".to_vec()],
            vec![b"PRIORITY=3".to_vec(), b"PRIORITY=5".to_vec()],
        ]],
    ];
    let mut followed = 0;
    for filter in &filters {
        let got = j.tail(filter, 50, None);
        if let Some(first) = got.first() {
            j.tail(filter, 20, Some(&first.location));
            // Following the file from its start (as a file that appears
            // while following is read), after the first entry shown; the
            // follow moves `last` on with each poll.
            let mut follow = Follow::new(Journal::open(&[]), Some(j.last_of(first)));
            follow.rescan(&[path.to_path_buf()]);
            followed += polls(&mut follow, filter);
        }
        // Following from inside the file, as after a tail whose open
        // counted fewer entries than the file has now.
        let mut from = Journal::open(&[path.to_path_buf()]);
        for f in &mut from.files {
            f.header.n_entries /= 2;
        }
        let last = got.first().map(|r| j.last_of(r));
        followed += polls(&mut Follow::new(from, last), filter);
    }
    errors + j.errors.len() + followed
}

/// Three polls of a follow (a failed read waits one wake, then is skipped
/// or reported), and the errors they reported.
fn polls(follow: &mut Follow, filter: &[Group]) -> usize {
    for _ in 0..3 {
        follow.poll(filter);
    }
    follow.errors.len()
}

/// The process's peak resident memory in KiB (VmHWM), since the last
/// `reset_peak`.
fn peak_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

/// Reset the peak to the current resident size, so each case's peak is
/// its own (proc(5), /proc/pid/clear_refs: "5"). Why not getrusage or
/// sysinfo: rustix 1.1 has no getrusage, sysinfo 0.40 has no peak, and
/// neither resets per case.
fn reset_peak() {
    let _ = std::fs::write("/proc/self/clear_refs", "5");
}

/// Keep the five largest `(value, case)` pairs.
fn top5<T: Ord>(list: &mut Vec<(T, u64)>, value: T, case: u64) {
    list.push((value, case));
    list.sort_by(|a, b| b.cmp(a));
    list.truncate(5);
}

pub fn run(opts: &Options) -> Result<()> {
    let seeds: Vec<Seed> = opts
        .seeds
        .iter()
        .map(|p| Seed::load(p))
        .collect::<Result<_>>()?;
    anyhow::ensure!(!seeds.is_empty(), "no seed files");
    std::fs::create_dir_all(&opts.work)?;
    // https://docs.rs/rustix/1.1.6/rustix/process/fn.setrlimit.html
    rustix::process::setrlimit(
        rustix::process::Resource::As,
        rustix::process::Rlimit {
            current: Some(opts.memory_limit),
            maximum: Some(opts.memory_limit),
        },
    )
    .context("setrlimit RLIMIT_AS")?;

    // The watchdog: a case past the timeout is reported and ends the run
    // (a hung read can't be cancelled from outside the thread).
    let case = Arc::new(AtomicU64::new(u64::MAX));
    let started = Arc::new(AtomicU64::new(0));
    let epoch = Instant::now();
    {
        let (case, started, timeout) = (case.clone(), started.clone(), opts.timeout);
        let work = opts.work.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let c = case.load(Ordering::SeqCst);
                let since = epoch
                    .elapsed()
                    .saturating_sub(Duration::from_millis(started.load(Ordering::SeqCst)));
                if c != u64::MAX && since > timeout {
                    eprintln!(
                        "fuzz: HANG case {c} ran over {timeout:?}; the file is {}",
                        work.join("case.journal").display()
                    );
                    std::process::exit(3);
                }
            }
        });
    }

    let path = opts.work.join("case.journal");
    let current = opts.work.join("current");
    let (mut panics, mut with_errors) = (0u64, 0u64);
    let mut slowest: Vec<(Duration, u64)> = Vec::new();
    let mut largest: Vec<(u64, u64)> = Vec::new();
    let base = peak_kib();
    let releases = query::RELEASES;
    let run_start = Instant::now();
    for i in opts.start..opts.start + opts.cases {
        let mut rng = fastrand::Rng::with_seed(opts.seed ^ i.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let bytes = mutate(&mut rng, &seeds);
        let release = releases[rng.usize(..releases.len())];
        std::fs::write(&path, &bytes)?;
        if let Some(keep) = &opts.keep {
            std::fs::create_dir_all(keep)?;
            std::fs::write(keep.join(format!("case-{i}.journal")), &bytes)?;
        }
        std::fs::write(&current, i.to_string())?;
        reset_peak();
        let t = Instant::now();
        started.store(
            u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
        case.store(i, Ordering::SeqCst);
        // Going on after a panic is sound here: each case rebuilds all its
        // state, and any panic fails the run at the end.
        let outcome = std::panic::catch_unwind(|| exercise(&path, release));
        case.store(u64::MAX, Ordering::SeqCst);
        let took = t.elapsed();
        match outcome {
            Ok(errors) => with_errors += u64::from(errors > 0),
            Err(_) => {
                panics += 1;
                let saved = opts.work.join(format!("panic-{i}.journal"));
                std::fs::write(&saved, &bytes)?;
                eprintln!("fuzz: PANIC case {i}; saved {}", saved.display());
            }
        }
        top5(&mut slowest, took, i);
        top5(&mut largest, peak_kib(), i);
        if (i + 1 - opts.start).is_multiple_of(1000) {
            eprintln!(
                "fuzz: {} cases, {panics} panics, {:?}",
                i + 1 - opts.start,
                run_start.elapsed()
            );
        }
    }
    println!(
        "fuzz: seed {:#x}, cases {}..{}, {panics} panics, {with_errors} with errors reported, {:?}, resident before the cases {base} KiB",
        opts.seed,
        opts.start,
        opts.start + opts.cases,
        run_start.elapsed(),
    );
    let big: Vec<String> = largest
        .iter()
        .map(|(k, i)| format!("case {i} {k} KiB"))
        .collect();
    println!("fuzz: largest peak {}", big.join(", "));
    let slow: Vec<String> = slowest
        .iter()
        .map(|(d, i)| format!("case {i} {d:?}"))
        .collect();
    println!("fuzz: slowest {}", slow.join(", "));
    if panics > 0 {
        std::process::exit(1);
    }
    Ok(())
}
