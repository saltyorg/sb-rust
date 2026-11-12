//! An attached child and a hangup sb itself receives (decision 2; exec
//! decision 12), in a test binary of its own: it listens for signals
//! (cli-core's `listen_for_signals`), which is process-wide, and a second
//! listener in `attached_signals.rs`'s process would act on that file's
//! signals too. While the child runs, a SIGHUP to sb is treated as a
//! SIGTERM is: not held, it cancels the token, which sends SIGTERM to the
//! child's pid; the run is interrupted by SIGHUP.

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending};
use signal_hook::consts::SIGHUP;

#[tokio::test]
async fn a_hangup_during_an_attached_child_is_treated_as_sigterm() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    // The child sends sb the hangup, then waits up to 5 s for a SIGTERM
    // (only exec's, as the hangup went to sb alone), which turns into its
    // exit; without one it exits 9.
    let ran = Cmd::new("sh")
        .arg("-c")
        .arg(
            "trap 'exit 7' TERM; kill -HUP $PPID; \
             i=0; while [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done; exit 9",
        )
        .attached(&shutdown)
        .await
        .expect("ran");
    // Interrupted, as a SIGTERM's cancellation (`attached.rs`).
    assert_eq!(
        ran.outcome.ending,
        Ending::Interrupted,
        "no SIGTERM reached the child (is SIGHUP ignored, the tests run under nohup?)"
    );
    assert!(ran.outcome.elapsed.as_secs() < 3, "{:?}", ran.outcome);
    assert_eq!(shutdown.signal(), Some(SIGHUP));
    assert!(shutdown.token().is_cancelled());
}
