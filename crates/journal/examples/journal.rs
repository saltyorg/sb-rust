//! The comparison CLI the container scripts run against journalctl
//! (crates/journal/tests/compare.sh and compat.sh): pages as `-o json
//! --all` lines, the verdicts and the fixtures. A thin wrapper: the logic
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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

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

/// `tail`'s arguments.
#[derive(Args)]
struct TailArgs {
    #[command(flatten)]
    from: Input,
    /// journalctl's `-u`.
    #[arg(long, short, conflicts_with = "matches")]
    unit: Option<String>,
    /// A FIELD=value match; several must all match, a field given twice
    /// either value.
    #[arg(long = "match")]
    matches: Vec<String>,
    #[arg(short, default_value_t = 500)]
    n: usize,
    #[command(flatten)]
    at: At,
    /// The release row whose journalctl the page matches (a canary
    /// passes its mapped row).
    #[arg(long, value_parser = Release::parse)]
    release: Release,
    /// The systemd-coredump user's uid, when it exists (26.04's `-u`).
    #[arg(long)]
    coredump_uid: Option<u32>,
    /// Print the page's flags and preads to stderr, as
    /// `sb-journal: records=N more_older=B more_newer=B preads=N`.
    #[arg(long)]
    stats: bool,
}

#[derive(Subcommand)]
enum Command {
    /// A page of entries (of a unit, or matching fields) as `-o json --all`
    /// lines; read errors to stderr.
    Tail(TailArgs),
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
        from,
        unit,
        matches,
        n,
        at,
        release,
        coredump_uid,
        stats,
    } = args;
    let source = from.source();
    let query = match unit {
        Some(u) => Query::unit(
            &release,
            &UnitName::new(&u)?,
            coredump_uid.map(Uid::new),
            &source,
        ),
        None => tools::matches(&matches)?.unwrap_or_else(tools::every_entry),
    };
    let journal = Journal::open(source, &release)?;
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
        } => {
            let mut out = BufWriter::new(std::io::stdout().lock());
            tools::gen_boot(&mut out, &boot, &prefix, &realtimes)?;
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
