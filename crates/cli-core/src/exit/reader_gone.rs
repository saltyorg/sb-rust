//! The reader-gone watch (spec: "The reader-gone watch"): a command that
//! waits a long time with nothing to write (`sb logs`'s piped stream on a
//! quiet log) learns that stdout's reader went away without writing, as
//! `journalctl -f` and GNU `tail -f` do, and ends with the quiet 141.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;

use rustix::event::{EventfdFlags, PollFd, PollFlags, eventfd, poll};
use rustix::fs::FileType;
use rustix::io::retry_on_intr;
use tokio::task::JoinHandle;

use super::{OutputError, output_error};

/// Resolves once the reader of sb's stdout is gone, with no write needed.
///
/// Only when stdout is a pipe or FIFO, as GNU tail watches it
/// (<https://github.com/coreutils/coreutils/blob/master/src/tail.c>,
/// `monitor_output`'s `S_ISFIFO`), or a socket, which journalctl watches
/// too: it polls any stdout for `EPOLLHUP|EPOLLERR`
/// (<https://github.com/systemd/systemd/blob/main/src/journal/journalctl-show.c>).
/// Then a task on the
/// blocking pool ([`crate::spawn::blocking`]) polls a duplicate of fd 1
/// with no requested events, so only `POLLERR`, `POLLHUP` or `POLLNVAL`
/// return (poll(2): they are "always returned in revents",
/// <https://man7.org/linux/man-pages/man2/poll.2.html>). Linux sets
/// `POLLERR` on a pipe's write end once no reader is left (`pipe_poll` in
/// <https://github.com/torvalds/linux/blob/master/fs/pipe.c>; pipe(7):
/// FIFOs behave the same, <https://man7.org/linux/man-pages/man7/pipe.7.html>),
/// and `POLLHUP` on a Unix stream socket whose peer closed (both
/// directions shut: a peer's half-close doesn't). Anything else (a
/// terminal, a regular file, `/dev/null`) gets no watch:
/// [`ReaderGone::wait`] never resolves, and no thread is held.
///
/// The same poll waits on a stop `eventfd`, which a drop writes, so the
/// blocking thread returns at once. This is sb-host's `FollowWait`
/// pattern, rustix's `poll` and `eventfd`
/// (<https://docs.rs/rustix/1.1.6/rustix/event/fn.poll.html>,
/// <https://docs.rs/rustix/1.1.6/rustix/event/fn.eventfd.html>), for the
/// reason written in `crates/host/src/journal.rs`: tokio's `AsyncFd::new`
/// is deprecated as unsound in tokio 1.54.1, and its replacement,
/// `AsyncFd::register`, is an `unsafe fn`, which the workspace forbids.
/// Why not tokio's `pipe::Sender::from_owned_fd` with
/// `ready(Interest::ERROR)`: it sets `O_NONBLOCK` on the open file
/// description fd 1 shares with its duplicates, so sb's own blocking
/// writes and a child given fd 1 would meet `EAGAIN`; it refuses sockets;
/// and it needs tokio's `net` feature and an I/O driver sb doesn't build.
#[derive(Debug)]
pub struct ReaderGone {
    /// The stop `eventfd`, while a watch runs: readable once stopped.
    stop: Option<Arc<OwnedFd>>,
    /// The poll, until a wait took its outcome.
    poll: Option<JoinHandle<io::Result<Woke>>>,
    /// A wait saw the reader gone; it stays gone.
    gone: bool,
}

impl ReaderGone {
    /// The watch on fd 1.
    ///
    /// # Errors
    ///
    /// The system's, when fd 1 can't be duplicated or the `eventfd`
    /// created (`EMFILE` past the process's descriptor limit).
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, when stdout is a pipe, FIFO or socket
    /// (`spawn_blocking`'s own panic).
    pub fn stdout() -> io::Result<Self> {
        Self::on(rustix::stdio::stdout())
    }

