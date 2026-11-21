//! `sb bench` (ops spec, "`sb bench`"; decision 11): this server's system
//! and its disk measured in Rust, in sb's own output. Progress is
//! term-ui's runner on stderr, one transient step per measurement, so a
//! run that succeeds leaves only the results; each section goes to
//! stdout when it ends (the runner done first, so the two never draw over
//! each other), then the total time and a timestamp. The sections follow
//! sb-go's bench.sh (`cmd/bench_script.sh`): System (with the network
//! rows), Disk write, Speedtest. They are drawn in the "ruled" layout
//! (ops spec, "Layout"; decision 43): term-ui's section heading, `sb
//! list`'s, over a ruled table of label and value rows, one label column
//! for every section and the footer.

pub mod disk;
pub mod network;
pub mod speedtest;
pub mod system;

/// Tests that write a stand-in program or start a child hold this, so no
/// other test's fork holds a stand-in's write descriptor when it runs
/// (exec fails with ETXTBSY otherwise), as sb-host's and sb-docker's.
#[cfg(test)]
pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::output_error;
use sb_cli_core::log::RunLog;
use sb_cli_core::theme::{self, Role, Theme};
use sb_host::DiskError;
use sb_term_ui::{Cell, Heading, Labels, Options, Runner, Table};

use disk::{Outcome, RunError, Size};
use network::Endpoints;
use sb_release::{Arch, Releases};
use speedtest::Pin;

/// What the Speedtest section needs: the client for the download, the
/// pinned archive, the machine it must run on, and the directory its
/// private directory goes in.
#[derive(Debug)]
pub struct Speedtest {
    pub releases: Releases,
    pub pin: Pin,
    pub arch: Arch,
    pub parent: PathBuf,
}

/// Where the Speedtest's private directory goes: `/tmp`, sticky and
/// root's, whatever `$TMPDIR` says (a non-sticky one another user can
/// write would let them swap the directory before it runs).
const SPEEDTEST_PARENT: &str = "/tmp";

/// What a run measures, and where: the working directory for the disk
/// test, its run size, the free-space reader, the System section's
/// sources, the network endpoints (none: nothing is asked) and the
/// Speedtest (or why it can't run). [`Bench::for_sb`] is the command's.
#[derive(Debug)]
pub struct Bench {
    dir: PathBuf,
    size: Size,
    free_space: fn(&Path) -> Result<u64, DiskError>,
    sources: system::Sources,
    endpoints: Option<Endpoints>,
    speedtest: Result<Speedtest, String>,
}

impl Bench {
    /// The command's: the disk test in `dir` (the working directory,
    /// decision 12), 1 GiB a run, host's `free_space`, this host's files,
    /// the real services and Ookla's pinned archive for this machine,
    /// with `User-Agent: <user_agent>`. In a `testing` build (sb's own
    /// tests), the fakes the test variables name instead
    /// ([`crate::testing`], and sb-release's), and nothing asked without
    /// them.
    #[must_use]
    pub fn for_sb(dir: impl Into<PathBuf>, user_agent: &str) -> Self {
        let (endpoints, pin) = services(user_agent);
        let speedtest = pin.and_then(|pin| {
            Ok(Speedtest {
                releases: Releases::for_sb(user_agent).map_err(|err| err.to_string())?,
                pin,
                arch: Arch::current().map_err(|err| err.to_string())?,
                parent: PathBuf::from(SPEEDTEST_PARENT),
            })
        });
        Self {
            dir: dir.into(),
            size: Size::default(),
            free_space: sb_host::free_space,
            sources: system::Sources::default(),
            endpoints,
            speedtest,
        }
    }
}

/// The network endpoints and the Speedtest pin: the real ones.
#[cfg(not(feature = "testing"))]
fn services(user_agent: &str) -> (Option<Endpoints>, Result<Pin, String>) {
    let pin = Arch::current()
        .map(Pin::for_arch)
        .map_err(|err| err.to_string());
    (Some(Endpoints::production(user_agent)), pin)
}

/// The network endpoints and the Speedtest pin: a `testing` build's, from
/// the test variable, or none (sb-ops' own tests too).
#[cfg(feature = "testing")]
fn services(user_agent: &str) -> (Option<Endpoints>, Result<Pin, String>) {
    match crate::testing::from_env(user_agent) {
        Ok((endpoints, pin)) => (Some(endpoints), Ok(pin)),
        Err(why) => (None, Err(why)),
    }
}

