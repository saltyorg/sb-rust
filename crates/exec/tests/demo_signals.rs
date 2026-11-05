//! Real signals to a process running a piped child (exec plan Task 4):
//! SIGINT and SIGTERM to exec-demo stop its child's group and exec-demo
//! dies of the signal (exec decision 7); a second signal, with the group
//! ignoring SIGTERM, makes exec's forced-exit hook SIGKILL the group. The
//! signals go to exec-demo's pid only, never to the test process. Piped,
//! so std reads how exec-demo ended; why not on a PTY as the plan said:
//! the child is piped and doesn't touch the terminal, so a PTY adds
//! nothing, and a shell's `$?` can't tell a death by signal from an exit
//! with 130.
//!
//! This test binary is a child subreaper, so the grandchild orphaned when
//! exec-demo's `sh` goes is adopted here and reaped by pid (see
//! tests/groups.rs).

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitOptions, getpgid, getpid, kill_process, set_child_subreaper,
    test_kill_process_group, waitpid,
};

fn subreaper() {
    static SET: OnceLock<()> = OnceLock::new();
    SET.get_or_init(|| set_child_subreaper(Some(getpid())).expect("subreaper"));
}

fn pid(raw: i32) -> Pid {
    Pid::from_raw(raw).expect("pid")
}

/// exec-demo, killed if a test fails before it ends.
struct Demo(Child);

