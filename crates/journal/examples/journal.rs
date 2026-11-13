//! The comparison CLI the container scripts run against journalctl
//! (crates/journal/tests/compare.sh, compat.sh, follow.sh and stress.sh):
//! pages and a follow as `-o json --all` lines, the verdicts and the
//! fixtures. A thin wrapper: the logic
//! is `sb_journal::testing::tools`, unit-tested there (plan, "The
//! `testing` feature"). Built with `--features testing`.
//!
//! The arguments use clap's derive API
//! (<https://docs.rs/clap/4.6.7/clap/_derive/index.html>): one-of options
//! as an argument group (`#[group(multiple = false)]`, the tutorial's
//! <https://docs.rs/clap/4.6.7/clap/_derive/_tutorial/index.html#argument-relations>),
//! values through their `FromStr` or a parse function (`value_parser`).

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use sb_journal::testing::{self, tools};
use sb_journal::{
    Cursor, Journal, Limits, Position, Query, RELEASES, Release, Source, Uid, UnitName,
};
use std::io::{BufWriter, Write};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(about = "sb-journal against journalctl: pages, verdicts and fixtures")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Where to read: the local journal unless files or a directory are given.
#[derive(Args)]
struct Input {
    /// Read these files, as `journalctl --file`.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Read this directory, as `journalctl -D`.
    #[arg(long, conflicts_with = "files")]
    directory: Option<PathBuf>,
}

impl Input {
    fn source(&self) -> Source {
        match &self.directory {
            Some(d) => Source::Directory(d.clone()),
            None if self.files.is_empty() => Source::Local,
            None => Source::Files(self.files.clone()),
        }
    }
}

/// The page's position: the tail unless a cursor is given.
#[derive(Args)]
#[group(multiple = false)]
struct At {
    /// The page before this cursor.
    #[arg(long)]
    before: Option<Cursor>,
    /// The page after this cursor, as `journalctl --after-cursor`.
    #[arg(long)]
    after: Option<Cursor>,
}

impl At {
    fn position(&self) -> Position {
        match (self.before, self.after) {
            (Some(c), _) => Position::Before(c),
            (None, Some(c)) => Position::After(c),
            (None, None) => Position::Tail,
        }
    }
}

/// One selection of `select`.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct Selecting {
    /// Leave out systemd-coredump's entries of every other uid (repeat
    /// for each uid kept).
    #[arg(long = "keep-coredumps-of")]
    keep: Vec<u32>,
    /// Only systemd-coredump's entries of this uid.
    #[arg(long)]
    coredumps_of: Option<u32>,
    /// Only the entries after this realtime, in microseconds.
    #[arg(long)]
    after_realtime: Option<u64>,
}

impl Selecting {
    fn selection(self) -> tools::Selection {
        match (self.coredumps_of, self.after_realtime) {
            (Some(uid), _) => tools::Selection::CoredumpsOf(uid),
            (None, Some(t)) => tools::Selection::AfterRealtime(t),
            (None, None) => tools::Selection::KeepCoredumpsOf(self.keep),
        }
    }
}

/// What to read and match, as journalctl's: the source, `-u` or field
/// matches, and the release whose rules apply.
#[derive(Args)]
struct Matching {
    #[command(flatten)]
    from: Input,
    /// journalctl's `-u`.
    #[arg(long, short, conflicts_with = "matches")]
    unit: Option<String>,
    /// A FIELD=value match; several must all match, a field given twice
    /// either value.
    #[arg(long = "match")]
    matches: Vec<String>,
    /// The release row whose journalctl the page matches (a canary
    /// passes its mapped row).
    #[arg(long, value_parser = Release::parse)]
    release: Release,
    /// The systemd-coredump user's uid, when it exists (26.04's `-u`).
    #[arg(long)]
    coredump_uid: Option<u32>,
}

impl Matching {
    /// The journal and the query.
    fn open(&self) -> Result<(Journal, Query)> {
        let source = self.from.source();
        let query = match &self.unit {
            Some(u) => Query::unit(
                &self.release,
                &UnitName::new(u)?,
                self.coredump_uid.map(Uid::new),
                &source,
            ),
            None => tools::matches(&self.matches)?.unwrap_or_else(tools::every_entry),
        };
        Ok((Journal::open(source, &self.release)?, query))
    }
}

