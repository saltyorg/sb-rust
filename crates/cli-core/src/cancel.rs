//! Cancellation on SIGINT and SIGTERM (spec: "Cancellation").
//!
//! The first signal cancels a token every command holds and records the
//! exit code; a second runs the registered restore hooks (term-ui's
//! cursor and `ECHOCTL`; term-ui spec, decision 5), then exits at once
//! with that code. Signals are read on a thread with signal-hook's
//! iterator, its documented high-level API
//! (https://docs.rs/signal-hook/0.4.5/signal_hook/iterator/index.html), so
//! no async runtime is needed until a command wants one. The token is
//! tokio-util's, which works without a runtime and lets async commands
//! `.await` cancellation
//! (https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::Duration;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
pub use tokio_util::sync::CancellationToken;

/// The exit code for a run ended by a signal: 128 + the signal number, the
/// shell convention (130 for SIGINT, 143 for SIGTERM).
pub fn signal_exit_code(signal: i32) -> u8 {
    u8::try_from(128 + signal).unwrap_or(u8::MAX)
}

/// How the run is to end: the first request's code, and whether a signal
/// sent it. One record, so nothing can see the code without its origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ending {
    code: u8,
    by_signal: bool,
}

type Hook = Box<dyn Fn() + Send>;

/// How long the second-signal exit waits for the restore hooks: a hook
/// that blocks (a held stdout lock, a paused terminal) can't keep sb from
/// exiting, and working hooks take microseconds.
const RESTORE_DEADLINE: Duration = Duration::from_millis(500);

/// Restore hooks by id, so a [`RestoreHook`] can remove its own. Custom:
/// neither signal-hook nor std has ordered, removable exit hooks.
#[derive(Default)]
struct Hooks(Mutex<HookList>);

#[derive(Default)]
struct HookList {
    next: u64,
    hooks: Vec<(u64, Hook)>,
}

impl Hooks {
    fn lock(&self) -> MutexGuard<'_, HookList> {
        // A panic while the list was held can't leave it half-changed
        // (push and retain), so a poisoned lock is still usable.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Take every hook out, so running them holds no lock: a hook (or a
    /// guard dropped meanwhile) that needs the list can't deadlock the
    /// exit. The process is about to exit, so the list stays empty.
    fn take(&self) -> Vec<(u64, Hook)> {
        std::mem::take(&mut self.lock().hooks)
    }

    /// Run every hook in registration order. A panicking hook is caught,
    /// so it stops neither the others nor the exit (with sb's default
    /// `panic = "unwind"`).
    fn run(&self) {
        for (_, hook) in &self.take() {
            let _ = catch_unwind(AssertUnwindSafe(hook));
        }
    }

    /// Run the hooks on their own thread, wait for them up to
    /// [`RESTORE_DEADLINE`], then exit with `code` either way.
    fn run_then_exit(self: Arc<Self>, code: u8) -> ! {
        let (done, finished) = mpsc::channel();
        let restore = std::thread::Builder::new()
            .name("restore".into())
            .spawn(move || {
                self.run();
                let _ = done.send(());
            });
        // No thread (out of resources): exit without the hooks rather than
        // risk running a blocking one here.
        if restore.is_ok() {
            let _ = finished.recv_timeout(RESTORE_DEADLINE);
        }
        std::process::exit(i32::from(code))
    }
}

/// A registered restore hook; dropping it removes the hook.
#[must_use = "the hook is removed when this is dropped"]
pub struct RestoreHook {
    hooks: Arc<Hooks>,
    id: u64,
}

impl Drop for RestoreHook {
    fn drop(&mut self) {
        self.hooks.lock().hooks.retain(|(id, _)| *id != self.id);
    }
}

