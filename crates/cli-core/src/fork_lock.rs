//! The lock that keeps a program sb writes out of the children it starts
//! meanwhile. A child forked while one of sb's threads holds a file open
//! for writing inherits that handle until its own exec (`O_CLOEXEC`, which
//! std sets on every file it opens, closes it only then), and running the
//! file while any process holds it for writing fails with ETXTBSY: one
//! process's threads race themselves (rust-lang/rust#114554,
//! <https://github.com/rust-lang/rust/issues/114554>; Go's
//! golang/go#22315, <https://github.com/golang/go/issues/22315>). Writing
//! a temporary name and renaming it doesn't help: the inode keeps the
//! inherited writer.
//!
//! So sb-exec starts every child under [`forking`] (shared: children
//! start side by side), and code that writes a file sb then runs opens,
//! writes and closes it under [`writing`] (exclusive). No fork then
//! copies a descriptor table holding such a handle: one taken before the
//! handle was opened can't hold it, and none is taken until it is closed.
//! That holds for every child sb starts, since clippy sends every spawn in
//! library and binary code through sb-exec (CONSTRAINTS.md, E4), and
//! whichever thread starts it (the runtime's, the blocking pool's, the
//! probes' threads). (A dependency's own child is outside it: crossterm's
//! `tput`, which its terminal size falls back to when the terminal can't
//! say, in the TUI.) It is the lock golang/go#22315 settles on, "either
//! many forks or many closes, but never both", the roles turned around as
//! Go's `syscall.ForkLock` turns them
//! (<https://go.dev/src/syscall/forkpipe2.go>): many forks, one writer.
//! One static for the process, as sb links one sb-cli-core.
//!
//! Why the writes hold it from open to close, and a program downloaded
//! over seconds is copied under it (release's `download_program`),
//! rather than waiting out the spawns in flight once the file is closed:
//! a spawn can return before its child has let go of what it inherited
//! (golang/go#22315 rules vfork out so: the parent resumes before the
//! child closes its descriptors). A child forked before the open, or
//! after the close, never has the handle at all.
//!
//! Why not the others rust-lang/rust#114554 and golang/go#22315 discuss:
//! retrying the exec on ETXTBSY (Go's `cmd/go` test-script runner,
//! <https://go.googlesource.com/go/+/23c0121e4eb259cc1087d0f79a0803cbc71f500b>)
//! waits the window out instead of closing it, and a child slow to reach
//! its exec outlasts any bound; `O_TMPFILE` with `linkat`, or a rename,
//! keeps the same inode and its inherited writer; `fexecve` checks the
//! same inode's writers; `O_CLOFORK` isn't in Linux; a writing child
//! (sb's tests' `sh`) is a process per file. std has no lock of its own to
//! take (its spawn's environment lock is private), and no crate offers
//! one.
//!
//! A closure under [`writing`] must not start a child (it would wait for
//! itself), and keeps to the open, the writes and the close: a sync of
//! the file reopens it after.

use std::sync::{PoisonError, RwLock};

/// Shared while a child starts, exclusive while a program is written.
/// Nothing is guarded but the order, so a panic under it leaves nothing
/// to recover: a poisoned lock is taken as it is.
static LOCK: RwLock<()> = RwLock::new(());

/// `spawn`, a fork and exec, while no program file is open for writing:
/// the child's copy of the descriptor table is taken inside it.
pub fn forking<T>(spawn: impl FnOnce() -> T) -> T {
    let _shared = LOCK.read().unwrap_or_else(PoisonError::into_inner);
    spawn()
}

/// `write`, which opens, writes and closes a file sb will run, while no
/// child starts.
pub fn writing<T>(write: impl FnOnce() -> T) -> T {
    let _exclusive = LOCK.write().unwrap_or_else(PoisonError::into_inner);
    write()
}

#[cfg(test)]
mod tests {
    use std::sync::{TryLockError, mpsc};

    use super::*;
    use crate::testing::fork_held;

    /// These tests take the one process-wide lock, one at a time: a
    /// writer one leaves waiting holds back another's child start.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `taken` is the lock refusing for now (a poisoned lock,
    /// which the panic test leaves, is taken all the same).
    fn refused<T>(taken: &Result<T, TryLockError<T>>) -> bool {
        matches!(taken, Err(TryLockError::WouldBlock))
    }

    #[test]
    fn a_program_is_not_written_while_a_child_starts() {
        let _one = one_at_a_time();
        let held = fork_held();
        assert!(refused(&LOCK.try_write()), "a writer got in");
        drop(held);
        assert_eq!(writing(|| 1), 1);
    }

    #[test]
    fn no_child_starts_while_a_program_is_written() {
        let _one = one_at_a_time();
        let (inside, entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            writing(|| {
                inside.send(()).expect("inside");
                let _ = released.recv();
            });
        });
        entered.recv().expect("writing");
        assert!(refused(&LOCK.try_read()), "a child start got in");
        drop(release);
        writer.join().expect("writer");
        assert_eq!(forking(|| 2), 2);
    }

    #[test]
    fn children_start_side_by_side() {
        let _one = one_at_a_time();
        let _held = fork_held();
        assert!(
            !refused(&LOCK.try_read()),
            "one child's start held another's back"
        );
        assert_eq!(forking(|| 3), 3);
    }

    #[test]
    fn a_panic_under_the_lock_leaves_it_usable() {
        let _one = one_at_a_time();
        let panicked = std::panic::catch_unwind(|| writing(|| panic!("write failed")));
        assert!(panicked.is_err());
        assert_eq!(forking(|| 4), 4);
        assert_eq!(writing(|| 5), 5);
    }
}