/// `tail`'s arguments.
#[derive(Args)]
struct TailArgs {
    #[command(flatten)]
    matching: Matching,
    #[arg(short, default_value_t = 500)]
    n: usize,
    #[command(flatten)]
    at: At,
    /// Print the page's flags and preads to stderr, as
    /// `sb-journal: records=N more_older=B more_newer=B preads=N`.
    #[arg(long)]
    stats: bool,
}

/// `follow`'s arguments.
#[derive(Args)]
struct FollowArgs {
    #[command(flatten)]
    matching: Matching,
    /// The newest entries first, as `journalctl -f -n`.
    #[arg(short, default_value_t = 0)]
    n: usize,
    /// After the page, print `sb-journal: page read` to stderr and wait
    /// until this file exists before following from the page's end, so a
    /// script can write between the two (follow.sh's `between` scenarios).
    #[arg(long)]
    wait_for: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// A page of entries (of a unit, or matching fields) as `-o json --all`
    /// lines; read errors to stderr.
    Tail(TailArgs),
    /// The newest entries, then the new ones as they are written, as
    /// `journalctl -f -o json --all` prints them; read errors to stderr.
    /// It runs until it is stopped.
    Follow(FollowArgs),
    /// The flags a page should have, from journalctl's whole output for
    /// the same query: `more_older=B more_newer=B`.
    More {
        whole: PathBuf,
        page: PathBuf,
        #[command(flatten)]
        at: At,
    },
    /// Each entry's field as text, one line per entry (empty without it).
    Fields { file: PathBuf, name: String },
    /// The entries of a `-o json` stream a selection keeps, as they are.
    Select {
        file: PathBuf,
        #[command(flatten)]
        selection: Selecting,
    },
    /// Compare two `-o json` streams field by field, values exact; exits 1
    /// on any difference.
    Compare { expected: PathBuf, actual: PathBuf },
    /// Recompute every DATA object's hash and compare it with the stored
    /// one; exits 1 on a difference or an error.
    HashCheck {
        #[command(flatten)]
        from: Input,
    },
    /// Decode every entry of every file; exits 1 on an error.
    DecodeAll {
        #[command(flatten)]
        from: Input,
    },
    /// The supported releases, one per line: `VERSION_ID` and the systemd
    /// major version (the matrix's default list).
    Releases,
    /// The matrix's fixture in journal export format, for
    /// systemd-journal-remote.
    GenExport {
        #[arg(long, default_value_t = 3000)]
        entries: u64,
        /// 0 is the matrix's fixtures; 1 and up are later, on other boots.
        #[arg(long, default_value_t = 0)]
        generation: u8,
        /// The systemd-coredump user's uid: half the uid-1000 coredump
        /// decoys become that user's entries.
        #[arg(long)]
        coredump_uid: Option<u32>,
    },
    /// One boot's entries in export format, at the realtimes given.
    GenBoot {
        /// The boot id, 32 hex digits.
        #[arg(long)]
        boot: String,
        /// Messages are this and a count from 1.
        #[arg(long)]
        prefix: String,
        /// Seconds after the fixtures' base time, one entry each.
        #[arg(long = "realtime-s", required = true, value_delimiter = ',')]
        realtimes: Vec<u64>,
        /// Start the monotonic times this many milliseconds later.
        #[arg(long, default_value_t = 0)]
        monotonic_skip: u64,
        /// The source machine each entry names in `_MACHINE_ID=`, 32 hex
        /// digits.
        #[arg(long)]
        machine: Option<String>,
        /// Leave `_MACHINE_ID=` off the last entry.
        #[arg(long, requires = "machine")]
        untagged_tail: bool,
    },
}

