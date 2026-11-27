//! Prompts (term-ui spec, "Prompts"): dialoguer's, on the terminal, never
//! stdin, and never while the runner draws. Built with their first user:
//! the confirm with `release`'s self-update (term-ui decision 1).
//!
//! The terminal is `/dev/tty`, opened here: under `curl … | sudo bash`
//! stdin is the script. console reads keys from stdin when stdin is the
//! terminal, else from `/dev/tty` itself (console 0.16.6
//! `src/unix_term.rs`, `Input::unbuffered`), and dialoguer draws on the
//! `Term` it is given, here a read-write pair on the opened terminal
//! (<https://docs.rs/console/0.16.6/console/struct.Term.html#method.read_write_pair>).
//! With no terminal, or from a background process group (a read would
//! stop sb with SIGTTIN, a hang), the caller's flag is the way to answer,
//! and the error says so.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use dialoguer::Input;
use dialoguer::theme::Theme;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};
use sb_cli_core::cancel::{RestoreHook, Shutdown};
use sb_cli_core::theme::Role;

use crate::style::paint;

/// Why a prompt got no answer.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// No terminal to ask on, or sb runs in the background: the caller's
    /// flag answers instead.
    #[error("can't ask \"{question}\": {reason}; answer it with {flag}")]
    NoTerminal {
        question: String,
        reason: String,
        flag: &'static str,
    },
    /// A runner is drawing: a prompt would draw over its live lines (a
    /// bug in the command).
    #[error("can't ask \"{0}\" while the runner draws")]
    RunnerLive(String),
    /// Ctrl-C while asking: console raises SIGINT for it, and the confirm
    /// has the run's [`Shutdown`] record that delivery before it returns
    /// ([`Shutdown::interrupt_raised`]), so cli-core's `finish_run` ends
    /// sb by the signal, however late the signals thread reads it. Also
    /// when a signal ended the run before the confirm took the terminal
    /// (a Ctrl-C a moment before the question, say); it isn't asked then.
    /// And when a signal from outside (SIGTERM, SIGHUP, `kill -INT`)
    /// ended the run while the question waited, or its terminal hung up.
    #[error("interrupted")]
    Interrupted,
    /// Reading or drawing failed.
    #[error("can't ask \"{question}\": {error}")]
    Failed { question: String, error: String },
}

/// Ask `question`, yes or no, default no: `answer` when the caller has one
/// (its `flag`, such as `--yes`), else on the terminal; `shutdown` is the
/// run's, which a Ctrl-C at the question interrupts. The whole line
/// typed before Enter is the answer: `y` or `yes`, in any case, with
/// spaces around trimmed, accepts; anything else (`nay`, `no thank you`,
/// Enter alone) declines. It guards replacing a root-run binary, so a
/// stray `y` inside another word never accepts.
///
/// # Errors
///
/// [`PromptError::NoTerminal`] naming `flag` when there is no terminal or
/// sb is in the background; [`PromptError::RunnerLive`] while a runner is
/// live; [`PromptError::Interrupted`] on Ctrl-C, the run then interrupted
/// by SIGINT in `shutdown`;
/// [`PromptError::Failed`] when the terminal fails.
pub fn confirm(
    question: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
) -> Result<bool, PromptError> {
    let asked = confirm_on(
        question,
        answer,
        flag,
        shutdown,
        crate::runner_live(),
        || OpenOptions::new().read(true).write(true).open("/dev/tty"),
    );
    // Asked on the normal screen, the answered question is a line there,
    // as far as the blank line at the end knows (cli-core's `tail`).
    if answer.is_none() && asked.is_ok() {
        sb_cli_core::tail::other();
    }
    asked
}

/// [`confirm`] on the alternate screen, for a question asked while a
/// runner is paused over it (`Runner::pause_over`; term-ui decision 14):
/// the terminal switches to the alternate screen, `context` is written
/// there (what the question is about), the question is asked as
/// [`confirm`] asks it (its keys, texts and answer rules), and the normal
/// screen, the live tree on it, comes back untouched on every ending: an
/// answer, Ctrl-C, a failed read, and a second Ctrl-C (a restore hook on
/// `shutdown`). crossterm's commands, written to the opened terminal
/// (<https://docs.rs/crossterm/0.29.0/crossterm/terminal/struct.EnterAlternateScreen.html>,
/// <https://docs.rs/crossterm/0.29.0/crossterm/macro.execute.html>).
/// Why not dialoguer or console: neither has an alternate-screen call
/// (dialoguer 0.12.0 draws on the `Term` it is given; console 0.16.6's
/// `Term` clears and moves, it doesn't switch screens). An answer given,
/// or no terminal, is [`confirm`]'s: no screen.
///
/// # Errors
///
/// As [`confirm`]'s; [`PromptError::Failed`] when the screen can't be
/// switched.
pub fn confirm_over(
    question: &str,
    context: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
) -> Result<bool, PromptError> {
    confirm_over_on(
        question,
        context,
        answer,
        flag,
        shutdown,
        Here {
            runner_live: crate::runner_live(),
            // Only where a runner would draw live: indicatif's own rule for
            // stderr (a terminal, `TERM` set and not `dumb`; `is_hidden`,
            // <https://docs.rs/indicatif/0.18.6/indicatif/struct.ProgressDrawTarget.html#method.is_hidden>).
            // Elsewhere there is no live tree to keep, and a dumb terminal
            // may not have the alternate screen.
            alternate: !indicatif::ProgressDrawTarget::stderr().is_hidden(),
        },
        || OpenOptions::new().read(true).write(true).open("/dev/tty"),
    )
}

