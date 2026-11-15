//! Cancellation, timeouts and the process-group escalation, in-process
//! through the token (exec spec, "Cancellation, timeouts and process
//! groups"; plan Task 4). No real signal reaches the test process: the
//! token cancels, exec signals the child's own group.
//!
//! The test binary is a child subreaper (rustix's `set_child_subreaper`),
//! so a grandchild orphaned by its leader is adopted here and reaped by
//! its pid, its ending signal recorded; an unreaped one would sit as a
//! zombie and make its group look alive. Why not peek at the group with
//! `waitid` and reap what it reports, as the plan first said: rustix's
//! `WaitIdStatus` carries no pid, and `waitpgid` would also reap the
//! leader, which tokio reaps.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitOptions, getpgid, getpid, kill_process_group,
    set_child_subreaper, test_kill_process_group, waitid, waitpid,
};
use sb_exec::{Captured, Cmd, Ending};
use tokio_util::sync::CancellationToken;

/// The grace in these tests: short, so nothing waits the default 5 s.
const GRACE: Duration = Duration::from_millis(800);

/// What "within the grace plus one second" allows for.
const BOUND: Duration = Duration::from_millis(800 + 1000);

/// After SIGKILL, how long a stop gives the group's members before it
/// ends regardless (exec spec, "Reading after a stop"; exec's own
/// constant is private).
const LAST_CHECK: Duration = Duration::from_secs(1);

fn subreaper() {
    static SET: OnceLock<()> = OnceLock::new();
    SET.get_or_init(|| set_child_subreaper(Some(getpid())).expect("subreaper"));
}

