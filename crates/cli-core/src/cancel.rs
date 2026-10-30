//! Cancellation on SIGINT and SIGTERM (spec: "Cancellation").
//!
//! The first signal cancels a token every command holds and records the
//! exit code; a second exits at once with that code. Signals are read on a
//! thread with signal-hook's iterator, its documented high-level API
//! (https://docs.rs/signal-hook/0.4.5/signal_hook/iterator/index.html), so
//! no async runtime is needed until a command wants one. The token is
//! tokio-util's, which works without a runtime and lets async commands
//! `.await` cancellation
//! (https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::flag;
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

/// Shared shutdown state: the token commands watch, and how the run is to
/// end once the first request or signal says so.
#[derive(Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
    first: Arc<OnceLock<Ending>>,
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

    /// Turn SIGINT and SIGTERM into [`signalled`] on a thread, and make a
    /// second signal exit at once with the first one's code.
    ///
    /// The second-signal exit is signal-hook's documented double-Ctrl-C
    /// pattern, `flag::register_conditional_shutdown` armed by
    /// `flag::register`, registered first so a first signal only arms it
    /// (https://docs.rs/signal-hook/0.4.5/signal_hook/flag/fn.register_conditional_shutdown.html).
    /// It exits in the handler itself, skipping remaining cleanup. Each
    /// shutdown has a fixed code, so there is one per possible first signal:
    /// whichever flag the first signal set decides the code.
    ///
    /// [`signalled`]: Shutdown::signalled
    pub fn listen_for_signals(&self) -> std::io::Result<()> {
        let firsts = [SIGINT, SIGTERM].map(|signal| (signal, Arc::new(AtomicBool::new(false))));
        for second in [SIGINT, SIGTERM] {
            for (first, armed) in &firsts {
                flag::register_conditional_shutdown(
                    second,
                    i32::from(signal_exit_code(*first)),
                    Arc::clone(armed),
                )?;
            }
        }
        for (first, armed) in &firsts {
            flag::register(*first, Arc::clone(armed))?;
        }
        let mut signals = Signals::new([SIGINT, SIGTERM])?;
        let shutdown = self.clone();
        std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                for signal in signals.forever() {
                    shutdown.signalled(signal);
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
