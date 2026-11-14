//! Terminal setup and restore, and the event loop (tui spec, "Terminal,
//! signals and the run log").
//!
//! - **Setup** is ratatui's documented path, `ratatui::try_init` and
//!   `ratatui::try_restore`
//!   (<https://docs.rs/ratatui/0.30.2/ratatui/init/index.html>), with mouse
//!   capture through crossterm's `EnableMouseCapture`
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EnableMouseCapture.html>).
//! - **A panic** runs ratatui's hook (raw mode off, the alternate screen
//!   left), chained before cli-core's run-log hook and std's message
//!   (`RunLog::record_panics` is installed first, as ratatui's docs ask),
//!   plus [`mouse_off_on_panic`]'s hook for the mouse and the cursor, which
//!   ratatui's restore leaves as they are.
//! - **A second signal** (cli-core's forced exit) runs [`Tui::enter`]'s
//!   restore hook on cli-core's restore thread: raw mode off and the
//!   restoring sequence written through its own file descriptor
//!   (`Shutdown::on_forced_exit`'s contract). The first SIGTERM or SIGHUP
//!   only cancels the token: [`Tui::run`] returns [`Ending::Cancelled`] and
//!   the caller leaves the screen as on any other ending.
//! - **The event loop** is ratatui's async event handler recipe
//!   (<https://ratatui.rs/recipes/apps/terminal-and-event-handler/>): a task
//!   reads crossterm's `EventStream` in `tokio::select!` beside a 4/s tick
//!   and a 60/s render interval and sends each on an unbounded channel;
//!   the loop draws on a render event only when something changed.

