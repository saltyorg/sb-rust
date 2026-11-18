//! Interruption and terminal restore end to end, through the `demo`
//! binary on a pseudo-terminal (term-ui spec, "Cancellation", "Terminal
//! restore"; success criterion 6). The demo signals itself while its
//! second step runs. After it, `stty -a` shows whether the terminal's
//! modes were put back. Terminal runs go through util-linux `script`: why
//! not portable-pty, a dependency term-ui doesn't have (only sb's and
//! tui's tests do; crates/sb/tests/colour.rs); why not rexpect, the spec settled on
//! `script` for term-ui's PTY tests (docs/specs/term-ui.md, testing).
//! The modes are read from `stty -a`'s output by whole token, as the PTY
//! is `script`'s and the test can't `tcgetattr` it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Whether the `stty -a` output after the run sets `mode` (`echo`, not
/// `-echo`).
fn mode_on(text: &str, mode: &str) -> bool {
    let modes = text.rsplit("EXIT=").next().unwrap_or_default();
    modes.split_whitespace().any(|m| m == mode)
}

/// The echo and the control-character echo are on again, as before the
/// run.
fn echo_restored(text: &str) -> bool {
    mode_on(text, "echo") && mode_on(text, "echoctl")
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
    assert!(echo_restored(&text), "{text:?}");
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}

/// Ctrl-C twice, while the step is slow to stop: cli-core's hard exit
/// with 130, after the restore hook put the terminal back. Mid-run the
/// echo and the control-character echo are off (the demo runs as a job
/// of the same process group, still in the foreground).
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
    let mid_run_has = |mode: &str| mid_run.split_whitespace().any(|m| m == mode);
    assert!(
        mid_run_has("-echoctl"),
        "control-character echo not off mid-run: {text:?}"
    );
    assert!(mid_run_has("-echo"), "echo not off mid-run: {text:?}");
    // The hard exit: cli-core's ending, which would print "interrupted",
    // is skipped (no time bound: a busy machine could miss it).
    let plain = console::strip_ansi_codes(&text);
    assert!(!plain.contains("✗ interrupted"), "{text:?}");
    assert!(echo_restored(&text), "{text:?}");
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}

/// SIGTERM: exit 143, also marked interrupted.
#[test]
fn sigterm_ends_143() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal("\"$DEMO\" --terminate; echo EXIT=$?; stty -a", dir.path());
    assert_eq!(exit_code(&text), Some(143), "{text:?}");
    assert!(text.contains("interrupted · "), "{text:?}");
    assert!(echo_restored(&text), "{text:?}");
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
    assert!(echo_restored(&text), "{text:?}");
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
/// cursor never hidden and the echoes untouched.
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
    assert!(echo_restored(&text), "{text:?}");
}

/// Piped, so std sees how the demo ended: one signal ends it by that
/// signal after the run record is written, and so does the forced exit
/// on a second (exec decision 7); `$?` above still reads 130 and 143.
#[test]
fn an_interrupted_run_dies_of_its_signal() {
    use rustix::process::Signal;
    use std::os::unix::process::ExitStatusExt;
    for (flag, signal, record) in [
        ("--interrupt", Signal::INT, Some("interrupted (exit 130)")),
        ("--terminate", Signal::TERM, Some("interrupted (exit 143)")),
        ("--interrupt-twice", Signal::INT, None),
    ] {
        let dir = tempfile::tempdir().expect("temp dir");
        let out = Command::new(demo())
            .arg(flag)
            .env("SB_LOG_DIR", dir.path())
            .output()
            .expect("run demo");
        assert_eq!(
            out.status.signal(),
            Some(signal.as_raw()),
            "{flag}: {out:?}"
        );
        let log = std::fs::read_dir(dir.path())
            .expect("log dir")
            .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("log"))
            .collect::<String>();
        match record {
            Some(outcome) => assert!(
                log.contains(&format!("outcome=\"{outcome}\"")),
                "{flag}: {log}"
            ),
            None => assert!(
                !log.contains("run finished"),
                "{flag}: the forced exit skips the record: {log}"
            ),
        }
    }
}