/// The pids a script wrote to `file` ("leader grandchild", or "leader").
async fn pids(file: &Path) -> Vec<i32> {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.ends_with('\n')
        {
            return text
                .split_whitespace()
                .map(|p| p.parse().expect("pid"))
                .collect();
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no pids in {}",
            file.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait until `raw` ignores SIGTERM: a background subshell sets its trap
/// after its parent has already reported its pid, so a cancel sent before
/// then would end it by SIGTERM. Read from its `SigIgn` mask in
/// /proc/<pid>/status (proc(5)), which `exec` keeps.
async fn ignoring_term(raw: i32) {
    let status = format!("/proc/{raw}/status");
    let start = Instant::now();
    loop {
        let ignored = std::fs::read_to_string(&status).ok().and_then(|text| {
            let mask = text.lines().find_map(|l| l.strip_prefix("SigIgn:"))?;
            u64::from_str_radix(mask.trim(), 16).ok()
        });
        // SIGTERM is signal 15, bit 14 of the mask.
        if ignored.is_some_and(|mask| mask & (1 << 14) != 0) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{raw} never ignored SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn pid(raw: i32) -> Pid {
    Pid::from_raw(raw).expect("pid")
}

/// Reap the adopted grandchild `raw` on a thread of its own; returns the
/// signal that ended it (None if it exited). Until its leader is gone it
/// isn't this process's child (ECHILD), so it keeps trying.
fn reap(raw: i32) -> std::thread::JoinHandle<Option<i32>> {
    std::thread::spawn(move || {
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
    })
}

/// A script `body` that first writes its pids to the file it is given: `$$`
/// (the leader, whose pid is the group's id) and, if it has one, the
/// background grandchild's.
fn script(body: &'static str, file: &Path) -> Cmd {
    Cmd::new("sh")
        .arg("-c")
        .arg(body)
        .arg("sh")
        .value(file.to_str().expect("utf-8"))
        .grace(GRACE)
}

/// Run `cmd`, cancelling the token once its pids are written; returns the
/// result, the pids and how long the cancellation took to end the run.
async fn cancelled(cmd: Cmd, file: &Path) -> (Captured, Vec<i32>, Duration) {
    let token = CancellationToken::new();
    let control = async {
        let pids = pids(file).await;
        let leader = pid(pids[0]);
        // The group exec signals is the child's own.
        assert_eq!(getpgid(Some(leader)).expect("getpgid"), leader);
        // A grandchild here is one that ignores SIGTERM.
        if let Some(&grandchild) = pids.get(1) {
            ignoring_term(grandchild).await;
        }
        token.cancel();
        (pids, Instant::now())
    };
    let (done, (pids, sent)) = tokio::join!(cmd.capture(&token), control);
    (done.expect("ran"), pids, sent.elapsed())
}

fn pid_file() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    (dir, file)
}

/// Nothing of the group is left for this process to reap.
fn no_zombie(leader: i32) {
    let peek = waitid(
        WaitId::Pgid(Some(pid(leader))),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    );
    // None: no child of the group is waitable; ECHILD: none is left at all.
    assert!(matches!(peek, Ok(None) | Err(Errno::CHILD)), "{peek:?}");
}

/// How long a stop takes here and now when the child goes on SIGTERM: a
/// measure of how slow the host is at the moment (near nothing when idle,
/// up to a second under the heaviest load measured). It doesn't run the
/// zombie stop's two timed wake-ups, so it adds to the slack below, not
/// replaces it.
async fn a_stop_on_sigterm() -> Duration {
    let (_dir, file) = pid_file();
    let cmd = script("echo $$ > \"$1\"; exec sleep 30", &file);
    let (done, _, took) = cancelled(cmd, &file).await;
    assert_eq!(done.outcome.ending, Ending::Interrupted);
    took
}

/// A member nobody reaps (here: adopted by this subreaper and left) looks
/// alive to the group check (kill(-pgid, 0) succeeds on a zombie), so a
/// stop waits for it until a second after SIGKILL, then ends regardless:
/// within that, plus a stop on SIGTERM timed here and now, plus one and a
/// half seconds of slack for a loaded host. The zombie never goes, so a
/// stop that waited for it never ends; the 20 s limit fails that. The
/// slack is kept clear of a whole second on purpose: this bound doesn't
/// tell a last check of one second from one of two (a bound on that line
/// caught it only when the host was idle); the lower bound holds the
/// second.
async fn a_zombie_member_is_waited_for_a_second(body: &'static str) {
    subreaper();
    let quick = a_stop_on_sigterm().await;
    let (_dir, file) = pid_file();
    let (done, pids, took) = tokio::time::timeout(
        Duration::from_secs(20),
        cancelled(script(body, &file), &file),
    )
    .await
    .expect("the stop ended");
    assert_eq!(done.outcome.ending, Ending::Interrupted);
    let last = GRACE + LAST_CHECK;
    assert!(
        took >= last.checked_sub(Duration::from_millis(100)).unwrap(),
        "{took:?}"
    );
    assert!(
        took < last + quick + LAST_CHECK * 3 / 2,
        "{took:?} against {quick:?} for a stop on SIGTERM"
    );
    assert_eq!(reap(pids[1]).join().expect("reaper"), Some(9), "SIGKILL");
}

/// SIGKILL sent after the leader's reap (it went on SIGTERM).
#[tokio::test]
async fn a_zombie_left_after_the_leader_ends_the_stop_a_second_after_sigkill() {
    a_zombie_member_is_waited_for_a_second(
        "(trap '' TERM; exec sleep 30) & echo $$ $! > \"$1\"; wait",
    )
    .await;
}

/// SIGKILL sent before the leader's reap (it ignored SIGTERM too).
#[tokio::test]
async fn a_zombie_left_with_the_leader_ends_the_stop_a_second_after_sigkill() {
    a_zombie_member_is_waited_for_a_second(
        "trap '' TERM; (exec sleep 30) & echo $$ $! > \"$1\"; wait",
    )
    .await;
}

/// A timed-out child that ignores SIGTERM is `SIGKILLed` after the grace,
/// once: the timeout doesn't start the stop again.
#[tokio::test]
async fn a_timeout_on_a_child_that_ignores_sigterm_kills_it_after_the_grace() {
    subreaper();
    let limit = Duration::from_millis(200);
    let start = Instant::now();
    let done = Cmd::new("sh")
        .arg("-c")
        .arg("trap '' TERM; exec sleep 30")
        .timeout(limit)
        .grace(GRACE)
        .capture(&CancellationToken::new())
        .await
        .expect("ran");
    let took = start.elapsed();
    assert_eq!(done.outcome.ending, Ending::TimedOut);
    assert!(took >= limit + GRACE && took < limit + BOUND, "{took:?}");
}

/// A child that ends on its own leaves its group alone: a background
/// member it left running isn't exec's to kill.
#[tokio::test]
async fn a_normal_end_leaves_a_background_member_running() {
    subreaper();
    let (_dir, file) = pid_file();
    let done = script("sleep 30 >/dev/null 2>&1 & echo $$ $! > \"$1\"", &file)
        .capture(&CancellationToken::new())
        .await
        .expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(0));
    let pids = pids(&file).await;
    let member = pid(pids[1]);
    assert_eq!(getpgid(Some(member)).expect("getpgid"), pid(pids[0]));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pids[1])).expect("stat");
    assert!(!stat.contains(") Z "), "killed: {stat}");
    kill_process_group(pid(pids[0]), Signal::KILL).expect("kill the group");
    assert_eq!(reap(pids[1]).join().expect("reaper"), Some(9));
}