use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::AsFd as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as Input, EventStream, KeyModifiers,
};
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::{Command as _, execute};
use futures_util::{FutureExt as _, Stream, StreamExt as _};
use ratatui::backend::Backend;
use ratatui::{DefaultTerminal, Frame, Terminal};
use sb_cli_core::cancel::{RestoreHook, Shutdown};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Render events per second: frames are drawn at most this often (the
/// recipe's default).
const FRAME_RATE: f64 = 60.0;

/// Ticks per second, for a screen's timed work (the recipe's default).
const TICK_RATE: f64 = 4.0;

/// How long input is read and dropped after mouse capture is turned off,
/// so mouse codes already on their way don't reach the shell.
/// UNVERIFIED: no ratatui or crossterm documentation covers it. proto-logs
/// measured it (FINDINGS.md): mouse codes sent 10, 50 and 100 ms after
/// quitting left 600 bytes in the shell without the drain, none with it.
const DRAIN: Duration = Duration::from_millis(150);

/// Whether a screen is up and not yet restored: set by [`Tui::enter`],
/// taken by whichever restores it first (the panic hook, [`Tui::leave`]
/// or the drop), so a panic on the screen's own thread restores it once,
/// before std's message. Process-wide, as the terminal and the panic hook
/// are: one screen at a time.
static SCREEN: AtomicBool = AtomicBool::new(false);

/// The panic hook for the mouse, installed once per process.
static MOUSE_HOOK: Once = Once::new();

/// What a screen does with its events; [`Tui::run`] calls it.
pub trait App {
    /// A terminal event (a key press, the mouse, a resize, focus).
    /// Ctrl-C never reaches it: the loop ends with
    /// [`Ending::Interrupted`].
    fn input(&mut self, input: Input) -> Flow;

    /// The 4/s tick.
    fn tick(&mut self) -> Flow {
        Flow::Unchanged
    }

    /// A source woke the screen ([`Tui::waker`]): take what it sent.
    fn wake(&mut self) -> Flow {
        Flow::Unchanged
    }

    /// Draw the screen; called at most 60 times a second, only after a
    /// change.
    fn draw(&mut self, frame: &mut Frame<'_>);
}

/// What [`App::input`] and [`App::tick`] ask of the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Something changed: draw at the next render event.
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
    Tick,
    Render,
    Input(Input),
    /// Reading the terminal failed, or its input ended; the task stops.
    Failed(io::Error),
}

/// A full-screen terminal: raw mode, the alternate screen and mouse
/// capture, with its event task. [`Tui::leave`] restores the terminal;
/// dropping it without leaving restores it too, without the input drain.
#[derive(Debug)]
pub struct Tui {
    terminal: DefaultTerminal,
    events: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<()>,
    token: CancellationToken,
    /// What the screen's sources wake it with (docs/specs/tui.md,
    /// "Verification notes": the wake-up path).
    wake: Arc<Notify>,
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
    /// or raw mode, the alternate screen or mouse capture can't be turned
    /// on (the terminal is restored first).
    pub fn enter(shutdown: &Shutdown) -> io::Result<Self> {
        // Its own handle for the restore thread, which mustn't take std's
        // stdout lock (`Shutdown::on_forced_exit`).
        let out = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        let terminal = ratatui::try_init().inspect_err(|_| ratatui::restore())?;
        mouse_off_on_panic();
        SCREEN.store(true, Ordering::SeqCst);
        let restore = shutdown.on_forced_exit(forced_exit_restore(out));
        if let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
            SCREEN.store(false, Ordering::SeqCst);
            ratatui::restore();
            return Err(err);
        }
        let (tx, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(read_events(EventStream::new(), tx));
        Ok(Self {
            terminal,
            events,
            task,
            token: shutdown.token(),
            wake: Arc::new(Notify::new()),
            _restore: restore,
            left: false,
        })
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
        run_loop(
            &mut self.terminal,
            &mut self.events,
            app,
            &self.token,
            &wake,
        )
        .await
    }

    /// The screen's wake: a source calls `notify_one` on it after each
    /// event it sends, and the loop calls [`App::wake`]. `Notify` keeps
    /// one permit, so a wake is never lost and a burst costs one
    /// (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html#method.notify_one>).
    #[must_use]
    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Restore the terminal: mouse capture off, then input read and dropped
    /// for 150 ms (`DRAIN`) through the same `EventStream` (crossterm forbids
    /// mixing it with `read`/`poll`,
    /// <https://docs.rs/crossterm/0.29.0/crossterm/event/index.html>), then
    /// ratatui's restore.
    ///
    /// # Errors
    ///
    /// The I/O error when mouse capture or the terminal's modes can't be
    /// turned off; every step is tried either way.
    ///
    /// The event task has ended when this returns, so its `EventStream`
    /// reads the terminal no more. Dropped before it finishes, the screen
    /// is still restored by the drop.
    pub async fn leave(mut self) -> io::Result<()> {
        let mouse = execute!(io::stdout(), DisableMouseCapture);
        let _ = tokio::time::timeout(DRAIN, async { while self.events.recv().await.is_some() {} })
            .await;
        self.task.abort();
        let _ = (&mut self.task).await;
        let restored = ratatui::try_restore();
        SCREEN.store(false, Ordering::SeqCst);
        self.left = true;
        mouse.and(restored)
    }
}

/// Restores the terminal unless [`Tui::leave`] or the panic hook already
/// did. The event task is aborted, not awaited (a drop can't wait): its
/// `EventStream` goes at the runtime's next turn.
impl Drop for Tui {
    fn drop(&mut self) {
        self.task.abort();
        if !self.left && SCREEN.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
            ratatui::restore();
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

/// Mouse capture off, the alternate screen left, the cursor shown, as
/// crossterm writes them (`Command::write_ansi`), built before any hook
/// needs it. Why not `execute!` in the hook: it formats and flushes
/// several writes on the restore thread; one prepared write is the
/// "tiny" hook `Shutdown::on_forced_exit` asks for.
fn restore_sequence() -> String {
    let mut sequence = String::new();
    // Writing to a `String` can't fail.
    let _ = DisableMouseCapture.write_ansi(&mut sequence);
    let _ = LeaveAlternateScreen.write_ansi(&mut sequence);
    let _ = Show.write_ansi(&mut sequence);
    sequence
}

/// Turn mouse capture off and show the cursor when a panic comes while a
/// screen is up, then run the hook before it (ratatui's restore, then
/// cli-core's and std's). ratatui's restore does neither. The hook takes
/// [`SCREEN`], so the [`Tui`] dropped by the unwinding doesn't restore
/// the terminal a second time, after std's message. A panic on another
/// thread or task restores the terminal too (ratatui's hook does so for
/// any panic) while the screen goes on: screens treat a panicked task as
/// the end of the screen.
/// std's `take_hook`/`set_hook` chaining
/// (<https://doc.rust-lang.org/std/panic/fn.take_hook.html>). Installed
/// once: ratatui adds a hook of its own on every `try_init`, so on a later
/// screen ratatui's restore runs before this one, which still turns the
/// mouse off.
fn mouse_off_on_panic() {
    MOUSE_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if SCREEN.swap(false, Ordering::SeqCst) {
                let _ = execute!(io::stdout(), DisableMouseCapture, Show);
            }
            previous(info);
        }));
    });
}

