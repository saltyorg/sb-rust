//! The service logs: sb-journal's reader, re-exported by name (host spec,
//! decision 6), with the host's glue (journal spec, API):
//! - [`coredump_uid`], the systemd-coredump user's uid for [`Query::unit`];
//! - [`page`] and [`drain`], which run the reader's blocking reads under
//!   tokio's `spawn_blocking` (journal spec, assumption 3).
//!
//! [`Journal::open`] and [`Journal::follow`] block too (a directory read;
//! the watches, the files opened and the first batch read) and have no
//! helper here (the plan names `page` and `drain` only): a caller on a runtime runs them under
//! `spawn_blocking` itself.
//!
//! [`crate::ubuntu_release`] gives the [`Release`] to open the journal
//! and build queries with.
//!
//! A follow is waited for as the [`Follower`] docs give it, steps 1 to 4:
//! the caller waits for [`Follower::wake_fd`] to be readable, runs
//! [`drain`], and again while [`Batch::more`], drains once more after a batch
//! with errors (the drain may not have read the events to `EAGAIN`), and
//! only then clears the readiness. Each [`drain`] is awaited to its end,
//! never raced in a `select!` (see its cancel safety). That wait
//! stays with the caller, which owns its runtime and what else it waits on
//! (a key, a resize); sb-host adds no loop of its own. The spec names
//! tokio's `AsyncFd` for it, but tokio 1.54.1 deprecates `AsyncFd::new` as
//! unsound (tokio PR #8587) and its replacement, `AsyncFd::register`, is
//! an `unsafe fn`, which the workspace forbids: how the first consumer
//! waits is the maintainer's decision (sb-journal plan, Task 10).

use std::io;
use std::sync::Arc;

use nix::unistd::User;

#[doc(inline)]
pub use sb_journal::{
    Batch, BootId, BootRule, Cursor, FieldName, Follower, Journal, KnownBad, Limits, MachineId,
    OpenError, OverLimit, Page, PageEnd, Position, Priority, QUERY_GROUPS_MAX, Query, QueryError,
    RELEASES, ReadError, ReadErrorKind, ReadErrors, Record, Release, ReleaseError, Source, Uid,
    UnitName, UnitRule,
};

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
/// (<https://docs.rs/nix/0.28.0/nix/unistd/struct.User.html#method.from_name>;
/// host spec, decision 2).
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
/// The lookup moves to a host-level module when ownership (owner.rs) needs
/// it with the gid.
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
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>). A
/// panic in `f` is resumed in the caller, as `JoinError::into_panic`'s
/// example does
/// (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.into_panic>),
/// so it reads as if `f` had run inline. The other `JoinError`, a
/// cancelled task, comes only from a runtime shut down while this future is
/// still polled from outside it, which sb's `block_on` main never does;
/// tokio's own `into_panic` panic then names the cancellation.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(value) => value,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

#[cfg(test)]
mod tests {
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
}
