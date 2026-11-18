//! The service logs: sb-journal's reader, re-exported by name (host spec,
//! decision 6), with the host's glue (journal spec, API):
//! - [`coredump_uid`], the systemd-coredump user's uid for [`Query::unit`];
//! - [`page`] and [`drain`], which run the reader's blocking reads under
//!   tokio's `spawn_blocking` (journal spec, assumption 3);
//! - [`FollowWait`], the wait for a follow's next entries, which a
//!   [`FollowStop`] or dropping it ends (journal spec, decision 12).
//!
//! [`Journal::open`] and [`Journal::follow`] block too (a directory read;
//! the watches, the files opened and the first batch read) and have no
//! helper here (the plan names `page` and `drain` only): a caller on a runtime runs them under
//! `spawn_blocking` itself.
//!
//! [`crate::ubuntu_release`] gives the [`Release`] to open the journal
//! and build queries with.
//!
//! A follow is waited for as the journal spec's wake protocol gives it
//! ("Waiting for new entries"):
//! 1. [`FollowWait::wait`] until [`Wake::Woken`]; [`Wake::Stopped`] ends
//!    the follow;
//! 2. [`drain`];
//! 3. [`drain`] again while the batch has [`Batch::more`];
//! 4. after a batch with errors, [`drain`] once more (reading the events
//!    may have failed before `EAGAIN`);
//! 5. wait again.
//!
//! The wait is `poll`'s, level-triggered: the wake descriptor reads
//! readable for as long as an event is queued, and the drains empty the
//! queue, so there is no readiness to clear (the `clear_ready` step of
//! tokio's `AsyncFd`, which the spec named first). A descriptor whose
//! events every drain failed to read would wake the loop at once, each time
//! with the drain's error to show; inotify's events fit the drain's
//! buffer, so this isn't expected. Why not `AsyncFd`:
//! tokio 1.54.1 deprecates `AsyncFd::new` as "unsound due to lack of I/O
//! safety" (<https://github.com/tokio-rs/tokio/pull/8587>), and its
//! replacement, `AsyncFd::register`, is an `unsafe fn`, which the
//! workspace forbids (maintainer, 2026-11-11).
//!
//! The wait can race a key or a resize in `select!` (it is cancel-safe);
//! each [`drain`] is awaited to its end (it isn't). The loop stays with
//! the caller, which owns its runtime and what else it waits on:
//!
//! ```no_run
//! use sb_host::journal::{self, Follower, FollowWait, Limits, Wake};
//!
//! async fn follow(mut follower: Follower, limits: Limits) -> std::io::Result<()> {
//!     let mut waiter = FollowWait::new(&follower)?;
//!     // The viewer keeps `waiter.stop_handle()` and stops it on close.
//!     while waiter.wait().await? == Wake::Woken {
//!         let mut retried = false;
//!         loop {
//!             let (next, batch) = journal::drain(follower, limits).await;
//!             follower = next;
//!             // Show `batch.records` and `batch.errors` here.
//!             if batch.more {
//!                 continue;
//!             }
//!             if !batch.errors.is_empty() && !retried {
//!                 retried = true;
//!                 continue;
//!             }
//!             break;
//!         }
//!     }
//!     Ok(())
//! }
//! ```

use std::io;
use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use nix::unistd::User;
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec, eventfd, poll};
// `poll` again on `EINTR`, a signal handled on the polling thread
// (<https://docs.rs/rustix/1.1.6/rustix/io/fn.retry_on_intr.html>).
use rustix::io::retry_on_intr;
use tokio::task::JoinHandle;

#[doc(inline)]
pub use sb_journal::{
    Batch, BootId, BootRule, Cursor, FieldName, Follower, Journal, KnownBad, Limits, MachineId,
    OpenError, OverLimit, Page, PageEnd, Position, Priority, QUERY_GROUPS_MAX, Query, QueryError,
    RELEASES, ReadError, ReadErrorKind, ReadErrors, Record, Release, ReleaseError, Source, Uid,
    UnitName, UnitRule,
};

