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
/// Restore hooks the child registers: `run`, `removed`, `panic` (a
/// panicking hook, then one that prints) or `block` (one that prints, then
/// one that never returns). Empty (the default) registers none, which is
/// what keeps the first four tests checking that nothing else changed.
const HOOKS: &str = "SB_SIGNAL_HOOKS";

type Lines = std::io::Lines<BufReader<ChildStdout>>;

/// The child, killed if a test fails before it exits (otherwise it would
/// linger for up to 40 seconds). std's `Child` has no `Drop`; why not
/// tokio's `Command::kill_on_drop`: async-only, and this test drives a std
/// `Child` synchronously.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for KillOnDrop {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

/// In the child: listen for signals, say so, wait for cancellation, report
/// it, then stay busy (as if cleaning up) for longer than any test waits.
fn child_main() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    let restored = || {
        println!("restored");
        std::io::stdout().flush().expect("flush");
    };
    // Kept alive until the process exits; `removed` drops its guard.
    let mut hooks = Vec::new();
    match std::env::var(HOOKS).as_deref() {
        Ok("run") => hooks.push(shutdown.on_forced_exit(restored)),
        Ok("removed") => drop(shutdown.on_forced_exit(restored)),
        Ok("block") => {
            hooks.push(shutdown.on_forced_exit(restored));
            hooks.push(shutdown.on_forced_exit(|| std::thread::sleep(Duration::from_secs(60))));
        }
        Ok("panic") => {
            hooks.push(shutdown.on_forced_exit(|| panic!("a broken hook")));
            hooks.push(shutdown.on_forced_exit(restored));
        }
        _ => {}
    }
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

fn spawn(test: &str) -> (KillOnDrop, Lines) {
    spawn_with(test, "")
}

/// The child for `test`, registering `hooks` (see [`HOOKS`]).
fn spawn_with(test: &str, hooks: &str) -> (KillOnDrop, Lines) {
    // Guarded from the start, so a failure while waiting for "ready" kills
    // it too.
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().expect("test binary"))
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env(HOOKS, hooks)
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn child"),
    );
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

/// Interrupt the child twice; returns its exit code and the lines it
/// printed after the first signal was reported.
fn interrupt_twice(test: &str, hooks: &str) -> (Option<i32>, Vec<String>) {
    let (mut child, mut lines) = spawn_with(test, hooks);
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    send(&child, Signal::INT);
    let code = child.wait().expect("wait").code();
    (code, lines.map(|l| l.expect("utf-8")).collect())
}

/// A registered restore hook runs once before the second-signal exit,
/// whose code is unchanged (term-ui spec, decision 5).
#[test]
fn a_restore_hook_runs_once_before_the_second_signal_exit() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (code, after) = interrupt_twice(
        "a_restore_hook_runs_once_before_the_second_signal_exit",
        "run",
    );
    assert_eq!(code, Some(130));
    assert_eq!(after, ["restored"]);
}

/// A removed hook doesn't run: a runner that has ended mustn't restore
/// stale terminal state later.
#[test]
fn a_removed_restore_hook_does_not_run() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (code, after) = interrupt_twice("a_removed_restore_hook_does_not_run", "removed");
    assert_eq!(code, Some(130));
    assert!(after.is_empty(), "{after:?}");
}

/// A panicking hook can't stop the exit, nor the hooks after it.
/// (Its panic message in the test output is expected.)
#[test]
fn a_panicking_restore_hook_does_not_stop_the_exit() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (code, after) = interrupt_twice("a_panicking_restore_hook_does_not_stop_the_exit", "panic");
    assert_eq!(code, Some(130));
    assert_eq!(after, ["restored"]);
}

/// A hook that blocks can't keep sb from exiting: the exit comes after a
/// short deadline, with the hooks before it run.
#[test]
fn a_blocking_restore_hook_does_not_stop_the_exit() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let start = Instant::now();
    let (code, after) = interrupt_twice("a_blocking_restore_hook_does_not_stop_the_exit", "block");
    assert_eq!(code, Some(130));
    assert_eq!(after, ["restored"]);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}