/// What [`confirm_over`] reads of the process, passed in so a test
/// doesn't read the process-wide state other tests change.
#[derive(Clone, Copy)]
struct Here {
    /// Whether a runner is live.
    runner_live: bool,
    /// Whether to switch to the alternate screen; else `context` is
    /// written above the question on the screen as it is.
    alternate: bool,
}

/// [`confirm_over`] with the terminal from `terminal` and the process's
/// state given (`here`), as [`confirm_on`].
fn confirm_over_on(
    question: &str,
    context: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
    here: Here,
    terminal: impl FnOnce() -> std::io::Result<File>,
) -> Result<bool, PromptError> {
    let Here {
        runner_live,
        alternate,
    } = here;
    if answer.is_some() || runner_live {
        return confirm_on(question, answer, flag, shutdown, runner_live, terminal);
    }
    let tty = match open_terminal(terminal) {
        Ok(tty) => tty,
        // No terminal: the confirm's own error, naming the flag.
        Err(reason) => {
            return Err(PromptError::NoTerminal {
                question: question.to_owned(),
                reason,
                flag,
            });
        }
    };
    let failed = |error: std::io::Error| PromptError::Failed {
        question: question.to_owned(),
        error: error.to_string(),
    };
    let screen = if alternate {
        Some(Screen::enter(&tty, shutdown).map_err(failed)?)
    } else {
        None
    };
    let mut out = &tty;
    if !context.is_empty() {
        writeln!(out, "{context}\n").map_err(failed)?;
    }
    let asked = confirm_on(question, None, flag, shutdown, false, || tty.try_clone());
    // On the normal screen, the context and the answered question are
    // lines there (cli-core's `tail`); on the alternate one, gone.
    if screen.is_none() && asked.is_ok() {
        sb_cli_core::tail::other();
    }
    drop(screen);
    asked
}

/// The alternate screen, entered on a terminal and left when dropped, or
/// by the restore hook on a second Ctrl-C (cli-core's forced exit).
struct Screen {
    tty: File,
    _hook: RestoreHook,
}

impl Screen {
    fn enter(tty: &File, shutdown: &Shutdown) -> std::io::Result<Self> {
        let mut tty = tty.try_clone()?;
        let hook_tty = tty.try_clone()?;
        // At the top left: the alternate screen keeps the cursor where the
        // live area left it (`?1049h` saves it, and clears, but doesn't
        // move it).
        execute!(tty, EnterAlternateScreen, MoveTo(0, 0))?;
        let hook = shutdown.on_forced_exit(move || {
            // `&File` writes, so the hook stays `Fn`.
            let mut out = &hook_tty;
            let _ = execute!(out, LeaveAlternateScreen);
        });
        Ok(Self { tty, _hook: hook })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        // Nothing better to do if the terminal fails here.
        let _ = execute!(self.tty, LeaveAlternateScreen);
    }
}

/// Whether a question can be asked here, by [`confirm`]'s rule: `/dev/tty`
/// opens and sb is in its foreground process group. For a command that
/// must know before its first step whether its questions can be asked
/// (`sb update`'s up-front flag check, ops spec).
///
/// # Errors
///
/// Why not, in the confirm's words: `no terminal (<cause>)` or `sb runs
/// in the background`.
pub fn can_ask() -> Result<(), String> {
    open_terminal(|| OpenOptions::new().read(true).write(true).open("/dev/tty")).map(drop)
}

/// The terminal from `terminal`, if sb is in its foreground process group
/// (a read from the background stops sb; rustix's `tcgetpgrp`,
/// <https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcgetpgrp.html>).
fn open_terminal(terminal: impl FnOnce() -> std::io::Result<File>) -> Result<File, String> {
    let tty = terminal().map_err(|err| format!("no terminal ({err})"))?;
    let foreground =
        rustix::termios::tcgetpgrp(&tty).is_ok_and(|group| group == rustix::process::getpgrp());
    if !foreground {
        return Err("sb runs in the background".to_owned());
    }
    Ok(tty)
}

