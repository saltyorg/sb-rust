//! The failure block end to end, through the `demo` binary (term-ui spec,
//! "Failure", success criterion 4): shown once by the runner, exit 1,
//! cli-core adding nothing. The demo logs only under a temporary
//! `SB_LOG_DIR`. Terminal runs go through util-linux `script`, for the
//! reason in crates/sb/tests/colour.rs (why not portable-pty); not
//! rexpect either: every PTY test uses `script` (cli-core spec, testing;
//! `crates/sb/tests/validate_config_signals.rs`).

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
        .split_once("✗ Cloudflare token invalid   ")
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
    let stopped = "✗ Checking Docker Hub login   stopped · ";
    assert!(after.iter().any(|l| l.starts_with(stopped)), "{text}");
    let last = lines.last().copied().unwrap_or_default();
    assert!(
        last.starts_with("✗ Checking credentials   sub-step failed · "),
        "{text}"
    );
    assert_eq!(text.matches("the Cloudflare token was refused").count(), 1);
    // One failure block: the sub-step inside the stopped sibling reads
    // "stopped" with no block (and no pointer) of its own.
    assert_eq!(text.matches("full output").count(), 1, "{text}");
    assert!(
        text.contains("\nChecking Docker Hub login: ✗ Logging in   stopped · "),
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
    let out = on_a_terminal("--fail", dir.path());
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

/// A group sibling's panic on a terminal (ui-M3): std's message is the
/// last thing drawn. The live tree stops redrawing at once, though the
/// other sibling, in a group that doesn't stop on failure, works on for
/// about a second and the frames would otherwise keep coming; so nothing
/// overdraws the message.
#[test]
fn a_sibling_panic_stays_on_screen() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = on_a_terminal("--sibling-panic", dir.path());
    // The panic's exit code (`script -e` passes the demo's on).
    assert_eq!(out.status.code(), Some(101), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let message = "a sibling was asked to panic";
    let at = text
        .find(message)
        .unwrap_or_else(|| panic!("no message: {text:?}"));
    // After the message (and std's note under it) the terminal gets no
    // more drawing: no cursor movement, no line clearing, no spinner.
    let after = &text[at + message.len()..];
    for code in ["\x1b[1A", "\x1b[2K", "\x1b[J", "\r\x1b"] {
        assert!(
            !after.contains(code),
            "{code:?} after the message: {after:?}"
        );
    }
    assert!(
        !after.contains("Checking the other one"),
        "redrawn: {after:?}"
    );
    // The cursor is shown again at the end.
    assert!(after.contains("\x1b[?25h"), "cursor left hidden: {after:?}");
}

/// The demo's made-up token's prefix (`DEMO_TOKEN` in the demo).
const TOKEN: &str = "cf-demo";

/// The demo with `args` on a 100x30 pseudo-terminal, logging under
/// `log_dir`; what the terminal received.
fn on_a_terminal(args: &str, log_dir: &Path) -> Output {
    let program = demo().display().to_string();
    Command::new("script")
        .args([
            "-qec",
            &format!("stty cols 100 rows 30; exec '{program}' {args}"),
            "/dev/null",
        ])
        .env("TERM", "xterm-256color")
        .env("SB_LOG_DIR", log_dir)
        .output()
        .expect("run script")
}

/// A secret in a failure (validation quoting a malformed token read from
/// settings.yml) never shows: the output tail and the failure block show
/// the placeholder, piped and on a terminal, and the log withholds it too
/// (decision 8).
#[test]
fn a_secret_in_a_failure_never_shows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new(demo())
        .arg("--sensitive")
        .env("SB_LOG_DIR", dir.path())
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!text.contains(TOKEN), "{text}");
    assert!(
        text.contains("\nChecking Cloudflare token: cloudflare.token: [REDACTED]\n"),
        "{text}"
    );
    // Only the value is replaced; the rest of the error stays readable.
    let block = "  settings.yml: cloudflare.token is not a valid API token\n  \
                 got [REDACTED]: expected 40 characters, found 24\n";
    assert!(text.contains(block), "{text}");
    let terminal = on_a_terminal("--sensitive", dir.path());
    assert_eq!(terminal.status.code(), Some(1), "{terminal:?}");
    let screen = console::strip_ansi_codes(&String::from_utf8_lossy(&terminal.stdout)).into_owned();
    assert!(!screen.contains(TOKEN), "{screen:?}");
    let error = "settings.yml: cloudflare.token is not a valid API token";
    // The final block on the terminal: the placeholder in the cause, right
    // after the error's own line (box glyphs and spaces aside).
    let after = screen.rsplit(error).next().unwrap_or_default();
    let next = after.trim_start_matches(|c: char| c.is_whitespace() || "│├╰─".contains(c));
    assert!(
        next.starts_with("got [REDACTED]: expected 40 characters"),
        "{screen:?}"
    );
    // Neither run's log has it: the value replaced in place there too.
    let log = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    assert!(!log.contains(TOKEN), "{log}");
    assert!(
        log.contains("text=\"cloudflare.token: [REDACTED]\""),
        "{log}"
    );
    let record = format!("error=\"{error}: got [REDACTED]: expected 40 characters, found 24\"");
    assert!(log.contains(&record), "{log}");
}

/// A secret a command shows its user on purpose (`Line::new(..).secret(..)
/// .shown()`, as one creating a token would): on a terminal the user sees
/// it; piped (CI, the journal, a file) it is the placeholder, as stderr
/// there is a log; and it is never logged (decision 8).
#[test]
fn a_shown_secret_shows_only_on_a_terminal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let terminal = on_a_terminal("--new-token", dir.path());
    assert_eq!(terminal.status.code(), Some(0), "{terminal:?}");
    let screen = String::from_utf8_lossy(&terminal.stdout);
    assert!(
        screen.contains(&format!("new token: {TOKEN}")),
        "{screen:?}"
    );
    let piped = Command::new(demo())
        .arg("--new-token")
        .env("SB_LOG_DIR", dir.path())
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    let text = String::from_utf8_lossy(&piped.stderr);
    assert!(!text.contains(TOKEN), "{text}");
    assert!(text.contains("\n› new token: [REDACTED]\n"), "{text}");
    let log = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    assert!(!log.contains(TOKEN), "{log}");
    assert!(log.contains("text=\"new token: [REDACTED]\""), "{log}");
}

/// A failed child, piped (plain lines): how it ended before the step's
/// time, its message and last lines in the block; exit 1.
#[test]
fn a_failed_childs_plain_line_says_how_it_ended() {
    let out = Command::new(demo())
        .arg("--child-fail")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("SB_LOG_DIR")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    let block = text
        .split_once("✗ Reading accounts.yml   exit 3 · ")
        .map_or_else(
            || panic!("the failed step's line: {text}"),
            |(_, rest)| rest,
        );
    let lines: Vec<&str> = block.lines().skip(1).take(2).collect();
    assert_eq!(
        lines,
        [
            "  systemctl is-active [ARG] exited with code 3",
            "  inactive"
        ],
        "{text}"
    );
}