/// The width bench's tables lay out at when stdout isn't a terminal: a
/// long cell (a Speedtest that couldn't be prepared, its reason) wraps
/// as on a terminal this wide, instead of drawing the table at its
/// natural width (ops spec, "`sb bench`", Results; the maintainer,
/// 2026-11-18). This is bench's own choice; term-ui's piped tables
/// otherwise keep their natural width (term-ui spec, "Piped"). Why not
/// console's `Term::size()`, whose fallback is this width on Unix: that
/// number is undocumented (console 0.16.6, `unix_term.rs`'s
/// `DEFAULT_WIDTH`; 79 on Windows), and the spec fixes 80.
const PIPED_WIDTH: usize = 80;

/// The label column's width, every section's and the footer's: `TCP
/// Congestion Ctrl` and the two-space gap (ops spec, "Layout").
const LABEL: usize = 21;

/// The width of Disk write's rates column and Speedtest's Upload and
/// Download columns, the two-space gap included: `1234.56 Mbps` and the
/// gap, so the columns end flush over 1 Gbps too (the maintainer,
/// 2026-11-21, after the example's 13).
const RATE: usize = 14;

/// A section's heading: its name bold in the Service role, over a Muted
/// rule (`sb list`'s heading, term-ui's).
fn heading(name: &str) -> Heading {
    Heading::new(name, Role::Service)
}

/// A row's label, Muted.
fn label(text: &str) -> Cell {
    Cell::new(text).role(Role::Muted)
}

/// A ruled table of label and value rows, without headings, the label
/// column [`LABEL`] wide.
fn rows() -> Table {
    Table::ruled(Vec::<String>::new()).min_width(0, LABEL)
}

/// Where the sections go: stdout, fitted to its terminal or, piped, to
/// [`PIPED_WIDTH`], and in colour as its choice says, as term-ui's
/// `Table::print` does; or a test's buffer. Why not `Table::print`: it
/// writes to stdout only and keeps a piped table's natural width, and
/// the sections' order is tested on a buffer; this is `print`'s three
/// calls, from term-ui's own `render`, `stdout_width` and `colour_on`.
#[derive(Debug)]
pub struct Out<W> {
    writer: W,
    theme: Theme,
    colour: bool,
    width: Option<usize>,
    /// How many sections are written: a blank line goes before every
    /// heading but the first.
    sections: usize,
}

impl Out<sb_cli_core::tail::Tracked<io::Stdout>> {
    /// Stdout, as [`Table::print`] writes it but at [`PIPED_WIDTH`] when
    /// it isn't a terminal.
    #[must_use]
    pub fn stdout() -> Self {
        Out::new(
            // Recorded for the blank line at the end (cli-core's `tail`).
            sb_cli_core::tail::Tracked::stdout(io::stdout()),
            theme::current().clone(),
            sb_term_ui::colour_on(sb_term_ui::Stream::Stdout),
            Some(sb_term_ui::stdout_width().unwrap_or(PIPED_WIDTH)),
        )
    }
}

impl<W: Write> Out<W> {
    /// Sections written to `writer` in `theme`, coloured when `colour` is
    /// on, fitted to `width` (natural width with none, the rules then
    /// [`PIPED_WIDTH`] long).
    fn new(writer: W, theme: Theme, colour: bool, width: Option<usize>) -> Self {
        Self {
            writer,
            theme,
            colour,
            width,
            sections: 0,
        }
    }

    /// The writer, the sections written to it.
    #[cfg(test)]
    fn into_inner(self) -> W {
        self.writer
    }

    /// The rules' width.
    fn rule_width(&self) -> usize {
        self.width.unwrap_or(PIPED_WIDTH)
    }

    /// A section: a blank line unless it is the first, its heading, then
    /// its rows.
    ///
    /// # Errors
    ///
    /// The writer failing.
    fn section(&mut self, heading: &Heading, table: &Table) -> anyhow::Result<()> {
        let gap = if self.sections == 0 { "" } else { "\n" };
        self.sections += 1;
        let heading = heading.render(&self.theme, self.colour, self.rule_width());
        let table = table.render(&self.theme, self.colour, self.width);
        writeln!(self.writer, "{gap}{heading}{table}").map_err(output_error)?;
        Ok(())
    }

    /// The footer: a blank line, a Muted rule, then `Finished in` and
    /// `Timestamp` as rows of the label column.
    ///
    /// # Errors
    ///
    /// The writer failing.
    fn footer(&mut self, finished: &str, timestamp: &str) -> anyhow::Result<()> {
        let rule = sb_term_ui::rule(&self.theme, Role::Muted, self.colour, self.rule_width());
        let mut table = rows();
        table.row([label("Finished in"), Cell::new(finished)]);
        table.row([label("Timestamp"), Cell::new(timestamp)]);
        let table = table.render(&self.theme, self.colour, self.width);
        writeln!(self.writer, "\n{rule}\n{table}").map_err(output_error)?;
        Ok(())
    }
}