/// What a user types during a live run: a word and Enter, then another.
const TYPED: &str = "typedxyz\rmorexyz\r";
/// What the user types once sb has ended, for the shell's `read`.
const AFTER: &str = "afterxyz\r";

/// Typing and pressing Enter mid-run leaves no stale line: the terminal
/// doesn't echo the keys into the live area (ui-M2; term-ui spec,
/// "Terminal restore"), not even the newline with `echonl` set, and
/// both echoes are on again after the run. The keys are sent through
/// `script`'s stdin once the run has quieted the terminal. At the end
/// they are discarded, at an ordinary ending and at the forced exit of a
/// second Ctrl-C alike: the shell's `read` after the run gets only what
/// is typed once sb has ended.
#[test]
fn keys_typed_during_a_live_run_are_discarded_at_its_end() {
    for (flag, code) in [("", 0), ("--interrupt-twice", 130)] {
        let text = type_during_a_run(flag);
        assert_eq!(exit_code(&text), Some(code), "{flag}: {text:?}");
        let (during, after) = text.split_once("EXIT=").unwrap_or((&text, ""));
        assert!(
            !during.contains("typedxyz") && !during.contains("morexyz"),
            "{flag}: the keys were echoed into the live area: {text:?}"
        );
        assert!(
            !after.contains("typedxyz") && !after.contains("morexyz"),
            "{flag}: the keys reached the shell after the run: {text:?}"
        );
        assert!(echo_restored(&text), "{flag}: {text:?}");
        assert!(mode_on(&text, "echonl"), "{flag}: {text:?}");
        assert!(
            text.contains("GOT=afterxyz"),
            "{flag}: the shell read no line: {text:?}"
        );
    }
}

/// Run the demo with `flag` on a terminal, type [`TYPED`] once it has
/// switched the echo off, and [`AFTER`] once it has ended; returns all
/// that `script` printed.
fn type_during_a_run(flag: &str) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut child = Command::new("timeout")
        .args([
            "30",
            "script",
            "-qec",
            // Mid-run: polled until the demo has switched the echo off,
            // then the marker (only then) asks the test to type. After the
            // run, the second marker asks for the line `read` gets.
            &format!(
                "stty cols 100 rows 30 echonl; \"$DEMO\" {flag} & \
                 for _ in $(seq 100); do \
                   stty -a | grep -qw -- -echo && {{ echo READY-TO-TYPE; break; }}; sleep 0.05; \
                 done; \
                 wait $!; echo EXIT=$?; stty -a; echo READY-AFTER; read -r typed; echo GOT=$typed"
            ),
            "/dev/null",
        ])
        .env("DEMO", demo())
        .env("SB_LOG_DIR", dir.path())
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run script");
    let output = Arc::new(Mutex::new(Vec::new()));
    let mut stdout = child.stdout.take().expect("stdout");
    let reader = {
        let output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut chunk = [0; 4096];
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                output
                    .lock()
                    .expect("output")
                    .extend_from_slice(&chunk[..read]);
            }
        })
    };
    let printed = || String::from_utf8_lossy(&output.lock().expect("output")).into_owned();
    let wait_for = |marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !printed().contains(marker) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        printed().contains(marker)
    };
    // Else the keys would land after the run and prove nothing.
    assert!(
        wait_for("READY-TO-TYPE"),
        "{flag}: the echo was never off mid-run: {:?}",
        printed()
    );
    // Kept open until the run ends: `script` ends the session at its end.
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(TYPED.as_bytes()).expect("type");
    assert!(
        wait_for("READY-AFTER"),
        "{flag}: the run never ended: {:?}",
        printed()
    );
    stdin.write_all(AFTER.as_bytes()).expect("type after");
    reader.join().expect("reader");
    drop(stdin);
    let _ = child.wait();
    printed()
}

/// A failed run puts the terminal back too.
#[test]
fn a_failed_run_restores_the_terminal() {
    let dir = tempfile::tempdir().expect("temp dir");
    let text = on_a_terminal("\"$DEMO\" --fail; echo EXIT=$?; stty -a", dir.path());
    assert_eq!(exit_code(&text), Some(1), "{text:?}");
    assert!(echo_restored(&text), "{text:?}");
    assert!(cursor_hidden_then_shown(&text), "{text:?}");
}
