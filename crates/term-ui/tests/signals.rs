//! Interruption and terminal restore end to end, through the `demo`
//! binary on a pseudo-terminal (term-ui spec, "Cancellation", "Terminal
//! restore"; success criterion 6). The demo signals itself while its
//! second step runs. After it, `stty -a` shows whether the terminal's
//! modes were put back. Terminal runs go through util-linux `script`: why
//! not portable-pty, a reader thread and a dependency for a few lines of
//! test (crates/sb/tests/colour.rs); why not rexpect, the spec settled on
//! `script` for term-ui's PTY tests (docs/specs/term-ui.md, testing).

use std::path::{Path, PathBuf};
use std::process::Command;

fn demo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_demo"))
}

/// Run `shell` (with `$DEMO` set) on a terminal; returns all it printed.
fn on_a_terminal(shell: &str, log_dir: &Path) -> String {
    let out = Command::new("timeout")
        .args([
            "30",
            "script",
            "-qec",
            &format!("stty cols 100 rows 30; {shell}"),
            "/dev/null",
        ])
        .env("DEMO", demo())
        .env("SB_LOG_DIR", log_dir)
        .env("TERM", "xterm-256color")
        .output()
        .expect("run script");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The exit code `echo EXIT=$?` printed.
fn exit_code(text: &str) -> Option<i32> {
    let rest = text.split("EXIT=").nth(1)?;
    rest.trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// The control-character echo is on again, as before the run.
fn echoctl_restored(text: &str) -> bool {
    let modes = text.rsplit("EXIT=").next().unwrap_or_default();
    modes.contains("echoctl") && !modes.contains("-echoctl")
}

/// The cursor was hidden while live, and shown again after the last time.
fn cursor_hidden_then_shown(text: &str) -> bool {
    match (text.rfind("\x1b[?25l"), text.rfind("\x1b[?25h")) {
        (Some(hidden), Some(shown)) => shown > hidden,
        _ => false,
    }
}

/// Ctrl-C once: exit 130, the step marked interrupted, cli-core's block
/// shown once, the terminal put back.
#[test]
fn one_sigint_ends_130_with_the_terminal_restored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal("\"$DEMO\" --interrupt; echo EXIT=$?; stty -a", dir.path());
    assert_eq!(exit_code(&text), Some(130), "{text:?}");
    assert!(text.contains("interrupted · "), "the step's mark: {text:?}");
    // cli-core's block, once (its colour codes left out).
    let plain = console::strip_ansi_codes(&text);
    assert_eq!(plain.matches("✗ interrupted\r\n").count(), 1, "{text:?}");
    assert!(
        !text.contains("full output"),
        "no pointer of the step's own: {text:?}"
    );
    assert!(echoctl_restored(&text), "{text:?}");
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}

/// Ctrl-C twice, while the step is slow to stop: cli-core's hard exit
/// with 130, after the restore hook put the terminal back. Mid-run the
/// control-character echo is off (the demo runs as a job of the same
/// process group, still in the foreground).
#[test]
fn a_second_sigint_still_restores_the_terminal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal(
        // Mid-run modes: polled until the demo has quieted the terminal
        // (no fixed sleep, which a busy machine could outrun).
        "\"$DEMO\" --interrupt-twice & \
         for _ in $(seq 100); do stty -a | grep -q -- -echoctl && break; sleep 0.05; done; \
         stty -a; wait $!; echo EXIT=$?; stty -a",
        dir.path(),
    );
    assert_eq!(exit_code(&text), Some(130), "{text:?}");
    let mid_run = text.split("EXIT=").next().unwrap_or_default();
    assert!(
        mid_run.contains("-echoctl"),
        "echo not off mid-run: {text:?}"
    );
    // The hard exit: cli-core's ending, which would print "interrupted",
    // is skipped (no time bound: a busy machine could miss it).
    let plain = console::strip_ansi_codes(&text);
    assert!(!plain.contains("✗ interrupted"), "{text:?}");
    assert!(echoctl_restored(&text), "{text:?}");
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}

/// SIGTERM: exit 143, also marked interrupted.
#[test]
fn sigterm_ends_143() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal("\"$DEMO\" --terminate; echo EXIT=$?; stty -a", dir.path());
    assert_eq!(exit_code(&text), Some(143), "{text:?}");
    assert!(text.contains("interrupted · "), "{text:?}");
    assert!(echoctl_restored(&text), "{text:?}");
}

/// A background run (its own process group, job control on) leaves the
/// terminal alone: it neither changes the modes nor stops on SIGTTOU.
#[test]
fn a_background_run_leaves_the_terminal_alone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal(
        "set -m; \"$DEMO\" & wait $!; echo EXIT=$?; stty -a",
        dir.path(),
    );
    assert_eq!(exit_code(&text), Some(0), "{text:?}");
    assert!(echoctl_restored(&text), "{text:?}");
    assert!(
        !text.contains("\x1b[?25l"),
        "the cursor was hidden: {text:?}"
    );
}

/// Moved out of the foreground mid-run (as Ctrl-Z then `bg` would), sb
/// doesn't touch the terminal's modes at the end: from the background
/// that would stop it on SIGTTOU (exit 150, or a hang).
#[test]
fn leaving_the_foreground_mid_run_never_stops_on_sigttou() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal("\"$DEMO\" --leave-foreground; echo EXIT=$?", dir.path());
    assert_eq!(exit_code(&text), Some(0), "{text:?}");
    // The cursor is still shown from the background; the modes stay as the
    // demo left them, as no job-control shell here puts its own back.
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}

/// Plain on a terminal (`TERM=dumb`): the terminal is left alone, the
/// cursor never hidden and the control-character echo untouched.
#[test]
fn a_dumb_terminal_is_left_alone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal(
        "TERM=dumb \"$DEMO\" --interrupt; echo EXIT=$?; stty -a",
        dir.path(),
    );
    assert_eq!(exit_code(&text), Some(130), "{text:?}");
    assert!(
        !text.contains("\x1b[?25l"),
        "the cursor was hidden: {text:?}"
    );
    assert!(echoctl_restored(&text), "{text:?}");
}
