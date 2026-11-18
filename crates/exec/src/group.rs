//! A piped child's process group (spec, "Cancellation, timeouts and
//! process groups"): SIGTERM and SIGCONT to the group, SIGKILL after the
//! grace while members remain, the group checked after its leader is
//! reaped, a SIGKILL when the run is dropped, and one for every live group
//! on cli-core's forced exit. Signals go through rustix
//! (<https://docs.rs/rustix/1.1.6/rustix/process/fn.kill_process_group.html>,
//! `test_kill_process_group` for "any member left"). Why not
//! process-wrap's tokio `ProcessGroup` and `KillOnDrop` (10.0.1): its
//! kill on drop is tokio's `kill_on_drop`, which reaches the pid only
//! (`src/tokio/kill_on_drop.rs`), it has no SIGTERM, grace and SIGKILL
//! escalation; its group signal is the same `killpg`.
//!
//! The group's id is the child's pid (`process_group(0)`), so it is never
//! sb's or the harness's own group. Linux doesn't reuse a group's id while
//! a member lives (docs/platform-notes/exec-processes.md), so the id is
//! sb's to signal while the leader is unreaped, and after that only when a
//! check finds members. What remains is the moment between that check and
//! the signal: should the last member go then, and the pid numbers wrap
//! round to a new group with that id, the signal would reach it. pidfds
//! can't name a group, so no API closes this; it needs the whole pid space
//! used within milliseconds.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rustix::process::{
    Pid, PidfdFlags, Signal, kill_process_group, pidfd_open, pidfd_send_signal,
    test_kill_process_group,
};
use sb_cli_core::cancel::{RestoreHook, Shutdown};
use tokio::time::Instant;

/// How often the group is checked for members after its leader is reaped.
const POLL: Duration = Duration::from_millis(10);

/// After SIGKILL, how long the members get to go before the run ends
/// regardless (spec: within the grace plus one second).
pub(crate) const LAST_CHECK: Duration = Duration::from_secs(1);

/// The groups exec created whose run hasn't ended, for the forced exit.
static LIVE: Mutex<BTreeSet<i32>> = Mutex::new(BTreeSet::new());

/// The attached children running, for the forced exit (decision 12): a
/// pidfd each (rustix's `pidfd_open`,
/// <https://docs.rs/rustix/1.1.6/rustix/process/fn.pidfd_open.html>), as
/// they stay in sb's group and have only their pid. A pidfd names that
/// process alone: once it is reaped, a signal through it fails rather than
/// reach a process that got the pid again. A process-wide static, as
/// `LIVE`: the hook has no other handle.
static ATTACHED: Mutex<BTreeMap<i32, OwnedFd>> = Mutex::new(BTreeMap::new());

fn attached() -> MutexGuard<'static, BTreeMap<i32, OwnedFd>> {
    ATTACHED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A child's pid as rustix takes it.
pub(crate) fn pid_of(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

fn live() -> MutexGuard<'static, BTreeSet<i32>> {
    // Inserts and removes can't leave the set half-changed.
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// SIGKILL every live group that still has members when cli-core's forced
/// exit runs (a second signal; cli-core spec, term-ui decision 5). The
/// binary registers it once, with its `Shutdown`, before running commands.
pub fn on_forced_exit(shutdown: &Shutdown) -> RestoreHook {
    shutdown.on_forced_exit(kill_live_groups)
}

/// The forced exit's hook: SIGKILL each running attached child (through
/// its pidfd) and each live group that has members. Each signal goes
/// before its log line: a stuck log writer can hold the hook past its
/// deadline, and the kill mustn't wait on it.
fn kill_live_groups() {
    for (&raw, pidfd) in attached().iter() {
        let _ = pidfd_send_signal(pidfd, Signal::KILL);
        tracing::debug!(pid = raw, "forced exit: SIGKILL to the attached child");
    }
    for &raw in live().iter() {
        if let Some(group) = Pid::from_raw(raw)
            && test_kill_process_group(group).is_ok()
        {
            let _ = kill_process_group(group, Signal::KILL);
            tracing::debug!(group = raw, "forced exit: SIGKILL to the child's group");
        }
    }
}

/// A running attached child, registered for the forced exit (its pidfd,
/// opened right after the spawn, while the pid is surely the child's)
/// until this is dropped, which `attached` does after the reap.
pub(crate) struct AttachedPid(Option<i32>);

impl AttachedPid {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        let Some(pid) = pid.and_then(pid_of) else {
            return Self(None);
        };
        let raw = pid.as_raw_nonzero().get();
        // The kernel opens a pidfd close-on-exec (pidfd_open(2)), so no
        // child inherits it.
        match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(pidfd) => {
                attached().insert(raw, pidfd);
                Self(Some(raw))
            }
            // Not by pid instead: that would bring back the reuse window.
            // Seen in the log; only a file-descriptor limit does it here.
            Err(err) => {
                tracing::warn!(
                    pid = raw,
                    error = %err,
                    "attached child not registered for the forced exit"
                );
                Self(None)
            }
        }
    }
}