/// sb-journal's crafted journal files, for tests above sb-host (sb's
/// `sb logs` end to end through `SB_JOURNAL_DIR`), which reach the
/// reader only through sb-host (deny.toml's wrappers); never in a release
/// build.
#[cfg(feature = "testing")]
pub use sb_journal::testing;

/// The user systemd-coredump runs as (its package's sysusers entry).
const COREDUMP_USER: &str = "systemd-coredump";

/// A user that couldn't be looked up, in sb-go's words: `look up user <u>:
/// <cause>` (host spec, Ownership).
#[derive(Debug, thiserror::Error)]
// The cause is part of the text only, not a `source` as well: the failure
// block shows the whole chain, which would print it twice (as sb-docker's
// control errors).
#[error("look up user {user}: {cause}")]
pub struct UserLookupError {
    user: String,
    cause: io::Error,
}

impl UserLookupError {
    /// The user name looked up.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }
}

/// The systemd-coredump user's uid, or `None` when there is no such user
/// (the package isn't installed): what [`Query::unit`] takes for 26.04's
/// coredump term (journal spec, decisions 1 and 4).
///
/// The lookup is nix's `User::from_name`, `getpwnam_r`
/// (<https://docs.rs/nix/0.31.3/nix/unistd/struct.User.html#method.from_name>;
/// host spec, decision 2). It blocks, as an NSS lookup can (a network
/// backend most of all): on a runtime, run it under `spawn_blocking`, as
/// [`Journal::open`] and [`Journal::follow`] are run.
///
/// # Errors
///
/// [`UserLookupError`] when the user database can't be read. glibc
/// documents that some NSS backends report a missing user as an error
/// (`ENOENT`, `ESRCH`, `EBADF`, `EPERM`) rather than none; with the files
/// and systemd backends a Saltbox host uses, a missing user is `Ok(None)`.
/// UNVERIFIED for other backends; the caller decides how to treat it.
pub fn coredump_uid() -> Result<Option<Uid>, UserLookupError> {
    uid_of(COREDUMP_USER)
}

/// `user`'s uid from the system's user database. Why not sysinfo's
/// `Users`: it parses /etc/passwd itself, where `getpwnam_r` is what the
/// rest of the system answers with (host spec, Ownership and decision 2).
/// owner.rs has its own lookup, with the gid; both are one call to nix's
/// `User::from_name`.
fn uid_of(user: &str) -> Result<Option<Uid>, UserLookupError> {
    uid_with(user, |name| {
        User::from_name(name).map(|found| found.map(|user| user.uid.as_raw()))
    })
}

/// `user`'s uid as `lookup` finds it (M-MOCKABLE-SYSCALLS: the tests
/// inject the failure).
fn uid_with(
    user: &str,
    lookup: impl FnOnce(&str) -> nix::Result<Option<u32>>,
) -> Result<Option<Uid>, UserLookupError> {
    match lookup(user) {
        Ok(uid) => Ok(uid.map(Uid::new)),
        Err(errno) => Err(UserLookupError {
            user: user.to_owned(),
            cause: errno.into(),
        }),
    }
}

/// [`Journal::page`] under `spawn_blocking`: the `n` entries matching `q`
/// at `at`, oldest first. The values are owned, so the read can run on
/// tokio's blocking pool while the caller's task waits.
///
/// # Panics
///
/// Outside a tokio runtime (`spawn_blocking`'s own panic); when the read
/// panics, its panic resumed here; and when the read was cancelled by the
/// runtime's shutdown while this future was still polled (tokio's
/// `JoinError::into_panic` panic).
pub async fn page(journal: Arc<Journal>, q: Query, at: Position, n: usize, limits: Limits) -> Page {
    blocking(move || journal.page(&q, &at, n, &limits)).await
}