/// [`confirm`] with the terminal from `terminal`, `runner_live` saying
/// whether a runner is live (a parameter, so a test doesn't read the
/// process-wide count that other tests' runners raise at the same time).
fn confirm_on(
    question: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
    runner_live: bool,
    terminal: impl FnOnce() -> std::io::Result<File>,
) -> Result<bool, PromptError> {
    if let Some(answer) = answer {
        return Ok(answer);
    }
    if runner_live {
        return Err(PromptError::RunnerLive(question.to_owned()));
    }
    let no_terminal = |reason: String| PromptError::NoTerminal {
        question: question.to_owned(),
        reason,
        flag,
    };
    let tty = open_terminal(terminal).map_err(no_terminal)?;
    let failed = |error: String| PromptError::Failed {
        question: question.to_owned(),
        error,
    };
    // What was typed before the question showed is dropped, so a stray
    // `y` and Enter (or a paste) can't answer a question never seen
    // (rustix's `tcflush`,
    // <https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcflush.html>). The
    // terminal's input queue is one, whichever descriptor reads it.
    let _ = rustix::termios::tcflush(&tty, rustix::termios::QueueSelector::IFlush);
    // Held until the question ends, whichever way.
    let raw = RawMode::hold(&tty, shutdown).map_err(|err| failed(err.to_string()))?;
    unasked_if_interrupted(shutdown)?;
    let modes = (
        raw.tty.try_clone().map_err(|err| failed(err.to_string()))?,
        raw.original.clone(),
    );
    let gate = Gate::new(tty.try_clone().map_err(|err| failed(err.to_string()))?);
    let open = Arc::clone(&gate.open);
    let still_open = Arc::clone(&open);
    let term = console::Term::read_write_pair(tty, gate);
    let theme = YesNo {
        palette: Palette {
            colour: console::colors_enabled_stderr(),
        },
    };
    let prompt = question.to_owned();
    let (answer, answered) = mpsc::channel();
    sb_cli_core::spawn::thread(
        std::thread::Builder::new().name("question".into()),
        move || {
            only_consoles_signal();
            let read = read_answer(&term, &theme, &prompt);
            // Left (a signal ended the run): a key that woke it had
            // console put back the raw mode it found; the terminal's own
            // modes go back, as the confirm put them back when it left.
            // Accepted limit: a key dialoguer draws nothing for (an
            // arrow, Esc), typed in the moments between the confirm
            // leaving and sb's exit, leaves console's raw mode with
            // dialoguer reading on; an interactive shell puts its own
            // modes back after a job a signal ended.
            if_left(&still_open, || {
                let (tty, original) = &modes;
                put_back(tty, original);
            });
            let _ = answer.send(read);
        },
    )
    .map_err(|err| failed(err.to_string()))?;
    match wait_for_answer(&answered, shutdown, POLL) {
        Waited::Answer(read) => answered_with(question, read, shutdown, HANGUP),
        Waited::Signalled => {
            // The question is left waiting on its thread for a key that
            // won't matter: sb ends by the signal. Its drawing is shut
            // off first, then its line ended here, as after a Ctrl-C.
            abandon(&open, &raw.tty);
            Err(PromptError::Interrupted)
        }
    }
}

/// Run `put_back` if the confirm has left the question (`open`, the
/// [`Gate`]'s flag, shut).
fn if_left(open: &Mutex<bool>, put_back: impl FnOnce()) {
    if !*open.lock().unwrap_or_else(PoisonError::into_inner) {
        put_back();
    }
}

/// The confirm's ending from the question's `read`: the answer, Ctrl-C,
/// a gone terminal (its hangup's signal waited for up to `hangup`), or a
/// failed read.
fn answered_with(
    question: &str,
    read: std::io::Result<String>,
    shutdown: &Shutdown,
    hangup: Duration,
) -> Result<bool, PromptError> {
    let failed = |err: std::io::Error| PromptError::Failed {
        question: question.to_owned(),
        error: err.to_string(),
    };
    match read {
        Ok(line) => Ok(accepts(&line)),
        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
            // console raised SIGINT before returning (UNVERIFIED as
            // API, checked in its source: `read_single_key`,
            // <https://docs.rs/console/0.16.6/src/console/unix_term.rs.html>),
            // for Ctrl-C (a key here, [`RawMode`]) or for a signal that
            // interrupted its `poll` (only a SIGINT can: the question's
            // thread blocks the others, [`only_consoles_signal`]); both
            // are a SIGINT. Recorded now, not when the signals thread gets
            // to it, or the run could end with exit 1 before the thread
            // records it. Why not console's `Term::read_key_raw`, which
            // returns `Key::CtrlC` without raising: dialoguer 0.12.0's
            // `Input` reads with `read_key` (`src/prompts/input.rs`), and
            // replacing `Input` would mean line editing of our own.
            let _ = shutdown.interrupt_raised();
            Err(PromptError::Interrupted)
        }
        Err(err) => {
            // A terminal that is gone (a hangup: its reads end or fail
            // with EIO or ENXIO, [`gone`]) is a SIGHUP on its way, which
            // nothing orders before the read's end: the run is
            // interrupted by it if it comes within `hangup` ([`HANGUP`]).
            if gone(&err) && signalled_within(shutdown, hangup, POLL) {
                Err(PromptError::Interrupted)
            } else {
                Err(failed(err))
            }
        }
    }
}

/// How often the waiting confirm looks for a signal that ended the run:
/// the bound on how long sb takes to end at a question once its signals
/// thread has read one, as cli-core's run lock waits (`run_lock`'s
/// `POLL`).
const POLL: Duration = Duration::from_millis(100);

/// How long a read that found the terminal gone waits for its hangup's
/// SIGHUP (see `confirm_on`). The session leader's shell (or sudo) passes
/// it on within milliseconds; the bound only delays a run whose read failed
/// with no signal coming (under `nohup`).
const HANGUP: Duration = Duration::from_secs(1);

/// The question on its thread: dialoguer's text input reads the whole
/// line, empty allowed
/// (<https://docs.rs/dialoguer/0.12.0/dialoguer/struct.Input.html#method.interact_text_on>,
/// `allow_empty`). Why not its `Confirm`: with `wait_for_newline` it keeps
/// the last `y` or `n` typed and ignores other keys, so `nay` accepts
/// (0.12.0 `src/prompts/confirm.rs`). Why no validator: one would ask
/// again, and anything but `y`/`yes` is a plain no.
fn read_answer(term: &console::Term, theme: &YesNo, question: &str) -> std::io::Result<String> {
    Input::<String>::with_theme(theme)
        .with_prompt(question)
        .allow_empty(true)
        .interact_text_on(term)
        .map_err(|dialoguer::Error::IO(err)| {
            // dialoguer's `Input` never hides the cursor (its `Confirm`
            // did); still, after Ctrl-C or a failed read, the cursor is
            // shown whatever drew last (console's own call,
            // <https://docs.rs/console/0.16.6/console/struct.Term.html#method.show_cursor>).
            // UNVERIFIED: dialoguer documents no pattern for it; checked
            // against its source (0.12.0 `src/prompts/input.rs` returns the
            // read's error as it is).
            let _ = term.show_cursor();
            // And the question's line ended, so what follows starts clean.
            let _ = term.write_line("");
            err
        })
}