/// Shared shutdown state: the token commands watch, how the run is to end
/// once the first request or signal says so, and the hooks a second
/// signal runs before exiting.
#[derive(Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
    first: Arc<OnceLock<Ending>>,
    hooks: Arc<Hooks>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// A frozen copy of the current state, for reading the ending more than
    /// once (the log and the screen) without a signal changing it between.
    pub fn snapshot(&self) -> Self {
        let frozen = Self::new();
        if let Some(&ending) = self.first.get() {
            let _ = frozen.first.set(ending);
            frozen.token.cancel();
        }
        frozen
    }

    /// The token to watch; cancelled by the first request or signal.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Ask the run to end with `code` (sb-go's `Shutdown(code)`; 0 ends it
    /// successfully). Returns whether this was the first request; only the
    /// first sets the code.
    pub fn request(&self, code: u8) -> bool {
        self.record(code, false)
    }

    fn record(&self, code: u8, by_signal: bool) -> bool {
        let first = self.first.set(Ending { code, by_signal }).is_ok();
        self.token.cancel();
        first
    }

    /// The requested exit code, if the run was asked to end.
    pub fn requested(&self) -> Option<u8> {
        self.first.get().map(|ending| ending.code)
    }

    /// Whether a signal (not a command) ended the run.
    pub fn interrupted(&self) -> bool {
        self.first.get().is_some_and(|ending| ending.by_signal)
    }

    /// A signal arrived: record it with its code and cancel. Only the first
    /// counts; a second never gets here (see [`listen_for_signals`]).
    ///
    /// [`listen_for_signals`]: Shutdown::listen_for_signals
    pub fn signalled(&self, signal: i32) {
        self.record(signal_exit_code(signal), true);
    }

    /// Run `hook` before a second signal's exit, until the returned guard
    /// is dropped. Hooks run on their own thread while the rest of the run
    /// may still be busy, so they stay tiny (write a few bytes, one
    /// `tcsetattr`) and write through their own handle (the tty's fd or
    /// `/dev/tty`), never std's `stdout()`/`stderr()`, whose lock a stuck
    /// writer may hold. A hook still running after a short deadline is
    /// abandoned and sb exits; one that panics doesn't stop the exit.
    pub fn on_forced_exit(&self, hook: impl Fn() + Send + 'static) -> RestoreHook {
        let mut list = self.hooks.lock();
        let id = list.next;
        list.next += 1;
        list.hooks.push((id, Box::new(hook)));
        drop(list);
        RestoreHook {
            hooks: Arc::clone(&self.hooks),
            id,
        }
    }

    /// Turn SIGINT and SIGTERM into [`signalled`] on a thread; a second
    /// signal runs the restore hooks, then exits at once with the first
    /// signal's code (a command's own earlier request doesn't change it),
    /// skipping remaining cleanup.
    ///
    /// The structure of signal-hook's background-thread example, an
    /// iterator thread that restores the terminal (there for SIGTSTP)
    /// (https://docs.rs/signal-hook/0.4.5/signal_hook/#a-complex-signal-handling-with-a-background-thread).
    /// Why not that example's double Ctrl-C (`flag::register_conditional_shutdown`,
    /// used here before): it exits inside the signal handler, where
    /// restoring the terminal isn't async-signal-safe (term-ui spec,
    /// decision 5). Accepted limits: the iterator reports a signal that
    /// arrived twice before the thread read it once, and signals read
    /// together in arbitrary order (`SignalsInfo::pending`), so two signals
    /// within microseconds count as one (a third exits) and which of
    /// SIGINT and SIGTERM was first is then arbitrary; a person's two
    /// Ctrl-C presses are far apart. A third signal while the hooks run
    /// waits for the deadline.
    ///
    /// [`signalled`]: Shutdown::signalled
    pub fn listen_for_signals(&self) -> std::io::Result<()> {
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let shutdown = self.clone();
        std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                let mut first = None;
                for signal in signals.forever() {
                    match first {
                        None => {
                            first = Some(signal);
                            shutdown.signalled(signal);
                        }
                        Some(first) => {
                            Arc::clone(&shutdown.hooks).run_then_exit(signal_exit_code(first))
                        }
                    }
                }
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_requested_at_first() {
        let shutdown = Shutdown::new();
        assert_eq!(shutdown.requested(), None);
        assert!(!shutdown.token().is_cancelled());
    }

    #[test]
    fn a_request_cancels_the_token_and_keeps_its_code() {
        let shutdown = Shutdown::new();
        let token = shutdown.token();
        assert!(shutdown.request(5));
        assert!(token.is_cancelled());
        assert_eq!(shutdown.requested(), Some(5));
    }

    /// A command may end the run successfully (sb-go's `Shutdown(ctx, 0)`):
    /// 0 is a code like any other, not "nothing requested".
    #[test]
    fn a_request_for_0_is_recorded() {
        let shutdown = Shutdown::new();
        shutdown.request(0);
        assert_eq!(shutdown.requested(), Some(0));
    }

    /// The first request decides the exit code; later ones don't change it.
    #[test]
    fn the_first_request_wins() {
        let shutdown = Shutdown::new();
        assert!(shutdown.request(130));
        assert!(!shutdown.request(143));
        assert_eq!(shutdown.requested(), Some(130));
    }

    /// A snapshot is frozen: a signal after it doesn't change it, so the
    /// log and the screen (both read from it) agree.
    #[test]
    fn a_snapshot_does_not_change_after_a_later_signal() {
        let shutdown = Shutdown::new();
        let before = shutdown.snapshot();
        shutdown.signalled(signal_hook::consts::SIGINT);
        assert_eq!(before.requested(), None);
        assert!(!before.token().is_cancelled());
        let after = shutdown.snapshot();
        assert_eq!(after.requested(), Some(130));
        assert!(after.interrupted());
        assert!(after.token().is_cancelled());
    }

    /// A signal's code is the shell convention, 128 + the signal number.
    #[test]
    fn signal_codes_follow_the_shell_convention() {
        assert_eq!(signal_exit_code(signal_hook::consts::SIGINT), 130);
        assert_eq!(signal_exit_code(signal_hook::consts::SIGTERM), 143);
    }

    /// Hooks run in order; a removed one doesn't, a panicking one doesn't
    /// stop the rest.
    #[test]
    fn restore_hooks_run_unless_removed_and_survive_a_panic() {
        let shutdown = Shutdown::new();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let hook = |name: &'static str| {
            let ran = Arc::clone(&ran);
            move || ran.lock().expect("lock").push(name)
        };
        let _first = shutdown.on_forced_exit(hook("first"));
        drop(shutdown.on_forced_exit(hook("removed")));
        let _broken = shutdown.on_forced_exit(|| panic!("a broken hook"));
        let _last = shutdown.on_forced_exit(hook("last"));
        shutdown.hooks.run();
        assert_eq!(*ran.lock().expect("lock"), ["first", "last"]);
    }

    /// Running the hooks holds no lock a hook or a dropping guard needs: a
    /// hook that drops another hook's guard returns (bounded, so a
    /// regression fails instead of hanging).
    #[test]
    fn a_hook_can_drop_a_guard_while_hooks_run() {
        let shutdown = Shutdown::new();
        let slot = Arc::new(Mutex::new(Some(shutdown.on_forced_exit(|| {}))));
        let held = Arc::clone(&slot);
        // Never dropped: on a regression its drop would wait on the stuck
        // run and hang the test instead of failing it.
        let _dropper = std::mem::ManuallyDrop::new(
            shutdown.on_forced_exit(move || drop(held.lock().expect("lock").take())),
        );
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            shutdown.hooks.run();
            let _ = done.send(());
        });
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "running the hooks deadlocked"
        );
        assert!(slot.lock().expect("lock").is_none());
    }

    /// A command's own request isn't an interruption; a signal is.
    #[test]
    fn only_signals_count_as_interrupted() {
        let by_command = Shutdown::new();
        by_command.request(3);
        assert!(!by_command.interrupted());
        let by_signal = Shutdown::new();
        by_signal.signalled(signal_hook::consts::SIGINT);
        assert!(by_signal.interrupted());
        assert_eq!(by_signal.requested(), Some(130));
    }
}
