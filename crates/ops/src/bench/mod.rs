//! `sb bench` (ops spec, "`sb bench`"; decision 11): this server's system
//! and its disk measured in Rust, in sb's own output. Progress is
//! term-ui's runner on stderr, one transient step per measurement, so a
//! run that succeeds leaves only the results; each section's table goes
//! to stdout when the section ends (the runner done first, so the two
//! never draw over each other), then the total time and a timestamp.
//! Speedtest and the network rows come with the network part (the
//! spec's slice 4).

pub mod disk;
pub mod system;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::output_error;
use sb_cli_core::log::RunLog;
use sb_cli_core::theme;
use sb_host::DiskError;
use sb_term_ui::{Labels, Options, Runner, Table};

use disk::{Outcome, RunError, Size};

/// What a run measures, and where: the working directory for the disk
/// test, its run size, the free-space reader, and the System section's
/// sources. [`Bench::new`] is the real command's.
#[derive(Debug, Clone)]
pub struct Bench {
    dir: PathBuf,
    size: Size,
    free_space: fn(&Path) -> Result<u64, DiskError>,
    sources: system::Sources,
}

impl Bench {
    /// The command's: the disk test in `dir` (the working directory,
    /// decision 12), 1 GiB a run, host's `free_space`, this host's files.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            size: Size::default(),
            free_space: sb_host::free_space,
            sources: system::Sources::default(),
        }
    }
}

/// Where the tables go: stdout, fitted to its terminal and in colour as
/// its choice says, as term-ui's `Table::print` does; or a test's buffer.
/// Why not `Table::print`: it writes to stdout only, and the sections'
/// order is tested on a buffer; this is `print`'s three calls, from
/// term-ui's own `render`, `stdout_width` and `colour_on`.
#[derive(Debug)]
pub struct Out<W> {
    writer: W,
    colour: bool,
    width: Option<usize>,
}

impl Out<io::Stdout> {
    /// Stdout, as [`Table::print`] writes it.
    #[must_use]
    pub fn stdout() -> Self {
        Self {
            writer: io::stdout(),
            colour: sb_term_ui::colour_on(sb_term_ui::Stream::Stdout),
            width: sb_term_ui::stdout_width(),
        }
    }
}

impl<W: Write> Out<W> {
    fn table(&mut self, table: &Table) -> anyhow::Result<()> {
        let text = table.render(theme::current(), self.colour, self.width);
        writeln!(self.writer, "{text}").map_err(output_error)?;
        Ok(())
    }

    fn line(&mut self, text: &str) -> anyhow::Result<()> {
        writeln!(self.writer, "{text}").map_err(output_error)?;
        Ok(())
    }
}

/// `sb bench`, the disk test in `dir`, the tables on stdout.
///
/// # Errors
///
/// As [`run`].
pub async fn bench(dir: &Path, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    run(&Bench::new(dir), shutdown, log, &mut Out::stdout()).await
}

/// The sections in order, each table written to `out` when its section
/// ends, then the total time and the timestamp. A failed measurement is
/// its row; only sb itself failing ends the run (ops spec, "Exit").
///
/// # Errors
///
/// sysinfo unable to read the system, a write to `out` failing, or the
/// run interrupted (the runner's interrupted ending; the disk test's file
/// is gone by then).
pub async fn run<W: Write>(
    bench: &Bench,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
    out: &mut Out<W>,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut runner = Runner::start("sb bench", shutdown, log, Options::default());
    let facts = runner
        .step(
            Labels::new("Reading system information", "Read system information").transient(),
            async |step| system::gather(&bench.sources, &step.token()).await,
        )
        .await;
    runner.finish();
    out.table(&system::table(&facts?))?;

    let mut runner = Runner::start("sb bench", shutdown, log, Options::default());
    let outcome = disk_test(&mut runner, bench, shutdown).await;
    runner.finish();
    out.table(&disk::table(&outcome?))?;

    out.line(&format!("Finished in: {}", finished_in(started.elapsed())))?;
    let now = jiff::Timestamp::now().to_zoned(sb_tui::line::local_zone());
    out.line(&format!(
        "Timestamp:   {}",
        now.strftime("%Y-%m-%d %H:%M:%S %Z")
    ))?;
    Ok(())
}