/// `sb bench`, the disk test in `dir`, the tables on stdout, requests
/// sent with `User-Agent: <user_agent>`.
///
/// # Errors
///
/// As [`run`].
pub async fn bench(
    dir: &Path,
    user_agent: &str,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    run(
        &Bench::for_sb(dir, user_agent),
        shutdown,
        log,
        &mut Out::stdout(),
    )
    .await
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
            async |step| {
                system::gather(&bench.sources, bench.endpoints.as_ref(), &step.token()).await
            },
        )
        .await;
    runner.finish();
    out.section(&heading("System"), &system::table(&facts?))?;

    let mut runner = Runner::start("sb bench", shutdown, log, Options::default());
    let outcome = disk_test(&mut runner, bench, shutdown).await;
    runner.finish();
    out.section(&heading("Disk write"), &disk::table(&outcome?))?;

    // No runner without steps: one with none would print its title.
    let outcome = match &bench.speedtest {
        Ok(st) => {
            let mut runner = Runner::start("sb bench", shutdown, log, Options::default());
            let outcome = speedtest_section(&mut runner, st).await;
            runner.finish();
            outcome?
        }
        Err(why) => {
            tracing::info!(why = why.as_str(), "speedtest unavailable");
            speedtest::Outcome::Unavailable(why.clone())
        }
    };
    out.section(&speedtest::heading(&outcome), &speedtest::table(&outcome))?;

    let now = jiff::Timestamp::now().to_zoned(sb_tui::line::local_zone());
    out.footer(
        &finished_in(started.elapsed()),
        &now.strftime("%Y-%m-%d %H:%M:%S %Z").to_string(),
    )
}

/// The Speedtest section's steps: `Preparing Speedtest` (the download,
/// the extraction and the ELF check), then `Speedtest: <node>` for each
/// of bench.sh's nodes. A Speedtest that can't be prepared is the
/// section's one row; an interrupt ends the run, the private directory
/// removed.
async fn speedtest_section(
    runner: &mut Runner,
    st: &Speedtest,
) -> Result<speedtest::Outcome, sb_cli_core::exit::Exit> {
    let prepared = runner
        .step(
            Labels::new("Preparing Speedtest", "Prepared Speedtest").transient(),
            async |step| match speedtest::prepare(&st.releases, &st.pin, st.arch, &st.parent, step)
                .await
            {
                Ok(prepared) => Ok(Ok(prepared)),
                Err(err) if step.token().is_cancelled() => Err(err),
                Err(err) => {
                    tracing::warn!(error = format!("{err:#}"), "speedtest unavailable");
                    Ok(Err(format!("{err:#}")))
                }
            },
        )
        .await?;
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(why) => return Ok(speedtest::Outcome::Unavailable(why)),
    };
    run_nodes(
        runner,
        &prepared,
        &speedtest::NODES,
        speedtest::NODE_TIMEOUT,
    )
    .await
}

/// `prepared` against each of `nodes`, a step each, `timeout` each; the
/// private directory goes when `prepared` does, after the last.
async fn run_nodes(
    runner: &mut Runner,
    prepared: &speedtest::Prepared,
    nodes: &[speedtest::Node],
    timeout: Duration,
) -> Result<speedtest::Outcome, sb_cli_core::exit::Exit> {
    let mut results = Vec::with_capacity(nodes.len());
    for &node in nodes {
        let label = format!("Speedtest: {}", node.name);
        let measured = runner
            .step(Labels::new(&label, &label).transient(), async |step| {
                speedtest::run_node(
                    prepared.program(),
                    prepared.home(),
                    node,
                    timeout,
                    &step.token(),
                )
                .await
            })
            .await?;
        results.push((node, measured));
    }
    Ok(speedtest::Outcome::Nodes(results))
}

