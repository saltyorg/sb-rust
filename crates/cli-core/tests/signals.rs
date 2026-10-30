//! SIGINT and SIGTERM against a real process (spec: "Cancellation").
//!
//! Each test re-runs this test binary as the child (filtered to one test,
//! with `SB_SIGNAL_CHILD` set), so the signals reach a separate process and
//! never the test harness.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use sb_cli_core::cancel::Shutdown;

const CHILD: &str = "SB_SIGNAL_CHILD";

type Lines = std::io::Lines<BufReader<ChildStdout>>;

/// In the child: listen for signals, say so, wait for cancellation, report
/// it, then stay busy (as if cleaning up) for longer than any test waits.
fn child_main() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    println!("ready");
    std::io::stdout().flush().expect("flush");
    let token = shutdown.token();
    let start = Instant::now();
    while !token.is_cancelled() && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "cancelled {:?} interrupted {}",
        shutdown.requested(),
        shutdown.interrupted()
    );
    std::io::stdout().flush().expect("flush");
    std::thread::sleep(Duration::from_secs(20));
}

fn spawn(test: &str) -> (Child, Lines) {
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    // libtest prints "test <name> ... " (no newline) before the child's own
    // output, so "ready" ends a line rather than being one.
    for line in lines.by_ref() {
        if line.expect("line").ends_with("ready") {
            break;
        }
    }
    (child, lines)
}

fn send(child: &Child, signal: Signal) {
    let pid = Pid::from_raw(child.id().try_into().expect("pid")).expect("pid");
    kill_process(pid, signal).expect("kill");
}

fn next_line(lines: &mut Lines) -> String {
    lines.next().expect("a line").expect("utf-8")
}

/// The first SIGINT cancels with code 130 and the run goes on (cleanup);
/// a second SIGINT ends it at once with the same code.
#[test]
fn sigint_cancels_then_a_second_exits_130() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("sigint_cancels_then_a_second_exits_130");
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    let start = Instant::now();
    send(&child, Signal::INT);
    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(130));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "second signal took {:?}",
        start.elapsed()
    );
}

/// SIGTERM works the same way with 143, also for the second signal.
#[test]
fn sigterm_cancels_then_a_second_exits_143() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("sigterm_cancels_then_a_second_exits_143");
    send(&child, Signal::TERM);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(143) interrupted true"
    );
    send(&child, Signal::TERM);
    assert_eq!(child.wait().expect("wait").code(), Some(143));
}

/// The first signal decides the code: SIGINT then SIGTERM exits 130.
#[test]
fn the_first_signal_decides_the_code() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("the_first_signal_decides_the_code");
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    send(&child, Signal::TERM);
    assert_eq!(child.wait().expect("wait").code(), Some(130));
}

/// The reverse order: SIGTERM then SIGINT exits 143.
#[test]
fn a_first_sigterm_decides_143() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("a_first_sigterm_decides_143");
    send(&child, Signal::TERM);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(143) interrupted true"
    );
    send(&child, Signal::INT);
    assert_eq!(child.wait().expect("wait").code(), Some(143));
}