impl Drop for AttachedPid {
    fn drop(&mut self) {
        if let Some(raw) = self.0 {
            attached().remove(&raw);
        }
    }
}

/// A running child's group; dropped before its leader is reaped (the run
/// dropped), it SIGKILLs the group, and dropped during a stop, what is
/// left of it.
pub(crate) struct Group {
    id: Pid,
    reaped: bool,
    stopping: bool,
}

impl Group {
    /// The group of a child just spawned with `process_group(0)`.
    pub(crate) fn of(child: u32) -> Option<Self> {
        Some(Self::of_pid(Pid::from_raw(i32::try_from(child).ok()?)?))
    }

    /// The group whose id is `leader`'s pid.
    fn of_pid(leader: Pid) -> Self {
        live().insert(leader.as_raw_nonzero().get());
        Self {
            id: leader,
            reaped: false,
            stopping: false,
        }
    }

    /// Ask the group to end: SIGTERM, then SIGCONT, as a stopped process
    /// acts on SIGTERM only once continued. Only before the leader is
    /// reaped (its pid holds the id).
    pub(crate) fn terminate(&mut self) {
        tracing::debug!(
            group = self.id.as_raw_nonzero().get(),
            "SIGTERM to the child's group"
        );
        self.stopping = true;
        let _ = kill_process_group(self.id, Signal::TERM);
        let _ = kill_process_group(self.id, Signal::CONT);
    }

    /// SIGKILL to the group, while the leader is unreaped.
    pub(crate) fn kill(&self) {
        tracing::debug!(
            group = self.id.as_raw_nonzero().get(),
            "SIGKILL to the child's group"
        );
        let _ = kill_process_group(self.id, Signal::KILL);
    }

    /// The leader was reaped: from now on the id is signalled only after a
    /// check finds members, and dropping this doesn't signal.
    pub(crate) fn reaped(&mut self) {
        self.reaped = true;
    }

    fn has_members(&self) -> bool {
        test_kill_process_group(self.id).is_ok()
    }

    /// After the leader's reap in a stop: wait for the members to go,
    /// SIGKILL them at `kill_at` if any are left (unless `killed` already),
    /// then give them [`LAST_CHECK`] before returning either way. A member
    /// is checked for, not waited on: it isn't sb's child (no `wait` reaches
    /// it; rustix's `waitpgid` and process-wrap reap only sb's own), so this
    /// polls. A zombie no one reaps counts as a member (kill(-pgid, 0)
    /// succeeds on one), so such a stop takes the whole grace and second.
    pub(crate) async fn finish(&self, kill_at: Instant, mut killed: bool) {
        let mut last = kill_at + LAST_CHECK;
        while self.has_members() {
            let now = Instant::now();
            if !killed && now >= kill_at {
                self.kill();
                killed = true;
                last = now + LAST_CHECK;
            }
            if killed && now >= last {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if !self.reaped || (self.stopping && self.has_members()) {
            self.kill();
        }
        live().remove(&self.id.as_raw_nonzero().get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    /// The forced exit's hook SIGKILLs a live group with members; only the
    /// group this test made, its id the child's pid. It kills every group
    /// in the process-wide sets, as does the attached test below: each
    /// test's children are only ever these two `sleep`s, which both
    /// expect SIGKILL; any other child belongs in tests/ (its own process).
    #[test]
    fn the_forced_exit_kills_live_groups() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("sleep");
        let group = Group::of(child.id()).expect("group");
        assert_eq!(
            rustix::process::getpgid(Some(group.id)).expect("getpgid"),
            group.id
        );
        kill_live_groups();
        let status = child.wait().expect("wait");
        // Reaped: a failing check below mustn't signal its id again.
        let mut group = group;
        group.reaped();
        assert_eq!(status.signal(), Some(9), "{status:?}");
        drop(group);
        assert!(!live().contains(&i32::try_from(child.id()).expect("pid")));
    }

    /// The forced exit SIGKILLs a registered attached child's pid, and the
    /// registration ends with its guard.
    #[test]
    fn the_forced_exit_kills_attached_children() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let registered = AttachedPid::new(Some(child.id()));
        let pid = i32::try_from(child.id()).expect("pid");
        assert!(attached().contains_key(&pid), "registered");
        kill_live_groups();
        // Deregistered before the reap here (`attached` does it after,
        // which its pidfd makes safe).
        drop(registered);
        let status = child.wait().expect("wait");
        assert_eq!(status.signal(), Some(9), "{status:?}");
        assert!(!attached().contains_key(&pid));
    }
}
