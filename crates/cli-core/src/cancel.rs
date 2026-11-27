//! Cancellation on SIGINT, SIGTERM and SIGHUP (a hangup, unless ignored
//! at startup as under `nohup`), Ctrl-\\ as the hard escape, and signals
//! held for an attached child (spec: "Cancellation").
//!
//! The first signal cancels a token every command holds and records the
//! exit code; a second runs the registered restore hooks (term-ui's
//! cursor and echoes; term-ui spec, decision 5), then ends sb at once
//! by the first signal (exec decision 7). Signals are read on a thread
//! with signal-hook's iterator, its documented high-level API
//! (<https://docs.rs/signal-hook/0.4.5/signal_hook/iterator/index.html>), so
//! no async runtime is needed until a command wants one. The token is
//! tokio-util's, which works without a runtime and lets async commands
//! `.await` cancellation
//! (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html>).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;
pub use tokio_util::sync::CancellationToken;

/// The exit code for a run ended by a signal: 128 + the signal number, the
/// shell convention (130 for SIGINT, 143 for SIGTERM).
#[must_use]
pub fn signal_exit_code(signal: i32) -> u8 {
    u8::try_from(128 + signal).unwrap_or(u8::MAX)
}

/// End the process by `signal`'s default action, as if it had never been
/// caught: signal-hook's `emulate_default_handler` resets the handler,
/// unblocks the signal and raises it
/// (<https://docs.rs/signal-hook/0.4.5/signal_hook/low_level/fn.emulate_default_handler.html>).
/// For SIGINT, SIGTERM and SIGHUP it never returns (should the raise fail, it
/// aborts, so sb dies of SIGABRT); a signal it doesn't know, or one whose
/// default isn't to end a process, exits with 128 + the signal instead.
pub(crate) fn end_by(signal: i32) -> ! {
    if signal == SIGQUIT {
        // SIGQUIT's default action dumps core; sb's run ends by it, not
        // with a core (exec decision 7). Not dumpable, rather than
        // RLIMIT_CORE 0: a `core_pattern` that pipes to a helper (apport,
        // systemd-coredump) gets the core whatever the limit says, and
        // `PR_SET_DUMPABLE` 0 stops every dump
        // (https://docs.rs/rustix/1.1.6/rustix/process/fn.set_dumpable_behavior.html).
        use rustix::process::{DumpableBehavior, set_dumpable_behavior};
        let _ = set_dumpable_behavior(DumpableBehavior::NotDumpable);
    }
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    std::process::exit(i32::from(signal_exit_code(signal)))
}

/// How the run is to end: the first request's code, and the signal that
/// sent it, if one did. One record, so nothing can see the code without
/// its origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ending {
    code: u8,
    signal: Option<i32>,
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

    /// Run the hooks, then end by `signal` either way (the ending kills
    /// the process, so only the part before it can be tested in-process).
    fn run_then_end_by(self: Arc<Self>, signal: i32) -> ! {
        self.run_within(RESTORE_DEADLINE);
        end_by(signal)
    }

    /// Run the hooks on their own thread and wait for them up to
    /// `deadline`; a hook still running then is abandoned.
    fn run_within(self: Arc<Self>, deadline: Duration) {
        let (done, finished) = mpsc::channel();
        // The caller's span goes along, so a hook's records carry the run
        // id (`crate::spawn`).
        let restore = crate::spawn::thread(
            std::thread::Builder::new().name("restore".into()),
            move || {
                self.run();
                let _ = done.send(());
            },
        );
        // No thread (out of resources): exit without the hooks rather than
        // risk running a blocking one here.
        if restore.is_ok() {
            let _ = finished.recv_timeout(deadline);
        }
    }
}

/// A registered restore hook; dropping it removes the hook.
#[must_use = "the hook is removed when this is dropped"]
pub struct RestoreHook {
    hooks: Arc<Hooks>,
    id: u64,
}

impl std::fmt::Debug for RestoreHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreHook")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
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
    /// The signals held for an attached child (exec decision 2).
    holds: Arc<Holds>,
}

/// The ending and whether the token is cancelled; the hooks and holds are
/// left out (reading the holds takes their lock).
impl std::fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shutdown")
            .field("ending", &self.first.get())
            .field("cancelled", &self.token.is_cancelled())
            .finish_non_exhaustive()
    }
}

/// How long a hold's start or end waits for the signals thread to decide
/// on a delivery it hasn't read yet. The thread reads within microseconds;
/// the bound only matters if it is stuck (a forced exit's restore hooks,
/// up to [`RESTORE_DEADLINE`], after which sb ends).
const SETTLE: Duration = Duration::from_secs(1);

/// How long [`Hold::child_ended_by`] waits for sb's own copy of the signal
/// that killed the child. The terminal sends Ctrl-C to sb and the child
/// together, but nothing orders sb's handler (or the thread's reading)
/// before the reap that tells exec the child died. A child killed by a
/// signal sb never got costs this wait once.
const DELIVERY: Duration = Duration::from_millis(100);

/// The held signals: SIGINT and SIGQUIT, by slot.
const HELD: [i32; 2] = [SIGINT, SIGQUIT];
/// SIGINT's slot in [`HELD`].
const SIGINT_SLOT: usize = 0;

fn slot(signal: i32) -> Option<usize> {
    HELD.iter().position(|held| *held == signal)
}

