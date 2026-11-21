//! Terminal setup and restore, and the event loop (tui spec, "Terminal,
//! signals and the run log").
//!
//! - **Setup** is ratatui's `ratatui::try_init`'s steps
//!   (<https://docs.rs/ratatui/0.30.2/ratatui/init/index.html>): raw mode,
//!   the alternate screen, then `Terminal::new` over a `CrosstermBackend`,
//!   as `CrosstermBackend`'s own example builds one over any writer
//!   (<https://docs.rs/ratatui-crossterm/0.1.2/ratatui_crossterm/struct.CrosstermBackend.html>),
//!   here a [`FRAME_BUFFER`]-sized `BufWriter`; ratatui's
//!   `ratatui::try_restore` on the way out. With mouse
//!   capture through crossterm's `EnableMouseCapture`
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EnableMouseCapture.html>)
//!   and bracketed paste through its `EnableBracketedPaste`
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EnableBracketedPaste.html>),
//!   so a paste comes as one `Event::Paste` and its newlines aren't
//!   Enter; every way out turns both off again.
//! - **Why not `ratatui::try_init` itself:** it builds its backend on
//!   std's `Stdout`, which is line-buffered with a 1 KiB buffer
//!   (<https://doc.rust-lang.org/std/io/struct.Stdout.html>), and a frame
//!   holds no newline: a frame went out 1 KiB at a time, 4 to 26 writes,
//!   each taking stdout's lock (perf round 3). Over the buffer a frame is
//!   one write, crossterm's lazy execution
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/index.html#lazy-execution>).
//!   Its panic hook (raw mode off, the alternate screen left) is private:
//!   [`mouse_off_on_panic`]'s hook does that through `ratatui::restore`.
//! - **On `/dev/tty`** (the inline fallback, tui spec "The inline
//!   fallback"; [`Tui::on_tty`]) the same steps run on a `CrosstermBackend`
//!   over the tty file, buffered the same. Why not stdout there: it is the
//!   pipe the chosen log goes to. crossterm reads its
//!   input and the size from the terminal either way (stdin when it is
//!   one, else `/dev/tty`: its `tty_fd` and `window_size`).
//! - **A panic** runs [`mouse_off_on_panic`]'s hook: the mouse, bracketed
//!   paste and the cursor, then ratatui's restore (raw mode off, the
//!   alternate screen left), chained before cli-core's run-log hook and
//!   std's message (`RunLog::record_panics` is installed first, as
//!   ratatui's docs ask of its own hook).
//! - **A second signal** (cli-core's forced exit) runs [`Tui::enter`]'s
//!   restore hook on cli-core's restore thread: raw mode off and the
//!   restoring sequence written through its own file descriptor
//!   (`Shutdown::on_forced_exit`'s contract). The first SIGTERM or SIGHUP
//!   only cancels the token: [`Tui::run`] returns [`Ending::Cancelled`] and
//!   the caller leaves the screen as on any other ending.
//! - **The event loop** follows ratatui's async event handler recipe
//!   (<https://ratatui.rs/recipes/apps/terminal-and-event-handler/>): a task
//!   reads crossterm's `EventStream` in `tokio::select!` and sends each
//!   event on an unbounded channel. Why not the recipe's 60/s render
//!   interval and 4/s tick beside it: they woke every screen 75-88 times
//!   a second with nothing to do, each wake also polling the
//!   `EventStream`, whose lock spins and yields while its reader thread
//!   holds it (perf round 3: 2.4-3.9 ms of CPU a second, idle; the
//!   maintainer's go-ahead, 2026-11-21). The loop keeps the timers
//!   itself, each armed only while wanted and on the grid its interval
//!   ticked on ([`Grid`]): a frame's while one is pending, 60 a second,
//!   and a tick's at the time a screen asks for one ([`App::tick_at`]),
//!   4 a second.

use std::fs::File;
use std::io::{self, BufWriter, Stdout, Write as _};
use std::os::fd::AsFd as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::Duration;

use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as Input, EventStream, KeyModifiers,
};
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{Command as _, execute, queue};
use futures_util::{Stream, StreamExt as _};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::{Frame, Terminal};
use sb_cli_core::cancel::{RestoreHook, Shutdown};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Frames per second at most (the recipe's default render rate).
const FRAME_RATE: f64 = 60.0;

/// The grid a screen's tick comes on, per second (the recipe's default
/// tick rate): a tick asked for at a time comes at the grid's next one,
/// as it did when the tick came 4 times a second.
const TICK_RATE: f64 = 4.0;

/// How long input is read and dropped after mouse capture is turned off
/// while the mouse is in use ([`MOUSE_RECENT`]), so mouse codes already
/// on their way don't reach the shell.
/// UNVERIFIED: no ratatui or crossterm documentation covers it. proto-logs
/// measured it (FINDINGS.md): mouse codes sent 10, 50 and 100 ms after
/// quitting left 600 bytes in the shell without the drain, none with it.
const DRAIN: Duration = Duration::from_millis(150);

/// How long the drain waits for the terminal's answer to [`STATUS_QUERY`]
/// at most (the 1 s cap, the maintainer 2026-11-21): it ends the moment
/// the answer comes, so only a terminal that never answers waits it
/// whole. 150 ms left the answer of a link slower than that to reach the
/// shell; a second covers the round trips of SSH across the world and
/// mobile links.
const ANSWER_WAIT: Duration = Duration::from_secs(1);

/// The primary device attributes query (DA1, `ESC [ c`), which every
/// VT100-compatible terminal answers with `ESC [ ? ... c`
/// (<https://vt100.net/docs/vt510-rm/DA1.html>). Written after mouse
/// capture is turned off, its answer comes after every mouse report the
/// terminal sent before it took the mouse off: the end of the drain.
const STATUS_QUERY: &[u8] = b"\x1b[c";

/// A mouse report this recent when the screen is left drains the whole
/// [`DRAIN`], as before: a terminal that answers the query itself (mosh
/// answers it at once, ahead of reports still on their way from the real
/// terminal) can't end the drain early while the mouse is in use.
const MOUSE_RECENT: Duration = Duration::from_secs(1);

/// The screen's write buffer, in bytes: a frame fits it whole, so it is
/// one write (a 240x70 screen's first page is about 26 KB; 8 KiB, std's
/// `BufWriter` default, still split a wheel frame there in two or three).
const FRAME_BUFFER: usize = 64 * 1024;

/// Whether a screen is up and not yet restored: set by [`Tui::enter`],
/// taken by whichever restores it first (the panic hook, [`Tui::leave`]
/// or the drop), so a panic on the screen's own thread restores it once,
/// before std's message. Process-wide, as the terminal and the panic hook
/// are: one screen at a time.
static SCREEN: AtomicBool = AtomicBool::new(false);

/// The panic hook for the mouse, installed once per process.
static MOUSE_HOOK: Once = Once::new();

/// The tty a [`Tui::on_tty`] screen draws on, for the panic hook: ratatui's
/// own hook restores stdout only.
static TTY: Mutex<Option<File>> = Mutex::new(None);

/// What a screen does with its events; [`Tui::run`] calls it.
pub trait App {
    /// A terminal event (a key press, the mouse, a resize, focus).
    /// Ctrl-C never reaches it: the loop ends with
    /// [`Ending::Interrupted`].
    fn input(&mut self, input: Input) -> Flow;

    /// When the screen next wants [`App::tick`] (asked again after
    /// every event): never, unless it says.
    fn tick_at(&self) -> Option<Instant> {
        None
    }

    /// The screen's timed work, at [`App::tick_at`].
    fn tick(&mut self) -> Flow {
        Flow::Unchanged
    }

    /// Whether the screen's first frame has nothing to show until its
    /// source's first wake: it then waits for that wake, a frame time at
    /// most, and is drawn at once with what the wake brought, so the
    /// screen opens with its content in one frame, not an empty frame
    /// then the content on the next frame's time.
    fn first_frame_waits(&self) -> bool {
        false
    }

    /// A source woke the screen ([`Tui::waker`]): take what it sent.
    fn wake(&mut self) -> Flow {
        Flow::Unchanged
    }

    /// Draw the screen; called at most 60 times a second, only after a
    /// change (and once at the start).
    fn draw(&mut self, frame: &mut Frame<'_>);
}

/// What [`App::input`] and [`App::tick`] ask of the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Something changed: draw the next frame.
    Redraw,
    /// Nothing to draw.
    Unchanged,
    /// Close the screen.
    Quit,
}

/// How [`Tui::run`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The screen asked to quit ([`Flow::Quit`]).
    Quit,
    /// Ctrl-C, a key in raw mode (tui decision 13: sb exits 130 quietly).
    Interrupted,
    /// The run's token was cancelled (a first SIGTERM or SIGHUP).
    Cancelled,
}

