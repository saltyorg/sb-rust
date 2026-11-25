//! `sb validate-config` interrupted on a terminal (config spec, success
//! criterion 8; term-ui criterion 6 with sb's own command): with the
//! credential checks held open by sb-config's fake, a SIGINT or SIGTERM
//! ends the run within seconds, the open steps marked interrupted, exit
//! 130 or 143, the terminal's modes and cursor put back, and the run log
//! saying so. Terminal runs go through util-linux `script`, as term-ui's
//! and sb's colour tests do (why not portable-pty or rexpect:
//! crates/term-ui/tests/signals.rs). Root runs (`SB_TEST_ROOT=1`).

mod common;
mod dead_dns;
mod root_gate;

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use dead_dns::DeadDns;
use rustix::process::{Pid, Signal, kill_process};
use sb_config::testing::{Answer, Fake, saltbox_root};

const ZONES: &str = r#"{"success":true,"result":[{"id":"z1","name":"example.com"}]}"#;

/// How long sb may take to end after the signal: the checks are dropped
/// at once, so seconds, not the 10 s request timeout and its retries.
const ENDS_WITHIN: Duration = Duration::from_secs(5);

/// `script` (under `timeout`), and sb itself once its pid is known,
/// killed if a test fails before they exit: SIGKILL to `timeout` doesn't
/// reach its children.
struct KillOnDrop(Child, Option<i32>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.1.and_then(Pid::from_raw) {
            let _ = kill_process(pid, Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The last drawing of the step row starting `start`: from there to the
/// next row's branch or mark. Live frames are redrawn with cursor codes,
/// not newlines, so one line of the output holds a whole tree; a row is
/// cut out of it rather than taken as a line.
fn last_row<'a>(plain: &'a str, start: &str) -> Option<&'a str> {
    let at = plain.rfind(start)?;
    let rest = &plain[at + start.len()..];
    let end = rest
        .find(['├', '╰', '│', '✓', '✗', '\r', '\n'])
        .unwrap_or(rest.len());
    Some(&plain[at..at + start.len() + end])
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

/// The echo and the control-character echo are on again (`stty -a` after
/// the run).
fn echo_restored(text: &str) -> bool {
    let modes = text.rsplit("EXIT=").next().unwrap_or_default();
    let on = |mode: &str| modes.split_whitespace().any(|m| m == mode);
    on("echo") && on("echoctl")
}

/// The cursor was hidden while live, and shown again after the last time.
fn cursor_hidden_then_shown(text: &str) -> bool {
    match (text.rfind("\x1b[?25l"), text.rfind("\x1b[?25h")) {
        (Some(hidden), Some(shown)) => shown > hidden,
        _ => false,
    }
}

/// Run `sb validate-config` on a terminal with both checks held by a
/// fake, send `signal` to sb once both are open, and return everything the
/// terminal showed and the run log. sb runs in the foreground of the
/// terminal's shell, as a user starts it (the terminal as its stdin,
/// signals at their defaults): a shell writes its own pid, then execs sb.
fn interrupted(signal: Signal, logs: &Path) -> (String, String) {
    let root = saltbox_root("valid").expect("root");
    let fake = Fake::start().expect("fake");
    fake.script("/client/v4/zones", Answer::json(200, ZONES).held());
    fake.script("/v2/users/login/", Answer::json(200, "{}").held());
    // Beside the log directory, not in it: sb refuses a log directory
    // holding files that aren't its logs.
    let pid_dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("pid dir");
    let pid_file = pid_dir.path().join("sb.pid");
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let shell = format!(
        "stty cols 100 rows 30; \
         sh -c 'echo $$ > \"$1\"; exec \"$0\" validate-config' '{program}' '{}'; \
         echo EXIT=$?; stty -a",
        pid_file.display()
    );
    let mut cmd = Command::new("timeout");
    common::apply_sb_env(&mut cmd);
    cmd.args(["30", "script", "-qec", &shell, "/dev/null"])
        .env("TERM", "xterm-256color")
        .env("SB_LOG_DIR", logs)
        .env("SB_SALTBOX_DIR", root.path())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = KillOnDrop(cmd.spawn().expect("run script"), None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.seen().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.seen().len(), 2, "both checks open");
    // Written before sb starts, so it is there; read until whole anyway.
    let pid = (0..100)
        .find_map(|_| {
            let pid = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok());
            if pid.is_none() {
                std::thread::sleep(Duration::from_millis(10));
            }
            pid
        })
        .expect("sb's pid");
    child.1 = Some(pid);
    kill_process(Pid::from_raw(pid).expect("pid"), signal).expect("kill");
    let sent = Instant::now();
    // Read to the end: script closes its output when the shell is done.
    let mut text = String::new();
    child
        .0
        .stdout
        .take()
        .expect("script's output")
        .read_to_string(&mut text)
        .expect("read script's output");
    // The shell is done, so sb has exited and been reaped: its pid may be
    // reused, so it is no longer killed on a failure.
    child.1 = None;
    assert!(
        sent.elapsed() < ENDS_WITHIN,
        "ended {:?} after the signal",
        sent.elapsed()
    );
    fake.release();
    let log = std::fs::read_to_string(logs.join("sb.log")).unwrap_or_default();
    (text, log)
}

/// SIGINT (Ctrl-C) and SIGTERM: 130 and 143; every open step, the
/// checks and their parents, interrupted with its whole label;
/// cli-core's "interrupted" block and the log's pointer once; the
/// terminal put back; the log saying so (term-ui criterion 6).
#[test]
fn a_signal_during_the_checks_interrupts_and_restores() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        let logs = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("logs");
        let (text, log) = interrupted(signal, logs.path());
        assert_eq!(exit_code(&text), Some(code), "{signal:?}: {text}");
        let plain = console::strip_ansi_codes(&text);
        for label in [
            "Validating Saltbox configuration",
            "Validating accounts.yml",
            "Validating Cloudflare API credentials",
            "Validating Docker Hub credentials",
        ] {
            let row = last_row(&plain, &format!("✗ {label}"));
            assert!(
                row.is_some_and(|r| r.contains("interrupted ·") && !r.contains('…')),
                "{signal:?} {label}: {row:?}\n{text}"
            );
        }
        assert_eq!(
            plain.matches("✗ interrupted\r\n").count(),
            1,
            "{signal:?}: {text}"
        );
        let pointer = format!("details: {}", logs.path().join("sb.log").display());
        assert_eq!(plain.matches(&pointer).count(), 1, "{signal:?}: {text}");
        assert!(echo_restored(&text), "{signal:?}: {text}");
        assert!(cursor_hidden_then_shown(&text), "{signal:?}: {text}");
        assert!(
            log.contains(&format!(r#"outcome="interrupted (exit {code})""#)),
            "{signal:?}: {log}"
        );
    }
}

/// Ctrl-C ends the run promptly while a host name lookup hangs on a dead
/// DNS server (config-L11; the namespaces are `dead_dns`'s).
#[test]
fn ctrl_c_ends_promptly_while_dns_hangs() {
    if !root_gate::requested() {
        return;
    }
    let dns = DeadDns::new();
    let root = saltbox_root("valid").expect("root");
    let logs = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("logs");
    let mut cmd = dns.sb(&["validate-config"]);
    cmd.env("SB_LOG_DIR", logs.path())
        .env("SB_SALTBOX_DIR", root.path())
        .env(
            "CLOUDFLARE_BASE_URL",
            "http://cloudflare.sb-test.invalid/client/v4/",
        )
        .env(
            "SB_DOCKERHUB_URL",
            "http://dockerhub.sb-test.invalid/v2/users/login/",
        )
        .stdin(Stdio::null());
    let mut child = KillOnDrop(cmd.spawn().expect("run unshare"), None);
    let pid = child.0.id();
    // Both checks are looking their host up.
    let deadline = Instant::now() + Duration::from_secs(10);
    while dead_dns::lookups(pid) < 2 && Instant::now() < deadline {
        // Ended early (no `ip`, a mount refused): say so, not
        // "both lookups open".
        if let Some(status) = child.0.try_wait().expect("check sb") {
            panic!("ended before both lookups: {status:?}\n{}", dns.output());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        dead_dns::lookups(pid),
        2,
        "both lookups open\n{}",
        dns.output()
    );
    let pid = i32::try_from(pid).expect("pid");
    kill_process(Pid::from_raw(pid).expect("pid"), Signal::INT).expect("kill");
    let sent = Instant::now();
    let status = child.0.wait().expect("wait for sb");
    let took = sent.elapsed();
    let log = std::fs::read_to_string(logs.path().join("sb.log")).unwrap_or_default();
    let output = dns.output();
    assert!(
        took < ENDS_WITHIN,
        "ended {took:?} after the signal\n{log}\n{output}"
    );
    // sb ends by the signal itself, which a shell shows as `$?` 130.
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(Signal::INT.as_raw()),
        "{status:?}\n{log}\n{output}"
    );
    assert!(
        log.contains(r#"outcome="interrupted (exit 130)""#),
        "{log}\n{output}"
    );
}
