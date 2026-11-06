//! `attached` refused while a term-ui runner is live, in a test binary of
//! its own: whether one is live is process-wide (term-ui's
//! `runner_live`), so a runner here would refuse the other attached
//! tests' children.

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, SpawnCause};

/// No attached child while a term-ui runner draws (it would fight the
/// child for the terminal).
#[tokio::test]
async fn a_live_runner_refuses_it() {
    let shutdown = Shutdown::new();
    let term = indicatif::InMemoryTerm::new(10, 80);
    let options = sb_term_ui::Options {
        target: sb_term_ui::Target::InMemory {
            term,
            colour: false,
        },
    };
    let run = sb_term_ui::Runner::start("sb test", &shutdown, None, options);
    let err = Cmd::new("true")
        .attached(&shutdown)
        .await
        .expect_err("refused");
    assert!(matches!(err.cause(), SpawnCause::RunnerLive), "{err}");
    run.finish();
}