/// How long after sb's signals thread read the first SIGTERM or SIGHUP
/// another SIGTERM or SIGHUP isn't a second signal (decisions 2 and 9;
/// cli-M2): a copy, not the user asking again. Stopping a session scope
/// (a reboot during an SSH session, `loginctl terminate-session`,
/// `systemctl stop`) sends every process in it SIGTERM, then SIGHUP
/// (logind's scopes have `SendSIGHUP=yes`), and under sudo, sudo relays
/// its own copy of each; an interactive bash re-sends a hangup to its jobs
/// before the kernel hangs up the foreground group. The copies came within
/// half a millisecond (docs/platform-notes/exec-processes.md, "Doubled
/// signals"); 250 ms leaves room for a loaded host and is still far
/// shorter than a person signalling twice on purpose. A signal at or past
/// the window is a second signal, as before.
const REPEAT_WINDOW: Duration = Duration::from_millis(250);

/// Whether `signal`, at `now`, is a copy of the `first` signal counted
/// (and when the signals thread read it) rather than a second one: a
/// SIGTERM or SIGHUP inside [`REPEAT_WINDOW`] of a first SIGTERM or SIGHUP,
/// whichever came first (SIGHUP is handled as SIGTERM is, decision 2; sb
/// still ends by the first). SIGINT pairs with neither: two Ctrl-C's still
/// force the exit (decision 9). The time is passed in, so tests drive it
/// (M-MOCKABLE-SYSCALLS).
fn repeats(first: (i32, Instant), signal: i32, now: Instant) -> bool {
    let (first, at) = first;
    let terminating = |signal| matches!(signal, SIGTERM | SIGHUP);
    terminating(signal) && terminating(first) && now.saturating_duration_since(at) < REPEAT_WINDOW
}

/// Whether `signal` was ignored when sb started (`nohup` ignores SIGHUP;
/// decision 8), read from `/proc/self/status`. A status that can't be
/// read or parsed counts as not ignored, so a hangup cancels the run
/// rather than kill it.
///
/// Why not signal-hook, rustix or nix: none queries a disposition
/// safely. signal-hook (0.4.5) has no query; rustix's `sigaction` is in
/// its `runtime` module, `unsafe` and not meant for applications; nix's
/// `sigaction` is an `unsafe fn` behind its `signal` feature. The
/// workspace forbids `unsafe`. procfs-core's `Status::sigign` parses the
/// same line; not added for one field.
fn ignored_at_startup(signal: i32) -> bool {
    std::fs::read_to_string("/proc/self/status").is_ok_and(|status| ignored_in(&status, signal))
}

/// Whether `status` (`/proc/<pid>/status`'s text) marks `signal` as
/// ignored: its `SigIgn` line, a hexadecimal mask in which signal `n` is
/// bit `n - 1`, documented in `proc_pid_status(5)`
/// (<https://man7.org/linux/man-pages/man5/proc_pid_status.5.html>).
fn ignored_in(status: &str, signal: i32) -> bool {
    let Some(bit) = signal
        .checked_sub(1)
        .and_then(|bit| u32::try_from(bit).ok())
        .filter(|&bit| bit < u64::BITS)
    else {
        return false;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .is_some_and(|mask| mask & (1 << bit) != 0)
}

/// The hold count and what was noted, under one lock with a condvar, so
/// the signals thread's decision and a hold's start, question and end see
/// one state (a compound invariant: one `Mutex`, not loose atomics).
/// Custom: no dependency holds signals for a child; signal-hook gives the
/// delivery flags (`flag::register`, safe), std the lock.
#[derive(Default)]
struct Holds {
    state: Mutex<HoldState>,
    changed: Condvar,
    /// Set by signal-hook's handler when the signal is delivered (before
    /// the iterator's wake-up: actions run in registration order;
    /// UNVERIFIED as API, checked in signal-hook-registry 1.4.8's
    /// `src/lib.rs`, "Actions are stored and executed in the order they
    /// were registered", and pinned by `Cargo.lock`), cleared by the signals
    /// thread under the lock once it has decided: set means "delivered,
    /// not yet decided". Decided while held, it is noted; so a hold that
    /// starts or ends waits for it ([`Holds::settled`]) rather than let
    /// the thread decide a delivery from inside the hold as outside it.
    arrived: [Arc<AtomicBool>; 2],
}

#[derive(Default)]
struct HoldState {
    /// How many holds are on (they nest).
    count: usize,
    /// The held signals noted, by slot, since the outermost hold began.
    noted: [bool; 2],
    /// A SIGINT sb raised at itself was decided by
    /// [`Shutdown::interrupt_raised`], before the signals thread read it:
    /// the thread's next read of SIGINT is that delivery, unless another
    /// came since (its delivery flag set again).
    raised: bool,
    /// The first signal counted, by the signals thread or by
    /// [`Shutdown::interrupt_raised`], so that either decides a later one
    /// as a second, a command's own request before it or not.
    counted: Option<i32>,
}

impl Holds {
    fn lock(&self) -> MutexGuard<'_, HoldState> {
        // Nothing under the lock can panic between its writes (field
        // stores only), so a poisoned lock still holds a whole state.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn pending(&self) -> bool {
        self.arrived.iter().any(|flag| flag.load(Ordering::SeqCst))
    }

    /// Wait (up to [`SETTLE`]) until the signals thread has decided every
    /// delivery so far. The thread clears a flag and notifies under the
    /// lock, so the wake-up can't be lost.
    fn settled<'a>(&self, state: MutexGuard<'a, HoldState>) -> MutexGuard<'a, HoldState> {
        self.changed
            .wait_timeout_while(state, SETTLE, |_| self.pending())
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