/// A verdict's exit status.
fn verdict(passed: bool) -> ExitCode {
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

/// The release for commands that read no page: the boot order is all a
/// release changes in reading, and they don't merge.
fn any_release() -> Release {
    RELEASES[0]
}

/// A page as `-o json --all` lines; read errors and `--stats` to stderr.
fn tail(args: TailArgs) -> Result<ExitCode> {
    let TailArgs {
        matching,
        n,
        at,
        stats,
    } = args;
    let (journal, query) = matching.open()?;
    let release = matching.release;
    let page = journal.page(&query, &at.position(), n, &Limits::default());
    for e in &page.errors.shown {
        eprintln!("sb-journal: {e}");
    }
    if page.errors.omitted > 0 {
        eprintln!("sb-journal: {} more errors", page.errors.omitted);
    }
    print_lines(page.records.iter().map(|r| tools::json(r, &release)))?;
    if stats {
        eprintln!(
            "sb-journal: records={} {} preads={}",
            page.records.len(),
            tools::More::of(&page),
            testing::counts(&journal).preads
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// How long `--wait-for` waits for its file, and how often it looks.
const WAIT_FOR: (Duration, Duration) = (Duration::from_secs(60), Duration::from_millis(50));

/// The page at the tail (`-n 0`: an empty one), then the follow from its
/// end (`Journal::follow`): what was written since as its first batch,
/// then each batch as it comes (`tools::next_batch`, waiting on the wake
/// descriptor), each printed by `tools::write_records`, until stopped.
fn follow(args: FollowArgs) -> Result<ExitCode> {
    let FollowArgs {
        matching,
        n,
        wait_for,
    } = args;
    let (journal, query) = matching.open()?;
    let release = matching.release;
    let limits = Limits::default();
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut err = std::io::stderr().lock();
    let page = journal.page(&query, &Position::Tail, n, &limits);
    tools::write_records(&page.records, &page.errors, &release, &mut out, &mut err)?;
    if let Some(go) = wait_for {
        writeln!(err, "sb-journal: page read")?;
        let (timeout, every) = WAIT_FOR;
        anyhow::ensure!(
            tools::wait_for(&go, every, timeout),
            "{} didn't appear within {timeout:?}",
            go.display()
        );
    }
    let (mut follower, mut last) = journal.follow(query, &page.end, &limits)?;
    tools::write_records(&last.records, &last.errors, &release, &mut out, &mut err)?;
    let wake = follower.wake_fd()?;
    loop {
        if let Some(batch) = tools::next_batch(&mut follower, wake.as_fd(), &last, &limits, None)? {
            tools::write_records(&batch.records, &batch.errors, &release, &mut out, &mut err)?;
            last = batch;
        }
    }
}

/// Each item on its own line of stdout, buffered.
fn print_lines<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> Result<ExitCode> {
    let mut out = BufWriter::new(std::io::stdout().lock());
    for item in items {
        writeln!(out, "{item}")?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

fn main() -> Result<ExitCode> {
    match Cli::parse().command {
        Command::Tail(args) => tail(args),
        Command::Follow(args) => follow(args),
        Command::More { whole, page, at } => {
            println!(
                "{}",
                tools::more(&read(&whole)?, &read(&page)?, &at.position())?
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Fields { file, name } => print_lines(tools::fields(&read(&file)?, &name)?),
        Command::Select { file, selection } => {
            let text = read(&file)?;
            print_lines(tools::select(&text, &selection.selection())?)
        }
        Command::Compare { expected, actual } => {
            let c = tools::compare(&read(&expected)?, &read(&actual)?)?;
            println!("{c}");
            Ok(verdict(c.passed()))
        }
        Command::HashCheck { from } => {
            let journal = Journal::open(from.source(), &any_release())?;
            let (files, errors) = tools::files(&journal);
            let h = tools::hash_check(&files, &errors);
            println!("{h}");
            Ok(verdict(h.passed()))
        }
        Command::DecodeAll { from } => {
            let started = Instant::now();
            let journal = Journal::open(from.source(), &any_release())?;
            let (files, errors) = tools::files(&journal);
            let d = tools::decode_all(&files, &errors);
            println!("{d}, {:?}", started.elapsed());
            Ok(verdict(d.passed()))
        }
        Command::Releases => print_lines(
            RELEASES
                .iter()
                .map(|r| format!("{} {}", r.version_id(), r.systemd())),
        ),
        Command::GenExport {
            entries,
            generation,
            coredump_uid,
        } => {
            let mut out = BufWriter::new(std::io::stdout().lock());
            tools::gen_export(&mut out, entries, generation, coredump_uid)?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        Command::GenBoot {
            boot,
            prefix,
            realtimes,
            monotonic_skip,
            machine,
            untagged_tail,
        } => {
            let mut out = BufWriter::new(std::io::stdout().lock());
            let machine = machine
                .as_deref()
                .map(|id| tools::SourceMachine { id, untagged_tail });
            tools::gen_boot(
                &mut out,
                &boot,
                &prefix,
                &realtimes,
                monotonic_skip,
                machine,
            )?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// clap's own check of the argument definitions
/// (<https://docs.rs/clap/4.6.7/clap/_derive/_tutorial/index.html#testing>).
#[test]
fn verify_cli() {
    use clap::CommandFactory;
    Cli::command().debug_assert();
}