/// The recipe's event task: `input` (crossterm's `EventStream`), the tick
/// and the render interval, each sent on `tx`, until the loop goes or the
/// input fails or ends. Key events are sent only when pressed, as the
/// recipe does.
///
/// Where it differs from the recipe: no cancellation token of its own
/// (the loop going closes the channel, and [`Tui::leave`] aborts and
/// awaits it); one `Input` variant for crossterm's events instead of one
/// per kind; read errors and the end of input as `Failed`, which ends the
/// loop; and missed ticks skipped rather than fired in a burst after a
/// stall (a slow frame, a stopped process), so a screen's tick work runs
/// once, not once per missed tick
/// (<https://docs.rs/tokio/1.54.1/tokio/time/enum.MissedTickBehavior.html>).
///
/// Known limit: crossterm 0.29.0's reader retries on end of input rather
/// than ending its stream, so a terminal that hangs up isn't seen here;
/// the hangup's SIGHUP ends the screen through the token instead (under
/// `nohup` nothing does, until a key or a signal).
async fn read_events<S>(mut input: S, tx: mpsc::UnboundedSender<Event>)
where
    S: Stream<Item = io::Result<Input>> + Unpin,
{
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / TICK_RATE));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut render = tokio::time::interval(Duration::from_secs_f64(1.0 / FRAME_RATE));
    render.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let next = input.next().fuse();
        let event = tokio::select! {
            read = next => match read {
                Some(Ok(Input::Key(key))) if !key.is_press() => continue,
                Some(Ok(read)) => Event::Input(read),
                Some(Err(err)) => Event::Failed(err),
                None => Event::Failed(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the terminal's input ended",
                )),
            },
            _ = tick.tick() => Event::Tick,
            _ = render.tick() => Event::Render,
        };
        let last = matches!(event, Event::Failed(_));
        if tx.send(event).is_err() || last {
            return;
        }
    }
}

