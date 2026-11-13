//! `attached` with real children (exec spec, `attached`; plan Task 8b):
//! sb's own stdin, stdout and stderr and sb's process group, cli-core's
//! hold on SIGINT and SIGQUIT while it runs, cancellation and timeouts by
//! pid. Real signals from a terminal are exec-demo's tests
//! (`tests/demo_signals.rs`); here, what needs no terminal.

use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending, SpawnCause};

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script).arg("sh")
}

#[tokio::test]
async fn an_exit_code_is_the_outcome() {
    let shutdown = Shutdown::new();
    let ran = sh("exit 3").attached(&shutdown).await.expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Exited(3));
    assert!(ran.check().is_err(), "exit 3 is a failure once checked");
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let shutdown = Shutdown::new();
    let err = Cmd::new("no-such-program-sb-test")
        .attached(&shutdown)
        .await
        .expect_err("not found");
    assert!(
        matches!(err.cause(), SpawnCause::Io(io) if io.kind() == std::io::ErrorKind::NotFound),
        "{err}"
    );
}

/// The child's stdin is sb's own (decision 11): bytes are refused before
/// anything is spawned.
#[tokio::test]
async fn stdin_bytes_are_refused() {
    let shutdown = Shutdown::new();
    let err = Cmd::new("cat")
        .stdin(b"x".to_vec())
        .attached(&shutdown)
        .await
        .expect_err("refused");
    assert!(
        matches!(err.cause(), SpawnCause::StdinRefused("attached")),
        "{err}"
    );
}

/// No `GIT_TERMINAL_PROMPT` or ssh's askpass rule (git and ssh may ask the
/// user, on sb's terminal), and
/// `COLORTERM` from the profile: removed under the tests' plain theme
/// (`ascii`), whatever the command set.
#[tokio::test]
async fn the_environment_follows_the_rules() {
    let shutdown = Shutdown::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("env");
    let _ = sh("env > \"$1\"")
        .value(file.to_string_lossy().into_owned())
        .env("COLORTERM", "truecolor")
        .attached(&shutdown)
        .await
        .expect("ran");
    let env = std::fs::read_to_string(&file).expect("env");
    assert!(!env.contains("GIT_TERMINAL_PROMPT="), "{env}");
    assert!(!env.lines().any(|l| l.starts_with("COLORTERM=")), "{env}");
    // ssh may ask on sb's terminal too: the askpass rule is for children
    // without one (exec-M1).
    assert!(!env.lines().any(|l| l == "SSH_ASKPASS=/bin/false"), "{env}");
    assert!(
        !env.lines().any(|l| l == "SSH_ASKPASS_REQUIRE=force"),
        "{env}"
    );
}

/// The hold is on while the child runs, and only then.
#[tokio::test]
async fn the_signals_are_held_while_it_runs() {
    let shutdown = Shutdown::new();
    let watch = shutdown.clone();
    let seen = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        watch.held()
    });
    let ran = sh("sleep 0.4").attached(&shutdown).await.expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Exited(0));
    assert!(seen.await.expect("watched"), "held while it ran");
    assert!(!shutdown.held(), "released after");
}

/// Cancelling the token stops the child by its pid (SIGTERM, which this
/// one turns into its own exit): interrupted, at once.
#[tokio::test]
async fn cancellation_reaches_the_childs_pid() {
    let shutdown = Shutdown::new();
    let cancel = shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.signalled(15);
    });
    let start = Instant::now();
    let ran = sh("trap 'exit 7' TERM; while :; do sleep 0.1; done")
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

/// A stopped child gets SIGCONT with the SIGTERM, so it acts on it within
/// the grace rather than wait for the SIGKILL.
#[tokio::test]
async fn a_stopped_child_is_continued_to_take_the_sigterm() {
    let shutdown = Shutdown::new();
    let cancel = shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.signalled(15);
    });
    let start = Instant::now();
    let ran = sh("trap 'exit 7' TERM; kill -STOP $$; while :; do sleep 0.1; done")
        .grace(Duration::from_secs(10))
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

/// A child ignoring SIGTERM is `SIGKILLed` after the grace; the timeout
/// works by pid the same way.
#[tokio::test]
async fn a_timeout_reaches_the_childs_pid() {
    let shutdown = Shutdown::new();
    let start = Instant::now();
    let ran = sh("trap '' TERM; while :; do sleep 0.1; done")
        .timeout(Duration::from_millis(200))
        .grace(Duration::from_millis(300))
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::TimedOut);
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
}

/// A run already interrupted starts no interactive child: not started,
/// the outcome interrupted.
#[tokio::test]
async fn an_interrupted_run_starts_no_child() {
    let shutdown = Shutdown::new();
    shutdown.signalled(15);
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("ran");
    let ran = sh("touch \"$1\"")
        .value(file.to_string_lossy().into_owned())
        .attached(&shutdown)
        .await
        .expect("an outcome");
    assert_eq!(ran.outcome.ending, Ending::Interrupted);
    assert!(!file.exists(), "nothing was spawned");
}

/// A child killed by a signal sb never received (here, by itself) isn't
/// sb's interruption: its own failure, and the run goes on.
#[tokio::test]
async fn a_child_signalled_on_its_own_is_not_an_interruption() {
    let shutdown = Shutdown::new();
    let ran = sh("kill -INT $$; sleep 1")
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Signalled(2));
    assert!(!shutdown.interrupted());
    assert!(!shutdown.token().is_cancelled());
}
