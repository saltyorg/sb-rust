//! SIGINT and SIGTERM against a real process (spec: "Cancellation").
//!
//! Each test re-runs this test binary as the child (filtered to one test,
//! with `SB_SIGNAL_CHILD` set), so the signals reach a separate process and
//! never the test harness.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdout, Command, Stdio, Termination};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{RunEnd, finish_run};
use signal_hook::consts::{SIGCHLD, SIGINT, SIGTERM};

const CHILD: &str = "SB_SIGNAL_CHILD";
/// Restore hooks the child registers: `run`, `removed`, `panic` (a
/// panicking hook, then one that prints) or `block` (one that prints, then
/// one that never returns). Empty (the default) registers none, which is
/// what keeps the first four tests checking that nothing else changed.
const HOOKS: &str = "SB_SIGNAL_HOOKS";
/// `finish`: after the first signal the child ends as sb's `main` does,
/// through `finish_run` (to stderr) and its ending's `report`, instead of
/// staying busy, leaving a line on stdout unfinished: std's own flush
/// comes after `report`, too late for a signal's default action.
const END: &str = "SB_SIGNAL_END";

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
    // `hold`: an attached child's hold on SIGINT and SIGQUIT (exec
    // decision 2), kept until the process exits (as `hooks` are).
    let mut holds = Vec::new();
    match std::env::var(HOOKS).as_deref() {
        Ok("hold") => holds.push(shutdown.hold()),
        // `credit`: a hold whose child dies of the SIGINT (exec asks once
        // the child is reaped; here it asks until sb's copy is noted).
        Ok("credit") => {
            let hold = shutdown.hold();
            std::thread::spawn(
                move || {
                    while !hold.child_ended_by(Some(signal_hook::consts::SIGINT)) {}
                },
            );
        }
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
    if std::env::var(END).as_deref() == Ok("finish") {
        let ending = shutdown.snapshot();
        print!("ending");
        let _ = finish_run(&ending, None, None, &mut std::io::stderr()).report();
        println!("still running");
        return;
    }
    std::thread::sleep(Duration::from_secs(20));
}

fn spawn(test: &str) -> (KillOnDrop, Lines) {
    spawn_with(test, "", "")
}

/// The child for `test`, registering `hooks` (see [`HOOKS`]) and ending
/// as `end` says (see [`END`]).
fn spawn_with(test: &str, hooks: &str, end: &str) -> (KillOnDrop, Lines) {
    // Guarded from the start, so a failure while waiting for "ready" kills
    // it too.
    let mut child = KillOnDrop(
        Command::new(std::env::current_exe().expect("test binary"))
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env(HOOKS, hooks)
            .env(END, end)
            .stdout(Stdio::piped())
            // The ending's block, for the `finish` test to read.
            .stderr(if end == "finish" {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
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
/// a second SIGINT ends it at once, by SIGINT itself (exec decision 7).
#[test]
fn sigint_cancels_then_a_second_ends_by_sigint() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("sigint_cancels_then_a_second_ends_by_sigint");
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    let start = Instant::now();
    send(&child, Signal::INT);
    let status = child.wait().expect("wait");
    assert_eq!(status.signal(), Some(SIGINT));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "second signal took {:?}",
        start.elapsed()
    );
}

/// SIGTERM works the same way with 143, and ends by SIGTERM.
#[test]
fn sigterm_cancels_then_a_second_ends_by_sigterm() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("sigterm_cancels_then_a_second_ends_by_sigterm");
    send(&child, Signal::TERM);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(143) interrupted true"
    );
    send(&child, Signal::TERM);
    assert_eq!(child.wait().expect("wait").signal(), Some(SIGTERM));
}

/// The first signal decides the ending: SIGINT then SIGTERM ends by SIGINT.
#[test]
fn the_first_signal_decides_the_ending() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("the_first_signal_decides_the_ending");
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    send(&child, Signal::TERM);
    assert_eq!(child.wait().expect("wait").signal(), Some(SIGINT));
}

/// The reverse order: SIGTERM then SIGINT ends by SIGTERM.
#[test]
fn a_first_sigterm_decides_the_ending() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn("a_first_sigterm_decides_the_ending");
    send(&child, Signal::TERM);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(143) interrupted true"
    );
    send(&child, Signal::INT);
    assert_eq!(child.wait().expect("wait").signal(), Some(SIGTERM));
}

/// Interrupt the child twice; returns the signal that ended it and the
/// lines it printed after the first signal was reported.
fn interrupt_twice(test: &str, hooks: &str) -> (Option<i32>, Vec<String>) {
    let (mut child, mut lines) = spawn_with(test, hooks, "");
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true"
    );
    send(&child, Signal::INT);
    let signal = child.wait().expect("wait").signal();
    (signal, lines.map(|l| l.expect("utf-8")).collect())
}

/// A registered restore hook runs once before the second-signal exit,
/// which still ends by the first signal (term-ui spec, decision 5).
#[test]
fn a_restore_hook_runs_once_before_the_second_signal_exit() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (signal, after) = interrupt_twice(
        "a_restore_hook_runs_once_before_the_second_signal_exit",
        "run",
    );
    assert_eq!(signal, Some(SIGINT));
    assert_eq!(after, ["restored"]);
}

/// A removed hook doesn't run: a runner that has ended mustn't restore
/// stale terminal state later.
#[test]
fn a_removed_restore_hook_does_not_run() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (signal, after) = interrupt_twice("a_removed_restore_hook_does_not_run", "removed");
    assert_eq!(signal, Some(SIGINT));
    assert!(after.is_empty(), "{after:?}");
}