/// What one disk step found.
enum Measured {
    Skipped(&'static str),
    Rate(Option<f64>),
}

/// The disk test's steps, `Disk write test (n/3)`: the first checks the
/// free space first; the blocking work runs on cli-core's `spawn::blocking`, its
/// file checked against the step's token between blocks.
async fn disk_test(
    runner: &mut Runner,
    bench: &Bench,
    shutdown: &Shutdown,
) -> Result<Outcome, sb_cli_core::exit::Exit> {
    let mut rates = Vec::with_capacity(disk::RUNS);
    for n in 1..=disk::RUNS {
        let label = format!("Disk write test ({n}/{})", disk::RUNS);
        let measured = runner
            .step(Labels::new(&label, &label).transient(), async |step| {
                let token = step.token();
                let (dir, size, free_space) = (bench.dir.clone(), bench.size, bench.free_space);
                let shutdown = shutdown.clone();
                let first = n == 1;
                let ran = sb_cli_core::spawn::blocking(move || {
                    if first && let Some(why) = short_of_space(&dir, free_space) {
                        return Ok(Measured::Skipped(why));
                    }
                    let result = disk::run(&dir, size, &shutdown, || !token.is_cancelled());
                    Ok(match result {
                        Ok(elapsed) => {
                            let rate = disk::rate(size.bytes(), elapsed);
                            tracing::info!(run = n, mb_per_s = rate, "disk write measured");
                            Measured::Rate(Some(rate))
                        }
                        Err(RunError::Interrupted) => anyhow::bail!("interrupted"),
                        // The first run finds the directory unwritable; a
                        // later one failing to create keeps the runs
                        // already measured.
                        Err(err @ RunError::Create(_)) if first => {
                            tracing::warn!(error = %err, "disk test skipped");
                            Measured::Skipped(disk::NO_WRITE)
                        }
                        Err(err @ (RunError::Create(_) | RunError::Write(_))) => {
                            tracing::warn!(run = n, error = %err, "disk write failed");
                            Measured::Rate(None)
                        }
                    })
                });
                ran.await?
            })
            .await?;
        match measured {
            Measured::Skipped(why) => return Ok(Outcome::Skipped(why)),
            Measured::Rate(rate) => rates.push(rate),
        }
    }
    Ok(Outcome::Runs(rates))
}

/// Why the disk test can't run in `dir` for its free space: under 2 GiB,
/// or unreadable; none when there is room.
fn short_of_space(
    dir: &Path,
    free_space: fn(&Path) -> Result<u64, DiskError>,
) -> Option<&'static str> {
    match free_space(dir) {
        Ok(free) if free < disk::MIN_FREE => {
            tracing::info!(free, "disk test skipped: not enough space");
            Some(disk::NO_SPACE)
        }
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(error = %err, "disk test skipped: free space unknown");
            Some(disk::NO_FREE_SPACE)
        }
    }
}

