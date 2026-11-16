//! What a reconcile leaves in sb's run log, through cli-core's own
//! `RunLog`, as `sb` opens it: the process's global subscriber, the run's
//! span entered on the main thread, a current-thread runtime. A test
//! binary of its own: the global subscriber is set once per process, and
//! tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).
//! Nothing here touches `/srv`, apt or the network: every program is a
//! stand-in on a temporary root.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::log::RunLog;
use sb_python_env::testing::Tree;
use sb_python_env::{Force, reconcile_venv};
use sb_term_ui::{Labels, Options, Runner, Target};

/// Tests that write stand-in programs and run them hold this from writing
/// them until their run is over (ETXTBSY otherwise).
static SPAWNS: Mutex<()> = Mutex::new(());

/// A user no host has: the ownership pass is skipped, so a test runs
/// without root.
const UNKNOWN: &str = "sb-rust-user-that-does-not-exist-7fcd24d1";

/// The process's one run log, in a directory kept for the process.
fn run_log() -> &'static (RunLog, tempfile::TempDir) {
    static LOG: OnceLock<(RunLog, tempfile::TempDir)> = OnceLock::new();
    LOG.get_or_init(|| {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
        (log, dir)
    })
}

/// The log's text so far.
fn log_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("sb.log")).expect("log")
}

/// A forced rebuild for `user`, in a step, as `sb reinstall-venv` runs
/// it: inside the run's span, on a current-thread runtime. The screen
/// comes back with the step's error, if any.
fn reinstall(tree: &Tree, user: &str) -> (Option<String>, String) {
    let (log, _) = run_log();
    let _run = log.enter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let term = indicatif::InMemoryTerm::new(80, 400);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let shutdown = Shutdown::new();
        let mut runner = Runner::start("sb test", &shutdown, None, options);
        let env = tree.env();
        let mut error = None;
        let labels = Labels::new("Reinstalling", "Reinstalled");
        let _ = runner
            .step(labels, async |step| {
                let result = reconcile_venv(step, &env, Some(user), Force::Venv).await;
                error = result.as_ref().err().map(|err| format!("{err:#}"));
                result.map(|_| ()).map_err(anyhow::Error::from)
            })
            .await;
        runner.finish();
        (error, term.contents())
    })
}

/// Every line of a run that gives the tree to the Saltbox user carries
/// the run id, the ownership lines from the blocking pool's threads
/// included (the maintainer, 2026-11-15: grepping a run id missed them).
#[test]
fn every_line_of_an_ownership_pass_carries_the_run_id() {
    // Ownership is given only as root (check.sh runs the tests so).
    if sb_cli_core::root::effective_uid() != 0 {
        return;
    }
    let (log, dir) = run_log();
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = Tree::new();
    let _ = tree.healthy();
    let (error, screen) = reinstall(&tree, "nobody");
    assert_eq!(error, None, "{screen}");
    let text = log_text(dir.path());
    let run = format!("run{{run={}}}", log.id());
    for record in ["recursive ownership", "ownership path="] {
        assert!(text.contains(record), "no {record:?} line:\n{text}");
    }
    let without: Vec<&str> = text.lines().filter(|line| !line.contains(&run)).collect();
    assert!(
        without.is_empty(),
        "lines without {run}:\n{}",
        without.join("\n")
    );
}

/// The children's start records show the values sb computes, the uv pip
/// sync's venv Python and requirements file among them, and nothing as
/// `[ARG]`: only credentials are masked (the maintainer, 2026-11-15).
#[test]
fn the_childrens_records_show_the_paths_sb_computes() {
    let (_, dir) = run_log();
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let tree = Tree::new();
    let _ = tree.healthy();
    let (error, screen) = reinstall(&tree, UNKNOWN);
    assert_eq!(error, None, "{screen}");
    let venv = std::fs::read_link(&tree.layout.active_venv).expect("link");
    let python = venv.join("bin/python3");
    let text = log_text(dir.path());
    // This tree's run: the log holds the other test's too.
    let root = tree.root.path().display().to_string();
    let sync = text
        .lines()
        .find(|line| {
            line.contains("child started") && line.contains(" pip sync ") && line.contains(&root)
        })
        .unwrap_or_else(|| panic!("no uv pip sync record:\n{text}"));
    for shown in [
        format!("--python {}", python.display()),
        tree.layout.lock.display().to_string(),
        "masked=0".to_string(),
    ] {
        assert!(sync.contains(&shown), "{shown:?} not in {sync}");
    }
    assert!(!text.contains("[ARG]"), "{text}");
}