/// What the signals thread does with a signal (cli-core spec,
/// "Cancellation"; exec decisions 2 and 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    /// The first: record it and cancel the token.
    Cancel,
    /// A second: run the restore hooks, then end by the first.
    ForcedExit(i32),
    /// Held: the terminal sent it to the attached child too; sb notes it,
    /// takes no action and doesn't count it ("wait and cooperative exit").
    Note,
    /// SIGQUIT outside a hold: Ctrl-\ stays the hard escape (term-ui
    /// decision 5): the restore hooks run (bounded by the deadline), then
    /// sb ends by it, whatever the run was doing (maintainer, 2026-11-05).
    HardEscape,
    /// A SIGINT sb raised at itself, decided already by
    /// [`Shutdown::interrupt_raised`]: nothing more to do.
    Raised,
}

/// The signals thread's decision for `signal`, given the first signal
/// counted so far and whether an attached child holds the signals.
fn decide(first: Option<i32>, held: bool, signal: i32) -> Act {
    match (signal, held, first) {
        (SIGQUIT, false, _) => Act::HardEscape,
        (SIGINT | SIGQUIT, true, _) => Act::Note,
        (_, _, None) => Act::Cancel,
        (_, _, Some(first)) => Act::ForcedExit(first),
    }
}

/// While this lives, SIGINT and SIGQUIT are held (exec's `attached`): noted
/// and not acted on, as the terminal delivers them to the attached child
/// itself. A SIGTERM or SIGHUP still cancels the token (exec passes a
/// SIGTERM to the child's pid), and a second one still forces the exit. Noted, not
/// blocked: a blocked SIGINT would stay pending and act once the hold
/// ends, and the child must keep the default dispositions (exec decision
/// 2), which blocking in sb would not change anyway.
///
/// Starting and ending a hold, and [`Hold::child_ended_by`], block the
/// calling thread briefly (see [`SETTLE`] and [`DELIVERY`]): from async
/// code, call them where a short stall is fine or through
/// `spawn_blocking`.
#[must_use = "the signals are held only while this lives"]
pub struct Hold {
    shutdown: Shutdown,
}

impl std::fmt::Debug for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No state: reading it would take the lock, and a `{:?}` must never
        // wait on the signals thread.
        f.debug_struct("Hold").finish_non_exhaustive()
    }
}