/// The total time as bench.sh printed it: `<s> sec`, or `<m> min <s> sec`
/// past a minute.
fn finished_in(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds > 60 {
        format!("{} min {} sec", seconds / 60, seconds % 60)
    } else {
        format!("{seconds} sec")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Free-space readers: closures as `fn` pointers, which is what
    /// [`Bench`] holds.
    const PLENTY: fn(&Path) -> Result<u64, DiskError> = |_| Ok(disk::MIN_FREE);
    const SHORT: fn(&Path) -> Result<u64, DiskError> = |_| Ok(disk::MIN_FREE - 1);

    fn unreadable(path: &Path) -> Result<u64, DiskError> {
        Err(DiskError::Unreadable {
            path: path.to_path_buf(),
            cause: io::Error::other("no"),
        })
    }

    /// A bench on `dir` with tiny runs and `free_space`, sources of this
    /// host but a missing systemd-detect-virt.
    fn small(dir: &Path, free_space: fn(&Path) -> Result<u64, DiskError>) -> Bench {
        Bench {
            dir: dir.to_path_buf(),
            size: Size {
                blocks: 2,
                block: 4096,
            },
            free_space,
            sources: system::Sources {
                detect_virt: dir.join("no-detect-virt"),
                zpool: dir.join("no-zpool"),
                ..system::Sources::default()
            },
        }
    }

    async fn output(bench: &Bench, shutdown: &Shutdown) -> (anyhow::Result<()>, String) {
        let mut out = Out {
            writer: Vec::new(),
            colour: false,
            width: None,
        };
        let result = run(bench, shutdown, None, &mut out).await;
        (result, String::from_utf8(out.writer).expect("utf-8"))
    }

    #[test]
    fn the_command_tests_the_given_directory_with_the_real_sizes() {
        let bench = Bench::new("/some/dir");
        assert_eq!(bench.dir, Path::new("/some/dir"));
        assert_eq!(bench.size, Size::default());
        assert_eq!(bench.sources.cpuinfo, system::Sources::default().cpuinfo);
    }

    #[tokio::test]
    async fn a_run_prints_both_tables_then_the_time() {
        let dir = tempfile::tempdir().expect("dir");
        let (result, text) = output(&small(dir.path(), PLENTY), &Shutdown::new()).await;
        result.expect("run");
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{text}"))
        };
        assert!(at("System") < at("CPU Model"));
        assert!(at("Virtualization") < at("Disk write"));
        assert!(at("1st run") < at("2nd run") && at("2nd run") < at("3rd run"));
        assert!(at("3rd run") < at("│ Average") && at("│ Average") < at("Finished in: "));
        assert!(text.contains("│ Virtualization      │ Dedicated"), "{text}");
        assert_eq!(text.matches(" MB/s │").count(), 4, "{text}");
        let last = text.lines().last().expect("a line");
        assert!(last.starts_with("Timestamp:   20"), "{text}");
        // Its shape: the exact text is `finished_in`'s test (a busy disk
        // can make the run take seconds).
        let finished = text
            .lines()
            .find_map(|line| line.strip_prefix("Finished in: "))
            .expect("finished line");
        assert!(finished.ends_with(" sec"), "{text}");
        assert!(finished.starts_with(|c: char| c.is_ascii_digit()), "{text}");
        assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_none());
    }

    #[tokio::test]
    async fn under_two_gib_free_skips_the_disk_test() {
        let dir = tempfile::tempdir().expect("dir");
        let (result, text) = output(&small(dir.path(), SHORT), &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains(disk::NO_SPACE), "{text}");
        assert!(!text.contains("1st run"), "{text}");
        assert!(text.contains("Finished in: "), "{text}");
    }

    #[tokio::test]
    async fn unknown_free_space_skips_the_disk_test() {
        let dir = tempfile::tempdir().expect("dir");
        let (result, text) = output(&small(dir.path(), unreadable), &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains(disk::NO_FREE_SPACE), "{text}");
    }

    #[tokio::test]
    async fn a_directory_that_cannot_be_written_skips_the_disk_test() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("file");
        std::fs::write(&file, "").expect("file");
        let (result, text) = output(&small(&file, PLENTY), &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains(disk::NO_WRITE), "{text}");
        assert!(!text.contains("1st run"), "{text}");
    }

    /// The run [`interrupting`] interrupts.
    static INTERRUPTED: std::sync::OnceLock<Shutdown> = std::sync::OnceLock::new();

    /// Plenty of space, read as Ctrl-C arrives: the disk test's first
    /// block then finds its step cancelled.
    fn interrupting(path: &Path) -> Result<u64, DiskError> {
        INTERRUPTED.get().expect("set").signalled(2);
        PLENTY(path)
    }

    #[tokio::test]
    async fn an_interrupt_stops_the_disk_test_and_removes_its_file() {
        let dir = tempfile::tempdir().expect("dir");
        let shutdown = INTERRUPTED.get_or_init(Shutdown::new);
        let (result, text) = output(&small(dir.path(), interrupting), shutdown).await;
        assert!(result.is_err(), "{text}");
        assert!(text.contains("CPU Model"), "{text}");
        assert!(!text.contains("Disk write"), "{text}");
        assert!(!text.contains("Finished in"), "{text}");
        assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_none());
    }

    #[tokio::test]
    async fn an_interrupt_before_the_start_prints_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        let shutdown = Shutdown::new();
        shutdown.signalled(2);
        let (result, text) = output(&small(dir.path(), PLENTY), &shutdown).await;
        assert!(result.is_err(), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    #[test]
    fn the_total_time_is_seconds_then_minutes() {
        assert_eq!(finished_in(Duration::from_millis(59_900)), "59 sec");
        assert_eq!(finished_in(Duration::from_secs(60)), "60 sec");
        assert_eq!(finished_in(Duration::from_secs(61)), "1 min 1 sec");
        assert_eq!(finished_in(Duration::from_secs(185)), "3 min 5 sec");
    }

    #[test]
    fn free_space_decides_whether_the_test_runs() {
        let dir = Path::new("/");
        assert_eq!(short_of_space(dir, PLENTY), None);
        assert_eq!(short_of_space(dir, SHORT), Some(disk::NO_SPACE));
        assert_eq!(short_of_space(dir, unreadable), Some(disk::NO_FREE_SPACE));
    }
}