/// What one disk step found.
enum DiskRun {
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
                        return Ok(DiskRun::Skipped(why));
                    }
                    let result = disk::run(&dir, size, &shutdown, || !token.is_cancelled());
                    Ok(match result {
                        Ok(elapsed) => {
                            let rate = disk::rate(size.bytes(), elapsed);
                            tracing::info!(run = n, mb_per_s = rate, "disk write measured");
                            DiskRun::Rate(Some(rate))
                        }
                        Err(RunError::Interrupted) => anyhow::bail!("interrupted"),
                        // The first run finds the directory unwritable; a
                        // later one failing to create keeps the runs
                        // already measured.
                        Err(err @ RunError::Create(_)) if first => {
                            tracing::warn!(error = %err, "disk test skipped");
                            DiskRun::Skipped(disk::NO_WRITE)
                        }
                        Err(err @ (RunError::Create(_) | RunError::Write(_))) => {
                            tracing::warn!(run = n, error = %err, "disk write failed");
                            DiskRun::Rate(None)
                        }
                    })
                });
                ran.await?
            })
            .await?;
        match measured {
            DiskRun::Skipped(why) => return Ok(Outcome::Skipped(why)),
            DiskRun::Rate(rate) => rates.push(rate),
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
            endpoints: None,
            speedtest: Err("not in this test".to_owned()),
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
        let mut out = Out::new(Vec::new(), Theme::plain(), false, None);
        let result = run(bench, shutdown, None, &mut out).await;
        (result, String::from_utf8(out.into_inner()).expect("utf-8"))
    }

    #[test]
    fn the_command_tests_the_given_directory_with_the_real_sizes() {
        let bench = Bench::for_sb("/some/dir", "sb/test");
        assert_eq!(bench.dir, Path::new("/some/dir"));
        assert_eq!(bench.size, Size::default());
        assert_eq!(bench.sources.cpuinfo, system::Sources::default().cpuinfo);
        // This test build has the `testing` feature and no test variable:
        // nothing is asked.
        assert!(bench.endpoints.is_none());
        let why = bench.speedtest.expect_err("no speedtest");
        assert!(why.contains("never reaches the real services"), "{why}");
    }

    #[tokio::test]
    async fn a_run_prints_both_tables_then_the_time() {
        let _spawns = super::SPAWNS.lock().await;
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
        assert!(at("3rd run") < at("\nAverage ") && at("\nAverage ") < at("\nFinished in "));
        assert!(
            text.contains("\nVirtualization       Dedicated\n"),
            "{text}"
        );
        assert_eq!(text.matches(" MB/s\n").count(), 4, "{text}");
        let last = text.lines().last().expect("a line");
        assert!(last.starts_with("Timestamp            20"), "{text}");
        // Its shape: the exact text is `finished_in`'s test (a busy disk
        // can make the run take seconds).
        let finished = text
            .lines()
            .find_map(|line| line.strip_prefix("Finished in          "))
            .expect("finished line");
        assert!(finished.ends_with(" sec"), "{text}");
        assert!(finished.starts_with(|c: char| c.is_ascii_digit()), "{text}");
        assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_none());
    }

    #[tokio::test]
    async fn under_two_gib_free_skips_the_disk_test() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let (result, text) = output(&small(dir.path(), SHORT), &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains(disk::NO_SPACE), "{text}");
        assert!(!text.contains("1st run"), "{text}");
        assert!(text.contains("\nFinished in "), "{text}");
    }

    #[tokio::test]
    async fn unknown_free_space_skips_the_disk_test() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let (result, text) = output(&small(dir.path(), unreadable), &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains(disk::NO_FREE_SPACE), "{text}");
    }

    #[tokio::test]
    async fn a_directory_that_cannot_be_written_skips_the_disk_test() {
        let _spawns = super::SPAWNS.lock().await;
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
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let shutdown = INTERRUPTED.get_or_init(Shutdown::new);
        let (result, text) = output(&small(dir.path(), interrupting), shutdown).await;
        assert!(result.is_err(), "{text}");
        assert!(text.contains("CPU Model"), "{text}");
        assert!(!text.contains("Disk write"), "{text}");
        assert!(!text.contains("Finished in"), "{text}");
        assert!(!text.contains("Speedtest"), "{text}");
        assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_none());
    }

    #[tokio::test]
    async fn an_interrupt_before_the_start_prints_nothing() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let shutdown = Shutdown::new();
        shutdown.signalled(2);
        let (result, text) = output(&small(dir.path(), PLENTY), &shutdown).await;
        assert!(result.is_err(), "{text}");
        assert!(text.is_empty(), "{text}");
    }

    /// A gzipped tar holding `program` as `speedtest`, its pin on `fake`.
    fn served(fake: &sb_release::testing::Fake, dir: &Path, program: &[u8]) -> Pin {
        let path = dir.join("speedtest.tgz");
        let file = std::fs::File::create(&path).expect("create");
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_size(u64::try_from(program.len()).expect("size"));
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "speedtest", program)
            .expect("append");
        builder.into_inner().expect("tar").finish().expect("gz");
        let bytes = std::fs::read(&path).expect("read");
        fake.script(
            "/app/speedtest.tgz",
            sb_release::testing::Answer::new(200, bytes.clone()),
        );
        Pin {
            url: format!("{}/app/speedtest.tgz", fake.url()),
            size: u64::try_from(bytes.len()).expect("size"),
            sha256: hex::encode(sb_release::sha256_file(&path).expect("digest")),
        }
    }

    #[tokio::test]
    async fn the_speedtest_runs_each_node_with_the_verified_program() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = sb_release::testing::Fake::start().expect("fake");
        std::fs::create_dir(dir.path().join("tmp")).expect("tmp");
        // The stand-in sb: an ELF for this machine that answers no JSON.
        let pin = served(&fake, dir.path(), &sb_release::testing::stand_in("1.0.0"));
        let bench = Bench {
            speedtest: Ok(Speedtest {
                releases: fake.releases("sb/test").expect("releases"),
                pin,
                arch: Arch::current().expect("arch"),
                parent: dir.path().join("tmp"),
            }),
            ..small(dir.path(), SHORT)
        };
        let (result, text) = output(&bench, &Shutdown::new()).await;
        result.expect("run");
        assert!(at_most_once(&text, SPEEDTEST_HEADINGS), "{text}");
        assert_eq!(
            text.matches("Failed to parse results").count(),
            12,
            "{text}"
        );
        assert!(
            text.find("Tokyo, JP") > text.find("Speedtest.net"),
            "{text}"
        );
        assert!(
            text.find(SPEEDTEST_HEADINGS) < text.find("\nFinished in "),
            "{text}"
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        // The private directory went with the section.
        assert_eq!(entries(&dir.path().join("tmp")), Vec::<PathBuf>::new());
    }

    fn entries(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect()
    }

    /// Ctrl-C, `after` from now.
    fn interrupt_after(shutdown: &Shutdown, after: Duration) {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            shutdown.signalled(2);
        });
    }

    #[tokio::test]
    async fn the_speedtest_s_directory_is_private_and_goes_with_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&tmp).expect("tmp");
        let fake = sb_release::testing::Fake::start().expect("fake");
        let pin = served(&fake, dir.path(), &sb_release::testing::stand_in("1.0.0"));
        let releases = fake.releases("sb/test").expect("releases");
        let shutdown = Shutdown::new();
        let mut runner = Runner::start("sb test", &shutdown, None, Options::default());
        let prepared = runner
            .step(Labels::new("Preparing", "Prepared"), async |step| {
                speedtest::prepare(&releases, &pin, Arch::current()?, &tmp, step).await
            })
            .await
            .expect("prepared");
        runner.finish();
        let mode = std::fs::metadata(prepared.home())
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(prepared.home().starts_with(&tmp));
        // Only the program is left in it; the archive went once read.
        assert_eq!(
            entries(prepared.home()),
            vec![prepared.program().to_path_buf()]
        );
        drop(prepared);
        assert_eq!(entries(&tmp), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn an_interrupted_download_ends_the_run_without_the_section() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&tmp).expect("tmp");
        let fake = sb_release::testing::Fake::start().expect("fake");
        let mut pin = served(&fake, dir.path(), &sb_release::testing::stand_in("1.0.0"));
        // A slow answer, so Ctrl-C lands while it downloads.
        let slow = sb_release::testing::Fake::start().expect("fake");
        let bytes = std::fs::read(dir.path().join("speedtest.tgz")).expect("archive");
        slow.script(
            "/app/speedtest.tgz",
            sb_release::testing::Answer::new(200, bytes).trickled(16, Duration::from_secs(1)),
        );
        pin.url = format!("{}/app/speedtest.tgz", slow.url());
        let bench = Bench {
            speedtest: Ok(Speedtest {
                releases: slow.releases("sb/test").expect("releases"),
                pin,
                arch: Arch::current().expect("arch"),
                parent: tmp.clone(),
            }),
            ..small(dir.path(), SHORT)
        };
        let shutdown = Shutdown::new();
        interrupt_after(&shutdown, Duration::from_millis(500));
        let (result, text) = output(&bench, &shutdown).await;
        assert!(result.is_err(), "{text}");
        assert!(text.contains("Disk write"), "{text}");
        assert!(!text.contains("Speedtest"), "{text}");
        assert!(!text.contains("Finished in"), "{text}");
        assert_eq!(entries(&tmp), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn an_interrupted_node_ends_the_run_and_removes_the_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let _spawns = super::SPAWNS.lock().await;
        let parent = tempfile::tempdir().expect("dir");
        let dir = tempfile::tempdir_in(parent.path()).expect("private");
        let program = dir.path().join("speedtest");
        std::fs::write(&program, "#!/bin/sh\nsleep 30\n").expect("write");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let prepared = speedtest::Prepared::of(dir, program);
        let shutdown = Shutdown::new();
        let mut runner = Runner::start("sb test", &shutdown, None, Options::default());
        interrupt_after(&shutdown, Duration::from_millis(300));
        let started = Instant::now();
        let outcome = run_nodes(
            &mut runner,
            &prepared,
            &speedtest::NODES[..2],
            Duration::from_secs(60),
        )
        .await;
        runner.finish();
        assert!(outcome.is_err(), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(20));
        drop(prepared);
        assert_eq!(entries(parent.path()), Vec::<PathBuf>::new());
    }

    /// Speedtest's headings line, there only when the nodes ran.
    const SPEEDTEST_HEADINGS: &str = "Upload      Download    Latency";

    fn at_most_once(text: &str, needle: &str) -> bool {
        text.matches(needle).count() == 1
    }

    #[tokio::test]
    async fn a_speedtest_that_fails_its_checks_is_one_row() {
        let _spawns = super::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = sb_release::testing::Fake::start().expect("fake");
        std::fs::create_dir(dir.path().join("tmp")).expect("tmp");
        let pin = served(&fake, dir.path(), b"#!/bin/sh\necho {}\n");
        let speedtest = |pin: Pin| Speedtest {
            releases: fake.releases("sb/test").expect("releases"),
            pin,
            arch: Arch::current().expect("arch"),
            parent: dir.path().join("tmp"),
        };
        // Not an ELF: refused before it runs.
        let bench = Bench {
            speedtest: Ok(speedtest(pin.clone())),
            ..small(dir.path(), SHORT)
        };
        let (result, text) = output(&bench, &Shutdown::new()).await;
        result.expect("run");
        assert!(
            text.contains("\nSkipped              Speedtest unavailable: "),
            "{text}"
        );
        assert!(!text.contains(SPEEDTEST_HEADINGS), "{text}");
        // Refused after its download: nothing left of it.
        assert_eq!(entries(&dir.path().join("tmp")), Vec::<PathBuf>::new());
        // Another digest: refused as it downloads.
        let other = sb_release::testing::Fake::start().expect("fake");
        other.script(
            "/app/speedtest.tgz",
            sb_release::testing::Answer::new(200, b"other".to_vec()),
        );
        let bench = Bench {
            speedtest: Ok(Speedtest {
                releases: other.releases("sb/test").expect("releases"),
                pin: Pin {
                    url: format!("{}/app/speedtest.tgz", other.url()),
                    size: 5,
                    sha256: "00".repeat(32),
                },
                arch: Arch::current().expect("arch"),
                parent: dir.path().join("tmp"),
            }),
            ..small(dir.path(), SHORT)
        };
        let (result, text) = output(&bench, &Shutdown::new()).await;
        result.expect("run");
        assert!(text.contains("Speedtest unavailable: "), "{text}");
        assert!(text.contains("digest"), "{text}");
        // Nothing to run with: the reason.
        let (result, text) = output(&small(dir.path(), SHORT), &Shutdown::new()).await;
        result.expect("run");
        assert!(
            text.contains("Speedtest unavailable: not in this test"),
            "{text}"
        );
        assert!(text.contains("\nFinished in "), "{text}");
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

    /// The design example's System values (invented values,
    /// ops decision 43), with the network rows of a run with network or
    /// without.
    fn example_facts(network: bool) -> system::Facts {
        use network::{Isp, Online};
        use system::{Cpu, Facts, Usage};
        let isp = if network {
            Isp {
                org: Some("AS64500 Example Hosting Ltd".to_owned()),
                city: Some("Lisbon".to_owned()),
                country: Some("PT".to_owned()),
                region: Some("Lisbon".to_owned()),
            }
        } else {
            Isp::default()
        };
        Facts {
            cpu: Cpu {
                model: "Example CPU 16-Core Processor".to_owned(),
                cores: 16,
                mhz: Some("3000.000".to_owned()),
                cache: Some("1024 KB".to_owned()),
                aes: true,
                virt: true,
            },
            disk: Some(Usage {
                total: (40 << 40) / 10,
                used: (15 << 40) / 10,
            }),
            ram: Usage {
                total: (1000 << 30) / 10,
                used: (250 << 30) / 10,
            },
            swap: Usage {
                total: 4 << 30,
                used: (10 << 30) / 10,
            },
            uptime: Some(12 * 86_400 + 3 * 3_600 + 30 * 60),
            load: Some((0.50, 0.40, 0.30)),
            os: Some("Ubuntu 24.04.5 LTS".to_owned()),
            arch: "x86_64".to_owned(),
            kernel: Some("6.8.0-90-generic".to_owned()),
            congestion: Some("bbr".to_owned()),
            virtualization: "Dedicated".to_owned(),
            online: Online {
                v4: network,
                v6: false,
            },
            isp,
            ca_missing: false,
        }
    }

    /// The example's Speedtest: nine nodes measured, three failed.
    fn example_nodes() -> speedtest::Outcome {
        use speedtest::Measured;
        let speeds = [
            Some(("900.00", "950.00", "1.00")),
            None,
            Some(("700.00", "750.00", "120.00")),
            Some(("600.00", "650.00", "100.00")),
            Some(("500.00", "550.00", "20.00")),
            Some(("400.00", "450.00", "10.00")),
            None,
            None,
            Some(("300.00", "350.00", "200.00")),
            Some(("200.00", "250.00", "250.00")),
            Some(("100.00", "150.00", "220.00")),
            Some(("600.00", "640.00", "270.00")),
        ];
        let nodes = speedtest::NODES.iter().zip(speeds).map(|(&node, speed)| {
            let measured = speed.map_or(Measured::Failed, |(up, down, latency)| Measured::Speeds {
                upload: format!("{up} Mbps"),
                download: format!("{down} Mbps"),
                latency: format!("{latency} ms"),
            });
            (node, measured)
        });
        speedtest::Outcome::Nodes(nodes.collect())
    }

    /// The example's whole report, through the sections `run` writes,
    /// with the network or without (the example's offline run: its own
    /// disk rates and Speedtest unavailable).
    fn example(network: bool, theme: Theme, colour: bool, width: usize) -> String {
        let mut out = Out::new(Vec::new(), theme, colour, Some(width));
        out.section(&heading("System"), &system::table(&example_facts(network)))
            .expect("system");
        let rates = if network {
            [800.0, 820.0, 840.0]
        } else {
            [700.00, 710.00, 720.00]
        };
        let disk = disk::Outcome::Runs(rates.map(Some).to_vec());
        out.section(&heading("Disk write"), &disk::table(&disk))
            .expect("disk");
        let speedtest = if network {
            example_nodes()
        } else {
            speedtest::Outcome::Unavailable(
                "download release asset \"ookla-speedtest-1.2.0-linux-x86_64.tgz\": error \
                 sending request: client error (Connect): dns error: error resolving DNS: \
                 failed to lookup address information: Temporary failure in name resolution"
                    .to_owned(),
            )
        };
        out.section(
            &speedtest::heading(&speedtest),
            &speedtest::table(&speedtest),
        )
        .expect("speedtest");
        let (finished, timestamp) = if network {
            ("4 min 10 sec", "2026-01-15 10:00:00 UTC")
        } else {
            ("10 sec", "2026-01-15 10:05:00 UTC")
        };
        out.footer(finished, timestamp).expect("footer");
        String::from_utf8(out.into_inner()).expect("utf-8")
    }

    fn truecolor() -> Theme {
        use sb_cli_core::theme::{Palette, Profile};
        Theme::resolve(&Palette::default(), Profile::TrueColor)
    }

    /// The ruled layout, as the maintainer chose it (ops decision 43):
    /// plain and coloured at 80 and 120 columns, and the offline run's;
    /// the plain forms are the example's text but for the rate columns,
    /// 14 wide where the example's were 13 (decision 43, amended).
    #[test]
    fn the_report_is_the_ruled_layout() {
        for width in [80, 120] {
            insta::assert_snapshot!(
                format!("ruled-plain-{width}"),
                example(true, Theme::plain(), false, width)
            );
            insta::assert_snapshot!(
                format!("ruled-colour-{width}"),
                example(true, truecolor(), true, width).replace('\x1b', "␛")
            );
            insta::assert_snapshot!(
                format!("ruled-offline-plain-{width}"),
                example(false, Theme::plain(), false, width)
            );
            insta::assert_snapshot!(
                format!("ruled-offline-colour-{width}"),
                example(false, truecolor(), true, width).replace('\x1b', "␛")
            );
        }
    }

    /// Every line fits the width and none ends in a space; plain is the
    /// colour form without its codes; wider only lengthens the rules.
    #[test]
    fn the_report_fits_and_only_its_rules_grow() {
        // term-ui's own parser drops the codes (anstyle-parse's); its text
        // has no final newline.
        let strip = |text: &str| {
            sb_term_ui::DisplayText::from_bytes(text.as_bytes(), false)
                .as_str()
                .replace('─', "-")
        };
        for network in [true, false] {
            for width in [80, 120] {
                let text = example(network, truecolor(), true, width);
                let plain = example(network, Theme::plain(), false, width);
                assert_eq!(strip(&text), plain.trim_end_matches('\n'));
                for line in plain.lines() {
                    assert!(line.chars().count() <= width, "{line:?}");
                    assert!(!line.ends_with(' '), "{line:?}");
                }
            }
        }
        // Without a cell to wrap, a wider terminal only lengthens the rules.
        let narrow = example(true, Theme::plain(), false, 80);
        let wide = example(true, Theme::plain(), false, 120);
        assert_eq!(wide.replace(&"-".repeat(120), &"-".repeat(80)), narrow);
    }

    /// Over 1 Gbps and over 1000 MB/s (common on Saltbox servers), Disk
    /// write's rates and Speedtest's Upload column still end in the same
    /// column (decision 43, amended: 14 wide).
    #[test]
    fn four_digit_rates_keep_the_columns_flush() {
        let disk = disk::table(&disk::Outcome::Runs(vec![Some(1234.5); 3])).render(
            &Theme::plain(),
            false,
            Some(80),
        );
        let nodes = speedtest::table(&speedtest::Outcome::Nodes(vec![(
            speedtest::NODES[0],
            speedtest::Measured::Speeds {
                upload: "1234.56 Mbps".to_owned(),
                download: "9876.54 Mbps".to_owned(),
                latency: "0.46 ms".to_owned(),
            },
        )]))
        .render(&Theme::plain(), false, Some(80));
        let end = |text: &str, needle: &str| {
            let line = text
                .lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle}\n{text}"));
            line.find(needle).map(|at| at + needle.len())
        };
        assert_eq!(end(&disk, "1234.5 MB/s"), Some(LABEL + RATE), "{disk}");
        assert_eq!(end(&nodes, "1234.56 Mbps"), Some(LABEL + RATE), "{nodes}");
        assert_eq!(end(&nodes, "Upload"), Some(LABEL + RATE), "{nodes}");
    }

    /// The roles of the failure and skip cells: a failed node, a failed
    /// disk run and a run that couldn't parse in Problem; every
    /// `Skipped` reason in Warn; `Average` bold.
    #[test]
    fn failures_take_problem_and_skips_take_warn() {
        use sb_term_ui::style::paint;
        let theme = truecolor();
        let render = |table: &Table| table.render(&theme, true, Some(80));
        let failed = render(&speedtest::table(&speedtest::Outcome::Nodes(vec![
            (speedtest::NODES[1], speedtest::Measured::Failed),
            (speedtest::NODES[2], speedtest::Measured::Unparsed),
        ])));
        for text in ["Test failed", "Failed to parse results"] {
            assert!(
                failed.contains(&paint(&theme, Role::Problem, true, text)),
                "{failed:?}"
            );
        }
        let runs = render(&disk::table(&disk::Outcome::Runs(vec![
            Some(1.0),
            None,
            None,
        ])));
        assert!(
            runs.contains(&paint(&theme, Role::Problem, true, disk::FAILED)),
            "{runs:?}"
        );
        let bold = theme.style(Role::Muted).bold();
        assert!(!runs.contains(&bold.to_string()), "{runs:?}");
        assert!(runs.contains("\x1b[1mAverage\x1b[0m"), "{runs:?}");
        // The padding outside the codes, as tabled pads (decision 43).
        assert!(runs.contains("    \x1b[1m1.0 MB/s\x1b[0m"), "{runs:?}");
        for why in [disk::NO_SPACE, disk::NO_FREE_SPACE, disk::NO_WRITE] {
            let text = render(&disk::table(&disk::Outcome::Skipped(why)));
            assert!(
                text.contains(&paint(&theme, Role::Warn, true, why)),
                "{text:?}"
            );
            assert!(
                text.contains(&paint(&theme, Role::Muted, true, "Skipped")),
                "{text:?}"
            );
        }
        let unavailable = render(&speedtest::table(&speedtest::Outcome::Unavailable(
            "no network".to_owned(),
        )));
        assert!(
            unavailable.contains(&paint(
                &theme,
                Role::Warn,
                true,
                "Speedtest unavailable: no network"
            )),
            "{unavailable:?}"
        );
        // A missing CA bundle: release's error, in cli-core's words with
        // the fix, in Warn (decision 44).
        let missing = render(&speedtest::table(&speedtest::Outcome::Unavailable(
            sb_release::Error::MissingCa.to_string(),
        )));
        let said = format!("Speedtest unavailable: {}", sb_cli_core::tls::MISSING_CA);
        assert!(
            sb_term_ui::DisplayText::from_bytes(missing.as_bytes(), false)
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(&said),
            "{missing:?}"
        );
        assert!(
            missing.contains(&theme.style(Role::Warn).to_string()),
            "{missing:?}"
        );
        // No node count without nodes.
        let plain = Theme::plain();
        assert_eq!(
            speedtest::heading(&speedtest::Outcome::Unavailable(String::new()))
                .render(&plain, false, 3),
            "Speedtest\n---\n"
        );
    }
}