/// What the confirm waited for: the question's answer, or a signal that
/// ended the run while it waited.
#[derive(Debug)]
enum Waited {
    Answer(std::io::Result<String>),
    Signalled,
}

/// Wait for the question's thread's answer on `answered`, or for a signal
/// to end the run (`shutdown`), looked for every `poll`.
///
/// Why a thread: console reads each key with a `poll` on the terminal
/// that nothing else can wake (0.16.6 `src/unix_term.rs`,
/// `read_single_key`; no timeout or cancel in its API), so a signal sb
/// handles only cancels the run's token while the question waits on; and
/// a signal that does interrupt that `poll` (EINTR) is taken for Ctrl-C:
/// console raises SIGINT, so a SIGTERM or SIGHUP ended sb by SIGINT. Here
/// the question waits on its own thread, which blocks every signal but
/// console's own SIGINT ([`only_consoles_signal`]), and the main thread,
/// which the kernel offers a signal sent to sb first (see [`RawMode`]),
/// waits on std's channel (`Receiver::recv_timeout`,
/// <https://doc.rust-lang.org/std/sync/mpsc/struct.Receiver.html#method.recv_timeout>).
/// Why not wake console's `poll`: only a signal directed at the thread
/// can (nix's `pthread_kill`), and console takes the EINTR for Ctrl-C. Why not wait on the run's token (its
/// `cancelled` future): a command's own request to end cancels it too,
/// and that isn't a signal: the question waits on then, as before
/// (`Shutdown` has no wait that only a signal ends).
fn wait_for_answer(
    answered: &mpsc::Receiver<std::io::Result<String>>,
    shutdown: &Shutdown,
    poll: Duration,
) -> Waited {
    loop {
        match answered.recv_timeout(poll) {
            Ok(answer) => return Waited::Answer(answer),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shutdown.interrupted() {
                    return Waited::Signalled;
                }
            }
            // The thread ended without an answer (a panic in dialoguer).
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Waited::Answer(Err(std::io::Error::other(
                    "the question ended without an answer",
                )));
            }
        }
    }
}

/// Whether a signal ends the run within `bound`, looked for every `poll`.
fn signalled_within(shutdown: &Shutdown, bound: Duration, poll: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while !shutdown.interrupted() {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        std::thread::sleep(left.min(poll));
    }
    true
}

/// Whether a read's `err` says the terminal is gone: once it hung up, a
/// read ends (console: "Reached end of file") and a mode switch fails
/// with EIO, which console's per-key switch meets first (UNVERIFIED as
/// API, read in Linux 6.8's `drivers/tty/tty_io.c`, `hung_up_tty_read`
/// and `hung_up_tty_ioctl`; seen as "Input/output error" in
/// `a_hangup_at_the_question_ends_sb`). A key read that starts after the
/// hangup fails with ENXIO instead: console opens `/dev/tty` (stdin, if
/// it was the terminal, no longer passes `isatty`), which the hangup took
/// from the session (UNVERIFIED as API, read in console 0.16.6's
/// `Input::unbuffered`,
/// <https://docs.rs/console/0.16.6/src/console/unix_term.rs.html>, and
/// Linux 6.8's `tty_signal_session_leader` and `tty_open_current_tty`;
/// seen as "No such device or address" in the same test).
fn gone(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::UnexpectedEof
        || err.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
        || err.raw_os_error() == Some(rustix::io::Errno::NXIO.raw_os_error())
}

/// Block the signals on the calling thread (the question's) that a
/// handler could take there: every one but SIGINT, which console raises
/// at it for Ctrl-C. The kernel then never chooses this thread for a
/// signal sent to sb, so console's `poll` is never interrupted by one (it
/// would raise SIGINT for it, see [`wait_for_answer`]), and the signal goes
/// to a thread that handles it as cli-core's handlers expect. The job
/// control signals stay open too (SIGTSTP, SIGTTIN, SIGTTOU: sb handles
/// none, so they can't interrupt the `poll`): a terminal treats a blocked
/// SIGTTIN as ignored and fails a background read with EIO rather than
/// stop sb (UNVERIFIED as API, read in Linux 6.8's
/// `drivers/tty/tty_jobctrl.c`, `__tty_check_change`). nix's
/// `SigSet::thread_block`, `pthread_sigmask(SIG_BLOCK)`
/// (<https://docs.rs/nix/0.31.3/nix/sys/signal/struct.SigSet.html#method.thread_block>),
/// the way signal-hook says its handlers combine with masks (0.4.5's
/// crate docs, "Signal masks",
/// <https://docs.rs/signal-hook/0.4.5/signal_hook/#signal-masks>); why
/// not rustix's: its `sigprocmask` is `unsafe`. SIGKILL and SIGSTOP can't
/// be blocked and stay as they are, and a fault signal is delivered
/// whatever the mask (`pthread_sigmask(3)`).
fn only_consoles_signal() {
    use nix::sys::signal::{SigSet, Signal};
    let mut blocked = SigSet::all();
    for open in [
        Signal::SIGINT,
        Signal::SIGTSTP,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
    ] {
        blocked.remove(open);
    }
    if let Err(err) = blocked.thread_block() {
        tracing::warn!(error = %err, "the question's thread can't block signals");
    }
}