#[tokio::test]
async fn a_child_that_exits_on_sigterm_gets_no_sigkill() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script("echo $$ > \"$1\"; exec sleep 30", &file);
    let (done, pids, took) = cancelled(cmd, &file).await;
    assert_eq!(done.outcome.ending, Ending::Interrupted);
    assert!(took < GRACE, "{took:?}: waited for the grace");
    no_zombie(pids[0]);
}

#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed_after_the_grace() {
    subreaper();
    let (_dir, file) = pid_file();
    // `exec`: the leader is the group's only member, so no short-lived
    // child is left as a zombie of this subreaper.
    let cmd = script("trap '' TERM; echo $$ > \"$1\"; exec sleep 30", &file);
    let (done, _, took) = cancelled(cmd, &file).await;
    assert_eq!(done.outcome.ending, Ending::Interrupted);
    assert!(took >= GRACE && took < BOUND, "{took:?}");
}

/// The leader goes on SIGTERM; its grandchild ignores it and is still in
/// the group, so it gets SIGKILL after the grace.
#[tokio::test]
async fn a_grandchild_left_by_its_leader_is_killed_after_the_grace() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script(
        "(trap '' TERM; exec sleep 30) & echo $$ $! > \"$1\"; wait",
        &file,
    );
    let token = CancellationToken::new();
    let control = async {
        let pids = pids(&file).await;
        let reaper = reap(pids[1]);
        ignoring_term(pids[1]).await;
        token.cancel();
        (pids, reaper, Instant::now())
    };
    let (done, (pids, reaper, sent)) = tokio::join!(cmd.capture(&token), control);
    let took = sent.elapsed();
    assert_eq!(done.expect("ran").outcome.ending, Ending::Interrupted);
    assert!(took >= GRACE && took < BOUND, "{took:?}");
    assert_eq!(reaper.join().expect("reaper"), Some(9), "SIGKILL");
    no_zombie(pids[0]);
}

/// Both go on SIGTERM: no SIGKILL, long before the grace.
#[tokio::test]
async fn a_leader_and_grandchild_that_exit_on_sigterm_get_no_sigkill() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script("sleep 30 & echo $$ $! > \"$1\"; wait", &file);
    let token = CancellationToken::new();
    let control = async {
        let pids = pids(&file).await;
        let reaper = reap(pids[1]);
        token.cancel();
        (pids, reaper, Instant::now())
    };
    let (done, (pids, reaper, sent)) = tokio::join!(cmd.capture(&token), control);
    let took = sent.elapsed();
    assert_eq!(done.expect("ran").outcome.ending, Ending::Interrupted);
    assert!(took < GRACE, "{took:?}");
    assert_eq!(reaper.join().expect("reaper"), Some(15), "SIGTERM");
    no_zombie(pids[0]);
}

