//! A Ctrl-C that arrives just before an attached child starts, in a test
//! binary of its own (cli-core's `listen_for_signals` is process-wide;
//! hardening finding exec-L6): delivered but not yet decided when
//! `attached` first looks at the token, it is decided when the hold
//! starts (cli-core's `Shutdown::hold` settles it), and the child isn't
//! started. cli-core's `testing::signals` holds the signals thread's
//! decision back until the hold starts, so the raise is always undecided
//! at the first look and decided at the second, and the test asserts
//! that the hold met it so.

use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending};
use signal_hook::consts::SIGINT;

#[tokio::test]
async fn a_signal_decided_as_the_hold_starts_starts_no_child() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("ran");
    let cmd = Cmd::new("sh")
        .arg("-c")
        .arg("touch \"$1\"; sleep 5")
        .arg("sh")
        .shown_value(file.to_string_lossy().into_owned());
    // The handler only marks the delivery (raise(3) runs it before it
    // returns); the signals thread decides it only once the hold starts.
    let gate = sb_cli_core::testing::signals::decided_at_the_hold();
    signal_hook::low_level::raise(SIGINT).expect("raise");
    let ran = cmd.attached(&shutdown).await.expect("an outcome");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert_eq!(ran.outcome.elapsed, Duration::ZERO, "a child ran");
    assert_eq!(shutdown.signal(), Some(SIGINT));
    assert!(
        gate.held_a_delivery(),
        "the signal was decided before the hold: the second look wasn't the one that met it"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(!file.exists(), "a child ran");
}