/// The question's drawing, shut off once the confirm stops waiting for
/// it: its writes then fail, so dialoguer, should a key wake it, ends
/// instead of drawing over what sb writes next. The flag's lock is held
/// for each write, so the confirm's own ending ([`abandon`]) comes after
/// any write in progress. Why not console's `Term`: it has no way to stop
/// a prompt drawing; dialoguer draws on the `Term` it is given
/// (`read_write_pair`, a `Write` of ours).
#[derive(Debug)]
struct Gate {
    tty: File,
    open: Arc<Mutex<bool>>,
}

impl Gate {
    fn new(tty: File) -> Self {
        Self {
            tty,
            open: Arc::new(Mutex::new(true)),
        }
    }
}

impl std::io::Write for Gate {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if !*open {
            return Err(std::io::Error::other("the question was left"));
        }
        self.tty.write(buf)
    }

    /// As a write: dialoguer flushes after drawing too.
    fn flush(&mut self) -> std::io::Result<()> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if !*open {
            return Err(std::io::Error::other("the question was left"));
        }
        self.tty.flush()
    }
}

/// console asks the writer's descriptor whether it is a terminal.
impl AsRawFd for Gate {
    fn as_raw_fd(&self) -> RawFd {
        self.tty.as_raw_fd()
    }
}

/// Shut the question's drawing off ([`Gate`]) and end its line on `tty`,
/// the cursor shown, as the question's own ending after Ctrl-C does
/// (crossterm's `Show`, written to the terminal); nothing better to do if
/// the terminal fails.
fn abandon(open: &Mutex<bool>, tty: &File) {
    let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
    *open = false;
    let mut out = tty;
    let _ = execute!(out, crossterm::cursor::Show);
    let _ = out.write_all(b"\n");
}

/// The terminal held in raw mode for the whole question.
///
/// Its modes are put back when dropped, or by a restore hook on a second
/// signal (cli-core's forced exit). The mode is the one console sets for
/// each key it reads:
/// `cfmakeraw`, the output modes kept (console 0.16.6 `src/unix_term.rs`,
/// `read_single_key`; rustix's `Termios::make_raw`,
/// <https://docs.rs/rustix/1.1.6/rustix/termios/struct.Termios.html#method.make_raw>).
/// console puts the terminal's own modes back after each key, so without
/// this the terminal is in canonical mode with `ISIG` while the question
/// is drawn and between keys, and a Ctrl-C then is the kernel's SIGINT,
/// which console never sees: the run's token is cancelled while dialoguer
/// waits on for a key. Held here, a Ctrl-C at the question is always a
/// key, which console turns into its raise and `Interrupted`. Neither
/// dialoguer 0.12.0 nor console 0.16.6 documents its Ctrl-C handling or
/// holds raw mode over a prompt (`Term::read_key` says only that it reads
/// one key without echo). Why not crossterm's `enable_raw_mode` (0.29.0
/// `src/terminal/sys/unix.rs`): it clears the output modes too, so
/// console's and dialoguer's `\n` would no longer return the carriage; it
/// keeps the modes to put back in one process-wide static, which sb-tui's
/// screen uses; and it switches stdin when stdin is a terminal, not the
/// `/dev/tty` opened and checked here.
///
/// A Ctrl-C typed before the hold was the kernel's SIGINT, and its
/// handler has run on this thread once `tcsetattr` returns: the kernel
/// offers a signal sent to a process group to each process's main thread
/// first, unless that thread blocks it or has another signal pending
/// (UNVERIFIED as API: signal(7) says "an arbitrary thread"; checked in
/// Linux 6.8's `kernel/signal.c`, `complete_signal` and `wants_signal`),
/// the hold is taken on the thread that asks (sb's main thread), and a
/// handler runs before a system call returns to the thread's code. So the
/// delivery flag is set by then, for [`Shutdown::interrupt_raised`]. One
/// window is left (UNVERIFIED, read in Linux 6.8's
/// `drivers/tty/n_tty.c`, `isig`): the line discipline decides on a
/// Ctrl-C under the old modes, then lets go of the modes' lock before it
/// sends the signal, so a switch taking the lock in those microseconds
/// returns before the SIGINT exists. That SIGINT then ends the question
/// while it waits, as any signal does ([`wait_for_answer`]).
struct RawMode {
    tty: File,
    original: Termios,
    _hook: RestoreHook,
}

impl RawMode {
    fn hold(tty: &File, shutdown: &Shutdown) -> std::io::Result<Self> {
        let original = tcgetattr(tty)?;
        let mut raw = original.clone();
        raw.make_raw();
        raw.output_modes = original.output_modes;
        let tty = tty.try_clone()?;
        let hook_tty = tty.try_clone()?;
        let hook_modes = original.clone();
        // The hook first, so a forced exit right after the switch still
        // puts the modes back.
        let hook = shutdown.on_forced_exit(move || put_back(&hook_tty, &hook_modes));
        tcsetattr(&tty, OptionalActions::Now, &raw)?;
        Ok(Self {
            tty,
            original,
            _hook: hook,
        })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        put_back(&self.tty, &self.original);
    }
}

/// Put `modes` back on `tty`; nothing better to do if the terminal fails.
fn put_back(tty: &File, modes: &Termios) {
    let _ = tcsetattr(tty, OptionalActions::Now, modes);
}

