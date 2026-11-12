//! Processes a signal test starts and waits on (`hangup.rs`,
//! `docker_control_signals.rs`): cleaned up when a test fails part-way.
//! A module of its own, not `common`: every sb test binary includes
//! `common`, and these would be unused (dead code) in the others.

use std::process::Child;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// A started process (`script` under `timeout`, or sb), and sb itself once
/// its pid is known, killed (SIGKILL) if a test fails before they exit:
/// SIGKILL to `timeout` doesn't reach its children.
pub struct KillOnDrop(pub Child, pub Option<i32>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.1.and_then(Pid::from_raw) {
            let _ = kill_process(pid, Signal::KILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A stand-in `systemctl` (a `sleep` once it execs), killed (SIGKILL) when
/// the test ends if it is still there and still a `sleep`.
pub struct StandIn(pub i32);

impl Drop for StandIn {
    fn drop(&mut self) {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", self.0)).unwrap_or_default();
        if cmdline.starts_with(b"sleep")
            && let Some(pid) = Pid::from_raw(self.0)
        {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

/// `probe`'s first `Some` within ten seconds.
pub fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `pid` is gone (no process, or only a zombie left to reap).
pub fn gone(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| stat.contains(") Z "))
}
