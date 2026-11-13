//! `sb docker start`, `stop` and `restart` interrupted on a terminal
//! (docker spec, "Start, stop and restart", success criterion 6; plan
//! Task 7): a SIGINT or SIGTERM while sb waits on a job, on a delayed
//! request answer or on `systemctl` ends the run quickly with the open
//! steps interrupted, sb dying by the signal (`$?` 130 or 143; exec
//! decision 7) and the terminal put back; a job asked for gets decision
//! 5's notice, the service check doesn't. A second signal while a
//! stand-in `systemctl` ignores SIGTERM: exec's forced-exit hook SIGKILLs
//! it. Terminal runs go through util-linux `script`, as the
//! validate-config signal tests (why not portable-pty or rexpect:
//! crates/term-ui/tests/signals.rs). That sb dies by the signal itself,
//! not by exiting 130 or 143, is cli-core's tests' to show (exec decision
//! 7; a shell prints the same `$?` for both). Root runs
//! (`SB_TEST_ROOT=1`), the controller sb-docker's fake, never the real
//! one; each stand-in `systemctl` leaves a marker, so a test knows the
//! stand-in ran and not the host's.

mod common;
mod processes;
mod root_gate;

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use processes::{KillOnDrop, StandIn, gone, wait_for};
use rustix::process::{Pid, Signal, kill_process};
use sb_docker::testing::controller::{Answer, FakeController};

const UNIT: &str = "saltbox_managed_docker_controller.service";
const JOB: &str = r#"{"job_id":"job-1","status":"pending"}"#;

/// How long sb itself may take to end after the signal (spec: within a
/// second): the request or the wait is dropped at once, and `systemctl`
/// gets exec's SIGTERM. Timed to sb's own end, not to `script`'s teardown.
const ENDS_WITHIN: Duration = Duration::from_secs(1);

/// The last drawing of the step row starting `start` (live frames are
/// redrawn with cursor codes, so a row is cut out rather than taken as a
/// line).
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

/// A test's world: a stand-in `systemctl` running `script` after checking
/// sb-go's arguments, the controller's unit file, the logs and sb's pid
/// file, all in one temporary directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let program = bin.join("systemctl");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = 'is-active {UNIT}' ] || exit 9\n: > \"$(dirname \"$0\")/../ran\"\n{script}\n"
            ),
        )
        .expect("systemctl");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(dir.path().join("unit.service"), "").expect("unit");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self { dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `sb <args>` on a terminal, its controller `url`, started from the
    /// terminal's shell as a user starts it; returns the running `script`
    /// and sb's pid once it is known.
    fn start(&self, url: &str, args: &str) -> (KillOnDrop, i32) {
        let program = common::sb().get_program().to_string_lossy().into_owned();
        let pid_file = self.file("sb.pid");
        let shell = format!(
            "stty cols 110 rows 30; \
             sh -c 'echo $$ > \"$1\"; exec \"$0\" {args}' '{program}' '{}'; \
             echo EXIT=$?; stty -a",
            pid_file.display()
        );
        let mut cmd = Command::new("timeout");
        common::apply_sb_env(&mut cmd);
        cmd.args(["30", "script", "-qec", &shell, "/dev/null"])
            .env("TERM", "xterm-256color")
            .env("SB_LOG_DIR", self.file("logs"))
            .env("SB_DOCKER_CONTROLLER_URL", url)
            .env("SB_DOCKER_CONTROLLER_UNIT", self.file("unit.service"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.file("bin").display()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = KillOnDrop(cmd.spawn().expect("run script"), None);
        let pid = wait_for(|| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
        })
        .expect("sb's pid");
        child.1 = Some(pid);
        (child, pid)
    }
}

/// Send `signal` to `pid`, time sb's own end, and read the terminal to its
/// end (script closes its output when the shell is done); the text and
/// how long sb took.
fn signal_and_read(child: &mut KillOnDrop, pid: i32, signal: Signal) -> (String, Duration) {
    kill_process(Pid::from_raw(pid).expect("pid"), signal).expect("kill");
    let sent = Instant::now();
    wait_for(|| gone(pid).then_some(())).expect("sb ended");
    let took = sent.elapsed();
    let mut text = String::new();
    child
        .0
        .stdout
        .take()
        .expect("script's output")
        .read_to_string(&mut text)
        .expect("read script's output");
    // sb has exited and been reaped: its pid may be reused.
    child.1 = None;
    (text, took)
}

/// The run's common endings: the code, the terminal put back, and the
/// interrupted rows given.
fn assert_interrupted(text: &str, code: i32, took: Duration, rows: &[&str]) {
    assert!(
        took < ENDS_WITHIN,
        "ended {took:?} after the signal\n{text}"
    );
    assert_eq!(exit_code(text), Some(code), "{text}");
    let plain = console::strip_ansi_codes(text);
    for label in rows {
        let row = last_row(&plain, &format!("✗ {label}"));
        assert!(
            row.is_some_and(|r| r.contains("interrupted")),
            "{label}: {row:?}\n{text}"
        );
    }
    assert!(echo_restored(text), "{text}");
    assert!(cursor_hidden_then_shown(text), "{text}");
}

/// SIGINT and SIGTERM while sb waits between polls: the wait and its
/// parent interrupted, decision 5's notice, 130 and 143, the terminal put
/// back, no poll after the signal.
#[test]
fn a_signal_mid_wait_interrupts_with_the_notice() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        let world = World::new("echo active");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB));
        fake.script("/job_status/job-1", Answer::ok(r#"{"status":"running"}"#));
        let (mut child, pid) = world.start(fake.url(), "docker start");
        wait_for(|| (fake.seen().len() >= 2).then_some(())).expect("the first poll");
        // Into the 5 s sleep between polls.
        std::thread::sleep(Duration::from_millis(50));
        let (text, took) = signal_and_read(&mut child, pid, signal);
        assert!(world.file("ran").exists(), "the stand-in ran");
        assert_interrupted(
            &text,
            code,
            took,
            &["Starting Docker containers", "Waiting for Docker start job"],
        );
        let plain = console::strip_ansi_codes(&text);
        assert!(
            plain.contains(
                "the controller may still be starting containers; sb docker ps shows where they are"
            ),
            "{signal:?}: {text}"
        );
        // An extra poll on the way out would show here.
        assert_eq!(fake.seen().len(), 2, "{signal:?}: no poll after it");
    }
}

