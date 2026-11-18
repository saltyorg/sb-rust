//! A Ctrl-C that arrives just before an attached child starts, in a test
//! binary of its own (cli-core's `listen_for_signals` is process-wide;
//! hardening finding exec-L6): delivered but not yet decided when
//! `attached` first looks at the token, it is decided when the hold
//! starts (cli-core's `Shutdown::hold` settles it), and the child isn't
//! started. The test can't choose which way the race goes: when the
//! signals thread decides the raise before `attached`'s first look, it
//! passes without reaching the second one (about a run in four, with the
//! check removed). It never fails with the check in place, so a green
//! run alone doesn't prove the second look.

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
    // The handler only marks the delivery; the signals thread decides it
    // later, by the time the hold starts at the latest.
    signal_hook::low_level::raise(SIGINT).expect("raise");
    let ran = cmd.attached(&shutdown).await.expect("an outcome");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert_eq!(ran.outcome.elapsed, Duration::ZERO, "a child ran");
    assert_eq!(shutdown.signal(), Some(SIGINT));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!file.exists(), "a child ran");
}
