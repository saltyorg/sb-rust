//! What a reconcile leaves in sb's run log, through cli-core's own
//! `RunLog`, as `sb` opens it: the process's global subscriber, the run's
//! span entered on the main thread, a current-thread runtime. A test
//! binary of its own: the global subscriber is set once per process, and
//! tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).
//! Nothing here touches `/srv`, apt or the network: every program is a
//! stand-in on a temporary root.

use std::path::Path;
use std::sync::OnceLock;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::log::RunLog;
use sb_python_env::testing::Tree;
use sb_python_env::{Force, reconcile_venv};
use sb_term_ui::{Labels, Options, Runner, Target};

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
