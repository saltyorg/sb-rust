//! The fuzz harness (journal spec, Testing Strategy, "Fuzzing on stable"):
//! seed journal files mutated, cut and spliced, each case read as
//! hash-check, decode-all, the pages and a follow read it, under the
//! harness's own bounds. A thin wrapper: the mutations, the reads and the
//! failure rules are `sb_journal::testing::fuzz`, unit-tested there (plan,
//! "The `testing` feature"). Built with `--features testing`.
//!
//! The run fails (exit 1) on a panic, a case more than 16 MiB above the
//! seeds' base memory, a peak that can't be read or reset, fewer than
//! 9,000 cases or fewer than eight distinct seeds; a hang (a case past
//! 10 s) stops it at once (exit 3), and an allocation past the 3 GiB
//! address space aborts it, with `WORK/current` naming the case and
//! `WORK/current.journal` holding it. A run passes or fails without anyone
//! reading its output.
//!
//! The seeds are journal-remote 249's and 255's output, four writer
//! variants each, regenerated for each run and never committed:
//!
//! ```text
//! crates/journal/tests/seeds.sh DIR
//! systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% -p OOMPolicy=continue \
//!   cargo run -q -p sb-journal --features testing --example fuzz -- \
//!   --work DIR/work DIR/*.journal
//! ```
//!
//! Case `i` of run seed `s` is the same file on every run:
//! `--seed s --start i --cases 1 --keep DIR` writes it out (such a run
//! fails the case-count rule, by design).
//!
//! The arguments use clap's derive API
//! (<https://docs.rs/clap/4.6.7/clap/_derive/index.html>).

use anyhow::{Context, Result};
use clap::Parser;
use sb_journal::testing::fuzz::{self, CaseRun, Seed, Tally};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(about = "Mutate seed journal files and read each case, under the harness's bounds")]
struct Cli {
    /// Seed journal files: journal-remote's output of gen-export
    /// (crates/journal/tests/seeds.sh).
    #[arg(required = true)]
    seeds: Vec<PathBuf>,
    /// The run's seed: the same seed and case number give the same file.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// The first case.
    #[arg(long, default_value_t = 0)]
    start: u64,
    /// Cases to run; a run of fewer than the default (9,000, the spec's
    /// minimum) fails.
    #[arg(long, default_value_t = fuzz::CASES_MIN)]
    cases: u64,
    /// Where the cases are read from, and where the current case and any
    /// case that panicked are written.
    #[arg(long)]
    work: PathBuf,
    /// Also keep every case here, as `case-N.journal` (with --cases 1, to
    /// reproduce one; a whole run's cases fill it). Not the seeds'
    /// directory: a later `DIR/*.journal` would take them for seeds.
    #[arg(long)]
    keep: Option<PathBuf>,
}

/// The peak resident memory since the last reset, from
/// `/proc/self/status` (proc(5)); `None` when it can't be read.
fn peak_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| fuzz::peak_kib(&s))
}

/// Reset the peak to the current resident size, so each case's peak is its
/// own: proc(5), `/proc/pid/clear_refs`, "5". Why not getrusage or sysinfo:
/// rustix 1.1 has no getrusage, sysinfo 0.40 has no peak, and neither
/// resets per case. Why not the procfs crate (0.18's `Status::vmhwm` and
/// `Process::clear_refs(ClearRefs::PeakRSS)` do both): it isn't in the
/// tree, and these are a file read and a file write in a test-only tool.
fn reset_peak() -> Result<(), String> {
    std::fs::write("/proc/self/clear_refs", "5").map_err(|e| e.to_string())
}

/// The case running, and since when.
type Running = Arc<Mutex<Option<(u64, Instant)>>>;

/// The watchdog: a case past the timeout ends the run (a hung read can't
/// be cancelled from outside its thread).
fn watchdog(running: Running, saved: PathBuf) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let now = *running.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((case, since)) = now
                && let Some(failure) = fuzz::hang(case, since.elapsed(), fuzz::TIMEOUT)
            {
                eprintln!("fuzz: FAIL {failure}; the case is {}", saved.display());
                std::process::exit(3);
            }
        }
    });
}

fn load(paths: &[PathBuf]) -> Result<Vec<Seed>> {
    let mut seeds = Vec::new();
    for p in paths {
        let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
        let seed =
            Seed::parse(bytes).with_context(|| format!("{}: no journal objects", p.display()))?;
        seeds.push(seed);
    }
    Ok(fuzz::distinct(seeds))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let seeds = load(&cli.seeds)?;
    std::fs::create_dir_all(&cli.work)?;
    if let Some(keep) = &cli.keep {
        std::fs::create_dir_all(keep)?;
    }
    // https://docs.rs/rustix/1.1.6/rustix/process/fn.setrlimit.html
    rustix::process::setrlimit(
        rustix::process::Resource::As,
        rustix::process::Rlimit {
            current: Some(fuzz::ADDRESS_SPACE),
            maximum: Some(fuzz::ADDRESS_SPACE),
        },
    )
    .context("setrlimit RLIMIT_AS")?;

    let read = cli.work.join("case.journal");
    let saved = cli.work.join("current.journal");
    let current = cli.work.join("current");
    let running: Running = Arc::default();
    watchdog(Arc::clone(&running), saved.clone());
    // The base: the resident size with the seeds loaded and nothing else
    // read yet.
    let mut tally = Tally::new(seeds.len(), reset_peak(), peak_kib());
    let started = Instant::now();
    for i in cli.start..cli.start.saturating_add(cli.cases) {
        let (bytes, release) = fuzz::case(&seeds, cli.seed, i);
        write(&saved, &bytes)?;
        write(&current, i.to_string())?;
        if let Some(keep) = &cli.keep {
            write(&keep.join(format!("case-{i}.journal")), &bytes)?;
        }
        let reset = reset_peak();
        let t = Instant::now();
        *running.lock().unwrap_or_else(PoisonError::into_inner) = Some((i, t));
        // Going on after a panic is sound here: each case rebuilds all its
        // state, and any panic fails the run.
        let outcome = std::panic::catch_unwind(|| fuzz::exercise(&read, &bytes, &release));
        *running.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let took = t.elapsed();
        let (panicked, errors) = if let Ok(errors) = outcome {
            (false, errors.with_context(|| format!("case {i}"))?.errors)
        } else {
            let kept = cli.work.join(format!("panic-{i}.journal"));
            write(&kept, &bytes)?;
            eprintln!("fuzz: PANIC case {i}; saved {}", kept.display());
            (true, 0)
        };
        tally.record(CaseRun {
            case: i,
            panicked,
            errors,
            took,
            reset,
            peak_kib: peak_kib(),
        });
        let done = i - cli.start + 1;
        if done.is_multiple_of(1000) {
            eprintln!("fuzz: {done} cases, {:?}", started.elapsed());
        }
    }
    let report = tally.finish();
    println!(
        "fuzz: seed {:#x}, cases {}..{}, {:?}",
        cli.seed,
        cli.start,
        cli.start.saturating_add(cli.cases),
        started.elapsed()
    );
    println!("{report}");
    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// clap's own check of the argument definitions
/// (<https://docs.rs/clap/4.6.7/clap/_derive/_tutorial/index.html#testing>).
#[test]
fn verify_cli() {
    use clap::CommandFactory;
    Cli::command().debug_assert();
}