impl Hold {
    /// The attached child ended, by `signal` if one killed it. If sb itself
    /// received that signal while held, the run is interrupted by it (sb
    /// then ends by it); a child that exited, or died of a signal sb never
    /// got, isn't sb's interruption. Returns whether it was.
    ///
    /// Waits up to [`DELIVERY`] for sb's own copy of the signal (see
    /// there). The note is used up, so a later child under the same outer
    /// hold isn't credited with it.
    pub fn child_ended_by(&self, signal: Option<i32>) -> bool {
        let Some((signal, slot)) = signal.and_then(|signal| Some((signal, slot(signal)?))) else {
            return false;
        };
        let holds = &self.shutdown.holds;
        let mut state = holds
            .changed
            .wait_timeout_while(holds.lock(), DELIVERY, |state| !state.noted[slot])
            .unwrap_or_else(PoisonError::into_inner)
            .0;
        let noted = std::mem::take(&mut state.noted[slot]);
        drop(state);
        if noted {
            self.shutdown.signalled(signal);
        }
        noted
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let holds = &self.shutdown.holds;
        let mut state = holds.settled(holds.lock());
        state.count -= 1;
        if state.count == 0 {
            state.noted = [false; 2];
        }
    }
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A frozen copy of the current state, for reading the ending more than
    /// once (the log and the screen) without a signal changing it between.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        let frozen = Self::new();
        if let Some(&ending) = self.first.get() {
            let _ = frozen.first.set(ending);
            frozen.token.cancel();
        }
        frozen
    }

    /// The token to watch; cancelled by the first request or signal.
    #[must_use]
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Ask the run to end with `code` (sb-go's `Shutdown(code)`; 0 ends it
    /// successfully). Returns whether this was the first request; only the
    /// first sets the code.
    #[must_use]
    pub fn request(&self, code: u8) -> bool {
        self.record(code, None)
    }

    fn record(&self, code: u8, signal: Option<i32>) -> bool {
        let first = self.first.set(Ending { code, signal }).is_ok();
        self.token.cancel();
        first
    }

    /// The requested exit code, if the run was asked to end.
    #[must_use]
    pub fn requested(&self) -> Option<u8> {
        self.first.get().map(|ending| ending.code)
    }

    /// Whether a signal (not a command) ended the run.
    #[must_use]
    pub fn interrupted(&self) -> bool {
        self.signal().is_some()
    }

    /// The signal that ended the run, if one (not a command) did.
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        self.first.get().and_then(|ending| ending.signal)
    }

    /// A signal ended the run: record it with its code and cancel. Only the
    /// first request or signal sets the ending; a later call only cancels.
    /// The signals thread and [`Hold::child_ended_by`] call it.
    pub fn signalled(&self, signal: i32) {
        self.record(signal_exit_code(signal), Some(signal));
    }

    /// Hold SIGINT and SIGQUIT while an attached child runs (exec decision
    /// 2): see [`Hold`]. Holds nest; nothing noted before it counts.
    ///
    /// A signal delivered before the hold but not yet read by the signals
    /// thread is decided first, as outside the hold: the child, spawned
    /// after, never got it.
    pub fn hold(&self) -> Hold {
        // A test's gate on the signals thread opens here, so its delivery
        // is decided as the hold settles (`testing::signals`).
        #[cfg(feature = "testing")]
        crate::testing::signals::a_hold_starts(self.holds.pending());
        let mut state = self.holds.settled(self.holds.lock());
        if state.count == 0 {
            state.noted = [false; 2];
        }
        state.count += 1;
        drop(state);
        Hold {
            shutdown: self.clone(),
        }
    }

    /// Whether an attached child holds the signals.
    #[must_use]
    pub fn held(&self) -> bool {
        self.holds.lock().count > 0
    }

    /// The signals thread's step for `signal`, given the first signal it
    /// counted itself (one a hold recorded, its child dead of it, counts
    /// too, so the next signal forces the exit): under the holds' lock,
    /// decide, mark the delivery decided, note a held signal and record a
    /// first one, so [`Shutdown::interrupt_raised`] sees each delivery
    /// either undecided or recorded; the caller acts on the rest.
    fn step(&self, first: Option<i32>, signal: i32) -> Act {
        let holds = &self.holds;
        let mut state = holds.lock();
        let slot = slot(signal);
        let act = if signal == SIGINT
            && std::mem::take(&mut state.raised)
            && !slot.is_some_and(|slot| holds.arrived[slot].load(Ordering::SeqCst))
        {
            Act::Raised
        } else {
            decide(
                first.or(state.counted).or(self.signal()),
                state.count > 0,
                signal,
            )
        };
        if let Some(slot) = slot {
            holds.arrived[slot].store(false, Ordering::SeqCst);
            if act == Act::Note {
                state.noted[slot] = true;
            }
        }
        if act == Act::Cancel {
            state.counted.get_or_insert(signal);
            self.signalled(signal);
        }
        holds.changed.notify_all();
        act
    }

    /// sb raised SIGINT at itself and the raise has returned: console
    /// does so for Ctrl-C at a prompt, then returns `Interrupted`
    /// (UNVERIFIED: console's docs don't say so; checked against its
    /// source, `read_single_key`,
    /// <https://docs.rs/console/0.16.6/src/console/unix_term.rs.html>,
    /// pinned by `Cargo.lock`). Decide that
    /// delivery now, as the signals thread would, rather than wait for the
    /// thread to read it, and say whether a signal has ended the run (so
    /// `finish_run` ends sb by it). Nothing waits: the delivery flag
    /// (`Holds::arrived`) is set by signal-hook's handler itself
    /// (`flag::register`, an atomic store, async-signal-safe;
    /// <https://docs.rs/signal-hook/0.4.5/signal_hook/flag/fn.register.html>),
    /// and a signal raised at the calling thread is handled before
    /// `raise` returns (raise(3),
    /// <https://man7.org/linux/man-pages/man3/raise.3.html>). Decided
    /// here, it is the thread's next read of SIGINT, which then does
    /// nothing; a Ctrl-C delivered after it still counts as a second
    /// signal. A delivery that isn't a first (a second signal forces the
    /// exit) or that a hold would note is left to the thread.
    ///
    /// A caller that has just put the terminal in raw mode (term-ui's
    /// confirm) calls it too, for a Ctrl-C the kernel turned into a SIGINT
    /// just before: handled on the calling thread by then as well, it is
    /// decided here the same way, and nothing was delivered when no Ctrl-C
    /// came.
    #[must_use]
    pub fn interrupt_raised(&self) -> bool {
        let holds = &self.holds;
        let mut state = holds.lock();
        let first = state.counted.or(self.signal());
        // Decided first, then the flag taken: one swap, so a delivery
        // can't land between reading the flag and clearing it.
        if decide(first, state.count > 0, SIGINT) == Act::Cancel
            && holds.arrived[SIGINT_SLOT].swap(false, Ordering::SeqCst)
        {
            state.raised = true;
            state.counted.get_or_insert(SIGINT);
            self.signalled(SIGINT);
            holds.changed.notify_all();
        }
        drop(state);
        self.interrupted()
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

    /// Turn SIGINT, SIGTERM and SIGHUP into [`signalled`] on a thread; a
    /// second signal runs the restore hooks, then ends sb at once by the
    /// first signal (a command's own earlier request doesn't change it;
    /// exec decision 7), skipping remaining cleanup. A hangup is handled
    /// as SIGTERM is (decision 2), unless SIGHUP was ignored when sb
    /// started (`nohup`): then it is left ignored (decision 8). SIGQUIT is
    /// the hard escape (the hooks, then the end by it), and while a
    /// [`Hold`] is on, SIGINT and SIGQUIT are only noted. Call it once per
    /// process, before anything else registers a handler: each call starts
    /// its own thread.
    ///
    /// Signals are matched by number, so a `kill -INT` from another process
    /// counts as a Ctrl-C. Why not signal-hook's `WithOrigin`, which tells
    /// them apart: it needs the `extended-siginfo` feature, which compiles
    /// C, and isn't needed since exec decision 2's revision
    /// (docs/platform-notes/exec-processes.md). While held, the way to end sb from outside is a
    /// SIGTERM (`kill -TERM`), which sudo passes on to sb as well.
    ///
    /// The structure of signal-hook's background-thread example, an
    /// iterator thread that restores the terminal (there for SIGTSTP)
    /// (<https://docs.rs/signal-hook/0.4.5/signal_hook/#a-complex-signal-handling-with-a-background-thread>).
    /// Why not that example's double Ctrl-C (`flag::register_conditional_shutdown`,
    /// used here before): it exits inside the signal handler, where
    /// restoring the terminal isn't async-signal-safe (term-ui spec,
    /// decision 5). A SIGTERM or SIGHUP within [`REPEAT_WINDOW`] of a first
    /// SIGTERM or SIGHUP isn't a second signal (a session scope's stop,
    /// sudo's relay, an interactive bash's re-sent hangup). Accepted
    /// limits: a signal delivered twice before the thread read it is read
    /// once, and signals read together come in arbitrary order
    /// (`SignalsInfo::pending`), so which of SIGINT and SIGTERM, or of
    /// SIGTERM and SIGHUP, was first is then arbitrary (sb ends by the one
    /// read first); a person's two Ctrl-C presses are far apart. A third
    /// signal while the hooks run
    /// waits for the deadline. Likewise, a second SIGINT or SIGQUIT
    /// delivered while the thread decides the first is queued but its
    /// delivery flag is already cleared, so a hold starting or ending in
    /// those microseconds doesn't wait for it. Should this fail after the
    /// delivery flags are registered, they stay (SIGINT and SIGQUIT no
    /// longer act by default, and holds wait their bound); sb ends the run
    /// on that error.
    ///
    /// [`signalled`]: Shutdown::signalled
    ///
    /// # Errors
    ///
    /// The OS error when a delivery flag or the signal iterator can't be
    /// registered, or the listening thread can't be started.
    pub fn listen_for_signals(&self) -> std::io::Result<()> {
        // Before any handler is registered: a handler would replace an
        // inherited "ignored" (decision 8).
        let hangup = !ignored_at_startup(SIGHUP);
        // The delivery flags first, so each is set before the iterator
        // wakes the thread (see `Holds::arrived`).
        for (signal, flag) in HELD.iter().zip(&self.holds.arrived) {
            signal_hook::flag::register(*signal, Arc::clone(flag))?;
        }
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGQUIT])?;
        if hangup {
            signals.add_signal(SIGHUP)?;
        }
        let shutdown = self.clone();
        // The run's span is the thread's for its life, so its records
        // carry the run id (`crate::spawn`).
        crate::spawn::thread(
            std::thread::Builder::new().name("signals".into()),
            move || {
                // The first signal counted: this thread's, or the one a
                // hold recorded when its child died of it (then the next
                // signal forces the exit). A command's own request isn't
                // one: a signal after it still cancels first.
                // With when it came, for the repeat window.
                let mut first: Option<(i32, Instant)> = None;
                for signal in signals.forever() {
                    if first.is_some_and(|first| repeats(first, signal, Instant::now())) {
                        tracing::debug!(signal, "a copy of the first signal; not a second one");
                        continue;
                    }
                    // A test's gate holds the decision back until a hold
                    // starts (`testing::signals`).
                    #[cfg(feature = "testing")]
                    crate::testing::signals::before_a_decision();
                    match shutdown.step(first.map(|(first, _)| first), signal) {
                        Act::Cancel => first = Some((signal, Instant::now())),
                        Act::ForcedExit(first) => {
                            Arc::clone(&shutdown.hooks).run_then_end_by(first)
                        }
                        Act::Note => tracing::debug!(signal, "held signal noted"),
                        Act::HardEscape => Arc::clone(&shutdown.hooks).run_then_end_by(SIGQUIT),
                        Act::Raised => tracing::debug!(signal, "raised by sb, decided already"),
                    }
                }
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Debug` shows the ending and the token, under a hold too (it never
    /// takes the holds' lock); a restore hook shows its id.
    #[test]
    fn debug_shows_the_ending() {
        let shutdown = Shutdown::new();
        let hook = shutdown.on_forced_exit(|| {});
        let hold = shutdown.hold();
        assert_eq!(
            format!("{shutdown:?}"),
            "Shutdown { ending: None, cancelled: false, .. }"
        );
        drop(hold);
        let _ = shutdown.request(3);
        assert_eq!(
            format!("{shutdown:?}"),
            "Shutdown { ending: Some(Ending { code: 3, signal: None }), cancelled: true, .. }"
        );
        let shown = format!("{hook:?}");
        assert!(shown.starts_with("RestoreHook { id: "), "{shown}");
        assert!(shown.ends_with(", .. }"), "{shown}");
    }

    /// Every case of the signals thread's decision (cli-core spec, and exec
    /// decisions 2 and 12): outside a hold, the first SIGINT or SIGTERM
    /// cancels, a second forces the exit, and SIGQUIT is the hard escape;
    /// while an attached child holds the signals, SIGINT and SIGQUIT are
    /// noted only (never counted), and SIGTERM cancels, then forces.
    #[test]
    fn the_signals_thread_decides_each_case() {
        use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};
        for signal in [SIGINT, SIGTERM] {
            assert_eq!(decide(None, false, signal), Act::Cancel, "{signal}");
            assert_eq!(decide(Some(SIGINT), false, signal), Act::ForcedExit(SIGINT));
            assert_eq!(
                decide(Some(SIGTERM), false, signal),
                Act::ForcedExit(SIGTERM)
            );
        }
        assert_eq!(decide(None, false, SIGQUIT), Act::HardEscape);
        assert_eq!(decide(Some(SIGINT), false, SIGQUIT), Act::HardEscape);
        for signal in [SIGINT, SIGQUIT] {
            assert_eq!(decide(None, true, signal), Act::Note, "{signal}");
            // During a forwarded SIGTERM's grace: still not counted.
            assert_eq!(decide(Some(SIGTERM), true, signal), Act::Note, "{signal}");
        }
        assert_eq!(decide(None, true, SIGTERM), Act::Cancel);
        assert_eq!(
            decide(Some(SIGTERM), true, SIGTERM),
            Act::ForcedExit(SIGTERM)
        );
    }

    /// A held signal interrupts the run only if the attached child died of
    /// it; one the child survived (or never got from sb) doesn't.
    #[test]
    fn a_held_signal_counts_only_if_the_child_died_of_it() {
        use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};
        let shutdown = Shutdown::new();
        let hold = shutdown.hold();
        assert!(shutdown.held());
        assert_eq!(read(&shutdown, SIGINT), Act::Note);
        assert!(
            !shutdown.token().is_cancelled(),
            "a held SIGINT doesn't cancel"
        );
        assert!(!hold.child_ended_by(None), "it exited");
        assert!(!hold.child_ended_by(Some(SIGQUIT)), "sb got no SIGQUIT");
        assert!(!shutdown.interrupted());
        assert!(hold.child_ended_by(Some(SIGINT)));
        assert!(shutdown.interrupted());
        assert_eq!(shutdown.signal(), Some(SIGINT));
        drop(hold);
        assert!(!shutdown.held());
        // A signal read before the hold was decided outside it.
        let again = Shutdown::new();
        assert_eq!(read(&again, SIGQUIT), Act::HardEscape);
        let hold = again.hold();
        assert!(!hold.child_ended_by(Some(SIGQUIT)), "read before the hold");
        drop(hold);
        // A SIGTERM isn't held: it went the usual way.
        let terminated = Shutdown::new();
        let hold = terminated.hold();
        assert_eq!(read(&terminated, SIGTERM), Act::Cancel);
        assert!(!hold.child_ended_by(Some(SIGTERM)));
    }

    /// A copy of the first SIGTERM or SIGHUP inside the repeat window isn't
    /// a second signal (decision 9; a relayed copy under sudo, an
    /// interactive bash's re-sent hangup): inside it, no; at its edge and
    /// past it, a second signal as before. Another signal, or SIGINT,
    /// is never a repeat. The clock is injected (M-MOCKABLE-SYSCALLS).
    #[test]
    fn a_repeat_within_the_window_is_not_a_second_signal() {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
        let at = Instant::now();
        let inside = at + REPEAT_WINDOW / 2;
        let just_inside = at + REPEAT_WINDOW.saturating_sub(Duration::from_nanos(1));
        let edge = at + REPEAT_WINDOW;
        let past = at + REPEAT_WINDOW * 2;
        for signal in [SIGTERM, SIGHUP] {
            assert!(repeats((signal, at), signal, at), "{signal}");
            assert!(repeats((signal, at), signal, inside), "{signal}");
            assert!(repeats((signal, at), signal, just_inside), "{signal}");
            assert!(!repeats((signal, at), signal, edge), "{signal}");
            assert!(!repeats((signal, at), signal, past), "{signal}");
        }
        // Two SIGINTs still force the exit (decision 9).
        assert!(!repeats((SIGINT, at), SIGINT, inside));
        assert!(!repeats((SIGQUIT, at), SIGQUIT, inside));
        // A session scope's stop sends SIGTERM, then SIGHUP (SendSIGHUP):
        // inside the window either is a copy of the other, whichever came
        // first; past it, a second signal.
        assert!(repeats((SIGTERM, at), SIGHUP, inside));
        assert!(repeats((SIGHUP, at), SIGTERM, inside));
        assert!(!repeats((SIGTERM, at), SIGHUP, edge));
        assert!(!repeats((SIGHUP, at), SIGTERM, past));
        // SIGINT pairs with neither, however soon.
        assert!(!repeats((SIGINT, at), SIGTERM, inside));
        assert!(!repeats((SIGTERM, at), SIGINT, inside));
        assert!(!repeats((SIGHUP, at), SIGINT, inside));
        // A clock that went backwards (it can't, `Instant` is monotonic)
        // reads as inside.
        assert!(repeats((SIGTERM, inside), SIGTERM, at));
        assert_eq!(REPEAT_WINDOW, Duration::from_millis(250));
    }

    /// SIGHUP is decided as SIGTERM is (decision 2), under a hold too
    /// (exec decision 12): the first cancels, a second forces the exit by
    /// the first, and it is never held.
    #[test]
    fn a_hangup_is_decided_as_sigterm_is() {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        for held in [false, true] {
            assert_eq!(decide(None, held, SIGHUP), Act::Cancel, "{held}");
            assert_eq!(decide(Some(SIGHUP), held, SIGHUP), Act::ForcedExit(SIGHUP));
            assert_eq!(decide(Some(SIGHUP), held, SIGTERM), Act::ForcedExit(SIGHUP));
        }
        assert_eq!(decide(Some(SIGINT), false, SIGHUP), Act::ForcedExit(SIGINT));
        assert_eq!(slot(SIGHUP), None);
        assert_eq!(signal_exit_code(SIGHUP), 129);
    }

    /// `SigIgn` in `/proc/self/status` (`proc_pid_status(5)`): SIGHUP is bit 0 of the
    /// hex mask. Missing or malformed reads as not ignored, so a hangup
    /// cancels.
    #[test]
    fn the_status_says_whether_sighup_is_ignored() {
        use signal_hook::consts::{SIGHUP, SIGINT};
        let status = |mask: &str| {
            format!(
                "Name:\tsb\nSigBlk:\t0000000000000000\nSigIgn:\t{mask}\nSigCgt:\t0000000000004002\n"
            )
        };
        assert!(ignored_in(&status("0000000000000001"), SIGHUP));
        assert!(ignored_in(&status("0000000000001001"), SIGHUP));
        assert!(!ignored_in(&status("0000000000001000"), SIGHUP));
        assert!(ignored_in(&status("0000000000000002"), SIGINT));
        assert!(!ignored_in(&status("0000000000000002"), SIGHUP));
        assert!(!ignored_in(
            "Name:\tsb\nSigCgt:\t0000000000000001\n",
            SIGHUP
        ));
        assert!(!ignored_in(&status("not hex"), SIGHUP));
        assert!(!ignored_in(&status(""), SIGHUP));
        assert!(!ignored_in("", SIGHUP));
        assert!(!ignored_in(&status("ffffffffffffffff"), 0));
        assert!(!ignored_in(&status("ffffffffffffffff"), 65));
    }

    /// Each held signal has its own slot; nothing else has one.
    #[test]
    fn only_sigint_and_sigquit_have_slots() {
        use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};
        assert_eq!(slot(SIGINT), Some(0));
        assert_eq!(SIGINT_SLOT, 0);
        assert_eq!(slot(SIGQUIT), Some(1));
        assert_eq!(slot(SIGTERM), None);
    }

    /// With nothing delivered, a hold starts and ends at once: the wait for
    /// undecided deliveries is only for those.
    #[test]
    fn a_hold_with_nothing_pending_does_not_wait() {
        let shutdown = Shutdown::new();
        let start = std::time::Instant::now();
        drop(shutdown.hold());
        assert!(start.elapsed() < SETTLE / 2, "{:?}", start.elapsed());
    }

    /// A hold's `Debug` names it and shows no state (it never locks).
    #[test]
    fn a_hold_debugs_without_its_state() {
        assert_eq!(format!("{:?}", Shutdown::new().hold()), "Hold { .. }");
    }

    /// Holds nest: the signals stay held until the last one ends.
    #[test]
    fn holds_nest() {
        let shutdown = Shutdown::new();
        let outer = shutdown.hold();
        let inner = shutdown.hold();
        drop(inner);
        assert!(shutdown.held());
        drop(outer);
        assert!(!shutdown.held());
    }

    /// A note lasts until the last hold ends or a child is credited with
    /// it, whichever is first; a later child isn't credited again.
    #[test]
    fn a_note_lasts_until_the_last_hold_ends_or_is_used() {
        use signal_hook::consts::SIGINT;
        let shutdown = Shutdown::new();
        let outer = shutdown.hold();
        assert_eq!(read(&shutdown, SIGINT), Act::Note);
        // An inner hold starting keeps what the outer one noted.
        let inner = shutdown.hold();
        drop(inner);
        assert!(outer.child_ended_by(Some(SIGINT)), "the outer hold sees it");
        assert!(!outer.child_ended_by(Some(SIGINT)), "used up");
        drop(outer);
        let first = Shutdown::new();
        let hold = first.hold();
        assert_eq!(read(&first, SIGINT), Act::Note);
        drop(hold);
        let hold = first.hold();
        assert!(
            !hold.child_ended_by(Some(SIGINT)),
            "cleared with the last hold"
        );
    }

    /// The signals thread reads a delivery late: a hold that ends meanwhile
    /// waits, so a Ctrl-C delivered during it is noted, not counted.
    #[test]
    fn a_hold_ends_only_after_its_deliveries_are_read() {
        use signal_hook::consts::SIGINT;
        let shutdown = Shutdown::new();
        let hold = shutdown.hold();
        deliver(&shutdown, SIGINT);
        let late = read_later(&shutdown, SIGINT);
        drop(hold);
        assert_eq!(late.join().ok(), Some(Act::Note));
        assert!(!shutdown.token().is_cancelled());
    }

    /// The child's death can reach exec before sb's own copy of the signal
    /// is read: the question waits for it.
    #[test]
    fn a_child_s_death_waits_for_sb_s_copy_of_the_signal() {
        use signal_hook::consts::SIGINT;
        let shutdown = Shutdown::new();
        let hold = shutdown.hold();
        deliver(&shutdown, SIGINT);
        let late = read_later(&shutdown, SIGINT);
        assert!(hold.child_ended_by(Some(SIGINT)));
        assert_eq!(late.join().ok(), Some(Act::Note));
    }

    /// A delivery before the hold, read after it began, is decided as
    /// outside it: the child, spawned after, never got it.
    #[test]
    fn a_signal_before_the_hold_is_decided_outside_it() {
        use signal_hook::consts::SIGINT;
        let shutdown = Shutdown::new();
        deliver(&shutdown, SIGINT);
        let late = read_later(&shutdown, SIGINT);
        let hold = shutdown.hold();
        assert_eq!(late.join().ok(), Some(Act::Cancel));
        drop(hold);
    }

    /// Once a child died of a held signal (the run is interrupted), the
    /// next signal forces the exit, as a second signal does.
    #[test]
    fn the_signal_after_a_child_s_interruption_forces_the_exit() {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let shutdown = Shutdown::new();
        let hold = shutdown.hold();
        assert_eq!(read(&shutdown, SIGINT), Act::Note);
        assert!(hold.child_ended_by(Some(SIGINT)));
        drop(hold);
        assert_eq!(shutdown.step(None, SIGTERM), Act::ForcedExit(SIGINT));
        // A command's own request isn't a signal: the next one cancels.
        let requested = Shutdown::new();
        let _ = requested.request(3);
        assert_eq!(requested.step(None, SIGINT), Act::Cancel);
    }

    /// A SIGINT sb raised at itself (console, for Ctrl-C at a prompt) is
    /// recorded the moment the raise returns, before the signals thread
    /// reads it; the thread's read of that delivery then does nothing,
    /// and the next signal forces the exit.
    #[test]
    fn a_raised_interrupt_is_recorded_at_once() {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let shutdown = Shutdown::new();
        assert!(!shutdown.interrupt_raised(), "nothing delivered");
        deliver(&shutdown, SIGINT);
        assert!(shutdown.interrupt_raised());
        assert_eq!(shutdown.signal(), Some(SIGINT));
        assert!(shutdown.token().is_cancelled());
        assert!(!shutdown.holds.pending(), "decided");
        assert!(shutdown.interrupt_raised(), "and stays so");
        assert_eq!(shutdown.step(None, SIGINT), Act::Raised);
        assert_eq!(shutdown.step(None, SIGTERM), Act::ForcedExit(SIGINT));
    }

    /// A Ctrl-C delivered after the raise was decided, before the thread
    /// read either, is a second signal: the thread's one read forces the
    /// exit.
    #[test]
    fn a_ctrl_c_after_a_raised_interrupt_forces_the_exit() {
        use signal_hook::consts::SIGINT;
        let shutdown = Shutdown::new();
        deliver(&shutdown, SIGINT);
        assert!(shutdown.interrupt_raised());
        deliver(&shutdown, SIGINT);
        assert_eq!(shutdown.step(None, SIGINT), Act::ForcedExit(SIGINT));
    }

    /// A raised SIGINT the thread decided first is not decided again; one
    /// that would be a second signal, or that a hold notes, is left to the
    /// thread; after a command's own request it isn't an interruption.
    #[test]
    fn a_raised_interrupt_is_decided_once_as_the_thread_would() {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let read_first = Shutdown::new();
        assert_eq!(read(&read_first, SIGINT), Act::Cancel);
        assert!(read_first.interrupt_raised());
        assert_eq!(read(&read_first, SIGINT), Act::ForcedExit(SIGINT));

        let second = Shutdown::new();
        assert_eq!(read(&second, SIGTERM), Act::Cancel);
        deliver(&second, SIGINT);
        assert!(second.interrupt_raised(), "interrupted by the SIGTERM");
        assert!(second.holds.pending(), "left to the thread");
        assert_eq!(second.step(None, SIGINT), Act::ForcedExit(SIGTERM));

        let under_hold = Shutdown::new();
        let hold = under_hold.hold();
        deliver(&under_hold, SIGINT);
        assert!(!under_hold.interrupt_raised());
        assert_eq!(under_hold.step(None, SIGINT), Act::Note);
        drop(hold);

        // After a command's own request the raised SIGINT isn't the
        // ending, but it is counted: the next signal is a second.
        let requested = Shutdown::new();
        let _ = requested.request(3);
        deliver(&requested, SIGINT);
        assert!(!requested.interrupt_raised());
        assert_eq!(requested.requested(), Some(3));
        assert_eq!(requested.step(None, SIGINT), Act::Raised);
        assert_eq!(requested.step(None, SIGTERM), Act::ForcedExit(SIGINT));

        // After a request and a SIGTERM the thread counted, a raised
        // SIGINT is a second signal, left to the thread.
        let terminated = Shutdown::new();
        let _ = terminated.request(3);
        assert_eq!(read(&terminated, SIGTERM), Act::Cancel);
        deliver(&terminated, SIGINT);
        assert!(!terminated.interrupt_raised());
        assert_eq!(terminated.step(None, SIGINT), Act::ForcedExit(SIGTERM));
    }

    /// What signal-hook's handler does at delivery (`flag::register`).
    fn deliver(shutdown: &Shutdown, signal: i32) {
        if let Some(slot) = slot(signal) {
            shutdown.holds.arrived[slot].store(true, Ordering::SeqCst);
        }
    }

    /// Deliver and read `signal` at once, as the signals thread does.
    fn read(shutdown: &Shutdown, signal: i32) -> Act {
        deliver(shutdown, signal);
        shutdown.step(None, signal)
    }

    /// The signals thread reading `signal` late.
    fn read_later(shutdown: &Shutdown, signal: i32) -> std::thread::JoinHandle<Act> {
        let shutdown = shutdown.clone();
        std::thread::spawn(move || {
            // Well inside DELIVERY, so a loaded machine still has slack.
            std::thread::sleep(Duration::from_millis(20));
            shutdown.step(None, signal)
        })
    }

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
        let _ = shutdown.request(0);
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

    /// The hooks run on their own thread before the ending: working ones
    /// finish, and one that blocks is abandoned at the deadline.
    #[test]
    fn hooks_run_within_the_deadline_and_a_blocking_one_is_left() {
        let shutdown = Shutdown::new();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&ran);
        let _first = shutdown.on_forced_exit(move || seen.lock().expect("lock").push("first"));
        let (release, blocked) = mpsc::channel::<()>();
        let _stuck = shutdown.on_forced_exit(move || {
            let _ = blocked.recv();
        });
        let start = std::time::Instant::now();
        Arc::clone(&shutdown.hooks).run_within(Duration::from_millis(100));
        let waited = start.elapsed();
        assert_eq!(*ran.lock().expect("lock"), ["first"]);
        assert!(
            waited >= Duration::from_millis(100) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        drop(release);
        let quick = Shutdown::new();
        let start = std::time::Instant::now();
        Arc::clone(&quick.hooks).run_within(Duration::from_secs(5));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
    }

    /// A command's own request isn't an interruption; a signal is.
    #[test]
    fn only_signals_count_as_interrupted() {
        let by_command = Shutdown::new();
        let _ = by_command.request(3);
        assert!(!by_command.interrupted());
        let by_signal = Shutdown::new();
        by_signal.signalled(signal_hook::consts::SIGINT);
        assert!(by_signal.interrupted());
        assert_eq!(by_signal.requested(), Some(130));
    }
}