/// [`Journal::scan`] under `spawn_blocking`: every entry matching `q` at
/// `at`, newest first, handed to `each` on the blocking pool a step of at
/// most `step` records at a time, until `each` breaks or the journal ends.
///
/// # Cancel safety
///
/// Dropping this future doesn't stop the scan: it runs on the blocking
/// pool until `each` breaks or the journal ends. A caller that may go
/// away breaks from `each` (the log viewer's search does once its viewer
/// is gone or the search is stopped).
///
/// # Panics
///
/// As [`page`].
pub async fn scan<F>(
    journal: Arc<Journal>,
    q: Query,
    at: Position,
    step: usize,
    limits: Limits,
    mut each: F,
) -> ControlFlow<()>
where
    F: FnMut(Vec<Record>, ReadErrors) -> ControlFlow<()> + Send + 'static,
{
    blocking(move || journal.scan(&q, &at, step, &limits, &mut each)).await
}

/// [`Follower::drain`] under `spawn_blocking`: the follower moves in and
/// comes back out with what was written since its last drain. Drain again
/// while the batch has [`Batch::more`] before waiting on the wake
/// descriptor (see the module docs).
///
/// # Cancel safety
///
/// Not cancel-safe: the follower is on the blocking pool until the drain
/// ends, so dropping this future before it completes drops the follower
/// and its batch there, and the follow ends. Await it to completion; don't
/// race it in `select!` against a key or a resize.
///
/// # Panics
///
/// As [`page`].
pub async fn drain(mut follower: Follower, limits: Limits) -> (Follower, Batch) {
    blocking(move || {
        let batch = follower.drain(&limits);
        (follower, batch)
    })
    .await
}

/// `f` on tokio's blocking pool
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>),
/// its outcome taken as [`unpanicked`] takes it.
/// The caller's span goes along, so the thread's records carry the run id
/// (cli-core's `spawn`).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    unpanicked(sb_cli_core::spawn::blocking(f).await)
}