/// [`PromptError::Interrupted`] when a signal has ended the run, checked
/// once the confirm holds the terminal ([`RawMode`]): a Ctrl-C before the
/// hold was a SIGINT, delivered to this thread before the switch returned
/// (see there), and it interrupts the question, unasked, as the Ctrl-C at
/// it does. A command's own request to end isn't a signal: the question is
/// asked then.
fn unasked_if_interrupted(shutdown: &Shutdown) -> Result<(), PromptError> {
    if shutdown.interrupt_raised() {
        Err(PromptError::Interrupted)
    } else {
        Ok(())
    }
}

/// The confirm's answer from the whole line: `y` or `yes`, in any case,
/// spaces around trimmed (release spec, "Differences from sb-go" 8;
/// term-ui spec, "Prompts").
fn accepts(line: &str) -> bool {
    let line = line.trim();
    line.eq_ignore_ascii_case("y") || line.eq_ignore_ascii_case("yes")
}

/// The prompts' colours: term-ui's anstyle painting from the palette. Why
/// not dialoguer's `ColorfulTheme`: its fields are console's styles, which
/// write a bright colour as a 256-colour code (see `style.rs`), wrong
/// under the `ansi` profile; dialoguer's `Theme` trait is its documented
/// way to restyle
/// (<https://docs.rs/dialoguer/0.12.0/dialoguer/theme/trait.Theme.html>).
struct Palette {
    colour: bool,
}

impl Palette {
    fn paint(&self, role: Role, text: &str) -> String {
        paint(sb_cli_core::theme::current(), role, self.colour, text)
    }
}

/// The confirm's look on dialoguer's text input: the `Theme` trait's
/// confirm layout (`[y/N]`) with `ColorfulTheme`'s `?` prefix, and the
/// answer shown as `yes` or `no` from the line, as the confirm shows it.
struct YesNo {
    palette: Palette,
}