/// A panicking hook can't stop the exit, nor the hooks after it.
/// (Its panic message in the test output is expected.)
#[test]
fn a_panicking_restore_hook_does_not_stop_the_exit() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (signal, after) =
        interrupt_twice("a_panicking_restore_hook_does_not_stop_the_exit", "panic");
    assert_eq!(signal, Some(SIGINT));
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
    let (signal, after) =
        interrupt_twice("a_blocking_restore_hook_does_not_stop_the_exit", "block");
    assert_eq!(signal, Some(SIGINT));
    assert_eq!(after, ["restored"]);
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

/// One signal, then the run's own end (as sb's `main`): stdout is
/// flushed, then the process dies by the signal rather than exiting 130
/// or 143 (exec decision 7), so a parent sees it was interrupted.
#[test]
fn a_finished_interrupted_run_ends_by_its_signal() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    for (signal, number, code) in [(Signal::INT, SIGINT, 130), (Signal::TERM, SIGTERM, 143)] {
        let (mut child, mut lines) = spawn_with(
            "a_finished_interrupted_run_ends_by_its_signal",
            "",
            "finish",
        );
        send(&child, signal);
        assert_eq!(
            next_line(&mut lines),
            format!("cancelled Some({code}) interrupted true")
        );
        assert_eq!(next_line(&mut lines), "ending", "stdout flushed first");
        let status = child.wait().expect("wait");
        assert_eq!(status.signal(), Some(number), "{signal:?}: {status:?}");
        let rest: Vec<String> = lines.map(|l| l.expect("utf-8")).collect();
        assert!(rest.is_empty(), "{signal:?}: {rest:?}");
        let mut stderr = String::new();
        let pipe = child.stderr.as_mut().expect("stderr");
        std::io::Read::read_to_string(pipe, &mut stderr).expect("stderr");
        assert!(stderr.contains("✗ interrupted\n"), "{signal:?}: {stderr}");
    }
}

/// A signal whose default action doesn't end a process (SIGCHLD's is to
/// ignore it) still ends the run, with 128 + the signal, the fallback.
#[test]
fn a_signal_that_would_not_end_sb_still_ends_the_run() {
    if std::env::var_os(CHILD).is_some() {
        let _ = RunEnd::Signal(SIGCHLD).report();
        println!("still running");
        return;
    }
    let out = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "a_signal_that_would_not_end_sb_still_ends_the_run",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .expect("run child");
    assert_eq!(out.status.code(), Some(128 + SIGCHLD), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("still running"), "{stdout}");
}

/// While an attached child holds the signals, a SIGINT is noted, not
/// counted (exec decisions 2 and 12): the first SIGTERM is what cancels
/// (code 143), and a second SIGTERM forces the exit by SIGTERM.
#[test]
fn a_held_sigint_is_not_counted() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn_with("a_held_sigint_is_not_counted", "hold", "");
    send(&child, Signal::INT);
    std::thread::sleep(Duration::from_millis(200));
    send(&child, Signal::TERM);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(143) interrupted true",
        "the SIGINT didn't cancel"
    );
    send(&child, Signal::TERM);
    assert_eq!(child.wait().expect("wait").signal(), Some(SIGTERM));
}

/// A real Ctrl-C while held, through signal-hook's handler and the signals
/// thread: a child that died of it interrupts the run by SIGINT, and the
/// next signal forces the exit, ending sb by that first signal.
#[test]
fn a_child_that_died_of_a_held_sigint_interrupts_the_run() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) = spawn_with(
        "a_child_that_died_of_a_held_sigint_interrupts_the_run",
        "credit",
        "",
    );
    send(&child, Signal::INT);
    assert_eq!(
        next_line(&mut lines),
        "cancelled Some(130) interrupted true",
        "the hold credited the SIGINT"
    );
    send(&child, Signal::TERM);
    assert_eq!(
        child.wait().expect("wait").signal(),
        Some(signal_hook::consts::SIGINT),
        "the second signal forced the exit by the first"
    );
}

/// The hard escape puts the terminal back first: the restore hooks run
/// (bounded by the deadline), then sb ends by SIGQUIT (maintainer,
/// 2026-11-05, amending term-ui decision 5).
#[test]
fn the_hard_escape_runs_the_restore_hooks_first() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, mut lines) =
        spawn_with("the_hard_escape_runs_the_restore_hooks_first", "run", "");
    send(&child, Signal::QUIT);
    assert_eq!(next_line(&mut lines), "restored");
    assert_eq!(
        child.wait().expect("wait").signal(),
        Some(signal_hook::consts::SIGQUIT)
    );
}

/// Outside a hold, Ctrl-\'s SIGQUIT stays the hard escape: sb ends by it at
/// once, without a core dump. Limit: on a host whose core limit is 0 and
/// whose `core_pattern` is a file, no core is written either way, so the
/// check only bites where cores are on (an apport or systemd-coredump
/// host, as here); `end_by` making sb not dumpable is the guarantee.
#[test]
fn sigquit_is_a_hard_escape_without_a_core() {
    if std::env::var_os(CHILD).is_some() {
        return child_main();
    }
    let (mut child, _lines) = spawn("sigquit_is_a_hard_escape_without_a_core");
    send(&child, Signal::QUIT);
    let status = child.wait().expect("wait");
    assert_eq!(
        status.signal(),
        Some(signal_hook::consts::SIGQUIT),
        "{status:?}"
    );
    assert!(!status.core_dumped(), "{status:?}");
}
