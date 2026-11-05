//! An attached child and signals sb itself receives, in a test binary of
//! its own: it listens for SIGINT, SIGTERM and SIGQUIT (cli-core's
//! `listen_for_signals`), which is process-wide. The child sends the
//! signal to sb (this process, its parent) and to itself, as the
//! terminal sends Ctrl-C or Ctrl-\ to both, then survives it or dies of
//! it ("wait and cooperative exit", exec decision 2). Through a real
//! terminal: exec-demo's tests.

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending};
use signal_hook::consts::SIGINT;

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script).arg("sh")
}

#[tokio::test]
async fn held_signals_follow_the_child() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    // A Ctrl-\ the child catches and exits on: its own ending, sb goes on
    // (and sb, holding it, didn't take it as the hard escape).
    // The pause lets sb's handler run before the child's own copy ends it
    // (spec, known limits: a child that catches a held signal and exits
    // before sb's own copy is delivered).
    let ran = sh("trap 'exit 0' QUIT; kill -QUIT $PPID; sleep 0.2; kill -QUIT $$; sleep 1")
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Exited(0));
    assert!(!shutdown.interrupted());
    assert!(!shutdown.token().is_cancelled());
    // A Ctrl-C the child dies of: the run is interrupted by it. Last: once
    // interrupted, a further signal would force sb's exit.
    let ran = sh("kill -INT $PPID; sleep 0.2; kill -INT $$; sleep 1")
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert_eq!(shutdown.signal(), Some(SIGINT));
    assert!(shutdown.token().is_cancelled());
}