impl Theme for YesNo {
    fn format_input_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        _default: Option<&str>,
    ) -> fmt::Result {
        write!(
            f,
            "{} {prompt} {} ",
            self.palette.paint(Role::Accent, "?"),
            self.palette.paint(Role::Muted, "[y/N]")
        )
    }

    fn format_input_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        line: &str,
    ) -> fmt::Result {
        let answer = if accepts(line) {
            self.palette.paint(Role::Ok, "yes")
        } else {
            self.palette.paint(Role::Muted, "no")
        };
        write!(
            f,
            "{} {prompt} {answer}",
            self.palette.paint(Role::Accent, "?")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_tty() -> std::io::Result<File> {
        Err(std::io::Error::from_raw_os_error(6))
    }

    #[test]
    fn a_given_answer_is_used_without_asking() {
        for answer in [true, false] {
            let asked = confirm_on(
                "Do you want to update",
                Some(answer),
                "--yes",
                &Shutdown::new(),
                true,
                || unreachable!("not asked"),
            );
            assert_eq!(asked.expect("answered"), answer);
        }
    }

    /// No terminal and no answer: an error naming the flag, never a hang.
    #[test]
    fn without_a_terminal_the_error_names_the_flag() {
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            false,
            no_tty,
        )
        .expect_err("no terminal");
        assert!(matches!(err, PromptError::NoTerminal { flag: "--yes", .. }));
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\": no terminal (No such device or address \
             (os error 6)); answer it with --yes"
        );
    }

    /// The rule on its own, for a command's up-front check: no terminal,
    /// or one sb isn't in the foreground of, is a reason; the same
    /// reasons as the confirm's.
    #[test]
    fn the_terminal_rule_gives_the_confirms_reasons() {
        assert_eq!(
            open_terminal(no_tty).expect_err("no terminal"),
            "no terminal (No such device or address (os error 6))"
        );
        let file = tempfile::tempfile().expect("file");
        assert_eq!(
            open_terminal(|| Ok(file)).expect_err("not a terminal"),
            "sb runs in the background"
        );
    }

    /// While a runner draws, nothing is asked (the question would share
    /// the screen with its live lines), terminal or not.
    #[test]
    fn while_a_runner_draws_nothing_is_asked() {
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            true,
            || unreachable!("no terminal opened"),
        )
        .expect_err("refused");
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\" while the runner draws"
        );
    }

    /// A file that isn't a terminal reads as the background (its group
    /// can't be read), so it is refused the same way.
    #[test]
    fn a_file_that_isnt_a_terminal_is_refused() {
        let file = tempfile::tempfile().expect("file");
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            false,
            || Ok(file),
        )
        .expect_err("not a terminal");
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\": sb runs in the background; answer it \
             with --yes"
        );
    }

    /// On the alternate screen, the confirm's own rules first: a given
    /// answer asks nothing, a live runner refuses, and without a terminal
    /// (or from the background) the error names the flag; no screen is
    /// switched then (a file that isn't a terminal stays empty).
    #[test]
    fn the_alternate_screens_confirm_keeps_the_confirms_rules() {
        let shutdown = Shutdown::new();
        let ask = |answer, live, terminal: Box<dyn FnOnce() -> std::io::Result<File>>| {
            confirm_over_on(
                "Run all migration tags?",
                "The following tags need to be run to handle migrations:",
                answer,
                "--migrations",
                &shutdown,
                Here {
                    runner_live: live,
                    alternate: true,
                },
                terminal,
            )
        };
        for answer in [true, false] {
            let asked = ask(Some(answer), true, Box::new(|| unreachable!("not asked")));
            assert_eq!(asked.expect("answered"), answer);
        }
        let err =
            ask(None, true, Box::new(|| unreachable!("no terminal opened"))).expect_err("refused");
        assert!(matches!(err, PromptError::RunnerLive(_)), "{err}");
        let err = ask(None, false, Box::new(no_tty)).expect_err("no terminal");
        assert!(
            matches!(
                err,
                PromptError::NoTerminal {
                    flag: "--migrations",
                    ..
                }
            ),
            "{err}"
        );
        let file = tempfile::tempfile().expect("file");
        let seen = file.try_clone().expect("clone");
        let err = ask(None, false, Box::new(|| Ok(file))).expect_err("not a terminal");
        assert!(
            err.to_string().contains("sb runs in the background"),
            "{err}"
        );
        assert_eq!(seen.metadata().expect("metadata").len(), 0);
    }

    /// A signal that ended the run before the question took the terminal
    /// interrupts it, unasked; a command's own request doesn't, and
    /// neither does nothing at all.
    #[test]
    fn a_signal_before_the_question_interrupts_it() {
        let shutdown = Shutdown::new();
        assert!(unasked_if_interrupted(&shutdown).is_ok());
        let requested = Shutdown::new();
        let _ = requested.request(3);
        assert!(unasked_if_interrupted(&requested).is_ok());
        for signal in [rustix::process::Signal::INT, rustix::process::Signal::TERM] {
            let signalled = Shutdown::new();
            signalled.signalled(signal.as_raw());
            assert!(matches!(
                unasked_if_interrupted(&signalled),
                Err(PromptError::Interrupted)
            ));
        }
    }

    /// The terminal's modes go back after the question only once the
    /// confirm has left it.
    #[test]
    fn the_modes_go_back_only_after_leaving() {
        let open = Mutex::new(true);
        let mut put = 0;
        if_left(&open, || put += 1);
        assert_eq!(put, 0);
        *open.lock().expect("lock") = false;
        if_left(&open, || put += 1);
        assert_eq!(put, 1);
    }

    /// The question's read decides the confirm's ending: the line's
    /// answer; Ctrl-C (console's raise) interrupted at once; a gone
    /// terminal interrupted when its hangup's signal comes, else failed;
    /// any other failure failed at once.
    #[test]
    fn the_read_decides_the_ending() {
        let question = "Do you want to update";
        let none = Duration::ZERO;
        let quiet = Shutdown::new();
        assert!(answered_with(question, Ok("yes".to_owned()), &quiet, none).expect("yes"));
        assert!(!answered_with(question, Ok("nay".to_owned()), &quiet, none).expect("no"));
        let ctrl_c = std::io::Error::new(std::io::ErrorKind::Interrupted, "read interrupted");
        assert!(matches!(
            answered_with(question, Err(ctrl_c), &quiet, Duration::from_secs(10)),
            Err(PromptError::Interrupted)
        ));
        let eio = || std::io::Error::from_raw_os_error(5);
        let hung_up = Shutdown::new();
        hung_up.signalled(rustix::process::Signal::HUP.as_raw());
        assert!(matches!(
            answered_with(question, Err(eio()), &hung_up, none),
            Err(PromptError::Interrupted)
        ));
        let err = answered_with(question, Err(eio()), &quiet, Duration::from_millis(10))
            .expect_err("failed");
        assert!(matches!(err, PromptError::Failed { .. }), "{err}");
        // A key read that began after the hangup: console opens
        // `/dev/tty`, which the hangup took from the session.
        assert!(matches!(
            answered_with(
                question,
                Err(std::io::Error::from_raw_os_error(6)),
                &hung_up,
                none
            ),
            Err(PromptError::Interrupted)
        ));
        let err = answered_with(
            question,
            Err(std::io::Error::other("other")),
            &hung_up,
            Duration::from_secs(10),
        )
        .expect_err("failed");
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\": other"
        );
    }

    /// The confirm waits for the question's answer, whatever comes first.
    #[test]
    fn the_answer_is_waited_for() {
        let (answer, answered) = mpsc::channel();
        let sent = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            answer.send(Ok("yes".to_owned())).expect("send");
        });
        let waited = wait_for_answer(&answered, &Shutdown::new(), Duration::from_millis(5));
        assert!(matches!(waited, Waited::Answer(Ok(ref line)) if line == "yes"));
        sent.join().expect("sender");
    }

    /// A signal that ends the run while the question waits ends the wait
    /// (the question is left on its thread), however late it comes.
    #[test]
    fn a_signal_ends_the_wait() {
        let shutdown = Shutdown::new();
        let (_answer, answered) = mpsc::channel::<std::io::Result<String>>();
        let signalled = shutdown.clone();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            signalled.signalled(rustix::process::Signal::TERM.as_raw());
        });
        let started = Instant::now();
        let waited = wait_for_answer(&answered, &shutdown, Duration::from_millis(5));
        assert!(matches!(waited, Waited::Signalled), "{waited:?}");
        assert!(started.elapsed() >= Duration::from_millis(50));
        signal.join().expect("signal");
    }

    /// A command's own request to end isn't a signal: the question is
    /// still waited for (as [`unasked_if_interrupted`] still asks it).
    #[test]
    fn a_request_to_end_does_not_end_the_wait() {
        let shutdown = Shutdown::new();
        let _ = shutdown.request(3);
        let (answer, answered) = mpsc::channel();
        let sent = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            answer.send(Ok(String::new())).expect("send");
        });
        let waited = wait_for_answer(&answered, &shutdown, Duration::from_millis(5));
        assert!(matches!(waited, Waited::Answer(Ok(_))), "{waited:?}");
        sent.join().expect("sender");
    }

    /// A question's thread that ends without an answer is a failure, not
    /// a wait forever.
    #[test]
    fn a_question_that_ends_unanswered_fails() {
        let (answer, answered) = mpsc::channel::<std::io::Result<String>>();
        drop(answer);
        let waited = wait_for_answer(&answered, &Shutdown::new(), Duration::from_millis(5));
        assert!(matches!(waited, Waited::Answer(Err(_))), "{waited:?}");
    }

    /// A gone terminal's read waits for its hangup's signal, within the
    /// bound; with no signal it gives up at the bound.
    #[test]
    fn a_hangup_is_waited_for_within_the_bound() {
        let shutdown = Shutdown::new();
        let started = Instant::now();
        assert!(!signalled_within(
            &shutdown,
            Duration::from_millis(40),
            Duration::from_millis(5)
        ));
        assert!(started.elapsed() >= Duration::from_millis(40));
        let signalled = shutdown.clone();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            signalled.signalled(rustix::process::Signal::HUP.as_raw());
        });
        assert!(signalled_within(
            &shutdown,
            Duration::from_secs(10),
            Duration::from_millis(5)
        ));
        signal.join().expect("signal");
    }

    /// The end of a read, EIO and ENXIO say the terminal is gone; other
    /// errors don't.
    #[test]
    fn eof_eio_and_enxio_say_the_terminal_is_gone() {
        assert!(gone(&std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Reached end of file"
        )));
        assert!(gone(&std::io::Error::from_raw_os_error(5)));
        assert!(gone(&std::io::Error::from_raw_os_error(6)));
        assert!(!gone(&std::io::Error::from_raw_os_error(4)));
        assert!(!gone(&std::io::Error::other("other")));
    }

    /// Once left, the question's writes fail and the confirm's own ending
    /// is written: the drawing is shut off.
    #[test]
    fn a_left_question_draws_nothing_more() {
        let file = tempfile::tempfile().expect("file");
        let mut gate = Gate::new(file.try_clone().expect("clone"));
        gate.write_all(b"? Do you want to update [y/N] ")
            .expect("drawn while open");
        gate.flush().expect("flushed while open");
        abandon(&gate.open, &file);
        assert!(gate.write_all(b"y").is_err());
        assert!(gate.flush().is_err());
        let mut text = String::new();
        let mut read = file;
        std::io::Seek::rewind(&mut read).expect("rewind");
        std::io::Read::read_to_string(&mut read, &mut text).expect("read");
        assert_eq!(text, "? Do you want to update [y/N] \u{1b}[?25h\n");
    }

    /// The question's thread blocks every signal but SIGINT (console's
    /// raise for Ctrl-C) and the job control ones, read back from its
    /// `SigBlk` line (`proc_pid_status(5)`: signal `n` is bit `n - 1`).
    #[test]
    fn the_questions_thread_takes_only_sigint() {
        use rustix::process::Signal;
        let mask = std::thread::spawn(|| {
            only_consoles_signal();
            let status = std::fs::read_to_string("/proc/thread-self/status").expect("status");
            let mask = status
                .lines()
                .find_map(|line| line.strip_prefix("SigBlk:"))
                .expect("SigBlk");
            u64::from_str_radix(mask.trim(), 16).expect("hex")
        })
        .join()
        .expect("thread");
        let bit = |signal: Signal| 1u64 << (signal.as_raw() - 1);
        for signal in [Signal::INT, Signal::TSTP, Signal::TTIN, Signal::TTOU] {
            assert_eq!(mask & bit(signal), 0, "{signal:?} {mask:x}");
        }
        for signal in [
            Signal::TERM,
            Signal::HUP,
            Signal::QUIT,
            Signal::CHILD,
            Signal::WINCH,
        ] {
            assert_ne!(mask & bit(signal), 0, "{signal:?} {mask:x}");
        }
    }

    /// The whole line is the answer: `y` or `yes` in any case, spaces
    /// around trimmed; anything else declines, never the last `y` typed.
    #[test]
    fn only_y_or_yes_accepts() {
        for line in ["y", "Y", " yes ", "YES", "Yes", "\tyes"] {
            assert!(accepts(line), "{line:?}");
        }
        for line in [
            "",
            "  ",
            "n",
            "no",
            "nay",
            "no thank you",
            "yes please",
            "ye",
            "yy",
            "y y",
        ] {
            assert!(!accepts(line), "{line:?}");
        }
    }

    fn rendered(colour: bool, render: impl Fn(&YesNo, &mut String) -> fmt::Result) -> String {
        let mut text = String::new();
        render(
            &YesNo {
                palette: Palette { colour },
            },
            &mut text,
        )
        .expect("render");
        text
    }

    #[test]
    fn the_prompt_is_dialoguers_layout() {
        let asked = rendered(false, |theme, f| {
            theme.format_input_prompt(f, "Do you want to update", None)
        });
        assert_eq!(asked, "? Do you want to update [y/N] ");
        let answered = |line| {
            rendered(false, |theme, f| {
                theme.format_input_prompt_selection(f, "Do you want to update", line)
            })
        };
        assert_eq!(answered(" Yes "), "? Do you want to update yes");
        assert_eq!(answered("nay"), "? Do you want to update no");
        assert_eq!(answered(""), "? Do you want to update no");
    }

    /// With colour, the marks are painted; the question stays plain.
    #[test]
    fn colour_paints_the_marks_only() {
        let text = rendered(true, |theme, f| {
            theme.format_input_prompt(f, "Do you want to update", None)
        });
        let theme = sb_cli_core::theme::current();
        assert_eq!(
            text,
            format!(
                "{} Do you want to update {} ",
                paint(theme, Role::Accent, true, "?"),
                paint(theme, Role::Muted, true, "[y/N]")
            )
        );
    }
}