/// A stopped child acts on SIGTERM once continued (SIGCONT with it).
#[tokio::test]
async fn a_stopped_child_still_ends() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script("echo $$ > \"$1\"; kill -STOP $$; sleep 30", &file);
    let token = CancellationToken::new();
    let control = async {
        let pids = pids(&file).await;
        // Stopped: its state in /proc is T.
        let proc_stat = format!("/proc/{}/stat", pids[0]);
        let start = Instant::now();
        while !std::fs::read_to_string(&proc_stat).is_ok_and(|s| s.contains(") T ")) {
            assert!(start.elapsed() < Duration::from_secs(10), "never stopped");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        token.cancel();
        Instant::now()
    };
    let (done, sent) = tokio::join!(cmd.capture(&token), control);
    assert_eq!(done.expect("ran").outcome.ending, Ending::Interrupted);
    assert!(sent.elapsed() < GRACE, "{:?}", sent.elapsed());
}

#[tokio::test]
async fn a_timeout_reads_timed_out() {
    subreaper();
    let start = Instant::now();
    let done = Cmd::new("sleep")
        .arg("30")
        .timeout(Duration::from_millis(200))
        .grace(GRACE)
        .capture(&CancellationToken::new())
        .await
        .expect("ran");
    assert_eq!(done.outcome.ending, Ending::TimedOut);
    assert!(start.elapsed() < GRACE, "{:?}", start.elapsed());
    let err = done.check().expect_err("timed out fails");
    assert_eq!(err.to_string(), "sleep 30 timed out");
}

/// A child that catches SIGTERM and exits 0 was still interrupted.
#[tokio::test]
async fn an_exit_0_after_the_signal_reads_interrupted() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script(
        "trap 'exit 0' TERM; echo $$ > \"$1\"; while :; do sleep 0.05; done",
        &file,
    );
    let (done, _, _) = cancelled(cmd, &file).await;
    assert_eq!(done.outcome.ending, Ending::Interrupted);
    assert_eq!(
        done.check().expect_err("fails").to_string(),
        "sh -c trap 'exit 0' TERM; echo $$ > \"$1\"; while :; do sleep 0.05; done sh [ARG] was interrupted"
    );
}

/// Dropping the run kills the child's group at once.
#[tokio::test]
async fn a_dropped_run_leaves_no_member() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script("sleep 30 & echo $$ $! > \"$1\"; wait", &file);
    let token = CancellationToken::new();
    let pids = tokio::select! {
        _ = cmd.capture(&token) => panic!("ended on its own"),
        pids = pids(&file) => pids,
    };
    let reaper = reap(pids[1]);
    assert_eq!(reaper.join().expect("reaper"), Some(9), "SIGKILL");
    let leader = pid(pids[0]);
    let start = Instant::now();
    loop {
        // tokio reaps a dropped child in the background; whoever gets it.
        let _ = waitpid(Some(leader), WaitOptions::NOHANG);
        if test_kill_process_group(leader).is_err() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the group is left"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A child that ends on its own is never signalled, and its outcome is
/// its own.
#[tokio::test]
async fn an_untouched_child_keeps_its_outcome() {
    let done = Cmd::new("sh")
        .arg("-c")
        .arg("exit 7")
        .timeout(Duration::from_secs(30))
        .capture(&CancellationToken::new())
        .await
        .expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(7));
}

/// Dropped during a stop, after the leader went on SIGTERM: what is left
/// of the group is killed at once, not left to run.
#[tokio::test]
async fn a_run_dropped_during_a_stop_kills_whats_left() {
    subreaper();
    let (_dir, file) = pid_file();
    let cmd = script(
        "(trap '' TERM; exec sleep 30) & echo $$ $! > \"$1\"; wait",
        &file,
    );
    let token = CancellationToken::new();
    let control = async {
        let pids = pids(&file).await;
        let reaper = reap(pids[1]);
        ignoring_term(pids[1]).await;
        token.cancel();
        // Well inside the grace: the leader is gone, its grandchild not.
        tokio::time::sleep(Duration::from_millis(200)).await;
        reaper
    };
    let start = Instant::now();
    let reaper = tokio::select! {
        _ = cmd.capture(&token) => panic!("ended inside the grace"),
        reaper = control => reaper,
    };
    assert_eq!(reaper.join().expect("reaper"), Some(9), "SIGKILL");
    assert!(start.elapsed() < GRACE, "{:?}", start.elapsed());
}