    /// The watch on `fd`, as [`ReaderGone::stdout`] describes.
    fn on(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let watched = match rustix::fs::fstat(fd) {
            Ok(stat) => matches!(
                FileType::from_raw_mode(stat.st_mode),
                FileType::Fifo | FileType::Socket
            ),
            // Not expected: std's runtime reopens a closed fd 0-2 on
            // `/dev/null` before `main`, a character device, so even
            // `sb logs >&-` has an fd 1 to stat. Unwatched, as /dev/null is.
            Err(_) => false,
        };
        if !watched {
            return Ok(Self {
                stop: None,
                poll: None,
                gone: false,
            });
        }
        // Close-on-exec (`F_DUPFD_CLOEXEC`), so no child inherits this
        // duplicate (a child given fd 1 holds fd 1 itself, as any would);
        // it keeps the descriptor the poll waits on open whatever else
        // closes fd 1.
        let fd = fd.try_clone_to_owned()?;
        // Non-blocking, so a stop never blocks.
        let stop = Arc::new(eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)?);
        let poll = {
            let stop = Arc::clone(&stop);
            // The caller's span goes along (cli-core's `spawn`).
            crate::spawn::blocking(move || poll_gone(&fd, &stop))
        };
        Ok(Self {
            stop: Some(stop),
            poll: Some(poll),
            gone: false,
        })
    }

    /// Resolves once the reader is gone, to cli-core's output error for a
    /// broken pipe, so [`super::quiet_exit`] ends the run with 141, as a
    /// failed write does. Never resolves without a watch.
    ///
    /// # Cancel safety
    ///
    /// Cancel-safe: the poll runs on in its task, and the next wait takes
    /// its outcome. Once resolved, every later wait resolves at once.
    ///
    /// The poll's own error, other than `EINTR` (retried), isn't the
    /// reader's: it is logged, and the watch ends unresolved, as
    /// journalctl goes on without its watch; a write still sees the reader
    /// go. Not expected: poll(2) on two open descriptors fails only with
    /// `ENOMEM`.
    ///
    /// # Panics
    ///
    /// When the poll panicked (resumed here, as tokio's
    /// `JoinError::into_panic` example does,
    /// <https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.into_panic>),
    /// or the runtime shut down under it.
    pub async fn wait(&mut self) -> OutputError {
        if self.gone {
            return broken_pipe();
        }
        let Some(poll) = self.poll.as_mut() else {
            return std::future::pending().await;
        };
        let joined = poll.await;
        // Taken before a panic is resumed: a finished handle isn't polled
        // again.
        self.poll = None;
        match joined {
            Ok(Ok(Woke::Reader)) => {
                self.gone = true;
                broken_pipe()
            }
            // Only a drop stops the poll, and a dropped watch has no wait;
            // a stop that came some other way leaves it pending, not gone.
            Ok(Ok(Woke::Stop)) => std::future::pending().await,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "stdout's reader can't be watched; a write still sees it go");
                std::future::pending().await
            }
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        }
    }
}

impl Drop for ReaderGone {
    /// Stops the poll, so its blocking thread returns now. The poll holds
    /// its own reference to the `eventfd`, so it doesn't close under it.
    /// eventfd(2)'s write adds 1 to the counter, which nothing reads back;
    /// it fails only with `EAGAIN` at a counter of 2^64 - 2, which one stop
    /// can't reach, so the error is dropped.
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            rustix::io::write(stop.as_fd(), &1u64.to_ne_bytes()).ok();
        }
    }
}

/// The error a gone reader resolves to: a broken pipe on sb's output.
fn broken_pipe() -> OutputError {
    output_error(io::ErrorKind::BrokenPipe.into())
}

/// What ended a [`poll_gone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Woke {
    /// The watched descriptor: its reader is gone.
    Reader,
    /// The stop `eventfd`, which wins over the reader.
    Stop,
}