/// What the event task sends the loop.
#[derive(Debug)]
enum Event {
    Input(Input),
    /// Reading the terminal failed, or its input ended; the task stops.
    Failed(io::Error),
}

/// `out` behind a [`FRAME_BUFFER`], for a screen's backend.
fn framed<W: io::Write>(out: W) -> BufWriter<W> {
    BufWriter::with_capacity(FRAME_BUFFER, out)
}

/// Where a screen draws, each behind a [`FRAME_BUFFER`]: the backend
/// queues each cell's commands and ratatui flushes once a frame,
/// crossterm's lazy execution
/// (<https://docs.rs/crossterm/0.29.0/crossterm/index.html#lazy-execution>).
#[derive(Debug)]
enum Screen {
    /// stdout.
    Stdout(Terminal<CrosstermBackend<BufWriter<Stdout>>>),
    /// `/dev/tty` (stdout isn't a terminal).
    Tty(Terminal<CrosstermBackend<BufWriter<File>>>),
}

/// A full-screen terminal: raw mode, the alternate screen, mouse capture
/// and bracketed paste, with its event task. [`Tui::leave`] restores the terminal;
/// dropping it without leaving restores it too, without the input drain.
#[derive(Debug)]
pub struct Tui {
    screen: Screen,
    events: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<()>,
    token: CancellationToken,
    /// What the screen's sources wake it with (docs/specs/tui.md,
    /// "Verification notes": the wake-up path).
    wake: Arc<Notify>,
    /// When the event task last read a mouse report.
    mouse: Arc<Mutex<Option<std::time::Instant>>>,
    /// Removes the forced-exit hook when the screen goes.
    _restore: RestoreHook,
    left: bool,
}

impl Tui {
    /// Set the terminal up on stdout and start the event task; must run
    /// inside a tokio runtime, with no other screen up (one terminal, one
    /// screen at a time). `shutdown`'s token ends [`Tui::run`], and
    /// its forced exit restores the terminal.
    ///
    /// # Errors
    ///
    /// The I/O error when stdout can't be duplicated for the restore hook,
    /// or raw mode, the alternate screen, mouse capture or bracketed paste
    /// can't be turned
    /// on (the terminal is restored first).
    pub fn enter(shutdown: &Shutdown) -> io::Result<Self> {
        // Its own handle for the restore thread, which mustn't take std's
        // stdout lock (`Shutdown::on_forced_exit`).
        let out = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        mouse_off_on_panic();
        let terminal = enable_raw_mode()
            .and_then(|()| execute!(io::stdout(), EnterAlternateScreen))
            .and_then(|()| Terminal::new(CrosstermBackend::new(framed(io::stdout()))))
            .inspect_err(|_| ratatui::restore())?;
        SCREEN.store(true, Ordering::SeqCst);
        let restore = shutdown.on_forced_exit(forced_exit_restore(out));
        if let Err(err) = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste) {
            // Either mode may have reached the terminal: both off again,
            // which ratatui's restore doesn't do.
            let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
            SCREEN.store(false, Ordering::SeqCst);
            ratatui::restore();
            return Err(err);
        }
        Ok(Self::started(Screen::Stdout(terminal), shutdown, restore))
    }

    /// [`Tui::enter`] on `tty`, the terminal `/dev/tty` opened for reading
    /// and writing, when stdout isn't a terminal: raw mode, the alternate
    /// screen, mouse capture and bracketed paste as ratatui's init and
    /// [`Tui::enter`] set
    /// them, written to `tty`; the panic hook and the forced exit restore
    /// it there.
    ///
    /// # Errors
    ///
    /// The I/O error when `tty` can't be duplicated, or raw mode, the
    /// alternate screen, mouse capture or bracketed paste can't be turned
    /// on (the terminal
    /// is restored first).
    pub fn on_tty(shutdown: &Shutdown, tty: File) -> io::Result<Self> {
        let out = tty.try_clone()?;
        let mut hook_out = tty.try_clone()?;
        enable_raw_mode()?;
        let entered = execute!(
            hook_out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
        .and_then(|()| Terminal::new(CrosstermBackend::new(framed(tty))));
        let terminal = entered.inspect_err(|_| {
            let _ = hook_out.write_all(restore_sequence().as_bytes());
            let _ = disable_raw_mode();
        })?;
        mouse_off_on_panic();
        *TTY.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook_out);
        SCREEN.store(true, Ordering::SeqCst);
        let restore = shutdown.on_forced_exit(forced_exit_restore(out));
        Ok(Self::started(Screen::Tty(terminal), shutdown, restore))
    }

    /// The screen up, with its event task started.
    fn started(screen: Screen, shutdown: &Shutdown, restore: RestoreHook) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let mouse = Arc::default();
        let task = tokio::spawn(read_events(EventStream::new(), tx, Arc::clone(&mouse)));
        Self {
            screen,
            events,
            task,
            token: shutdown.token(),
            wake: Arc::new(Notify::new()),
            mouse,
            _restore: restore,
            left: false,
        }
    }

    /// Run `app` until it quits, Ctrl-C is pressed or the run's token is
    /// cancelled.
    ///
    /// # Errors
    ///
    /// The I/O error when drawing fails or the terminal's input can't be
    /// read or has ended.
    pub async fn run<A: App>(&mut self, app: &mut A) -> io::Result<Ending> {
        let wake = Arc::clone(&self.wake);
        match &mut self.screen {
            Screen::Stdout(terminal) => {
                run_loop(terminal, &mut self.events, app, &self.token, &wake).await
            }
            Screen::Tty(terminal) => {
                run_loop(terminal, &mut self.events, app, &self.token, &wake).await
            }
        }
    }

    /// Mouse capture and bracketed paste off, on the screen's terminal.
    fn mouse_off(&mut self) -> io::Result<()> {
        match &mut self.screen {
            Screen::Stdout(terminal) => execute!(
                terminal.backend_mut(),
                DisableMouseCapture,
                DisableBracketedPaste
            ),
            Screen::Tty(terminal) => execute!(
                terminal.backend_mut(),
                DisableMouseCapture,
                DisableBracketedPaste
            ),
        }
    }

    /// [`STATUS_QUERY`], on the screen's terminal.
    fn query(&mut self) -> io::Result<()> {
        match &mut self.screen {
            Screen::Stdout(terminal) => {
                let out = terminal.backend_mut();
                out.write_all(STATUS_QUERY)
                    .and_then(|()| io::Write::flush(out))
            }
            Screen::Tty(terminal) => {
                let out = terminal.backend_mut();
                out.write_all(STATUS_QUERY)
                    .and_then(|()| io::Write::flush(out))
            }
        }
    }

    /// The screen's rows erased in place, the last step before it is left,
    /// so a terminal without an alternate screen (GNU screen's default,
    /// tmux with `alternate-screen off`) keeps no frame with plain values
    /// (the maintainer, 2026-11-19: every screen is left blank). [`erase_rows`], on
    /// stdout or on the tty.
    fn clear(&mut self) -> io::Result<()> {
        match &mut self.screen {
            Screen::Stdout(terminal) => erase_rows(terminal.backend_mut()),
            Screen::Tty(terminal) => erase_rows(terminal.backend_mut()),
        }
    }

    /// Raw mode off and the alternate screen left: ratatui's restore, or
    /// its two steps on the tty (`ratatui::try_restore`'s, written there).
    /// The cursor comes back with the terminal's drop (ratatui's
    /// `Terminal` shows a cursor it hid).
    fn restore(&mut self) -> io::Result<()> {
        match &mut self.screen {
            Screen::Stdout(_) => ratatui::try_restore(),
            Screen::Tty(terminal) => {
                // The screen is down before its tty leaves the slot, so the
                // panic hook never sees a tty screen up with the slot empty
                // (it would write to stdout, the pipe). The slot's lock is
                // otherwise taken only while no screen is up.
                SCREEN.store(false, Ordering::SeqCst);
                TTY.lock().unwrap_or_else(PoisonError::into_inner).take();
                let raw = disable_raw_mode();
                raw.and(execute!(terminal.backend_mut(), LeaveAlternateScreen))
            }
        }
    }

    /// The screen's wake: a source calls `notify_one` on it after each
    /// event it sends, and the loop calls [`App::wake`]. `Notify` keeps
    /// one permit, so a wake is never lost and a burst costs one
    /// (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html#method.notify_one>).
    #[must_use]
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Restore the terminal: mouse capture and bracketed paste off, then
    /// input read and dropped until the terminal has sent every mouse
    /// report it will, then the rows erased and ratatui's restore.
    ///
    /// The drain: the event task and its `EventStream` go, then
    /// [`STATUS_QUERY`] is written and the terminal read here, everything
    /// dropped, until its answer comes, [`ANSWER_WAIT`] at most
    /// ([`drain_until_answered`]): a local terminal answers within a few
    /// milliseconds, where every exit waited the whole 150 ms before (perf
    /// round 3; the maintainer's go-ahead, 2026-11-21), and a slow link in
    /// its round trip. After a mouse
    /// report in the last [`MOUSE_RECENT`], the whole [`DRAIN`] through the
    /// `EventStream`, as before. Why not crossterm: it parses the answer
    /// into an internal event that neither a public call nor the
    /// `EventStream` shows, and its own waits for an answer
    /// (`cursor::position`, `terminal::supports_keyboard_enhancement`)
    /// are fixed at 2 s, and `cursor::position` asks on stdout, the pipe
    /// under the `/dev/tty` screen
    /// (<https://docs.rs/crossterm/0.29.0/crossterm/terminal/fn.supports_keyboard_enhancement.html>).
    /// The terminal is read only once the `EventStream` is dropped
    /// (crossterm forbids mixing it with its own `read`/`poll`,
    /// <https://docs.rs/crossterm/0.29.0/crossterm/event/index.html>). Its
    /// drop wakes its reader thread without joining it: should that thread
    /// still read the answer, the drain waits its whole [`ANSWER_WAIT`].
    /// What stays open: an answer later than [`ANSWER_WAIT`] (a link
    /// slower than a second) reaches the shell, about 10 bytes.
    ///
    /// # Errors
    ///
    /// The I/O error when mouse capture, bracketed paste or the terminal's
    /// modes can't be
    /// turned off; every step is tried either way.
    ///
    /// The event task has ended when this returns, so its `EventStream`
    /// reads the terminal no more. Dropped before it finishes, the screen
    /// is still restored by the drop.
    pub async fn leave(mut self) -> io::Result<()> {
        let mouse = self.mouse_off();
        let started = std::time::Instant::now();
        let recent = self
            .mouse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|at| at.elapsed() < MOUSE_RECENT);
        if recent {
            let _ =
                tokio::time::timeout(DRAIN, async { while self.events.recv().await.is_some() {} })
                    .await;
            self.task.abort();
            let _ = (&mut self.task).await;
            tracing::info!(
                millis = started.elapsed().as_millis(),
                "exit drain, the mouse in use"
            );
        } else {
            self.task.abort();
            let _ = (&mut self.task).await;
            let asked = self.query();
            let answered = sb_cli_core::spawn::blocking(|| drain_until_answered(ANSWER_WAIT)).await;
            tracing::info!(
                asked = asked.is_ok(),
                answered = matches!(answered, Ok(Ok(true))),
                millis = started.elapsed().as_millis(),
                "exit drain"
            );
        }
        let cleared = self.clear();
        let restored = self.restore();
        SCREEN.store(false, Ordering::SeqCst);
        self.left = true;
        mouse.and(cleared).and(restored)
    }
}

