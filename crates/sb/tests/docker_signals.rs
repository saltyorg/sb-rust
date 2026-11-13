//! `sb docker ps` interrupted (docker spec, success criterion 6): with an
//! inspect held open by sb-docker's fake Engine API, a SIGINT or SIGTERM
//! ends the run within seconds with cli-core's "interrupted" block, sb
//! dying of that signal (exec decision 7), nothing on stdout (table or
//! JSON), and the run log saying so.
//! `ps` has no runner and changes no terminal state (no raw mode, no
//! hidden cursor), so the runs are piped: there is nothing to restore, and
//! stdout is checked apart from stderr. Root runs (`SB_TEST_ROOT=1`).

mod common;
mod dead_dns;
mod root_gate;

use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use dead_dns::DeadDns;
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
/// `signal` once an inspect is open, and return the signal that ended it,
/// stdout, stderr and run log.
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
    (status.signal(), stdout, stderr, log)
}

#[test]
fn sigint_and_sigterm_end_the_run_as_interrupted() {
    if !root_gate::requested() {
        return;
    }
    for (signal, code) in [(Signal::INT, 130), (Signal::TERM, 143)] {
        for args in [&[][..], &["--json"]] {
            let (status, stdout, stderr, log) = interrupted(args, signal);
            assert_eq!(
                status,
                Some(signal.as_raw()),
                "{signal:?} {args:?}: {stderr}"
            );
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

/// Ctrl-C ends `ps` promptly while the lookup of `DOCKER_HOST`'s host name
/// hangs on a dead DNS server (Task 60, as validate-config's config-L11):
/// bollard's connector resolves it on a tokio blocking thread, which the
/// runtime's drop would wait for (the namespaces are `dead_dns`'s).
#[test]
fn ctrl_c_ends_promptly_while_dns_hangs() {
    if !root_gate::requested() {
        return;
    }
    let dns = DeadDns::new();
    let logs = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("logs");
    let mut cmd = dns.sb(&["docker", "ps"]);
    cmd.env("SB_LOG_DIR", logs.path())
        .env("DOCKER_HOST", "tcp://docker.sb-test.invalid:2375")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = KillOnDrop(cmd.spawn().expect("run unshare"));
    let pid = child.0.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    while dead_dns::lookups(pid) == 0 && Instant::now() < deadline {
        // Ended early (no `ip`, a bind mount refused): say so.
        if let Some(status) = child.0.try_wait().expect("check sb") {
            panic!("ended before the lookup: {status:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dead_dns::lookups(pid) > 0, "the lookup open");
    let raw = Pid::from_raw(pid.try_into().expect("pid")).expect("pid");
    kill_process(raw, Signal::INT).expect("kill");
    let sent = Instant::now();
    let status = child.0.wait().expect("wait for sb");
    let took = sent.elapsed();
    let log = std::fs::read_to_string(logs.path().join("sb.log")).unwrap_or_default();
    assert!(took < ENDS_WITHIN, "ended {took:?} after the signal\n{log}");
    assert_eq!(
        status.signal(),
        Some(Signal::INT.as_raw()),
        "{status:?}\n{log}"
    );
    assert!(log.contains(r#"outcome="interrupted (exit 130)""#), "{log}");
}