/// The loop: each event to `app`, a frame on a render event after a
/// change, until an [`Ending`] or an error. A cancelled `token` wins over
/// events already waiting, and the terminal's events over a source's
/// `wake`, so a busy follow can't starve input.
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
    // The first render event draws the screen.
    let mut changed = true;
    loop {
        let event = tokio::select! {
            biased;
            () = token.cancelled() => return Ok(Ending::Cancelled),
            event = events.recv() => Some(event),
            () = wake.notified() => None,
        };
        let Some(event) = event else {
            match app.wake() {
                Flow::Redraw => changed = true,
                Flow::Unchanged => {}
                Flow::Quit => return Ok(Ending::Quit),
            }
            continue;
        };
        let flow = match event {
            Some(Event::Render) => {
                if changed {
                    terminal
                        .draw(|frame| app.draw(frame))
                        .map_err(io::Error::other)?;
                    changed = false;
                }
                continue;
            }
            Some(Event::Tick) => app.tick(),
            Some(Event::Input(input)) => {
                if is_ctrl_c(&input) {
                    return Ok(Ending::Interrupted);
                }
                // A new size needs a new frame whatever the screen says.
                changed |= matches!(input, Input::Resize(..));
                app.input(input)
            }
            Some(Event::Failed(err)) => return Err(err),
            None => {
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

fn is_ctrl_c(input: &Input) -> bool {
    matches!(input, Input::Key(key)
        if key.code.is_char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};
    use futures_util::stream;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;

    /// Counts what the loop asks of it; answers keys from a script.
    #[derive(Default)]
    struct Script {
        inputs: Vec<Input>,
        ticks: usize,
        draws: usize,
        tick_flow: Option<Flow>,
        /// The inputs seen when each wake came.
        wakes: Vec<usize>,
        wake_flow: Option<Flow>,
    }

    impl App for Script {
        fn input(&mut self, input: Input) -> Flow {
            let flow = match &input {
                Input::Key(key) if key.code == KeyCode::Char('q') => Flow::Quit,
                Input::Key(key) if key.code == KeyCode::Char('r') => Flow::Redraw,
                _ => Flow::Unchanged,
            };
            self.inputs.push(input);
            flow
        }

        fn tick(&mut self) -> Flow {
            self.ticks += 1;
            self.tick_flow.unwrap_or(Flow::Unchanged)
        }

        fn wake(&mut self) -> Flow {
            self.wakes.push(self.inputs.len());
            self.wake_flow.unwrap_or(Flow::Unchanged)
        }

        fn draw(&mut self, frame: &mut Frame<'_>) {
            self.draws += 1;
            frame.render_widget(Paragraph::new(format!("draw {}", self.draws)), frame.area());
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Input {
        Input::Key(KeyEvent::new(code, modifiers))
    }

    fn char_key(c: char) -> Input {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Run the loop over `events` (then the channel stays open, so the
    /// loop waits) on a 20×2 test terminal.
    async fn run(
        app: &mut Script,
        events: Vec<Event>,
        token: &CancellationToken,
    ) -> io::Result<Ending> {
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        for event in events {
            tx.send(event).expect("send");
        }
        let ending = run_loop(&mut terminal, &mut rx, app, token, &Notify::new()).await;
        drop(tx);
        ending
    }

    #[tokio::test]
    async fn draws_on_a_render_event_only_after_a_change() {
        let mut app = Script::default();
        let events = vec![
            Event::Render,
            Event::Render,
            Event::Input(char_key('x')),
            Event::Render,
            Event::Input(char_key('r')),
            Event::Input(char_key('r')),
            Event::Render,
            Event::Render,
            Event::Input(char_key('q')),
        ];
        let ending = run(&mut app, events, &CancellationToken::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
        // The first frame, then one for the two redraws together.
        assert_eq!(app.draws, 2);
    }

    #[tokio::test]
    async fn the_frame_is_what_the_app_draws() {
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        for event in [Event::Render, Event::Input(char_key('q'))] {
            tx.send(event).expect("send");
        }
        let mut app = Script::default();
        run_loop(
            &mut terminal,
            &mut rx,
            &mut app,
            &CancellationToken::new(),
            &Notify::new(),
        )
        .await
        .expect("ends");
        terminal
            .backend()
            .assert_buffer_lines(["draw 1              ", "                    "]);
    }

    #[tokio::test]
    async fn ctrl_c_interrupts_without_reaching_the_app() {
        let mut app = Script::default();
        let events = vec![
            Event::Input(key(KeyCode::Char('c'), KeyModifiers::NONE)),
            Event::Input(key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            Event::Input(char_key('q')),
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
        let mut terminal = Terminal::new(TestBackend::new(30, 4)).expect("test terminal");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        let ran = tokio::time::timeout(
            Duration::from_millis(200),
            run_loop(&mut terminal, &mut rx, &mut viewer, &token, &wake),
        )
        .await;
        assert!(ran.is_err(), "the loop waits for input");
        tx.send(Event::Render).expect("send");
        tx.send(Event::Input(key(KeyCode::Char('c'), KeyModifiers::CONTROL)))
            .expect("send");
        let ending = run_loop(&mut terminal, &mut rx, &mut viewer, &token, &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Interrupted);
        terminal.backend().assert_buffer_lines([
            "app  service  following   / se",
            "--- start of log ---          ",
            "hello                         ",
            "--- following ---             ",
        ]);
        assert_eq!(viewer.leave(), None);
    }

    #[tokio::test]
    async fn ctrl_with_another_key_reaches_the_app() {
        let mut app = Script::default();
        let events = vec![
            Event::Input(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Event::Input(char_key('q')),
        ];
        assert_eq!(
            run(&mut app, events, &CancellationToken::new())
                .await
                .expect("ends"),
            Ending::Quit
        );
        assert_eq!(app.inputs.len(), 2);
    }

    #[tokio::test]
    async fn a_cancelled_token_ends_the_run_before_waiting_events() {
        let token = CancellationToken::new();
        token.cancel();
        let mut app = Script::default();
        let ending = run(&mut app, vec![Event::Input(char_key('q'))], &token).await;
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

    #[tokio::test]
    async fn a_failed_read_is_the_error() {
        let mut app = Script::default();
        let failed = Event::Failed(io::Error::new(io::ErrorKind::BrokenPipe, "gone"));
        let err = run(
            &mut app,
            vec![failed, Event::Input(char_key('q'))],
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

    #[tokio::test]
    async fn ticks_reach_the_app_and_can_ask_for_a_frame() {
        let mut app = Script {
            tick_flow: Some(Flow::Redraw),
            ..Script::default()
        };
        let events = vec![
            Event::Render,
            Event::Tick,
            Event::Render,
            Event::Input(char_key('q')),
        ];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!((app.ticks, app.draws), (1, 2));
    }

    /// A wake waiting with input: the input first, then one wake for
    /// any number of notifies, which can ask for a frame.
    #[tokio::test]
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
        tx.send(Event::Render).expect("send");
        tx.send(Event::Input(char_key('x'))).expect("send");
        let token = CancellationToken::new();
        let ran = tokio::time::timeout(
            Duration::from_millis(300),
            run_loop(&mut terminal, &mut rx, &mut app, &token, &wake),
        )
        .await;
        assert!(ran.is_err(), "the loop waits for more");
        assert_eq!(app.wakes, [1], "after the input, once");
        assert_eq!(app.draws, 1);
        tx.send(Event::Render).expect("send");
        tx.send(Event::Input(char_key('q'))).expect("send");
        let ending = run_loop(&mut terminal, &mut rx, &mut app, &token, &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
        assert_eq!(app.draws, 2, "the wake's frame");
    }

    #[tokio::test]
    async fn a_wake_can_quit() {
        let mut app = Script {
            wake_flow: Some(Flow::Quit),
            ..Script::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(20, 2)).expect("test terminal");
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let wake = Notify::new();
        wake.notify_one();
        let token = CancellationToken::new();
        let ending = run_loop(&mut terminal, &mut rx, &mut app, &token, &wake).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    #[tokio::test]
    async fn a_tick_can_quit() {
        let mut app = Script {
            tick_flow: Some(Flow::Quit),
            ..Script::default()
        };
        let ending = run(&mut app, vec![Event::Tick], &CancellationToken::new()).await;
        assert_eq!(ending.expect("ends"), Ending::Quit);
    }

    #[tokio::test]
    async fn a_resize_asks_for_a_frame() {
        let mut app = Script::default();
        let events = vec![
            Event::Render,
            Event::Input(Input::Resize(30, 3)),
            Event::Render,
            Event::Input(Input::FocusGained),
            Event::Render,
            Event::Input(char_key('q')),
        ];
        run(&mut app, events, &CancellationToken::new())
            .await
            .expect("ends");
        assert_eq!(app.draws, 2);
    }

    /// Collect what the event task sends until it stops.
    async fn sent(input: Vec<io::Result<Input>>) -> Vec<Event> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        read_events(stream::iter(input), tx).await;
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
                _ => None,
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

    #[tokio::test(start_paused = true)]
    async fn ticks_come_four_times_and_renders_sixty_times_a_second() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(read_events(stream::pending(), tx));
        // Each interval's first tick is immediate; then one second's worth.
        tokio::time::sleep(Duration::from_millis(1_001)).await;
        task.abort();
        let (mut ticks, mut renders) = (0, 0);
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Tick => ticks += 1,
                Event::Render => renders += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        // The render period isn't a whole number of milliseconds, and
        // tokio's timer rounds each deadline to one.
        assert_eq!(ticks, 5);
        assert!((60..=61).contains(&renders), "{renders} renders");
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_stops_when_the_loop_goes() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        // Returns at its first send instead of running forever.
        read_events(stream::pending(), tx).await;
    }

    #[test]
    fn the_restore_sequence_turns_the_mouse_off_leaves_the_screen_and_shows_the_cursor() {
        let sequence = restore_sequence();
        let mut expected = String::new();
        DisableMouseCapture
            .write_ansi(&mut expected)
            .expect("write");
        LeaveAlternateScreen
            .write_ansi(&mut expected)
            .expect("write");
        Show.write_ansi(&mut expected).expect("write");
        assert_eq!(sequence, expected);
        // The mouse modes crossterm turns on, then the screen and cursor.
        for code in ["\x1b[?1000l", "\x1b[?1006l", "\x1b[?1049l", "\x1b[?25h"] {
            assert!(sequence.contains(code), "{code:?} in {sequence:?}");
        }
    }
}