/// SIGINT and SIGTERM while the controller holds the request's answer:
/// the request interrupted with the notice ("stopping" for a stop), 130
/// and 143.
#[test]
fn a_signal_during_the_request_interrupts_with_the_notice() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        let world = World::new("echo active");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB).after(Duration::from_secs(30)));
        let (mut child, pid) = world.start(fake.url(), "docker stop");
        wait_for(|| (!fake.seen().is_empty()).then_some(())).expect("the request");
        let (text, took) = signal_and_read(&mut child, pid, signal);
        assert!(world.file("ran").exists(), "the stand-in ran");
        assert_interrupted(
            &text,
            code,
            took,
            &["Stopping Docker containers", "Requesting Docker stop job"],
        );
        let plain = console::strip_ansi_codes(&text);
        assert!(
            plain.contains(
                "the controller may still be stopping containers; sb docker ps shows where they are"
            ),
            "{signal:?}: {text}"
        );
    }
}

/// SIGINT and SIGTERM while a stand-in `systemctl` runs: the check
/// interrupted, the stand-in stopped by exec (not left behind), no "not
/// running" and no notice, 130 and 143; nothing asked of the controller.
#[test]
fn a_signal_during_the_check_interrupts_without_a_notice() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        let world = World::new("echo $$ > \"$(dirname \"$0\")/../systemctl.pid\"; exec sleep 30");
        let fake = FakeController::start().expect("fake");
        let (mut child, pid) = world.start(fake.url(), "docker restart");
        let pid_file = world.file("systemctl.pid");
        let stand_in = StandIn(
            wait_for(|| {
                std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
            })
            .expect("systemctl running"),
        );
        let (text, took) = signal_and_read(&mut child, pid, signal);
        assert_interrupted(
            &text,
            code,
            took,
            &[
                "Restarting Docker containers",
                "Checking Docker controller service",
            ],
        );
        assert!(
            wait_for(|| gone(stand_in.0).then_some(())).is_some(),
            "{signal:?}: the stand-in was left running"
        );
        let plain = console::strip_ansi_codes(&text);
        assert!(!plain.contains("not running"), "{signal:?}: {text}");
        assert!(!plain.contains("may still be"), "{signal:?}: {text}");
        assert_eq!(fake.seen().len(), 0);
    }
}

/// A second signal while a stand-in `systemctl` ignores SIGTERM (exec's
/// grace would hold it 5 s): cli-core's forced exit runs exec's hook,
/// which SIGKILLs its group at once; sb ends well inside the grace and no
/// stand-in is left.
#[test]
fn a_second_signal_kills_a_stuck_systemctl() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new(
        "trap '' TERM INT; echo $$ > \"$(dirname \"$0\")/../systemctl.pid\"; exec sleep 30",
    );
    let fake = FakeController::start().expect("fake");
    let (mut child, pid) = world.start(fake.url(), "docker start");
    let pid_file = world.file("systemctl.pid");
    let stand_in = StandIn(
        wait_for(|| {
            std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
        })
        .expect("systemctl running"),
    );
    let stuck = stand_in.0;
    kill_process(Pid::from_raw(pid).expect("pid"), Signal::INT).expect("first signal");
    // exec has sent SIGTERM, which the stand-in ignores: still there.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!gone(stuck), "ignores SIGTERM");
    let (text, took) = signal_and_read(&mut child, pid, Signal::INT);
    assert!(
        took < ENDS_WITHIN,
        "ended {took:?} after the second signal\n{text}"
    );
    assert!(
        wait_for(|| gone(stuck).then_some(())).is_some(),
        "the stand-in was left running\n{text}"
    );
    assert!(echo_restored(&text), "{text}");
    assert!(cursor_hidden_then_shown(&text), "{text}");
    // Ended by the first signal (cli-core's forced exit; exec decision 7).
    assert_eq!(exit_code(&text), Some(130), "{text}");
}