/// The value of a task on tokio's blocking pool. A panic in it is resumed
/// in the caller, as `JoinError::into_panic`'s example does
/// (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.into_panic>),
/// so it reads as if the task had run inline. The other `JoinError`, a
/// cancelled task, comes only from a runtime shut down while this future is
/// still polled from outside it, which sb's `block_on` main never does;
/// tokio's own `into_panic` panic then names the cancellation.
fn unpanicked<T>(joined: Result<T, tokio::task::JoinError>) -> T {
    match joined {
        Ok(value) => value,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// What a [`FollowWait::wait`] ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The wake descriptor is readable: drain (see the module docs).
    Woken,
    /// The wait was stopped, by a [`FollowStop`] or the waiter dropped; so
    /// is every wait after it. A stop wins over a queued event.
    Stopped,
}

/// The wait for a follow's next entries (journal spec, decision 12).
///
/// It polls the [`Follower`]'s wake descriptor and a stop `eventfd` on
/// tokio's blocking pool; the protocol is in the module docs.
///
/// Built on rustix's `poll` and `eventfd`
/// (<https://docs.rs/rustix/1.1.6/rustix/event/fn.poll.html>,
/// <https://docs.rs/rustix/1.1.6/rustix/event/fn.eventfd.html>) and
/// tokio's `spawn_blocking`, in place of tokio's `AsyncFd` (unsound to
/// construct safely in 1.54.1; see the module docs). A wait holds one
/// blocking thread for as long as it is pending, and the waiter holds two
/// descriptors: the wake descriptor's duplicate and the stop `eventfd`.
///
/// Dropping the waiter stops it, so a closed viewer frees its blocking
/// thread at once rather than at the journal's next write. Drop it with
/// its follower: its duplicate keeps the follow's inotify descriptor, and
/// its watches, open, and with no follower to drain them it would wake for
/// every write.
#[derive(Debug)]
pub struct FollowWait {
    /// The follower's [`Follower::wake_fd`].
    wake: Arc<OwnedFd>,
    /// Readable once stopped, for good: it is written to and never read.
    stop: Arc<OwnedFd>,
    /// The poll of a wait dropped before it ended, which the next wait
    /// takes over, so a waiter never runs more than one.
    pending: Option<JoinHandle<io::Result<Wake>>>,
}

impl FollowWait {
    /// A waiter on `follower`'s wake descriptor ([`Follower::wake_fd`]),
    /// with a stop `eventfd` of its own.
    ///
    /// # Errors
    ///
    /// The system's, when the wake descriptor can't be duplicated or the
    /// `eventfd` created (`EMFILE` past the process's descriptor limit).
    pub fn new(follower: &Follower) -> io::Result<Self> {
        Self::with_wake(follower.wake_fd()?)
    }

    /// A waiter on `wake`. The stop `eventfd` is close-on-exec, so no child
    /// inherits it, and non-blocking, so a stop never blocks.
    fn with_wake(wake: OwnedFd) -> io::Result<Self> {
        let stop = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?;
        Ok(Self {
            wake: Arc::new(wake),
            stop: Arc::new(stop),
            pending: None,
        })
    }

    /// A handle that stops this waiter's waits, the pending one and every
    /// later one, from any task or thread: the viewer's close.
    #[must_use]
    pub fn stop_handle(&self) -> FollowStop {
        FollowStop {
            stop: Arc::clone(&self.stop),
        }
    }

    /// Waits until the wake descriptor is readable ([`Wake::Woken`]) or
    /// the waiter is stopped ([`Wake::Stopped`]).
    ///
    /// # Cancel safety
    ///
    /// Cancel-safe: dropping this future before it ends (a `select!` that
    /// took a key) leaves its poll running, and the next `wait` takes that
    /// poll's outcome instead of starting another. Readiness is
    /// level-triggered, so an outcome taken late is still true, or at worst
    /// one empty drain; a stop since then still wins.
    ///
    /// # Errors
    ///
    /// `poll`'s, other than `EINTR`, which is retried.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime (`spawn_blocking`'s own panic), and as
    /// [`page`] when the runtime cancels the poll.
    pub async fn wait(&mut self) -> io::Result<Wake> {
        let task = self.pending.get_or_insert_with(|| {
            let (wake, stop) = (Arc::clone(&self.wake), Arc::clone(&self.stop));
            // The caller's span goes along (cli-core's `spawn`).
            sb_cli_core::spawn::blocking(move || poll_wake(&wake, &stop))
        });
        let joined = task.await;
        // Taken before a panic is resumed, so a later wait starts afresh
        // rather than polling a finished handle.
        self.pending = None;
        match unpanicked(joined)? {
            // A cancelled wait's poll may have been woken before a stop
            // that came after it: the stop wins still.
            Wake::Woken if is_stopped(&self.stop)? => Ok(Wake::Stopped),
            wake => Ok(wake),
        }
    }
}

impl Drop for FollowWait {
    /// Stops the pending wait, so its blocking thread returns now. The poll
    /// keeps its own references to both descriptors, so neither closes
    /// under it.
    fn drop(&mut self) {
        stop(&self.stop);
    }
}

/// Stops a [`FollowWait`]'s waits ([`FollowWait::stop_handle`]): cheap to
/// clone, `Send` and `Sync`. Dropping it stops nothing.
#[derive(Debug, Clone)]
pub struct FollowStop {
    stop: Arc<OwnedFd>,
}

impl FollowStop {
    /// Ends the pending wait, and every later one, with [`Wake::Stopped`].
    /// Stopping again, or after the waiter is gone, does nothing more.
    pub fn stop(&self) {
        stop(&self.stop);
    }
}

/// Makes `stop` readable for good: eventfd(2)'s write adds 1 to its
/// counter, which nothing reads back. It can fail only with `EAGAIN`, at a
/// counter of 2^64 - 2, which only that many stops reach and which is
/// readable already, so the error is dropped.
fn stop(stop: &OwnedFd) {
    rustix::io::write(stop, &1u64.to_ne_bytes()).ok();
}

/// Blocks until `stop` or `wake` is readable, `stop` first: rustix's
/// `poll` with no timeout, level-triggered.
fn poll_wake(wake: &OwnedFd, stop: &OwnedFd) -> io::Result<Wake> {
    let mut fds = [
        PollFd::new(stop, PollFlags::IN),
        PollFd::new(wake, PollFlags::IN),
    ];
    retry_on_intr(|| poll(&mut fds, None))?;
    // With no timeout, poll returns once one of them has events: the wake
    // descriptor's may be an error, which the drain meets.
    Ok(if fds[0].revents().is_empty() {
        Wake::Woken
    } else {
        Wake::Stopped
    })
}

/// Whether `stop` is readable now: rustix's `poll` with a zero timeout,
/// so it never blocks the runtime's thread.
fn is_stopped(stop: &OwnedFd) -> io::Result<bool> {
    const NOW: Timespec = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut fds = [PollFd::new(stop, PollFlags::IN)];
    retry_on_intr(|| poll(&mut fds, Some(&NOW)))?;
    Ok(!fds[0].revents().is_empty())
}

#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;
    use std::panic::AssertUnwindSafe;
    use std::time::Duration;

    use rustix::event::{EventfdFlags, eventfd};
    use tokio::time::timeout;

    use super::*;

    /// A user found gives its uid.
    #[test]
    fn a_user_found_gives_its_uid() {
        let uid = uid_with(COREDUMP_USER, |name| {
            assert_eq!(name, "systemd-coredump");
            Ok(Some(998))
        })
        .expect("looked up");
        assert_eq!(uid, Some(Uid::new(998)));
    }

    /// No such user (systemd-coredump not installed) isn't an error:
    /// `Query::unit` then requires `_UID=0` (journal spec, decision 1).
    #[test]
    fn no_such_user_gives_none() {
        assert_eq!(
            uid_with(COREDUMP_USER, |_| Ok(None)).expect("looked up"),
            None
        );
    }

    /// host spec, Ownership: `look up user <u>: <cause>`.
    #[test]
    fn a_failed_lookup_names_the_user_and_the_cause() {
        let err = uid_with(COREDUMP_USER, |_| Err(nix::errno::Errno::EIO)).expect_err("fails");
        assert_eq!(err.user(), "systemd-coredump");
        assert_eq!(
            err.to_string(),
            "look up user systemd-coredump: Input/output error (os error 5)"
        );
    }

    /// The system's lookup: root is uid 0 on every host, and a name no
    /// passwd entry can carry (`:` separates its fields) is no user.
    #[test]
    fn the_system_lookup_finds_root_and_not_a_name_with_a_colon() {
        assert_eq!(uid_of("root").expect("looked up"), Some(Uid::new(0)));
        assert_eq!(uid_of("no:such:user").expect("looked up"), None);
    }

    /// `coredump_uid` asks the system for systemd-coredump, whether or not
    /// this host has the user.
    #[test]
    fn the_coredump_uid_is_the_systems_systemd_coredump() {
        assert_eq!(
            coredump_uid().expect("looked up"),
            uid_of("systemd-coredump").expect("looked up")
        );
    }

    /// A panic on the blocking pool is the caller's, with its payload.
    #[tokio::test]
    #[should_panic(expected = "the read's own panic")]
    async fn a_panic_in_the_blocking_read_is_resumed_in_the_caller() {
        blocking(|| panic!("the read's own panic")).await;
    }

    /// The value comes back from the blocking pool.
    #[tokio::test]
    async fn the_blocking_value_comes_back() {
        assert_eq!(blocking(|| 7).await, 7);
    }

    // The wake wait (`FollowWait`), on an eventfd standing in for the
    // follow's inotify descriptor: both are readable while something is
    // queued on them, and a drain (here, nothing) is what empties them.

    /// How long a wait may take to end once it should: far above a poll's
    /// return, far below a viewer's patience.
    const PROMPTLY: Duration = Duration::from_secs(1);

    /// How long a wait that shouldn't end is given to show it doesn't.
    const A_WHILE: Duration = Duration::from_millis(100);

    /// `test` on a current-thread runtime of `blocking_threads` blocking
    /// threads, shut down without waiting for them: a poll that a broken
    /// stop leaves running fails the test (its `timeout`) instead of
    /// hanging the runtime's drop.
    fn on_runtime<T>(blocking_threads: usize, test: impl Future<Output = T>) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(blocking_threads)
            .enable_time()
            .build()
            .expect("a runtime");
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| runtime.block_on(test)));
        runtime.shutdown_background();
        outcome.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    /// A wake descriptor with nothing queued, and a duplicate of it to
    /// write to.
    fn quiet() -> (OwnedFd, OwnedFd) {
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).expect("an eventfd");
        let writer = rustix::io::fcntl_dupfd_cloexec(&fd, 0).expect("a duplicate");
        (fd, writer)
    }

    /// Something queued on the wake descriptor, as a journal write queues an
    /// inotify event.
    fn write_to(writer: &OwnedFd) {
        rustix::io::write(writer, &1u64.to_ne_bytes()).expect("written");
    }

    #[test]
    fn a_write_wakes_a_pending_wait() {
        on_runtime(2, async {
            let (wake, writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            let task = tokio::spawn(async move { waiter.wait().await });
            tokio::time::sleep(A_WHILE).await;
            assert!(!task.is_finished(), "nothing written yet");
            write_to(&writer);
            let ended = timeout(PROMPTLY, task).await.expect("woken promptly");
            assert_eq!(ended.expect("the task ran").expect("polled"), Wake::Woken);
        });
    }

    /// Level-triggered, as `poll` is: a descriptor already readable wakes
    /// the wait at once, and again until it is drained (here, the
    /// eventfd's counter read).
    #[test]
    fn a_readable_descriptor_wakes_every_wait_until_drained() {
        on_runtime(2, async {
            let (wake, writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            write_to(&writer);
            for _ in 0..2 {
                let woken = timeout(PROMPTLY, waiter.wait()).await.expect("at once");
                assert_eq!(woken.expect("polled"), Wake::Woken);
            }
            let mut counter = [0; 8];
            rustix::io::read(&writer, &mut counter).expect("drained");
            assert!(
                timeout(A_WHILE, waiter.wait()).await.is_err(),
                "woken after the drain"
            );
        });
    }

    #[test]
    fn a_wait_with_nothing_queued_stays_pending() {
        on_runtime(2, async {
            let (wake, _writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            assert!(
                timeout(A_WHILE, waiter.wait()).await.is_err(),
                "nothing woke it"
            );
        });
    }

    /// The viewer closing: its stop handle, used from another thread,
    /// ends the pending wait.
    #[test]
    fn the_stop_handle_ends_a_pending_wait_promptly() {
        on_runtime(2, async {
            let (wake, _writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            let stop = waiter.stop_handle();
            let task = tokio::spawn(async move { waiter.wait().await });
            tokio::time::sleep(A_WHILE).await;
            assert!(!task.is_finished(), "pending before the stop");
            std::thread::spawn(move || stop.stop())
                .join()
                .expect("stopped");
            let ended = timeout(PROMPTLY, task).await.expect("stopped promptly");
            assert_eq!(ended.expect("the task ran").expect("polled"), Wake::Stopped);
        });
    }

    /// A stop is for good: every wait after it, and a clone's second stop,
    /// returns `Stopped` at once.
    #[test]
    fn every_wait_after_a_stop_is_stopped_at_once() {
        on_runtime(2, async {
            let (wake, _writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            let stop = waiter.stop_handle();
            stop.stop();
            stop.clone().stop();
            for _ in 0..2 {
                let ended = timeout(PROMPTLY, waiter.wait()).await.expect("at once");
                assert_eq!(ended.expect("polled"), Wake::Stopped);
            }
        });
    }

    /// A stop wins over a queued event: the viewer is closing.
    #[test]
    fn a_stop_wins_over_a_readable_descriptor() {
        on_runtime(2, async {
            let (wake, writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            write_to(&writer);
            waiter.stop_handle().stop();
            let ended = timeout(PROMPTLY, waiter.wait()).await.expect("at once");
            assert_eq!(ended.expect("polled"), Wake::Stopped);
        });
    }

    /// A cancelled wait's poll woken before a stop: the next wait takes
    /// its outcome, and the stop still wins.
    #[test]
    fn a_stop_wins_over_a_cancelled_waits_wake() {
        on_runtime(2, async {
            let (wake, writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            assert!(timeout(A_WHILE, waiter.wait()).await.is_err(), "cancelled");
            write_to(&writer);
            let woken = timeout(PROMPTLY, async {
                while !waiter.pending.as_ref().is_some_and(JoinHandle::is_finished) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
            woken.await.expect("its poll was woken");
            waiter.stop_handle().stop();
            let ended = timeout(PROMPTLY, waiter.wait()).await.expect("at once");
            assert_eq!(ended.expect("polled"), Wake::Stopped);
        });
    }

    /// The stop eventfd is close-on-exec, so no child inherits it, and
    /// non-blocking, so a stop never blocks.
    #[test]
    fn the_stop_eventfd_is_close_on_exec_and_non_blocking() {
        let (wake, _writer) = quiet();
        let waiter = FollowWait::with_wake(wake).expect("a waiter");
        let fd_flags = rustix::io::fcntl_getfd(&*waiter.stop).expect("getfd");
        assert!(
            fd_flags.contains(rustix::io::FdFlags::CLOEXEC),
            "{fd_flags:?}"
        );
        let status = rustix::fs::fcntl_getfl(&*waiter.stop).expect("getfl");
        assert!(status.contains(rustix::fs::OFlags::NONBLOCK), "{status:?}");
    }

    /// A wait dropped before it ends (a `select!` that took a key) leaves
    /// its poll running; the next wait takes its outcome rather than
    /// starting another: after two cancelled waits on a runtime of two
    /// blocking threads, one is still free for another task.
    #[test]
    fn the_next_wait_takes_over_a_cancelled_one() {
        on_runtime(2, async {
            let (wake, writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            assert!(timeout(A_WHILE, waiter.wait()).await.is_err(), "cancelled");
            assert!(timeout(A_WHILE, waiter.wait()).await.is_err(), "again");
            let free = timeout(PROMPTLY, tokio::task::spawn_blocking(|| 7)).await;
            assert_eq!(free.expect("one poll, one thread").expect("ran"), 7);
            write_to(&writer);
            let woken = timeout(PROMPTLY, waiter.wait()).await.expect("woken");
            assert_eq!(woken.expect("polled"), Wake::Woken);
            assert!(waiter.pending.is_none(), "taken");
        });
    }

    /// Dropping the waiter (the viewer gone) ends its pending poll: on a
    /// runtime of one blocking thread, held by that poll, the next blocking
    /// task runs at once instead of queueing behind it for ever.
    #[test]
    fn dropping_the_waiter_frees_its_blocking_thread() {
        on_runtime(1, async {
            let (wake, _writer) = quiet();
            let mut waiter = FollowWait::with_wake(wake).expect("a waiter");
            assert!(timeout(A_WHILE, waiter.wait()).await.is_err(), "pending");
            drop(waiter);
            let freed = timeout(PROMPTLY, tokio::task::spawn_blocking(|| 7)).await;
            assert_eq!(freed.expect("the thread was freed").expect("ran"), 7);
        });
    }
}