/// Restores the terminal unless [`Tui::leave`] or the panic hook already
/// did. The event task is aborted, not awaited (a drop can't wait): its
/// `EventStream` goes at the runtime's next turn.
impl Drop for Tui {
    fn drop(&mut self) {
        self.task.abort();
        if !self.left && SCREEN.swap(false, Ordering::SeqCst) {
            let _ = self.mouse_off();
            let _ = self.clear();
            let _ = self.restore();
        }
    }
}

/// The hook cli-core's forced exit runs: [`restore_sequence`] through
/// `out`, mouse capture off first, so no mouse report comes once the
/// terminal echoes again; then raw mode off, crossterm's own call (its
/// mode lock, which only enable and disable hold, and one `tcsetattr`).
fn forced_exit_restore(out: File) -> impl Fn() + Send + 'static {
    let sequence = restore_sequence();
    move || {
        let _ = (&out).write_all(sequence.as_bytes());
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Every row of the backend's screen erased in place, one at a time
/// (crossterm's `MoveTo` and `Clear(CurrentLine)`, what ratatui's
/// `Backend::set_cursor_position` and
/// `Backend::clear_region(ClearType::CurrentLine)` write), then the cursor
/// home: queued and flushed once, crossterm's lazy execution, where each
/// backend call flushes on its own (144 writes at 240x70).
///
/// Why not ratatui's `Terminal::clear`
/// (<https://docs.rs/ratatui/0.30.2/ratatui/struct.Terminal.html#method.clear>)
/// or `Backend::clear`: both erase the display (ESC [2J), and GNU screen
/// 4.09 and tmux 3.2a without an alternate screen move a display erased so
/// into their scrollback, frame and values included, where row-by-row
/// erasing leaves nothing (docs/platform-notes/terminal-clear-scrollback.md).
/// `Terminal::clear` also asks the terminal for the cursor's position
/// first (ratatui-core 0.1.2 `terminal/buffers.rs`) and fails without
/// clearing when no answer comes. The size is the backend's (an ioctl, no
/// query).
fn erase_rows<W: io::Write>(backend: &mut CrosstermBackend<W>) -> io::Result<()> {
    let rows = backend.size()?.height;
    erase(backend, rows)
}

/// [`erase_rows`] of `rows` rows, on `out`.
fn erase(out: &mut impl io::Write, rows: u16) -> io::Result<()> {
    for row in 0..rows {
        queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    }
    queue!(out, MoveTo(0, 0))?;
    out.flush()
}

/// The rows the prepared restore sequence erases: the hook can't ask the
/// terminal's size, and a cursor moved past the last row stays on it, so
/// a bound past any real screen erases every row.
const ERASED_ROWS: u16 = 512;

/// Mouse capture and bracketed paste off, every row erased ("Blank on
/// exit", as [`Tui::leave`], [`ERASED_ROWS`] of them), the alternate
/// screen left, the cursor shown, as
/// crossterm writes them (`Command::write_ansi`), built before any hook
/// needs it. Why not `execute!` in the hook: it formats and flushes
/// several writes on the restore thread; one prepared write is the
/// "tiny" hook `Shutdown::on_forced_exit` asks for.
fn restore_sequence() -> String {
    let mut sequence = String::new();
    // Writing to a `String` can't fail.
    let _ = DisableMouseCapture.write_ansi(&mut sequence);
    let _ = DisableBracketedPaste.write_ansi(&mut sequence);
    for row in 0..ERASED_ROWS {
        let _ = MoveTo(0, row).write_ansi(&mut sequence);
        let _ = Clear(ClearType::CurrentLine).write_ansi(&mut sequence);
    }
    let _ = MoveTo(0, 0).write_ansi(&mut sequence);
    let _ = LeaveAlternateScreen.write_ansi(&mut sequence);
    let _ = Show.write_ansi(&mut sequence);
    sequence
}

/// Turn mouse capture and bracketed paste off and show the cursor when a
/// panic comes while a
/// screen is up, then raw mode off and the alternate screen left
/// (ratatui's restore, which `ratatui::try_init`'s own hook would run),
/// then the hook before it (cli-core's and std's). The hook takes
/// [`SCREEN`], so the [`Tui`] dropped by the unwinding doesn't restore
/// the terminal a second time, after std's message. A panic on another
/// thread or task restores the terminal too (ratatui's hook does so for
/// any panic) while the screen goes on: screens treat a panicked task as
/// the end of the screen.
/// std's `take_hook`/`set_hook` chaining
/// (<https://doc.rust-lang.org/std/panic/fn.take_hook.html>). Installed
/// once, for every screen of the process. A screen on the tty
/// ([`Tui::on_tty`]) is restored whole there ([`restore_sequence`], then
/// raw mode off). `try_lock`: a hook mustn't wait on a lock the panicking
/// thread may hold.
fn mouse_off_on_panic() {
    MOUSE_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SCREEN.swap(false, Ordering::SeqCst) {
                let tty = TTY.try_lock().ok().and_then(|mut tty| tty.take());
                if let Some(mut tty) = tty {
                    let _ = tty.write_all(restore_sequence().as_bytes());
                    let _ = disable_raw_mode();
                } else {
                    let _ = execute!(
                        io::stdout(),
                        DisableMouseCapture,
                        DisableBracketedPaste,
                        Show
                    );
                    ratatui::restore();
                }
            }
            previous(info);
        }));
    });
}

