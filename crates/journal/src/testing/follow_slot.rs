//! The journal-follow tests a few at a time, each after a check that an
//! inotify instance is there to take (maintainer, 2026-11-21). A follow
//! holds an inotify instance while it runs, and
//! `fs.inotify.max_user_instances` counts every process of the user: with
//! a test binary's follows all at once on a host whose other processes
//! hold most of root's instances, the follows failed with `EMFILE`, and
//! the tests with screens that didn't say why.
//!
//! nextest runs one test per process, so its `journal-follow` test group
//! (`.config/nextest.toml`) limits those runs. `cargo test` and `cargo
//! llvm-cov` run a binary's tests as threads of one process, and libtest
//! has no per-test limit, only `--test-threads` for the whole binary
//! (<https://doc.rust-lang.org/rustc/tests/index.html#--test-threads-num_threads>),
//! which would hold back every other test as well: there [`take`]'s slot,
//! of which a process has [`AT_ONCE`], does it, a count under std's
//! `Mutex` and `Condvar`
//! (<https://doc.rust-lang.org/std/sync/struct.Condvar.html>). Under
//! nextest a slot is always free.
//!
//! The probe catches instances used up, not nearly used up: it closes
//! its instance at once, and the test's follow comes later (in sb's
//! tests, in the child sb), so with fewer free than [`AT_ONCE`] two tests
//! can pass it and one follow still fail; that follow's own notice then
//! names the limit. `EMFILE` also comes from the process's own open
//! files limit, which a test process doesn't come near.

use std::sync::{Condvar, Mutex, PoisonError};

use rustix::fs::inotify::{self, CreateFlags};
use rustix::io::Errno;

/// The follow tests one test process runs at once, as the nextest group's
/// `max-threads`.
pub const AT_ONCE: usize = 2;

/// The slots taken in this process.
static TAKEN: Mutex<usize> = Mutex::new(0);
/// Signalled when a slot is given back.
static GIVEN_BACK: Condvar = Condvar::new();

/// One of the process's [`AT_ONCE`] follow slots, given back when
/// dropped. A test holds it for as long as it follows.
#[derive(Debug)]
#[must_use = "the slot is given back when dropped"]
pub struct Slot(());

impl Drop for Slot {
    fn drop(&mut self) {
        let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
        *taken -= 1;
        GIVEN_BACK.notify_one();
    }
}

/// A follow slot, waited for while [`AT_ONCE`] are taken; then a probe
/// that an inotify instance can be had (`inotify_init1` once, closed at
/// once).
///
/// # Panics
///
/// When the probe meets `EMFILE`: the host's inotify instances are used
/// up, and the message says so, with the limit.
#[track_caller]
pub fn take() -> Slot {
    let mut taken = TAKEN.lock().unwrap_or_else(PoisonError::into_inner);
    while *taken >= AT_ONCE {
        taken = GIVEN_BACK
            .wait(taken)
            .unwrap_or_else(PoisonError::into_inner);
    }
    *taken += 1;
    drop(taken);
    // Made before the probe, so a probe that panics gives it back.
    let slot = Slot(());
    let probe = inotify::init(CreateFlags::CLOEXEC).map(drop);
    if let Some(message) = used_up(probe, limit) {
        panic!("{message}");
    }
    slot
}

/// The message for a probe that met `EMFILE`, `limit` read only then;
/// `None` for any other outcome, which the test meets in its own way.
fn used_up(probe: rustix::io::Result<()>, limit: impl FnOnce() -> String) -> Option<String> {
    match probe {
        Err(Errno::MFILE) => Some(format!(
            "the host's inotify instances are used up (fs.inotify.max_user_instances = {}); \
             free some or raise it",
            limit()
        )),
        _ => None,
    }
}

/// `fs.inotify.max_user_instances` as the system gives it, or `unknown`.
fn limit() -> String {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_instances")
        .map_or_else(|_| "unknown".to_owned(), |value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn emfile_says_the_hosts_instances_are_used_up_with_the_limit() {
        assert_eq!(
            used_up(Err(Errno::MFILE), || "128".to_owned()).as_deref(),
            Some(
                "the host's inotify instances are used up \
                 (fs.inotify.max_user_instances = 128); free some or raise it"
            )
        );
    }

    #[test]
    fn a_probe_that_got_its_instance_or_another_error_says_nothing() {
        let unread = || -> String { panic!("the limit is read only for EMFILE") };
        assert_eq!(used_up(Ok(()), unread), None);
        assert_eq!(used_up(Err(Errno::NOMEM), unread), None);
    }

    /// The slots and the nextest group's `max-threads` are one number.
    #[test]
    fn the_slots_are_the_nextest_groups_threads() {
        let config = include_str!("../../../../.config/nextest.toml");
        assert!(
            config.contains(&format!("journal-follow = {{ max-threads = {AT_ONCE} }}")),
            "the journal-follow group's max-threads isn't AT_ONCE ({AT_ONCE})"
        );
    }

    #[test]
    fn the_limit_is_the_systems_value() {
        let value = limit();
        assert!(
            value == "unknown" || value.parse::<u64>().is_ok(),
            "{value:?}"
        );
    }

    /// With every slot taken, the next waits until one is given back.
    #[test]
    fn a_slot_past_the_limit_waits_for_one_given_back() {
        let held: Vec<Slot> = (0..AT_ONCE).map(|_| take()).collect();
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let slot = take();
            tx.send(()).expect("sent");
            drop(slot);
        });
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        drop(held);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a slot once one was given back");
        waiter.join().expect("joined");
    }
}
