//! `sb docker ps` interrupted (docker spec, success criterion 6): with an
//! inspect held open by sb-docker's fake Engine API, a SIGINT or SIGTERM
//! ends the run within seconds with cli-core's "interrupted" block and 130
//! or 143, nothing on stdout (table or JSON), and the run log saying so.
//! `ps` has no runner and changes no terminal state (no raw mode, no
//! hidden cursor), so the runs are piped: there is nothing to restore, and
//! stdout is checked apart from stderr. Root runs (`SB_TEST_ROOT=1`).

mod common;
mod root_gate;

use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use sb_docker::testing::{FakeEngine, Options};

/// sb, killed if a test fails before it exits. std's `Child` has no
/// `Drop`; as cli-core's signal tests.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// How long sb may take to end after the signal: the requests are dropped
/// at once, so seconds, not bollard's 120 s request timeout (which a
/// removed cancellation would wait for).
const ENDS_WITHIN: Duration = Duration::from_secs(5);

/// Run `sb docker ps args` against a fake that holds every inspect, send
/// `signal` once an inspect is open, and return its exit code, stdout,
/// stderr and run log.
fn interrupted(args: &[&str], signal: Signal) -> (Option<i32>, String, String, String) {
    let engine = FakeEngine::start(Options {
        hold: true,
        ..Options::default()
    })
    .expect("fake Engine API");
    let logs = tempfile::tempdir().expect("temp dir");
    let mut child = KillOnDrop(
        common::sb()
            .args(["docker", "ps"])
            .args(args)
            .env("DOCKER_HOST", engine.host())
            .env("SB_LOG_DIR", logs.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run sb"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.open_inspects() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(engine.open_inspects() > 0, "no inspect arrived");
    let pid = Pid::from_raw(child.0.id().try_into().expect("pid")).expect("pid");
    kill_process(pid, signal).expect("kill");
    let sent = Instant::now();
    while child.0.try_wait().expect("try_wait").is_none() {
        assert!(
            sent.elapsed() < ENDS_WITHIN,
            "still running {:?} after the signal",
            sent.elapsed()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = child.0.wait().expect("wait");
    let read = |pipe: Option<&mut dyn std::io::Read>| {
        let mut text = String::new();
        if let Some(pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    };
    let stdout = read(child.0.stdout.as_mut().map(|p| p as &mut dyn std::io::Read));
    let stderr = read(child.0.stderr.as_mut().map(|p| p as &mut dyn std::io::Read));
    let log = std::fs::read_to_string(logs.path().join("sb.log")).unwrap_or_default();
    (status.code(), stdout, stderr, log)
}

#[test]
fn sigint_and_sigterm_end_the_run_as_interrupted() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        for args in [&[][..], &["--json"]] {
            let (_, stdout, stderr, log) = interrupted(args, signal);
            assert!(stdout.is_empty(), "{signal:?} {args:?}: {stdout}");
            assert!(
                stderr.contains("interrupted"),
                "{signal:?} {args:?}: {stderr}"
            );
            assert_eq!(stderr.matches('✗').count(), 1, "{stderr}");
            assert!(
                log.contains(&format!(r#"outcome="interrupted (exit {code})""#)),
                "{log}"
            );
        }
    }
}