/// The recipe's event task: `input` (crossterm's `EventStream`), each
/// event sent on `tx`, until the loop goes or the input fails or ends.
/// Key events are sent only when pressed, as the recipe does. The time of
/// the last mouse report goes in `mouse`, for the exit drain.
///
/// Where it differs from the recipe: no tick or render interval (the
/// loop keeps its own, armed only when wanted: the module's "why not");
/// no cancellation token of its own (the loop going closes the channel,
/// and [`Tui::leave`] aborts and awaits it); one `Input` variant for
/// crossterm's events instead of one per kind; and read errors and the
/// end of input as `Failed`, which ends the loop.
///
/// Known limit: crossterm 0.29.0's reader retries on end of input rather
/// than ending its stream, so a terminal that hangs up isn't seen here;
/// the hangup's SIGHUP ends the screen through the token instead (under
/// `nohup` nothing does, until a key or a signal).
async fn read_events<S>(
    mut input: S,
    tx: mpsc::UnboundedSender<Event>,
    mouse: Arc<Mutex<Option<std::time::Instant>>>,
) where
    S: Stream<Item = io::Result<Input>> + Unpin,
{
    loop {
        let event = match input.next().await {
            Some(Ok(Input::Key(key))) if !key.is_press() => continue,
            Some(Ok(Input::Mouse(report))) => {
                *mouse.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(std::time::Instant::now());
                Event::Input(Input::Mouse(report))
            }
            Some(Ok(read)) => Event::Input(read),
            Some(Err(err)) => Event::Failed(err),
            None => Event::Failed(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the terminal's input ended",
            )),
        };
        let last = matches!(event, Event::Failed(_));
        if tx.send(event).is_err() || last {
            return;
        }
    }
}

/// When a timer of the loop fires: on a grid of `period` from the
/// screen's start, where the recipe's interval of that period ticked, so
/// frames come as often and batch as many changes as they did, and a
/// tick comes when it did.
#[derive(Clone, Copy, Debug)]
struct Grid {
    start: Instant,
    period: Duration,
}

impl Grid {
    fn new(start: Instant, per_second: f64) -> Self {
        Self {
            start,
            period: Duration::from_secs_f64(1.0 / per_second),
        }
    }

    /// The grid's first time at or after `at` (`at` itself before the
    /// start).
    fn next(&self, at: Instant) -> Instant {
        let Some(since) = at.checked_duration_since(self.start) else {
            return at;
        };
        let slots = since.as_secs_f64() / self.period.as_secs_f64();
        self.start + self.period.mul_f64(slots.ceil())
    }
}

/// What woke the loop.
enum Step {
    Event(Option<Event>),
    Draw,
    Tick,
    Wake,
}

/// `due`, or never.
async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

/// The loop: each event to `app`, a frame after a change, until an
/// [`Ending`] or an error. The first frame is drawn at once, as the
/// recipe's interval's first tick drew it, or for a screen whose first
/// frame waits for its source ([`App::first_frame_waits`]) at its first
/// wake, a frame time on at most. A key press's change is drawn at once,
/// or a frame time after the last frame if that came sooner: waiting for
/// the grid made a key's frame 8.7 ms late on average, up to 18 ms (perf
/// round 3). Any other change (the mouse, a paste, a source's lines) is
/// drawn at the frames' [`Grid`]'s next time, a frame time after the
/// last frame at the soonest, so changes within a frame time share one
/// frame and frames stay at most 60 a second: drawing those at once too
/// cost a wheel burst an extra frame, 43% more instructions. Nothing
/// pending, nothing wakes the loop but an event, a source's
/// `wake` or the screen's tick, at the ticks' grid's first time at or
/// after [`App::tick_at`] (once, however late: a stall runs the tick's
/// work once, as the recipe's skipped missed ticks did). A cancelled `token` wins over events already waiting, and the
/// terminal's events over a pending frame and a source's `wake`, so a busy
/// follow can't starve input; a frame a whole frame time late is drawn
/// before more events, so input can't starve the screen either.
async fn run_loop<B, A>(
    terminal: &mut Terminal<B>,
    events: &mut mpsc::UnboundedReceiver<Event>,
    app: &mut A,
    token: &CancellationToken,
    wake: &Notify,
) -> io::Result<Ending>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    A: App,
{
    let start = Instant::now();
    let (frames, ticks) = (Grid::new(start, FRAME_RATE), Grid::new(start, TICK_RATE));
    let mut waits = app.first_frame_waits();
    // The first frame, at once, or a frame time on while it waits.
    let mut due = Some(if waits { start + frames.period } else { start });
    let mut changed = true;
    let mut drawn: Option<Instant> = None;
    // A key press asked for the pending frame.
    let mut key = false;
    loop {
        let now = Instant::now();
        if changed && (due.is_none() || key) {
            let wanted = match drawn {
                None => now,
                Some(drawn) if key => now.max(drawn + frames.period),
                Some(drawn) => frames.next(now).max(drawn + frames.period),
            };
            due = Some(due.map_or(wanted, |due| due.min(wanted)));
            key = false;
        }
        let late = due.is_some_and(|due| now >= due + frames.period);
        let tick = app.tick_at().map(|at| ticks.next(at));
        let step = tokio::select! {
            biased;
            () = token.cancelled() => return Ok(Ending::Cancelled),
            () = std::future::ready(()), if late => Step::Draw,
            event = events.recv() => Step::Event(event),
            () = sleep_until(due), if due.is_some() => Step::Draw,
            () = sleep_until(tick), if tick.is_some() => Step::Tick,
            () = wake.notified() => Step::Wake,
        };
        let flow = match step {
            Step::Draw => {
                terminal
                    .draw(|frame| app.draw(frame))
                    .map_err(io::Error::other)?;
                // The frame's time: when it was due, as the recipe's
                // interval kept its grid (the timer fires up to a
                // millisecond late, which would add up over a run of
                // frames), or now for a late one.
                drawn = if late { Some(Instant::now()) } else { due };
                changed = false;
                due = None;
                waits = false;
                continue;
            }
            Step::Tick => app.tick(),
            Step::Wake => {
                let flow = app.wake();
                if waits && flow == Flow::Redraw {
                    // What the first frame waited for: now.
                    waits = false;
                    due = None;
                }
                flow
            }
            Step::Event(Some(Event::Input(input))) => {
                if is_ctrl_c(&input) {
                    return Ok(Ending::Interrupted);
                }
                // A new size needs a new frame whatever the screen says.
                changed |= matches!(input, Input::Resize(..));
                let pressed = matches!(input, Input::Key(_));
                let flow = app.input(input);
                key |= pressed && flow == Flow::Redraw;
                flow
            }
            Step::Event(Some(Event::Failed(err))) => return Err(err),
            Step::Event(None) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the terminal's event task ended",
                ));
            }
        };
        match flow {
            Flow::Redraw => changed = true,
            Flow::Unchanged => {}
            Flow::Quit => return Ok(Ending::Quit),
        }
    }
}

