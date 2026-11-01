//! The failure block end to end, through the `demo` binary (term-ui spec,
//! "Failure", success criterion 4): shown once by the runner, exit 1,
//! cli-core adding nothing. The demo logs only under a temporary
//! `SB_LOG_DIR`. Terminal runs go through util-linux `script`, for the
//! reason in crates/sb/tests/colour.rs (why not portable-pty); rexpect
//! comes with the first long-running command (tasks/todo.md).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn demo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_demo"))
}

/// The demo with `--fail`, piped, logging under `log_dir` when given.
fn failing(log_dir: Option<&Path>) -> Output {
    let mut cmd = Command::new(demo());
    cmd.arg("--fail").env_remove("CLICOLOR_FORCE");
    match log_dir {
        Some(dir) => cmd.env("SB_LOG_DIR", dir),
        None => cmd.env_remove("SB_LOG_DIR"),
    };
    cmd.output().expect("run demo")
}

/// Piped: ✗ with the failure label, every line of the error and its cause,
/// the pointer to the log; shown once; exit 1 with no cli-core block.
#[test]
fn a_failure_is_shown_once_with_its_error_and_pointer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = failing(Some(dir.path()));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    let block = text
        .split_once("✗ Cloudflare token invalid ")
        .map(|(_, rest)| rest)
        .expect("the failed step's line");
    let lines: Vec<&str> = block.lines().skip(1).collect();
    assert_eq!(lines[0], "  the Cloudflare token was refused");
    assert_eq!(lines[1], "  HTTP 403 from the Cloudflare API");
    assert_eq!(lines[2], "  the token lacks Zone:Read");
    let pointer = format!("  full output: {}", dir.path().join("sb.log").display());
    assert!(lines[3].starts_with(&pointer), "{text}");
    // Then the sibling it stopped (which may print a last line before it
    // sees the stop) and their parent, last.
    let after = &lines[4..];
    let stopped = "✗ Checking Docker Hub login stopped ";
    assert!(after.iter().any(|l| l.starts_with(stopped)), "{text}");
    let last = lines.last().copied().unwrap_or_default();
    assert!(
        last.starts_with("✗ Checking credentials sub-step failed "),
        "{text}"
    );
    assert_eq!(text.matches("the Cloudflare token was refused").count(), 1);
    // One failure block: the sub-step inside the stopped sibling reads
    // "stopped" with no block (and no pointer) of its own.
    assert_eq!(text.matches("full output").count(), 1, "{text}");
    assert!(
        text.contains("\nChecking Docker Hub login: ✗ Logging in stopped "),
        "{text}"
    );
    assert!(!text.contains("caused by:"), "cli-core repeated it: {text}");
}

/// The step's own record carries the error and causes; the run record
/// says how the run ended.
#[test]
fn the_failed_step_logs_its_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _ = failing(Some(dir.path()));
    let log = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    let record = log
        .lines()
        .find(|l| l.contains("outcome=\"failed\"") && l.contains("step="))
        .unwrap_or_else(|| panic!("no failed step record in:\n{log}"));
    assert!(record.contains(" WARN "), "{record}");
    assert!(
        record.contains("the Cloudflare token was refused: HTTP 403 from the Cloudflare API"),
        "{record}"
    );
    assert!(log.contains("run finished outcome=\"exit 1\""), "{log}");
}

/// Without a log, no pointer.
#[test]
fn no_pointer_without_a_log() {
    let out = failing(None);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("  the token lacks Zone:Read\n"), "{text}");
    assert!(!text.contains("full output"), "{text}");
}

/// On a terminal: exit 1, the block and its pointer drawn, no cli-core
/// block after it.
#[test]
fn a_failure_on_a_terminal_exits_1_with_the_block() {
    let dir = tempfile::tempdir().expect("temp dir");
    let program = demo().display().to_string();
    let out = Command::new("script")
        .args([
            "-qec",
            &format!("stty cols 100 rows 30; exec '{program}' --fail"),
            "/dev/null",
        ])
        .env("TERM", "xterm-256color")
        .env("SB_LOG_DIR", dir.path())
        .output()
        .expect("run script");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("the token lacks Zone:Read"), "{text:?}");
    assert!(text.contains("full output: "), "{text:?}");
    assert!(
        !text.contains("caused by:"),
        "cli-core repeated it: {text:?}"
    );
}

/// A panicking step shows only the panic's own message: no failure block
/// and no pointer, even in plain mode with a log.
#[test]
fn a_panic_shows_no_failure_block() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new(demo())
        .arg("--panic")
        .env("SB_LOG_DIR", dir.path())
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(101), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("the demo was asked to panic"), "{text}");
    assert!(text.contains("✗ Checking credentials "), "{text}");
    assert!(!text.contains("full output"), "{text}");
}