/// Blocks until `stop` is readable or `fd` has `POLLERR`, `POLLHUP` or
/// `POLLNVAL`: rustix's `poll` with no timeout, `EINTR` retried
/// (<https://docs.rs/rustix/1.1.6/rustix/io/fn.retry_on_intr.html>).
fn poll_gone(fd: &OwnedFd, stop: &OwnedFd) -> io::Result<Woke> {
    let mut fds = [
        PollFd::new(stop, PollFlags::IN),
        PollFd::new(fd, PollFlags::empty()),
    ];
    retry_on_intr(|| poll(&mut fds, None))?;
    Ok(if fds[0].revents().is_empty() {
        Woke::Reader
    } else {
        Woke::Stop
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use super::*;
    use crate::exit::quiet_exit;

    /// Long enough for a loaded host to wake a blocking thread; a watch
    /// that works resolves in microseconds.
    const WAIT: Duration = Duration::from_secs(10);
    /// How long a watch with its reader still open is watched not
    /// resolving: a regression that resolves at once shows well inside it.
    const STILL: Duration = Duration::from_millis(200);

    /// A runtime with the blocking pool and the timer, as sb's own.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a runtime")
    }

    /// The watch's error after the reader went: a broken pipe on sb's
    /// output, the quiet 141.
    fn assert_quiet_141(err: &OutputError) {
        assert_eq!(err.0.kind(), io::ErrorKind::BrokenPipe, "{err:?}");
        assert_eq!(quiet_exit(err), Some(141));
    }

    /// The reader end of a pipe dropped, with nothing written: the watch
    /// on the write end resolves with the quiet 141, and stays resolved.
    #[test]
    fn a_pipe_whose_reader_went_resolves_with_no_write() {
        runtime().block_on(async {
            let (reader, writer) = io::pipe().expect("a pipe");
            let mut gone = ReaderGone::on(writer.as_fd()).expect("the watch");
            drop(reader);
            let err = tokio::time::timeout(WAIT, gone.wait())
                .await
                .expect("the watch resolved after the reader went");
            assert_quiet_141(&err);
            let again = tokio::time::timeout(WAIT, gone.wait())
                .await
                .expect("a later wait resolved at once");
            assert_quiet_141(&again);
        });
    }

    /// With the reader open, the watch stays pending, written to or not,
    /// and the same watch resolves once the reader goes.
    #[test]
    fn a_pipe_with_its_reader_open_stays_pending() {
        runtime().block_on(async {
            let (reader, mut writer) = io::pipe().expect("a pipe");
            let mut gone = ReaderGone::on(writer.as_fd()).expect("the watch");
            assert!(
                tokio::time::timeout(STILL, gone.wait()).await.is_err(),
                "the watch resolved with its reader open"
            );
            writer.write_all(b"a line\n").expect("a write");
            assert!(
                tokio::time::timeout(STILL, gone.wait()).await.is_err(),
                "the watch resolved after a write, with its reader open"
            );
            drop(reader);
            let err = tokio::time::timeout(WAIT, gone.wait())
                .await
                .expect("the same watch resolved once the reader went");
            assert_quiet_141(&err);
        });
    }

    /// A socket whose peer closed resolves the watch, as a pipe does.
    #[test]
    fn a_socket_whose_peer_closed_resolves() {
        runtime().block_on(async {
            let (ours, peer) = UnixStream::pair().expect("a socket pair");
            let mut gone = ReaderGone::on(ours.as_fd()).expect("the watch");
            assert!(
                tokio::time::timeout(STILL, gone.wait()).await.is_err(),
                "the watch resolved with its peer open"
            );
            // A half-close is no hangup: the peer may still read.
            peer.shutdown(std::net::Shutdown::Write)
                .expect("a half-close");
            assert!(
                tokio::time::timeout(STILL, gone.wait()).await.is_err(),
                "the watch resolved at the peer's half-close"
            );
            drop(peer);
            let err = tokio::time::timeout(WAIT, gone.wait())
                .await
                .expect("the watch resolved after the peer closed");
            assert_quiet_141(&err);
        });
    }

    /// A regular file gets no watch: no poll is started, and a wait
    /// never resolves.
    #[test]
    fn a_regular_file_holds_no_thread_and_never_resolves() {
        runtime().block_on(async {
            let file = tempfile::tempfile().expect("a file");
            let mut gone = ReaderGone::on(file.as_fd()).expect("no watch");
            assert!(gone.poll.is_none(), "a poll started on a regular file");
            assert!(gone.stop.is_none(), "an eventfd made for a regular file");
            assert!(
                tokio::time::timeout(STILL, gone.wait()).await.is_err(),
                "a wait on a regular file resolved"
            );
        });
    }

    /// Dropping the watch ends its poll at once, with the reader still
    /// open, so its blocking thread is free.
    #[test]
    fn a_drop_frees_the_blocking_thread() {
        runtime().block_on(async {
            let (reader, writer) = io::pipe().expect("a pipe");
            let mut gone = ReaderGone::on(writer.as_fd()).expect("the watch");
            let poll = gone.poll.take().expect("a poll on a pipe");
            drop(gone);
            let woke = tokio::time::timeout(WAIT, poll)
                .await
                .expect("the poll ended after the drop")
                .expect("the poll didn't panic")
                .expect("the poll didn't fail");
            assert_eq!(woke, Woke::Stop);
            drop(reader);
        });
    }
}