/// Read the terminal and drop what comes until its answer to
/// [`STATUS_QUERY`], `limit` at most; whether the answer came. The
/// terminal is the one crossterm reads, stdin when it is one, else
/// `/dev/tty` (crossterm's `tty_fd`). Its bytes go through anstyle-parse's
/// VT parser
/// (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/struct.Parser.html>),
/// so an answer split across reads, or after mouse reports, is still
/// seen. A blocking `poll` and `read` (rustix's), run on the blocking
/// pool. The `read` follows a `poll` that saw input, so it doesn't wait,
/// as long as nothing else reads the terminal in between: crossterm's
/// reader thread could, briefly, after its `EventStream` is dropped.
/// UNVERIFIED against a race, by reading crossterm 0.29.0's
/// `event/stream.rs` and `event/source/unix/mio.rs`: the drop's wake is
/// queued before any answer can come, so the thread returns on it
/// without reading the terminal.
fn drain_until_answered(limit: Duration) -> io::Result<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use std::io::IsTerminal as _;
    let end = std::time::Instant::now() + limit;
    let stdin = io::stdin();
    let tty;
    let fd = if stdin.is_terminal() {
        stdin.as_fd()
    } else {
        tty = File::open("/dev/tty")?;
        tty.as_fd()
    };
    let mut parser = anstyle_parse::Parser::<anstyle_parse::DefaultCharAccumulator>::new();
    let mut answer = Answer(false);
    let mut buffer = [0_u8; 1024];
    loop {
        let left = end.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let timeout = Timespec::try_from(left).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        let n = match rustix::io::read(fd, &mut buffer) {
            Ok(0) => return Ok(false),
            Ok(n) => n,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
            Err(err) => return Err(err.into()),
        };
        for &byte in &buffer[..n] {
            parser.advance(&mut answer, byte);
        }
        if answer.0 {
            return Ok(true);
        }
    }
}

/// Whether the parsed input held the answer to [`STATUS_QUERY`]: a CSI
/// sequence with the `?` marker ending in `c`.
#[derive(Debug)]
struct Answer(bool);

impl anstyle_parse::Perform for Answer {
    fn csi_dispatch(
        &mut self,
        _params: &anstyle_parse::Params,
        intermediates: &[u8],
        _ignore: bool,
        action: u8,
    ) {
        self.0 |= action == b'c' && intermediates == b"?";
    }
}

/// systemd's `HIGH_RLIMIT_NOFILE` (`src/basic/def.h`, v249 and v255):
/// what journalctl asks for with `rlimit_nofile_bump` (tui spec,
/// "`RLIMIT_NOFILE`"; decided.md A-Q6).
const HIGH_RLIMIT_NOFILE: u64 = 256 * 1024;

/// Raise the soft `RLIMIT_NOFILE` as journalctl does, for the journal files
/// a page and a follow hold open and the follow's inotify instance, to
/// min(hard, [`HIGH_RLIMIT_NOFILE`]): never lowered, the hard limit left as
/// it is. Through rustix's `getrlimit` and `setrlimit`
/// (<https://docs.rs/rustix/1.1.6/rustix/process/fn.setrlimit.html>). The
/// logs commands call it at their start, and only they (tui decision 6).
/// A failure is logged and the run goes on.
///
/// The children those commands start inherit it: systemctl, every 5 s
/// for the picker's services. journalctl resets it to 1024 only for its
/// pager (`rlimit_nofile_safe`), as a program using `select(2)` breaks on
/// descriptors past 1024; systemctl is systemd's own and doesn't, and a
/// reset per child would need `pre_exec`, which is `unsafe`, forbidden
/// here. A new child of another kind would need that weighed again.
pub fn raise_open_files() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Nofile);
    let Some(soft) = raised_open_files(&limit) else {
        return;
    };
    let raised = Rlimit {
        current: Some(soft),
        maximum: limit.maximum,
    };
    match setrlimit(Resource::Nofile, raised) {
        Ok(()) => tracing::info!(from = ?limit.current, to = soft, "open files limit raised"),
        Err(err) => tracing::warn!(error = %err, "open files limit not raised"),
    }
}

/// The soft limit [`raise_open_files`] sets, given `limit`; `None` when the
/// soft limit is there or above already, or unlimited.
fn raised_open_files(limit: &rustix::process::Rlimit) -> Option<u64> {
    let target = limit
        .maximum
        .map_or(HIGH_RLIMIT_NOFILE, |hard| hard.min(HIGH_RLIMIT_NOFILE));
    limit.current.filter(|&soft| soft < target).map(|_| target)
}