impl Drop for Demo {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(file: &Path, mode: &str, grace_ms: u64) -> Demo {
    start_way(file, mode, grace_ms, None)
}

/// exec-demo running its child `way` (`pty`), or captured.
fn start_way(file: &Path, mode: &str, grace_ms: u64, way: Option<&str>) -> Demo {
    Demo(
        Command::new(env!("CARGO_BIN_EXE_exec-demo"))
            .arg(file)
            .arg(mode)
            .arg(grace_ms.to_string())
            .args(way)
            // Its "interrupted" block isn\'t the test\'s output.
            .stderr(Stdio::null())
            .spawn()
            .expect("exec-demo"),
    )
}

/// The leader's and grandchild's pids, once the child wrote them.
fn pids(file: &Path) -> (i32, i32) {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.ends_with('\n')
        {
            let pids: Vec<i32> = text
                .split_whitespace()
                .map(|p| p.parse().expect("pid"))
                .collect();
            return (pids[0], pids[1]);
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no pids");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reap `raw` once it is this process's (adopted) child; the signal that
/// ended it, if one did.
fn reap(raw: i32) -> Option<i32> {
    let start = Instant::now();
    loop {
        if let Ok(Some((_, status))) = waitpid(Some(pid(raw)), WaitOptions::NOHANG) {
            return status.terminating_signal();
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "{raw} never ended"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn send(demo: &Demo, signal: Signal) {
    kill_process(pid(demo.0.id().try_into().expect("pid")), signal).expect("kill");
}

/// The group exec-demo's child made, checked before anything is signalled.
fn check_group(leader: i32, grandchild: i32) {
    assert_eq!(
        getpgid(Some(pid(grandchild))).expect("getpgid"),
        pid(leader)
    );
}

fn wait(demo: &mut Demo) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = demo.0.try_wait().expect("try_wait") {
            return status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "exec-demo didn't end"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn one_signal_stops_the_group_and_ends_by_that_signal() {
    subreaper();
    for (signal, number) in [(Signal::INT, 2), (Signal::TERM, 15)] {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("pids");
        let mut demo = start(&file, "stops", 5000);
        let (leader, grandchild) = pids(&file);
        check_group(leader, grandchild);
        // Reaped as soon as it is adopted, so its zombie doesn't make the
        // group look alive to exec-demo for the whole grace.
        let reaper = std::thread::spawn(move || reap(grandchild));
        let sent = Instant::now();
        send(&demo, signal);
        let status = wait(&mut demo);
        let took = sent.elapsed();
        assert!(took < Duration::from_secs(2), "{signal:?}: {took:?}");
        assert_eq!(status.signal(), Some(number), "{signal:?}: {status:?}");
        assert_eq!(
            reaper.join().expect("reaper"),
            Some(15),
            "{signal:?}: the group got SIGTERM"
        );
        assert!(
            test_kill_process_group(pid(leader)).is_err(),
            "{signal:?}: group left"
        );
    }
}

/// The group ignores SIGTERM and the grace is long: the second signal's
/// forced exit SIGKILLs it, and exec-demo dies of the first signal.
#[test]
fn a_second_signal_kills_the_group_on_the_forced_exit() {
    subreaper();
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    let mut demo = start(&file, "ignores", 60_000);
    let (leader, grandchild) = pids(&file);
    check_group(leader, grandchild);
    send(&demo, Signal::INT);
    std::thread::sleep(Duration::from_millis(200));
    send(&demo, Signal::INT);
    let started = Instant::now();
    let status = wait(&mut demo);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(status.signal(), Some(2), "{status:?}");
    // exec-demo may have reaped its `sh` before it died; if not, the
    // orphan is this process's.
    // Adopted but not yet done dying (`Ok(None)`): ask again briefly.
    let asked = Instant::now();
    loop {
        match waitpid(Some(pid(leader)), WaitOptions::NOHANG) {
            Ok(Some((_, status))) => {
                assert_eq!(status.terminating_signal(), Some(9), "the leader: SIGKILL");
                break;
            }
            Err(Errno::CHILD) => break,
            Ok(None) if asked.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => panic!("the leader: {other:?}"),
        }
    }
    assert_eq!(reap(grandchild), Some(9), "the grandchild got SIGKILL");
    assert!(test_kill_process_group(pid(leader)).is_err(), "group left");
}

/// Without a signal the child ends on its own and so does exec-demo.
#[test]
fn an_untouched_run_ends_on_its_own() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    let mut demo = start(&file, "exits", 5000);
    let status = wait(&mut demo);
    assert_eq!(status.code(), Some(0), "{status:?}");
}

#[test]
fn a_wrong_call_says_how_to_call_it() {
    let bad = Command::new(env!("CARGO_BIN_EXE_exec-demo"))
        .args(["pids", "stops", "soon"])
        .output()
        .expect("exec-demo");
    assert_eq!(bad.status.code(), Some(2), "an unreadable grace");
    let out = Command::new(env!("CARGO_BIN_EXE_exec-demo"))
        .output()
        .expect("exec-demo");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("usage: exec-demo"));
}

/// The same through a PTY child (its own session): one signal stops the
/// session's group and exec-demo dies of it; a second, with the group
/// ignoring SIGTERM, makes the forced-exit hook SIGKILL it.
#[test]
fn a_pty_childs_session_is_stopped_and_killed() {
    subreaper();
    for (signal, number) in [(Signal::INT, 2), (Signal::TERM, 15)] {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("pids");
        let mut demo = start_way(&file, "stops", 5000, Some("pty"));
        let (leader, grandchild) = pids(&file);
        check_group(leader, grandchild);
        let reaper = std::thread::spawn(move || reap(grandchild));
        send(&demo, signal);
        let status = wait(&mut demo);
        assert_eq!(status.signal(), Some(number), "{signal:?}: {status:?}");
        assert_eq!(
            reaper.join().expect("reaper"),
            Some(15),
            "{signal:?}: SIGTERM"
        );
        assert!(
            test_kill_process_group(pid(leader)).is_err(),
            "{signal:?}: group left"
        );
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    let mut demo = start_way(&file, "ignores", 60_000, Some("pty"));
    let (leader, grandchild) = pids(&file);
    check_group(leader, grandchild);
    let reaper = std::thread::spawn(move || reap(grandchild));
    send(&demo, Signal::INT);
    std::thread::sleep(Duration::from_millis(200));
    send(&demo, Signal::INT);
    let status = wait(&mut demo);
    assert_eq!(status.signal(), Some(2), "{status:?}");
    assert_eq!(
        reaper.join().expect("reaper"),
        Some(9),
        "the grandchild got SIGKILL"
    );
    let asked = Instant::now();
    loop {
        match waitpid(Some(pid(leader)), WaitOptions::NOHANG) {
            Ok(Some(_)) | Err(Errno::CHILD) => break,
            Ok(None) if asked.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            other => panic!("the leader: {other:?}"),
        }
    }
    assert!(test_kill_process_group(pid(leader)).is_err(), "group left");
}

/// Through a PTY, a run that ends on its own ends exec-demo with 0.
#[test]
fn an_untouched_pty_run_ends_on_its_own() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    let mut demo = start_way(&file, "exits", 5000, Some("pty"));
    let status = wait(&mut demo);
    assert_eq!(status.code(), Some(0), "{status:?}");
}

/// A fourth argument other than `pty` is a usage error; `pty` runs the
/// child in a step (its end line on stderr), captured runs draw nothing.
#[test]
fn the_pty_argument_is_checked_and_draws_a_step() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    let wrong = Command::new(env!("CARGO_BIN_EXE_exec-demo"))
        .arg(&file)
        .args(["exits", "5000", "tty"])
        .output()
        .expect("exec-demo");
    assert_eq!(wrong.status.code(), Some(2), "{wrong:?}");
    for (way, drawn) in [(Some("pty"), true), (None, false)] {
        let out = Command::new(env!("CARGO_BIN_EXE_exec-demo"))
            .arg(&file)
            .args(["exits", "5000"])
            .args(way)
            .output()
            .expect("exec-demo");
        assert_eq!(out.status.code(), Some(0), "{way:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(stderr.contains("ran"), drawn, "{way:?}: {stderr}");
    }
}