fn is_ctrl_c(input: &Input) -> bool {
    matches!(input, Input::Key(key)
        if key.code.is_char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};
    use rustix::process::Rlimit;

    fn limit(current: Option<u64>, maximum: Option<u64>) -> Rlimit {
        Rlimit { current, maximum }
    }

    /// Up to min(hard, 262144), never down; an unlimited hard limit is
    /// the 262144.
    #[test]
    fn the_open_files_limit_rises_as_journalctls() {
        assert_eq!(
            raised_open_files(&limit(Some(1024), Some(1 << 20))),
            Some(262_144)
        );
        assert_eq!(raised_open_files(&limit(Some(1024), None)), Some(262_144));
        assert_eq!(
            raised_open_files(&limit(Some(1024), Some(4096))),
            Some(4096)
        );
        assert_eq!(
            raised_open_files(&limit(Some(4095), Some(4096))),
            Some(4096)
        );
        assert_eq!(raised_open_files(&limit(Some(4096), Some(4096))), None);
        assert_eq!(raised_open_files(&limit(Some(262_144), None)), None);
        assert_eq!(
            raised_open_files(&limit(Some(1 << 20), Some(1 << 20))),
            None
        );
        assert_eq!(raised_open_files(&limit(None, None)), None);
    }
    use futures_util::stream;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    /// Counts what the loop asks of it, and when; answers keys from a
    /// script: `q` quits, `r` and any paste ask for a frame, anything else
    /// doesn't.
    #[derive(Default)]
    struct Script {
        inputs: Vec<Input>,
        /// When each tick came.
        ticks: Vec<Instant>,
        draws: usize,
        /// When each frame was drawn, from the loop's start.
        drawn: Vec<Duration>,
        started: Option<Instant>,
        tick_flow: Option<Flow>,
        /// When the next tick is asked for; each tick asks for the next
        /// one `tick_again` later, or for none.
        tick_at: Option<Instant>,
        tick_again: Option<Duration>,
        /// The inputs seen when each wake came.
        wakes: Vec<usize>,
        wake_flow: Option<Flow>,
        first_frame_waits: bool,
    }

    impl App for Script {
        fn input(&mut self, input: Input) -> Flow {
            let flow = match &input {
                Input::Key(key) if key.code == KeyCode::Char('q') => Flow::Quit,
                Input::Key(key) if key.code == KeyCode::Char('r') => Flow::Redraw,
                Input::Paste(_) => Flow::Redraw,
                _ => Flow::Unchanged,
            };
            self.inputs.push(input);
            flow
        }

        fn tick_at(&self) -> Option<Instant> {
            self.tick_at
        }

        fn first_frame_waits(&self) -> bool {
            self.first_frame_waits
        }

        fn tick(&mut self) -> Flow {
            let now = Instant::now();
            self.ticks.push(now);
            self.tick_at = self.tick_again.map(|again| now + again);
            self.tick_flow.unwrap_or(Flow::Unchanged)
        }

        fn wake(&mut self) -> Flow {
            self.wakes.push(self.inputs.len());
            self.wake_flow.unwrap_or(Flow::Unchanged)
        }

        fn draw(&mut self, frame: &mut Frame<'_>) {
            self.draws += 1;
            let started = *self.started.get_or_insert_with(Instant::now);
            self.drawn.push(started.elapsed());
            frame.render_widget(Paragraph::new(format!("draw {}", self.draws)), frame.area());
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Input {
        Input::Key(KeyEvent::new(code, modifiers))
    }

    fn char_key(c: char) -> Input {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Run the loop on a 20×2 test terminal, each of `events` sent at its
    /// time from the start (then the channel stays open, so the loop
    /// waits), with `wake` as the sources' wake.
    async fn run_with(
        app: &mut Script,
        events: Vec<(Duration, Event)>,
        token: &CancellationToken,
        wake: &Notify,
    ) -> io::Result<Ending> {
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let start = Instant::now();
        let feed = async {
            for (at, event) in events {
                tokio::time::sleep_until(start + at).await;
                tx.send(event).expect("send");
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            ending = run_loop(&mut terminal, &mut rx, app, token, wake) => ending,
            () = feed => unreachable!("the feed never ends"),
        }
    }

    async fn run(
        app: &mut Script,
        events: Vec<(Duration, Event)>,
        token: &CancellationToken,
    ) -> io::Result<Ending> {
        run_with(app, events, token, &Notify::new()).await
    }

    fn input(at: u64, input: Input) -> (Duration, Event) {
        (ms(at), Event::Input(input))
    }

    fn paste() -> Input {
        Input::Paste("text".into())
    }

    /// The first frame at once; a change but a key's on the next frame of
    /// the 60/s grid, several in one frame time together; none without a
    /// change.
    #[tokio::test(start_paused = true)]
    async fn the_first_frame_comes_at_once_and_a_change_on_the_grid() {
        let mut app = Script::default();
        let events = vec![
            input(5, char_key('x')),
            input(20, paste()),
            input(25, paste()),
            input(40, char_key('x')),
            input(205, paste()),
            input(500, char_key('q')),
        ];
        let ending = run(&mut app, events, &CancellationToken::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
        // 33.3 and 216.7 ms, tokio's timer rounding each up to a
        // millisecond.
        assert_eq!(app.drawn, [ms(0), ms(34), ms(217)]);
    }

    /// Changes every millisecond for a second, keys, pastes or the two
    /// mixed: 60 frames, one a frame time, as the recipe's render interval
    /// drew them.
    #[tokio::test(start_paused = true)]
    async fn frames_come_at_most_sixty_times_a_second() {
        let inputs: [fn(u64) -> Input; 3] = [
            |_| char_key('r'),
            |_| paste(),
            |at| if at % 3 == 0 { char_key('r') } else { paste() },
        ];
        for make in inputs {
            let mut app = Script::default();
            let mut events: Vec<_> = (1..=1_000).map(|at| input(at, make(at))).collect();
            events.push(input(1_001, char_key('q')));
            run(&mut app, events, &CancellationToken::new())
                .await
                .expect("ends");
            assert!((60..=62).contains(&app.draws), "{} frames", app.draws);
            let closest = app
                .drawn
                .windows(2)
                .map(|pair| pair[1].saturating_sub(pair[0]))
                .min()
                .expect("frames");
            assert!(closest >= ms(16), "{closest:?}");
        }
    }

    /// A key press's change is drawn at once, or a frame time after the
    /// last frame when that came sooner; a key that changes nothing draws
    /// nothing.
    #[tokio::test(start_paused = true)]
    async fn a_key_press_draws_at_once() {
        let mut app = Script::default();
        let events = vec![
            input(100, char_key('r')),
            input(105, char_key('r')),
            input(150, char_key('x')),
            input(300, char_key('r')),
            input(500, char_key('q')),
        ];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!(app.drawn, [ms(0), ms(100), ms(117), ms(300)]);
    }

    /// A key brings a frame already pending on the grid forward; a change
    /// on the grid after a key's frame keeps a frame time from it.
    #[tokio::test(start_paused = true)]
    async fn a_key_brings_a_pending_frame_forward_and_the_grid_keeps_its_distance() {
        let mut app = Script::default();
        let events = vec![
            input(102, paste()),
            input(103, char_key('r')),
            input(110, char_key('r')),
            input(112, paste()),
            input(500, char_key('q')),
        ];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        // The paste's frame was due at 116.7 ms; the key drew it at 103.
        // The key at 110 waits to 119.7; the paste after it joins that
        // frame, past the grid's 116.7.
        assert_eq!(app.drawn, [ms(0), ms(103), ms(120)]);
    }

    /// A screen whose first frame waits draws it at its source's first
    /// wake that asks for a frame, at once, with nothing drawn before;
    /// without one, a frame time after the start.
    #[tokio::test(start_paused = true)]
    async fn a_first_frame_that_waits_comes_at_the_first_wake() {
        let token = CancellationToken::new();
        let first = |app: &Script, before: Instant| app.started.map(|at| at.duration_since(before));
        let mut app = Script {
            first_frame_waits: true,
            wake_flow: Some(Flow::Redraw),
            ..Script::default()
        };
        let wake = Notify::new();
        let woken = async {
            tokio::time::sleep(ms(5)).await;
            wake.notify_one();
            std::future::pending::<()>().await;
        };
        let before = Instant::now();
        tokio::select! {
            ending = run_with(&mut app, vec![input(100, char_key('q'))], &token, &wake) => {
                assert_eq!(ending.expect("ends"), Ending::Quit);
            }
            () = woken => unreachable!("never ends"),
        }
        assert_eq!((app.draws, first(&app, before)), (1, Some(ms(5))));
        let mut quiet = Script {
            first_frame_waits: true,
            ..Script::default()
        };
        let before = Instant::now();
        run(&mut quiet, vec![input(100, char_key('q'))], &token)
            .await
            .expect("ends");
        assert_eq!((quiet.draws, first(&quiet, before)), (1, Some(ms(17))));
    }

    /// An idle screen: one frame, no tick, the loop waiting on nothing
    /// but its events (paused time stays where it is, so a wake would
    /// draw or tick).
    #[tokio::test(start_paused = true)]
    async fn an_idle_screen_draws_once_and_never_ticks() {
        let mut app = Script::default();
        let events = vec![input(60_000, char_key('q'))];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!((app.draws, app.ticks.len()), (1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn the_frame_is_what_the_app_draws() {
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut app = Script::default();
        let token = CancellationToken::new();
        let ran = tokio::time::timeout(
            ms(100),
            run_loop(&mut terminal, &mut rx, &mut app, &token, &Notify::new()),
        )
        .await;
        assert!(ran.is_err(), "the loop waits for input");
        terminal
            .backend()
            .assert_buffer_lines(["draw 1              ", "                    "]);
        tx.send(Event::Input(char_key('q'))).expect("send");
        let ending = run_loop(&mut terminal, &mut rx, &mut app, &token, &Notify::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    #[tokio::test(start_paused = true)]
    async fn ctrl_c_interrupts_without_reaching_the_app() {
        let mut app = Script::default();
        let events = vec![
            input(1, key(KeyCode::Char('c'), KeyModifiers::NONE)),
            input(
                2,
                key(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ),
            input(3, char_key('q')),
        ];
        let ending = run(&mut app, events, &CancellationToken::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Interrupted);
        assert_eq!(app.inputs, [key(KeyCode::Char('c'), KeyModifiers::NONE)]);
    }

    /// The viewer on the loop: a source's page reaches it through the
    /// wake, and Ctrl-C ends the run as interrupted (sb exits 130, tui
    /// decision 13) with the source still open until the viewer goes.
    #[tokio::test]
    async fn the_viewer_takes_wakes_and_ctrl_c_interrupts_it() {
        use crate::line::Format;
        use crate::source::{Event as SourceEvent, PageKind};
        use crate::testing::{fake_source, line, position};
        use crate::viewer::{Kind, Viewer};

        let wake = Arc::new(Notify::new());
        let mut fake = None;
        let format = Format::new(jiff::tz::TimeZone::UTC, sb_cli_core::theme::Theme::plain());
        let mut viewer = Viewer::open("app", Kind::Service, format, Arc::clone(&wake), |outbox| {
            let (handle, source) = fake_source(outbox);
            fake = Some(source);
            handle
        });
        let fake = fake.expect("opened");
        assert!(fake.send(SourceEvent::Page {
            kind: PageKind::Tail,
            lines: vec![line(1, "hello")],
            oldest: Some(position(1)),
            newest: Some(position(1)),
            more_older: false,
            more_newer: false,
        }));
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        let ran = tokio::time::timeout(
            ms(200),
            run_loop(&mut terminal, &mut rx, &mut viewer, &token, &wake),
        )
        .await;
        assert!(ran.is_err(), "the loop waits for input");
        tx.send(Event::Input(key(KeyCode::Char('c'), KeyModifiers::CONTROL)))
            .expect("send");
        let ending = run_loop(&mut terminal, &mut rx, &mut viewer, &token, &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Interrupted);
        terminal.backend().assert_buffer_lines([
            "app  service  following       ",
            "--- start of log ---          ",
            "hello                         ",
            "--- following ---             ",
            "Esc back       ? keys · q quit",
        ]);
        assert_eq!(viewer.leave(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn ctrl_with_another_key_reaches_the_app() {
        let mut app = Script::default();
        let events = vec![
            input(1, key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            input(2, char_key('q')),
        ];
        assert_eq!(
            run(&mut app, events, &CancellationToken::new())
                .await
                .expect("ends"),
            Ending::Quit
        );
        assert_eq!(app.inputs.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_token_ends_the_run_before_waiting_events() {
        let token = CancellationToken::new();
        token.cancel();
        let mut app = Script::default();
        let ending = run(&mut app, vec![input(0, char_key('q'))], &token).await;
        assert_eq!(ending.expect("ends"), Ending::Cancelled);
        assert_eq!(app.inputs, [] as [Input; 0]);
    }

    #[tokio::test]
    async fn a_token_cancelled_while_waiting_ends_the_run() {
        let token = CancellationToken::new();
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut app = Script::default();
        let cancel = token.clone();
        let wake = Notify::new();
        let (ending, ()) = tokio::join!(
            run_loop(&mut terminal, &mut rx, &mut app, &token, &wake),
            async move {
                tokio::task::yield_now().await;
                cancel.cancel();
            }
        );
        assert_eq!(ending.expect("ends"), Ending::Cancelled);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_is_the_error() {
        let mut app = Script::default();
        let failed = Event::Failed(io::Error::new(io::ErrorKind::BrokenPipe, "gone"));
        let err = run(
            &mut app,
            vec![(ms(1), failed), input(2, char_key('q'))],
            &CancellationToken::new(),
        )
        .await
        .expect_err("fails");
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn the_event_task_ending_is_an_error() {
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(tx);
        let mut app = Script::default();
        let err = run_loop(
            &mut terminal,
            &mut rx,
            &mut app,
            &CancellationToken::new(),
            &Notify::new(),
        )
        .await
        .expect_err("fails");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    /// A tick comes at the 4/s grid's first time at or after the one
    /// asked for, and can ask for a frame; then the next asked for.
    #[tokio::test(start_paused = true)]
    async fn a_tick_comes_on_the_grid_when_asked_and_can_ask_for_a_frame() {
        let start = Instant::now();
        let mut app = Script {
            tick_flow: Some(Flow::Redraw),
            tick_at: Some(start + ms(1_100)),
            tick_again: Some(ms(5_000)),
            ..Script::default()
        };
        let events = vec![input(13_000, char_key('q'))];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        let at: Vec<_> = app.ticks.iter().map(|t| t.duration_since(start)).collect();
        assert_eq!(at, [ms(1_250), ms(6_250), ms(11_250)]);
        assert_eq!(app.draws, 4, "the first frame and one a tick");
    }

    /// A tick asked for before the loop starts comes at once.
    #[tokio::test(start_paused = true)]
    async fn a_tick_asked_for_in_the_past_comes_at_once() {
        let start = Instant::now();
        tokio::time::advance(ms(10)).await;
        let mut app = Script {
            tick_at: Some(start),
            ..Script::default()
        };
        let events = vec![input(100, char_key('q'))];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!(app.ticks, [start + ms(10)]);
    }

    /// A wake waiting with input: the input first, then one wake for
    /// any number of notifies, which can ask for a frame.
    #[tokio::test(start_paused = true)]
    async fn a_wake_comes_after_waiting_input_and_can_ask_for_a_frame() {
        let mut app = Script {
            wake_flow: Some(Flow::Redraw),
            ..Script::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let wake = Notify::new();
        wake.notify_one();
        wake.notify_one();
        tx.send(Event::Input(char_key('x'))).expect("send");
        let token = CancellationToken::new();
        let ran = tokio::time::timeout(
            ms(300),
            run_loop(&mut terminal, &mut rx, &mut app, &token, &wake),
        )
        .await;
        assert!(ran.is_err(), "the loop waits for more");
        assert_eq!(app.wakes, [1], "after the input, once");
        assert_eq!(app.draws, 2, "the first frame, then the wake's");
        tx.send(Event::Input(char_key('q'))).expect("send");
        let ending = run_loop(&mut terminal, &mut rx, &mut app, &token, &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    #[tokio::test(start_paused = true)]
    async fn a_wake_can_quit() {
        let mut app = Script {
            wake_flow: Some(Flow::Quit),
            ..Script::default()
        };
        let wake = Notify::new();
        wake.notify_one();
        let ending = run_with(&mut app, Vec::new(), &CancellationToken::new(), &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tick_can_quit() {
        let mut app = Script {
            tick_flow: Some(Flow::Quit),
            tick_at: Some(Instant::now()),
            ..Script::default()
        };
        let ending = run(&mut app, Vec::new(), &CancellationToken::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    /// A resize asks for a frame whatever the screen says; focus doesn't.
    #[tokio::test(start_paused = true)]
    async fn a_resize_asks_for_a_frame() {
        let mut app = Script::default();
        let events = vec![
            input(5, Input::Resize(30, 3)),
            input(50, Input::FocusGained),
            input(100, char_key('q')),
        ];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!(app.drawn, [ms(0), ms(17)]);
    }

    /// Collect what the event task sends until it stops.
    async fn sent(input: Vec<io::Result<Input>>) -> Vec<Event> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        read_events(stream::iter(input), tx, Arc::default()).await;
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[tokio::test(start_paused = true)]
    async fn only_pressed_keys_are_sent_and_the_input_ending_is_the_last_event() {
        let release = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
        let events = sent(vec![
            Ok(Input::Key(release)),
            Ok(Input::Key(repeat)),
            Ok(char_key('b')),
            Ok(Input::FocusLost),
        ])
        .await;
        let inputs: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::Input(input) => Some(input.clone()),
                Event::Failed(_) => None,
            })
            .collect();
        assert_eq!(inputs, [char_key('b'), Input::FocusLost]);
        assert!(
            matches!(events.last(), Some(Event::Failed(err)) if err.kind() == io::ErrorKind::UnexpectedEof),
            "{events:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_error_is_sent_and_ends_the_task() {
        let events = sent(vec![
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "no")),
            Ok(char_key('b')),
        ])
        .await;
        assert!(
            matches!(events.last(), Some(Event::Failed(err)) if err.kind() == io::ErrorKind::PermissionDenied),
            "{events:?}"
        );
        assert!(!events.iter().any(|event| matches!(event, Event::Input(_))));
    }

    /// A mouse report's time is kept for the exit drain; a key's isn't.
    #[tokio::test]
    async fn the_event_task_keeps_the_last_mouse_reports_time() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mouse = Arc::new(Mutex::new(None));
        let (tx, _rx) = mpsc::unbounded_channel();
        read_events(stream::iter([Ok(char_key('a'))]), tx, Arc::clone(&mouse)).await;
        assert_eq!(*mouse.lock().expect("lock"), None);
        let report = Input::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        let before = std::time::Instant::now();
        let (tx, _rx) = mpsc::unbounded_channel();
        read_events(stream::iter([Ok(report)]), tx, Arc::clone(&mouse)).await;
        let at = mouse.lock().expect("lock").expect("kept");
        assert!(at >= before);
    }

    /// The drain's end: the answer to the status query (`ESC [ ? ... c`),
    /// whole or split, after mouse reports or not; nothing else ends it,
    /// a secondary attributes answer (`ESC [ > ... c`) included.
    #[test]
    fn the_status_querys_answer_is_found_in_the_input() {
        let found = |chunks: &[&[u8]]| {
            let mut parser = anstyle_parse::Parser::<anstyle_parse::DefaultCharAccumulator>::new();
            let mut answer = Answer(false);
            for chunk in chunks {
                for &byte in *chunk {
                    parser.advance(&mut answer, byte);
                }
            }
            answer.0
        };
        assert!(found(&[b"\x1b[?62;22c"]));
        assert!(found(&[b"\x1b[?1;2c"]));
        assert!(found(&[
            b"\x1b[<35;10;5M\x1b[<35;11;5M",
            b"\x1b[?6",
            b"2;c"
        ]));
        assert!(found(&[b"typed c\x1b[A\x1b[?65;1;9c"]));
        assert!(!found(&[b"\x1b[<35;10;5M\x1b[<0;10;5m"]));
        assert!(!found(&[b"c\x1b[c"]));
        assert!(!found(&[b"\x1b[>0;95;0c"]));
        assert!(!found(&[b"\x1b[?62;22"]));
    }

    /// The event task sends input only: no tick or render of its own.
    #[tokio::test(start_paused = true)]
    async fn the_event_task_sends_nothing_without_input() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(read_events(stream::pending(), tx, Arc::default()));
        tokio::time::sleep(Duration::from_secs(10)).await;
        task.abort();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_stops_when_the_loop_goes() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        // Returns at its first send instead of running forever.
        let input = stream::iter([Ok(char_key('b'))]).chain(stream::pending());
        read_events(input, tx, Arc::default()).await;
    }

    /// What reaches the terminal, each write as it came.
    #[derive(Clone, Default)]
    struct Writes(Arc<Mutex<Vec<Vec<u8>>>>);

    impl io::Write for Writes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Writes {
        fn taken(&self) -> Vec<Vec<u8>> {
            std::mem::take(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
        }
    }

    /// A full 240x70 frame of styled text is one write, and so is the
    /// next one that changes every cell.
    #[test]
    fn a_frame_is_one_write() {
        use ratatui::style::{Color, Style};
        use ratatui::text::{Line as TextLine, Span};
        use ratatui::{TerminalOptions, Viewport};

        let writes = Writes::default();
        let area = ratatui::layout::Rect::new(0, 0, 240, 70);
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(framed(writes.clone())),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )
        .expect("terminal");
        for round in 0..2_u8 {
            terminal
                .draw(|frame| {
                    let lines: Vec<TextLine<'_>> = (0..70_u8)
                        .map(|row| {
                            let colour = Color::Indexed(row.wrapping_add(round));
                            let text = format!("{round}{row:>3} ").repeat(48);
                            TextLine::from(Span::styled(text, Style::new().fg(colour)))
                        })
                        .collect();
                    frame.render_widget(Paragraph::new(lines), frame.area());
                })
                .expect("draws");
            let taken = writes.taken();
            assert_eq!(taken.len(), 1, "round {round}");
            assert!(taken[0].len() > 16_800, "every cell: {}", taken[0].len());
        }
    }

    /// The rows erased at the exit are one write, crossterm's `MoveTo`
    /// and `Clear(CurrentLine)` for each row, then the cursor home.
    #[test]
    fn the_exit_erases_every_row_in_one_write() {
        let writes = Writes::default();
        let mut backend = CrosstermBackend::new(framed(writes.clone()));
        erase(&mut backend, 70).expect("erases");
        let mut expected = String::new();
        for row in 0..70 {
            MoveTo(0, row).write_ansi(&mut expected).expect("write");
            Clear(ClearType::CurrentLine)
                .write_ansi(&mut expected)
                .expect("write");
        }
        MoveTo(0, 0).write_ansi(&mut expected).expect("write");
        assert_eq!(writes.taken(), [expected.into_bytes()]);
    }

    #[test]
    fn the_restore_sequence_turns_the_mouse_off_leaves_the_screen_and_shows_the_cursor() {
        let sequence = restore_sequence();
        let mut expected = String::new();
        DisableMouseCapture
            .write_ansi(&mut expected)
            .expect("write");
        DisableBracketedPaste
            .write_ansi(&mut expected)
            .expect("write");
        for row in 0..ERASED_ROWS {
            MoveTo(0, row).write_ansi(&mut expected).expect("write");
            Clear(ClearType::CurrentLine)
                .write_ansi(&mut expected)
                .expect("write");
        }
        MoveTo(0, 0).write_ansi(&mut expected).expect("write");
        LeaveAlternateScreen
            .write_ansi(&mut expected)
            .expect("write");
        Show.write_ansi(&mut expected).expect("write");
        assert_eq!(sequence, expected);
        // The mouse modes crossterm turns on, then the screen and cursor.
        for code in [
            "\x1b[?1000l",
            "\x1b[?1006l",
            "\x1b[?2004l",
            "\x1b[?1049l",
            "\x1b[?25h",
        ] {
            assert!(sequence.contains(code), "{code:?} in {sequence:?}");
        }
    }
}
