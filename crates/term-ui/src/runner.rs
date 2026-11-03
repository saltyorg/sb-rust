//! The task runner (term-ui spec, "Runner API", approved at Checkpoint A):
//! one tree of steps on stderr, live on a terminal and plain otherwise.
//!
//! Live drawing is proto-runner's measured no-blink technique: one
//! indicatif bar holds the whole live area and is redrawn in place
//! (`MultiProgress::set_move_cursor`), finished lines go above it with
//! `MultiProgress::println`, and the area never shrinks while running
//! (amended decision 2; https://docs.rs/indicatif/0.18.6/indicatif/struct.MultiProgress.html#method.set_move_cursor).
//! A tokio task drives the spinner frames (amended decision 4). Why not
//! indicatif's steady tick: its `{spinner}` is one per bar, and one bar
//! here holds several spinners and per-step times inside `{msg}`, which a
//! steady tick wouldn't recompute; rendering the tree from a
//! `ProgressStyle::with_key` callback would need the shared state locked
//! from inside indicatif's draw while callers of `set_message` hold it.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use sb_cli_core::cancel::{CancellationToken, RestoreHook, Shutdown};
use sb_cli_core::exit::{Exit, quiet_exit, shown_chain};
use sb_cli_core::log::{RunLog, WITHHELD};
use sb_cli_core::theme::{self, Role};

use tokio::task::JoinSet;
use tracing::Instrument;

use crate::style::paint;
use crate::tree::{Draw, Status, Tree};
use crate::tty::Saved;

/// How often the spinner moves (proto-runner's reviewed pace).
const FRAME: Duration = Duration::from_millis(80);

/// A step's words: running, done, and (optional) failure, as sb-go's
/// Running/Success/Failure. "stopped", "interrupted" and "sub-step
/// failed" are shown after the running label; ✗ shows the failure label,
/// else the running label.
#[derive(Clone, Debug)]
pub struct Labels {
    pub(crate) running: String,
    pub(crate) done: String,
    pub(crate) failure: Option<String>,
}

impl Labels {
    pub fn new(running: impl Into<String>, done: impl Into<String>) -> Self {
        Self {
            running: running.into(),
            done: done.into(),
            failure: None,
        }
    }

    pub fn failure(self, failure: impl Into<String>) -> Self {
        Self {
            failure: Some(failure.into()),
            ..self
        }
    }
}

/// How and where the runner draws.
#[derive(Default)]
pub struct Options {
    /// Where to draw: stderr by default.
    pub target: Target,
}

/// Where the runner draws.
#[derive(Default)]
pub enum Target {
    #[default]
    Stderr,
    /// An in-memory screen of a fixed size, colour on or off, for the
    /// snapshot tests: indicatif's `InMemoryTerm`, only in tests or with
    /// term-ui's `testing` feature, so vt100 never ships in sb.
    #[cfg(any(test, feature = "testing"))]
    InMemory {
        term: indicatif::InMemoryTerm,
        colour: bool,
    },
}

/// The live area: one bar redrawn in place, never shrinking.
struct Area {
    multi: MultiProgress,
    bar: ProgressBar,
    tallest: usize,
}

impl Area {
    fn new(target: ProgressDrawTarget) -> Self {
        let multi = MultiProgress::with_draw_target(target);
        multi.set_move_cursor(true);
        let style = ProgressStyle::with_template("{msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        let bar = multi.add(ProgressBar::new_spinner().with_style(style));
        Self {
            multi,
            bar,
            tallest: 0,
        }
    }

    fn draw(&mut self, printed: &[String], live: &[String]) {
        for line in printed {
            // Nothing better to do if stderr fails; the run goes on.
            let _ = self.multi.println(line);
        }
        // indicatif's cursor-moving redraw doesn't clear rows below a
        // shorter area (draw_target.rs `draw_to_term`), so the area keeps
        // its tallest height; and it skips a wholly empty message
        // (style.rs), so a blank line is one space.
        self.tallest = self.tallest.max(live.len());
        let lines: Vec<&str> = (0..self.tallest)
            .map(|i| {
                live.get(i)
                    .map_or(" ", |l| if l.is_empty() { " " } else { l })
            })
            .collect();
        self.bar.set_message(lines.join("\n"));
    }

    /// Clear the live area. proto-runner's measured clear: the cursor-moving
    /// redraw off, so the clear is one erase per line and nothing is left.
    fn clear(&self) {
        self.multi.set_move_cursor(false);
        self.bar.finish_and_clear();
    }
}

/// Live (the tree redrawn in place) or plain (start and end lines).
enum Mode {
    Live(Area),
    Plain,
}

struct State {
    /// Set by the restore hook on a second Ctrl-C: no more drawing.
    stop: Arc<AtomicBool>,
    tree: Tree,
    mode: Mode,
    colour: bool,
    /// Whether the target is a terminal: shown sensitive text shows only
    /// there (anywhere else stderr is a log; decision 8).
    terminal: bool,
    size: Size,
    frame: usize,
    ended: bool,
}

#[derive(Clone, Copy)]
enum Size {
    Stderr,
    #[cfg(any(test, feature = "testing"))]
    Fixed {
        rows: usize,
        width: usize,
    },
}

impl Size {
    /// The screen's rows and width: the terminal's, else 24 x 80.
    fn get(self) -> (usize, usize) {
        match self {
            Size::Stderr => console::Term::stderr()
                .size_checked()
                .map_or((24, 80), |(rows, cols)| {
                    (usize::from(rows), usize::from(cols))
                }),
            #[cfg(any(test, feature = "testing"))]
            Size::Fixed { rows, width } => (rows, width),
        }
    }
}

impl State {
    /// Print what can be printed and redraw the live area (live mode).
    fn redraw(&mut self) {
        // Nothing after the restore hook ran, nor after the run's end (a
        // late close; indicatif's finished bar ignores messages anyway, so
        // this is defence in depth there).
        if self.stop.load(Ordering::SeqCst) || self.ended {
            return;
        }
        let Mode::Live(area) = &mut self.mode else {
            return;
        };
        let (rows, width) = self.size.get();
        let d = Draw {
            theme: theme::current(),
            colour: self.colour,
            width,
            rows,
            frame: self.frame,
            now: Instant::now(),
        };
        let printed = self.tree.take_printable(&d);
        let live = self.tree.live(&d);
        area.draw(&printed, &live);
    }

    /// End the run once: the last lines printed, the live area cleared.
    /// While a panic unwinds, indicatif draws nothing (`MultiState::draw`
    /// and `draw_to_term` return early when `std::thread::panicking()`),
    /// so the screen keeps its last frame and nothing more is drawn here.
    fn end(&mut self) {
        if self.ended {
            return;
        }
        self.tree.end();
        if std::thread::panicking() {
            self.ended = true;
            return;
        }
        // The last draw, then nothing more (see `redraw`).
        self.redraw();
        self.ended = true;
        // After the restore hook ran (`stop`), nothing more is drawn.
        if let Mode::Live(area) = &self.mode
            && !self.stop.load(Ordering::SeqCst)
        {
            area.clear();
        }
    }
}

type Shared = Arc<Mutex<State>>;

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    // Drawing state is whole between calls, so a poisoned lock is usable.
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One run of steps, drawn as one tree (the root line is the title).
/// Live on a terminal; plain when the target isn't one (piped, CI,
/// `TERM=dumb`). No verbose option: the run log has the detail
/// (decision 9).
pub struct Runner {
    shared: Shared,
    shutdown: Shutdown,
    frames: Option<tokio::task::JoinHandle<()>>,
    /// The terminal's modes and cursor, put back at the end.
    saved: Saved,
    /// Puts the terminal back on a second Ctrl-C; removed at the end.
    restore: Option<RestoreHook>,
}

impl Runner {
    /// Start drawing. Called inside the command's tokio runtime; the frame
    /// task runs on it. Never fails. `log` gives the failure block's
    /// pointer (Task 7).
    pub async fn start(
        title: &str,
        shutdown: &Shutdown,
        log: Option<&RunLog>,
        options: Options,
    ) -> Runner {
        let pointer = log.map(RunLog::pointer);
        // The title goes out before the live area exists: a
        // `MultiProgress::println` before its bar first drew was lost
        // (seen on indicatif's in-memory terminal).
        let on_stderr = matches!(options.target, Target::Stderr);
        let (mode, colour, size, terminal) = match options.target {
            Target::Stderr => {
                plain_line(title);
                // indicatif hides its stderr target when stderr isn't a
                // terminal or `TERM` is dumb or unset (draw_target.rs,
                // `ProgressDrawTarget::stderr`), so live follows it: a
                // hidden target would swallow every line.
                let target = ProgressDrawTarget::stderr();
                let mode = if target.is_hidden() {
                    Mode::Plain
                } else {
                    Mode::Live(Area::new(target))
                };
                let terminal = console::Term::stderr().is_term();
                (
                    mode,
                    crate::colour_on(crate::Stream::Stderr),
                    Size::Stderr,
                    terminal,
                )
            }
            // Always live: the in-memory screen is for the live snapshots.
            #[cfg(any(test, feature = "testing"))]
            Target::InMemory { term, colour } => {
                let _ = indicatif::TermLike::write_line(&term, title);
                let size = Size::Fixed {
                    rows: usize::from(indicatif::TermLike::height(&term)),
                    width: usize::from(indicatif::TermLike::width(&term)),
                };
                // No refresh limit, so every redraw reaches the screen.
                let target = ProgressDrawTarget::term_like_with_hz(Box::new(term), u8::MAX);
                (Mode::Live(Area::new(target)), colour, size, true)
            }
        };
        let plain = matches!(mode, Mode::Plain);
        // Live on stderr's terminal: quiet it (`ECHOCTL` off, the cursor
        // hidden) before the first frame.
        let saved = if plain || !on_stderr {
            Saved::default()
        } else {
            Saved::quiet()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(State {
            stop: Arc::clone(&stop),
            tree: {
                let mut tree = Tree::default();
                tree.set_pointer(pointer);
                tree
            },
            mode,
            colour,
            terminal,
            size,
            frame: 0,
            ended: false,
        }));
        lock(&shared).redraw();
        // cli-core puts the terminal back on a second Ctrl-C: drawing stops
        // first (no redraw starts once `stop` is set, and one under way is
        // waited for, briefly, well inside cli-core's 500 ms deadline).
        let restore = saved.changed().then(|| {
            let (saved, stop, shared) = (saved.clone(), Arc::clone(&stop), Arc::clone(&shared));
            shutdown.on_forced_exit(move || {
                stop.store(true, Ordering::SeqCst);
                for _ in 0..20 {
                    // Free, or poisoned (no draw under way either).
                    if !matches!(shared.try_lock(), Err(TryLockError::WouldBlock)) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                saved.clone().restore_for_exit();
            })
        });
        let frames = (!plain).then(|| tokio::spawn(frames(Arc::clone(&shared), stop)));
        Runner {
            shared,
            shutdown: shutdown.clone(),
            frames,
            saved,
            restore,
        }
    }

    /// Run one top-level step to its end, after the previous one. The
    /// body's result decides the ending ("Step endings" in the spec).
    pub async fn step<T>(
        &mut self,
        labels: Labels,
        body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
    ) -> Result<T, Exit> {
        let context = Context {
            shared: Arc::clone(&self.shared),
            shutdown: self.shutdown.clone(),
        };
        let token = self.shutdown.token();
        run_step(&context, None, labels, &token, None, body).await
    }

    /// End the run at its happy end: print what is still live and clear
    /// the live area. Dropping the runner does the same.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // The frame task first, so no frame lands after the last lines.
        if let Some(frames) = self.frames.take() {
            frames.abort();
        }
        lock(&self.shared).end();
        // Then the terminal, every ending; the hook goes with it, so a
        // later second Ctrl-C can't restore stale modes.
        self.saved.restore();
        self.restore = None;
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What every step needs: the drawing state and the run's shutdown.
#[derive(Clone)]
struct Context {
    shared: Shared,
    shutdown: Shutdown,
}

/// Start step `labels` under `parent` (none: at the top), unless `token`
/// is already cancelled (then it isn't drawn and its body doesn't run).
/// Inside a concurrent sibling, `prefix` (its label) goes in front of the
/// plain start line.
fn begin(
    context: &Context,
    parent: Option<usize>,
    labels: &Labels,
    token: &CancellationToken,
    prefix: Option<&str>,
) -> Option<(usize, Instant)> {
    if token.is_cancelled() {
        tracing::info!(step = labels.running.as_str(), "step skipped (cancelled)");
        return None;
    }
    let started = Instant::now();
    let mut state = lock(&context.shared);
    let id = state.tree.add(parent, labels.clone(), started);
    tracing::info!(step = labels.running.as_str(), "step started");
    if matches!(state.mode, Mode::Plain) {
        plain_line(&prefixed(prefix, &format!("- {}", labels.running)));
    }
    state.redraw();
    Some((id, started))
}

/// `text` with a concurrent sibling's label in front, for plain mode.
fn prefixed(prefix: Option<&str>, text: &str) -> String {
    match prefix {
        Some(label) => format!("{label}: {text}"),
        None => text.to_string(),
    }
}

/// How a step's body ended, written once: `stopped_by` is the step's (or
/// its group's) token, cancelled when a sibling failed (the shutdown's own
/// cancels are told apart first).
fn conclude<T>(
    context: &Context,
    mut guard: Ending,
    result: anyhow::Result<T>,
    stopped_by: Option<&CancellationToken>,
) -> Result<T, Exit> {
    let (status, outcome, failure) = match result {
        Ok(value) => (Status::Done, Ok(value), None),
        Err(err) => {
            let sibling_failed = stopped_by.is_some_and(CancellationToken::is_cancelled);
            let (status, outcome) = ending(&context.shutdown, &err, sibling_failed);
            // The error is logged with every ending (the spec's endings
            // table); only a failure shows it (a sensitive part withheld,
            // decision 8).
            let chain = shown_chain(err.as_ref());
            let screen = if status == Status::Failed {
                let colour = lock(&context.shared).colour;
                chain.iter().flat_map(|line| lines(line, colour)).collect()
            } else {
                Vec::new()
            };
            let failure = Failure {
                screen,
                record: chain.join(": "),
            };
            (status, outcome, Some(failure))
        }
    };
    guard.close(status, failure);
    outcome
}

/// Run a step under `parent` to its end (a top-level step, or a sub-step
/// one after another): the body borrows its `Step`. `prefix` is a
/// concurrent sibling's label, inherited by its sub-steps in plain mode.
async fn run_step<T>(
    context: &Context,
    parent: Option<usize>,
    labels: Labels,
    token: &CancellationToken,
    prefix: Option<String>,
    body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
) -> Result<T, Exit> {
    let Some((id, started)) = begin(context, parent, &labels, token, prefix.as_deref()) else {
        return Err(Exit(1));
    };
    // Ends the step if this future is dropped before the body returns.
    let guard = Ending::new(context, id, labels, started, prefix.clone());
    let mut step = Step {
        context: context.clone(),
        id,
        name: guard.labels.running.clone(),
        token: token.child_token(),
        prefix,
    };
    let result = body(&mut step).await;
    // The step's own token: cancelled, past the shutdown's checks, only by
    // an ancestor group's stop, so a sub-step inside a stopped sibling
    // reads "stopped" too, at any depth.
    conclude(context, guard, result, Some(&step.token))
}

/// A step's error: its lines for the screen (a failure's only; cleaned),
/// and the chain as one string for the step's record (logged as is; the
/// tracing subscriber escapes control characters).
struct Failure {
    screen: Vec<String>,
    record: String,
}

/// A started step's ending, written once: by [`Ending::close`] when the
/// body returned, or on drop when the step's future was dropped first
/// ("stopped": `select!`, a timeout) or a panic unwound through it (✗).
struct Ending {
    shared: Shared,
    id: usize,
    labels: Labels,
    started: Instant,
    open: bool,
    /// A concurrent sibling's label, in front of the plain end line of a
    /// sub-step inside it.
    prefix: Option<String>,
}

impl Ending {
    fn new(
        context: &Context,
        id: usize,
        labels: Labels,
        started: Instant,
        prefix: Option<String>,
    ) -> Self {
        Ending {
            shared: Arc::clone(&context.shared),
            id,
            labels,
            started,
            open: true,
            prefix,
        }
    }

    /// Write the ending; `failure` is a failed or cancelled body's error
    /// (none for a panic, which shows only its own message). After the
    /// run's end (a late close), only the tree and the log hear of it.
    fn close(&mut self, status: Status, failure: Option<Failure>) {
        self.open = false;
        {
            let mut state = lock(&self.shared);
            let screen = failure
                .as_ref()
                .map(|f| f.screen.clone())
                .unwrap_or_default();
            state.tree.set_error(self.id, screen);
            state.tree.finish(self.id, status, Instant::now());
            // Plain lines go through anstream, so they still print while a
            // panic unwinds; the live area doesn't (see `State::end`).
            if matches!(state.mode, Mode::Plain) && !state.ended {
                let colour = state.colour;
                let elapsed = self.started.elapsed();
                let line = end_line(theme::current(), &self.labels, status, elapsed, colour);
                plain_line(&prefixed(self.prefix.as_deref(), &line));
                // The failure block, plainly: the error, then the pointer.
                if let (Status::Failed, Some(failure)) = (status, &failure) {
                    for line in &failure.screen {
                        plain_line(&format!("  {line}"));
                    }
                    if let Some(pointer) = state.tree.pointer() {
                        plain_line(&format!("  full output: {pointer}"));
                    }
                }
            }
            if !std::thread::panicking() {
                state.redraw();
            }
        }
        // Logged outside the lock: the log write blocks on the file.
        let elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let step = self.labels.running.as_str();
        let outcome = outcome(status);
        match (status, failure) {
            (Status::Done, _) => tracing::info!(step, outcome, elapsed_ms, "step finished"),
            // The step's own record carries its error and causes, since the
            // run record then only says `exit 1`; the sensitive part is
            // already withheld (`shown_chain`).
            (_, Some(failure)) => {
                let error = failure.record;
                tracing::warn!(step, outcome, elapsed_ms, error, "step finished");
            }
            (_, None) => tracing::warn!(step, outcome, elapsed_ms, "step finished"),
        }
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        if self.open {
            let status = if std::thread::panicking() {
                Status::Failed
            } else {
                Status::Stopped
            };
            self.close(status, None);
        }
    }
}

/// How a failed body's step ends, and what comes back, in the spec's
/// order: interrupted, stopped by the command's own request, stopped by a
/// failing sibling, a quiet exit passed up, or failed. A signal and the
/// command's own request both cancel every step's token, so the shutdown
/// alone says which it was; a sibling's failure cancels only its group's.
fn ending<T>(
    shutdown: &Shutdown,
    err: &anyhow::Error,
    sibling_failed: bool,
) -> (Status, Result<T, Exit>) {
    if shutdown.interrupted() {
        (Status::Interrupted, Err(Exit(1)))
    } else if shutdown.requested().is_some() || sibling_failed {
        (Status::Stopped, Err(Exit(1)))
    } else if let Some(code) = quiet_exit(err.as_ref()) {
        (Status::SubStepFailed, Err(Exit(code)))
    } else {
        (Status::Failed, Err(Exit(1)))
    }
}

/// A step's outcome in the run log.
fn outcome(status: Status) -> &'static str {
    match status {
        Status::Running => "running",
        Status::Done => "done",
        Status::Failed => "failed",
        Status::Interrupted => "interrupted",
        Status::Stopped => "stopped",
        Status::SubStepFailed => "sub-step failed",
    }
}

/// A running step. Sequential bodies borrow it (`&mut`), so it can't
/// outlive their step; a concurrent sibling owns its own, and if it keeps
/// it past its end (a detached task), later lines are logged only and
/// `step`/`group` return `Exit(1)` without drawing.
pub struct Step {
    context: Context,
    id: usize,
    /// The running label, naming the step in the run log.
    name: String,
    token: CancellationToken,
    /// A concurrent sibling's label, in front of its lines (and its
    /// sub-steps' lines) in plain mode.
    prefix: Option<String>,
}

impl Step {
    /// Whether the step is still running (read from the tree, under the
    /// drawing lock, so it can't race the step's ending).
    fn is_open(&self) -> bool {
        lock(&self.context.shared).tree.is_running(self.id)
    }

    /// Run a sub-step (a deeper branch) to its end; this step's subtree
    /// stays live until this step ends (decision 7). See the spec's "Step
    /// endings".
    pub async fn step<T>(
        &mut self,
        labels: Labels,
        body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
    ) -> Result<T, Exit> {
        if !self.is_open() {
            tracing::info!(
                step = labels.running.as_str(),
                "step skipped (its parent ended)"
            );
            return Err(Exit(1));
        }
        let context = self.context.clone();
        let token = self.token.clone();
        let prefix = self.prefix.clone();
        run_step(&context, Some(self.id), labels, &token, prefix, body).await
    }

    /// Concurrent sub-steps: [`Group::spawn`] each, then [`Group::join`].
    pub fn group<T: Send + 'static>(&mut self) -> Group<'_, T> {
        self.new_group(true)
    }

    /// Concurrent sub-steps that all run to their end (config decision
    /// 3): [`GroupAll::spawn`] each, then [`GroupAll::join`].
    pub fn group_all<T: Send + 'static>(&mut self) -> GroupAll<'_, T> {
        GroupAll(self.new_group(false))
    }

    fn new_group<T: Send + 'static>(&mut self, stop_on_failure: bool) -> Group<'_, T> {
        Group {
            token: self.token.child_token(),
            parent: self,
            stop_on_failure,
            tasks: JoinSet::new(),
            slots: Vec::new(),
            indexes: HashMap::new(),
        }
    }

    /// The token this step watches: a child of `shutdown.token()`,
    /// cancelled by Ctrl-C, by the command's own `Shutdown::request`, or by
    /// a failing concurrent sibling (decision 7). A body stops early when
    /// it is cancelled and returns its error.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// An info notice (`›`, muted) under this step, kept in the tree.
    pub fn info(&mut self, text: impl Into<Line>) {
        self.notice(false, text.into());
    }

    /// A warning notice (orange `!`) under this step, kept in the tree.
    pub fn warn(&mut self, text: impl Into<Line>) {
        self.notice(true, text.into());
    }

    fn notice(&mut self, warn: bool, line: Line) {
        let (colour, terminal) = {
            let state = lock(&self.context.shared);
            (state.colour, state.terminal)
        };
        // Logged outside the lock: the log write blocks on the file.
        let step = self.name.as_str();
        // The log gets secret values replaced in place, always.
        for text in &lines(&line.masked(), colour) {
            let text = console::strip_ansi_codes(text);
            if warn {
                tracing::warn!(step, text = ?text, "notice");
            } else {
                tracing::info!(step, text = ?text, "notice");
            }
        }
        let mut state = lock(&self.context.shared);
        // A step kept past its end draws nothing more.
        if !state.tree.is_running(self.id) {
            return;
        }
        for text in line.on_screen(colour, terminal) {
            if matches!(state.mode, Mode::Plain) {
                let (role, mark) = if warn {
                    (Role::Warn, "!")
                } else {
                    (Role::Muted, "›")
                };
                let mark = paint(theme::current(), role, colour, mark);
                plain_line(&prefixed(self.prefix.as_deref(), &format!("{mark} {text}")));
            }
            state.tree.notice(self.id, warn, text);
        }
        state.redraw();
    }

    /// Output: shown in the live tail (or plainly) and logged (sensitive
    /// text: see [`Line`]). Text is
    /// split into lines (`\n` or `\r\n`); a `\r` keeps what follows it
    /// (progress output); only colour codes are kept, and only on a
    /// coloured target; tabs become spaces and other control characters
    /// are dropped; a line wider than the screen is cut with `…` on
    /// screen. The log gets each line as plain text, whole. The live area
    /// shows new lines on the next frame, so streamed output doesn't
    /// redraw the tree per line.
    pub fn output(&mut self, text: impl Into<Line>) {
        let line = text.into();
        let (colour, terminal) = {
            let state = lock(&self.context.shared);
            (state.colour, state.terminal)
        };
        // Logged outside the lock (the log write blocks on the file), as
        // plain text whatever the screen gets, and whole; secret values
        // replaced in place, always.
        let step = self.name.as_str();
        for text in &lines(&line.masked(), colour) {
            tracing::info!(step, text = ?console::strip_ansi_codes(text), "output");
        }
        let mut state = lock(&self.context.shared);
        // A step kept past its end draws nothing more.
        if !state.tree.is_running(self.id) {
            return;
        }
        for text in line.on_screen(colour, terminal) {
            if matches!(state.mode, Mode::Plain) {
                plain_line(&prefixed(self.prefix.as_deref(), &text));
            }
            state.tree.output(self.id, text);
        }
    }
}

/// Text as lines that draw predictably: std's `str::lines` (so `\r\n`
/// ends a line, and a final newline adds none), then only what follows the
/// last `\r` (progress output; a trailing one ignored). Empty text is one
/// empty line.
fn lines(text: &str, colour: bool) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    text.lines()
        .map(|line| {
            let line = line.trim_end_matches('\r');
            clean(line.rsplit('\r').next().unwrap_or(line), colour)
        })
        .collect()
}

/// One line with what would move the cursor or change widths behind the
/// tree's back taken out: escape codes other than colours (all of them with
/// colour off), and control characters; tabs become spaces to the next
/// 8-column stop. Escape codes found with console's `AnsiCodeIterator`
/// (https://docs.rs/console/0.16.6/console/struct.AnsiCodeIterator.html).
fn clean(line: &str, colour: bool) -> String {
    let mut out = String::new();
    let mut column = 0;
    for (piece, is_ansi) in console::AnsiCodeIterator::new(line) {
        if is_ansi {
            let sgr = piece.starts_with("\x1b[") && piece.ends_with('m');
            if colour && sgr {
                out.push_str(piece);
            }
            continue;
        }
        for c in piece.chars() {
            if c == '\t' {
                let spaces = 8 - column % 8;
                out.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            } else if !c.is_control() {
                out.push(c);
                column += console::measure_text_width(c.encode_utf8(&mut [0; 4]));
            }
        }
    }
    out
}

/// Concurrent sibling steps under one parent, drawn in full in start
/// order (decision 7). Each runs as a tokio task (`JoinSet`,
/// https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinSet.html), so bodies
/// own what they use and get their own `Step`; each runs in the run's span
/// (https://docs.rs/tracing/0.1.44/tracing/trait.Instrument.html#method.in_current_span).
/// The errgroup-style stop is a child `CancellationToken`
/// (https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html),
/// cancelled as soon as a sibling fails or panics ([`GroupAll`]: never by
/// its siblings). Borrows the parent, so it can't add lines or steps
/// meanwhile. Dropped without [`Group::join`], its open steps read
/// "stopped" at once (their tasks are
/// aborted too). Known limit: a sibling that panicked in a group dropped
/// without `join` reads "stopped", as tokio keeps the panic in the dropped
/// `JoinSet`.
#[must_use = "join the group; dropping it stops its steps"]
pub struct Group<'a, T> {
    parent: &'a mut Step,
    /// Cancelled when a sibling fails, in a stopping group (and by Ctrl-C
    /// or an ancestor's stop, through the parent's).
    token: CancellationToken,
    /// False for [`GroupAll`]: a failure or a panic cancels no sibling.
    stop_on_failure: bool,
    tasks: JoinSet<(usize, Result<T, Exit>)>,
    /// Each sibling's ending, until it is written: a task takes its own
    /// when its body returns; the group closes what is left when dropped,
    /// and a panicked sibling's when joined.
    slots: Vec<Slot>,
    /// Which sibling a task is, for a panic's `JoinError`.
    indexes: HashMap<tokio::task::Id, usize>,
}

type Slot = Arc<Mutex<Option<Ending>>>;

fn take(slot: &Slot) -> Option<Ending> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Cancels a group's token if a sibling's body panics, so the others stop
/// at once.
struct CancelOnPanic(CancellationToken);

impl Drop for CancelOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.cancel();
        }
    }
}

impl<T: Send + 'static> Group<'_, T> {
    /// Start a sibling now (its place in the tree is its start order).
    pub fn spawn<F, Fut>(&mut self, labels: Labels, body: F)
    where
        F: FnOnce(Step) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let index = self.slots.len();
        let context = self.parent.context.clone();
        let parent = self.parent.is_open().then_some(self.parent.id);
        let begun =
            parent.and_then(|parent| begin(&context, Some(parent), &labels, &self.token, None));
        // The ending exists from the start, so a sibling aborted before it
        // ever ran still ends.
        let ending = begun.map(|(id, started)| Ending::new(&context, id, labels, started, None));
        let step = ending.as_ref().map(|ending| Step {
            context: context.clone(),
            id: ending.id,
            name: ending.labels.running.clone(),
            token: self.token.child_token(),
            prefix: Some(ending.labels.running.clone()),
        });
        let slot: Slot = Arc::new(Mutex::new(ending));
        self.slots.push(Arc::clone(&slot));
        // The group's token: cancelled by an ancestor (its failure, Ctrl-C)
        // in either mode, so a body's error then reads "stopped"; only a
        // stopping group's failure cancels it too.
        let token = self.token.clone();
        let stop_on_failure = self.stop_on_failure;
        let handle = self.tasks.spawn(
            async move {
                let Some(step) = step else {
                    return (index, Err(Exit(1)));
                };
                let _cancel_on_panic = stop_on_failure.then(|| CancelOnPanic(token.clone()));
                let result = body(step).await;
                // Already closed: the group was dropped meanwhile.
                let Some(guard) = take(&slot) else {
                    return (index, Err(Exit(1)));
                };
                let outcome = conclude(&context, guard, result, Some(&token));
                // A failure stops the others at once (after its own ending,
                // so it doesn't read itself as stopped).
                if outcome.is_err() && stop_on_failure {
                    token.cancel();
                }
                (index, outcome)
            }
            .in_current_span(),
        );
        self.indexes.insert(handle.id(), index);
    }

    /// Wait for every sibling; results in spawn order (`join_next` mapped
    /// back by index; why not `JoinSet::join_all`: it returns completion
    /// order and panics on any `JoinError`; why not tokio-util's
    /// `JoinQueue`: joining in spawn order would see a later sibling's
    /// failure only after earlier ones finish). The first failure's `Exit`
    /// comes back once all have returned, as errgroup waits too. A
    /// panicking sibling is marked failed and the others are waited for,
    /// then the panic resumes (tokio's `JoinError::into_panic` with
    /// `std::panic::resume_unwind`, as tokio's docs advise).
    pub async fn join(mut self) -> Result<Vec<T>, Exit> {
        let (results, first) = self.wait().await;
        match first {
            Some(exit) => Err(exit),
            None => Ok(results.into_iter().flatten().flatten().collect()),
        }
    }

    /// Wait for every task: each sibling's result by spawn index (`None`
    /// for a task the runtime cancelled), and the first failure seen. A
    /// failure cancels the others in a stopping group. A panic resumes
    /// once all have returned.
    async fn wait(&mut self) -> (Vec<Option<Result<T, Exit>>>, Option<Exit>) {
        let mut results: Vec<Option<Result<T, Exit>>> =
            (0..self.slots.len()).map(|_| None).collect();
        let mut first = None;
        let mut panic = None;
        while let Some(joined) = self.tasks.join_next().await {
            match joined {
                Ok((index, Ok(value))) => results[index] = Some(Ok(value)),
                Ok((index, Err(exit))) => {
                    if self.stop_on_failure {
                        self.token.cancel();
                    }
                    // `Exit` isn't `Clone`: the same code, kept twice.
                    first.get_or_insert(Exit(exit.0));
                    results[index] = Some(Err(exit));
                }
                Err(err) if err.is_panic() => {
                    if self.stop_on_failure {
                        self.token.cancel();
                    }
                    if let Some(mut ending) = self
                        .indexes
                        .get(&err.id())
                        .and_then(|&i| take(&self.slots[i]))
                    {
                        ending.close(Status::Failed, None);
                    }
                    panic.get_or_insert(err.into_panic());
                }
                // A cancelled task (the runtime shutting down): no result.
                Err(_) => {
                    first.get_or_insert(Exit(1));
                }
            }
        }
        if let Some(panic) = panic {
            std::panic::resume_unwind(panic);
        }
        (results, first)
    }
}

/// Concurrent sibling steps that all run to their end (config decision
/// 3): a [`Group`] whose failures and panics cancel no sibling. Ctrl-C,
/// the command's own request and dropping it without
/// [`GroupAll::join`] act as on a [`Group`].
#[must_use = "join the group; dropping it stops its steps"]
pub struct GroupAll<'a, T>(Group<'a, T>);

impl<T: Send + 'static> GroupAll<'_, T> {
    /// Start a sibling now (its place in the tree is its start order).
    pub fn spawn<F, Fut>(&mut self, labels: Labels, body: F)
    where
        F: FnOnce(Step) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        self.0.spawn(labels, body);
    }

    /// Wait for every sibling; each one's result in spawn order (`Exit(1)`
    /// for one the runtime cancelled). A panicking sibling is marked
    /// failed and the others are waited for, then the panic resumes. The
    /// parent's body returning the first failure with `?` makes its step
    /// read "sub-step failed", as after [`Group::join`]. Joined in
    /// completion order, not with tokio-util's `JoinQueue` (spawn order):
    /// a panicked sibling's ending is written when it is joined, so it
    /// would read running until the earlier siblings finish.
    pub async fn join(mut self) -> Vec<Result<T, Exit>> {
        let (results, _) = self.0.wait().await;
        results
            .into_iter()
            .map(|result| result.unwrap_or(Err(Exit(1))))
            .collect()
    }
}

impl<T> Drop for Group<'_, T> {
    /// Close every sibling still open, at once ("stopped", or ✗ while a
    /// panic unwinds): their tasks are aborted with the `JoinSet`, but only
    /// on a later scheduler tick. The group's token is cancelled too, also
    /// after a successful `join`, so a `Step` kept past its end has a
    /// cancelled token.
    fn drop(&mut self) {
        self.token.cancel();
        // Each ending's own drop decides: "stopped", or ✗ while a panic
        // unwinds (the endings table's order).
        for slot in &self.slots {
            drop(take(slot));
        }
    }
}

/// A notice or output line: plain text, with secret values marked where
/// they sit (decision 8, amended): `Line::new("cloudflare.token: ")
/// .secret(token)`. A secret value shows as `[REDACTED]` in its place, on
/// screen and in the log; the rest of the line stays. From `&str`,
/// `String` and `&String`: all plain.
pub struct Line {
    parts: Vec<Part>,
    shown: bool,
}

/// A piece of a line: plain text, or a secret value.
struct Part {
    text: String,
    secret: bool,
}

impl Line {
    /// A line of plain text; add secret values with [`Line::secret`].
    pub fn new(text: impl Into<String>) -> Self {
        Line {
            parts: vec![Part {
                text: text.into(),
                secret: false,
            }],
            shown: false,
        }
    }

    /// A line that is a secret value as a whole (a credential printed on
    /// its own): `Line::new("").secret(value)`.
    pub fn sensitive(value: impl Into<String>) -> Self {
        Line::new("").secret(value)
    }

    /// Add a secret value at the end of the line.
    pub fn secret(mut self, value: impl Into<String>) -> Self {
        self.parts.push(Part {
            text: value.into(),
            secret: true,
        });
        self
    }

    /// Add plain text after a secret value.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.parts.push(Part {
            text: text.into(),
            secret: false,
        });
        self
    }

    /// Show the secret values to the command's user (a command whose job is
    /// to show them, such as creating a token): on a terminal only, since
    /// stderr anywhere else (a file, the journal, CI) is a log. Still never
    /// logged. No effect on a line without secrets.
    pub fn shown(self) -> Self {
        Line {
            shown: true,
            ..self
        }
    }

    /// The line with its secret values replaced in place.
    fn masked(&self) -> String {
        self.parts
            .iter()
            .map(|part| {
                if part.secret {
                    WITHHELD
                } else {
                    part.text.as_str()
                }
            })
            .collect()
    }

    /// The line as written.
    fn revealed(&self) -> String {
        self.parts.iter().map(|part| part.text.as_str()).collect()
    }

    /// The lines the screen gets.
    fn on_screen(&self, colour: bool, terminal: bool) -> Vec<String> {
        let text = if self.shown && terminal {
            self.revealed()
        } else {
            self.masked()
        };
        lines(&text, colour)
    }
}

impl From<&str> for Line {
    fn from(text: &str) -> Self {
        Line::new(text)
    }
}

impl From<String> for Line {
    fn from(text: String) -> Self {
        Line::new(text)
    }
}

impl From<&String> for Line {
    fn from(text: &String) -> Self {
        Line::new(text.clone())
    }
}

/// Move the spinner every [`FRAME`], skipping missed ticks
/// (https://docs.rs/tokio/1.54.1/tokio/time/enum.MissedTickBehavior.html).
/// `stop` is set by the restore hook on a second Ctrl-C, before it puts the
/// terminal back; the frame task then ends (and `State::redraw` draws
/// nothing more either).
async fn frames(shared: Shared, stop: Arc<AtomicBool>) {
    let mut tick = tokio::time::interval(FRAME);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let mut state = lock(&shared);
        if state.ended {
            return;
        }
        state.frame += 1;
        state.redraw();
    }
}

/// A plain-mode line on stderr (sb-go's plain mode when not interactive;
/// its verbose half is dropped, decision 9).
fn plain_line(line: &str) {
    // anstream strips the codes when stderr's choice is no colour.
    let _ = writeln!(anstream::stderr(), "{line}");
}

/// A plain-mode end line: `✓ <done label> 2s`, or ✗ with the label and
/// why.
fn end_line(
    theme: &theme::Theme,
    labels: &Labels,
    status: Status,
    elapsed: Duration,
    colour: bool,
) -> String {
    let secs = elapsed.as_secs();
    let (role, text) = match status {
        // A step is never still running when its end line is written.
        Status::Running | Status::Done => (Role::Ok, format!("✓ {} {secs}s", labels.done)),
        Status::Failed => (
            Role::Problem,
            format!(
                "✗ {} {secs}s",
                labels.failure.as_deref().unwrap_or(&labels.running)
            ),
        ),
        Status::Interrupted => (
            Role::Warn,
            format!("✗ {} interrupted {secs}s", labels.running),
        ),
        Status::Stopped => (Role::Warn, format!("✗ {} stopped {secs}s", labels.running)),
        Status::SubStepFailed => (
            Role::Warn,
            format!("✗ {} sub-step failed {secs}s", labels.running),
        ),
    };
    paint(theme, role, colour, &text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicatif::InMemoryTerm;

    fn screen() -> (InMemoryTerm, Options) {
        let term = InMemoryTerm::new(20, 100);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        (term, options)
    }

    /// Two steps run one after another; at the end the screen holds the
    /// finished tree and nothing of the live area.
    #[tokio::test]
    async fn a_run_ends_as_the_finished_tree() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let value = run
            .step(
                Labels::new("Reading accounts.yml", "accounts.yml read"),
                async |step| {
                    step.info("3 accounts");
                    Ok(3)
                },
            )
            .await;
        assert_eq!(value.ok(), Some(3));
        // The ending is drawn as soon as the body returns.
        assert!(
            term.contents().contains("✓ accounts.yml read"),
            "{}",
            term.contents()
        );
        run.step(
            Labels::new("Checking settings.yml", "settings.yml checked"),
            async |step| {
                step.warn("unknown key: downloads.extra");
                step.output("checking item 1 of 2");
                Ok(())
            },
        )
        .await
        .expect("step");
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ├── ✓ accounts.yml read                                       0s
        │   › 3 accounts
        ╰── ✓ settings.yml checked                                    0s
            ! unknown key: downloads.extra
        ");
    }

    /// While a step runs, the live area shows it with its spinner, and its
    /// output from the next frame on.
    #[tokio::test(start_paused = true)]
    async fn a_running_step_is_live() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let seen = run
            .step(
                Labels::new("Pulling images", "images pulled"),
                async |step| {
                    step.output("pulling sonarr");
                    tokio::time::sleep(FRAME).await;
                    Ok(term.contents())
                },
            )
            .await
            .expect("step");
        insta::assert_snapshot!(seen, @r"
        sb demo
        ├── ⠙ Pulling images                                          0s
        │   pulling sonarr
        ");
        drop(run);
        assert!(
            term.contents()
                .ends_with("╰── ✓ images pulled                                           0s")
        );
    }

    /// A body's error fails the step: ✗ with its failure label, and the
    /// quiet `Exit(1)` comes back, so the command's `?` ends the run.
    #[tokio::test]
    async fn a_failing_body_fails_the_step_quietly() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let result: Result<(), Exit> = run
            .step(
                Labels::new("Pulling", "pulled").failure("pull failed"),
                async |_| anyhow::bail!("timeout"),
            )
            .await;
        assert!(matches!(result, Err(Exit(1))));
        run.finish();
        assert!(
            term.contents().contains("╰── ✗ pull failed"),
            "{}",
            term.contents()
        );
    }

    /// An error that is already a quiet exit passes up unchanged, and the
    /// step reads "sub-step failed".
    #[tokio::test]
    async fn a_quiet_exit_passes_up_unchanged() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let result: Result<(), Exit> = run
            .step(Labels::new("Checking", "checked"), async |_| {
                Err(Exit(7).into())
            })
            .await;
        assert!(matches!(result, Err(Exit(7))));
        run.finish();
        assert!(
            term.contents().contains("sub-step failed · 0s"),
            "{}",
            term.contents()
        );
    }

    /// Ctrl-C during a step: the step reads "interrupted"; after the
    /// command's own request it reads "stopped".
    #[tokio::test]
    async fn cancelled_steps_say_why() {
        for (signal, why) in [(true, "interrupted · 0s"), (false, "stopped · 0s")] {
            let (term, options) = screen();
            let shutdown = Shutdown::new();
            let mut run = Runner::start("sb demo", &shutdown, None, options).await;
            let result: Result<(), Exit> = run
                .step(Labels::new("Waiting", "waited"), async |step| {
                    if signal {
                        shutdown.signalled(2);
                    } else {
                        shutdown.request(0);
                    }
                    assert!(step.token().is_cancelled());
                    anyhow::bail!("cancelled")
                })
                .await;
            assert!(matches!(result, Err(Exit(1))));
            run.finish();
            assert!(term.contents().contains(why), "{}", term.contents());
        }
    }

    /// Once the run is cancelled, a step isn't drawn and its body doesn't
    /// run.
    #[tokio::test]
    async fn no_step_starts_after_a_cancel() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        shutdown.request(0);
        let mut ran = false;
        let result = run
            .step(Labels::new("Late", "late"), async |_| {
                ran = true;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(Exit(1))) && !ran);
        run.finish();
        assert_eq!(term.contents(), "sb demo");
    }

    /// A step whose future is dropped (a timeout) reads "stopped", and the
    /// run goes on: the next step and the end still draw.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_step_reads_stopped() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let slow = run.step(Labels::new("Slow", "slow done"), async |_| {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), slow)
                .await
                .is_err()
        );
        run.step(Labels::new("Next", "next done"), async |_| Ok(()))
            .await
            .expect("step");
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ├── ✗ Slow                                          stopped · 0s
        ╰── ✓ next done                                               0s
        ");
    }

    /// A panicking body unwinds through the runner without a second panic
    /// (which would abort): the panic arrives as itself.
    #[tokio::test]
    async fn a_panicking_body_unwinds_cleanly() {
        let (_term, options) = screen();
        let local = tokio::task::LocalSet::new();
        let joined = local
            .run_until(async move {
                tokio::task::spawn_local(async move {
                    let shutdown = Shutdown::new();
                    // (A panic shows no failure block: see the plain test.)
                    let mut run = Runner::start("sb demo", &shutdown, None, options).await;
                    let _ = run
                        .step(
                            Labels::new("Boom", "boom"),
                            async |_| -> anyhow::Result<()> { panic!("a broken body") },
                        )
                        .await;
                })
                .await
            })
            .await;
        assert!(joined.is_err_and(|e| e.is_panic()));
    }

    /// The frame task moves the spinner while a step runs.
    #[tokio::test(start_paused = true)]
    async fn the_spinner_moves() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let (before, after) = run
            .step(Labels::new("Waiting", "waited"), async |_| {
                let before = term.contents();
                tokio::time::sleep(FRAME * 2).await;
                Ok((before, term.contents()))
            })
            .await
            .expect("step");
        let glyph = |screen: &str| screen.lines().nth(1).and_then(|l| l.chars().nth(4));
        assert_ne!(glyph(&before), glyph(&after), "{before}\n{after}");
        run.finish();
    }

    /// The live area never shrinks while running: a long tail followed by a
    /// short step leaves no old line on screen (blank lines are drawn over
    /// it), and an empty output line is drawn as a blank one.
    #[tokio::test(start_paused = true)]
    async fn the_live_area_never_shrinks() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        run.step(Labels::new("Long", "long done"), async |step| {
            for n in 0..6 {
                step.output(format!("tail line {n}"));
            }
            Ok(())
        })
        .await
        .expect("step");
        let during = run
            .step(Labels::new("Short", "short done"), async |step| {
                step.output("");
                tokio::time::sleep(FRAME).await;
                Ok(term.contents())
            })
            .await
            .expect("step");
        assert!(!during.contains("tail line"), "{during}");
        insta::assert_snapshot!(during, @r"
        sb demo
        ├── ✓ long done                                               0s
        ├── ⠙ Short                                                   0s
        │
        ");
        run.finish();
    }

    /// Every colour code a plain end line writes is of the profile's kind;
    /// `ascii` writes none.
    #[test]
    fn plain_end_lines_follow_the_profile() {
        use crate::style::sgr::{colour_codes, of_kind};
        use theme::{Palette, Profile, Theme};
        let labels = Labels::new("Pulling", "pulled");
        for profile in [
            Profile::TrueColor,
            Profile::Ansi256,
            Profile::Ansi,
            Profile::Ascii,
        ] {
            let theme = Theme::resolve(&Palette::default(), profile);
            let text: String = [Status::Done, Status::Failed, Status::Stopped]
                .into_iter()
                .map(|s| end_line(&theme, &labels, s, Duration::ZERO, true))
                .collect();
            let codes = colour_codes(&text);
            match profile {
                Profile::Ascii => assert!(!text.contains('\x1b'), "{text:?}"),
                _ => assert!(!codes.is_empty() && codes.iter().all(|c| of_kind(c, profile))),
            }
        }
    }

    fn plain_theme() -> theme::Theme {
        theme::Theme::resolve(&theme::Palette::default(), theme::Profile::Ascii)
    }

    /// Output text becomes lines: split at `\n`, only what follows the
    /// last `\r` kept (progress output), escape codes stripped with colour
    /// off.
    #[test]
    fn output_text_becomes_lines() {
        assert_eq!(lines("a\nb", true), ["a", "b"]);
        assert_eq!(lines("a\r\nb\r\n", true), ["a", "b"]);
        assert_eq!(lines("a\n", true), ["a"]);
        assert_eq!(lines("10%\r50%\r100%", true), ["100%"]);
        assert_eq!(lines("10%\r50%\r", true), ["50%"]);
        assert_eq!(lines("\x1b[32mok\x1b[0m", false), ["ok"]);
        assert_eq!(lines("\x1b[32mok\x1b[0m", true), ["\x1b[32mok\x1b[0m"]);
        assert_eq!(lines("", true), [""]);
        // Tabs to the next 8-column stop; other controls and escapes that
        // move the cursor are dropped, so widths are what the screen shows.
        assert_eq!(lines("ab\tc", true), ["ab      c"]);
        assert_eq!(lines("a\tb\tc", true), ["a       b       c"]);
        assert_eq!(
            lines("\x1b[31mab\x1b[0m\tc", true),
            ["\x1b[31mab\x1b[0m      c"]
        );
        assert_eq!(lines("a\x07b\x1b[K\x1b[1A", true), ["ab"]);
    }

    /// Output reaches the live area on the next frame, not per line.
    #[tokio::test(start_paused = true)]
    async fn output_waits_for_the_next_frame() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let (before, after) = run
            .step(Labels::new("Pulling", "pulled"), async |step| {
                tokio::task::yield_now().await;
                step.output("layer 1");
                let before = term.contents();
                // Past the next tick (a tick sharing this sleep's deadline may
                // run after it).
                tokio::time::sleep(FRAME * 2).await;
                Ok((before, term.contents()))
            })
            .await
            .expect("step");
        assert!(
            !before.contains("layer 1") && after.contains("layer 1"),
            "{before}\n{after}"
        );
        run.finish();
    }

    /// Secret values are replaced in place on screen too (maintainer,
    /// decision 8, amended), unless the command opts in with `shown` (the
    /// in-memory screen counts as a terminal).
    #[tokio::test(start_paused = true)]
    async fn sensitive_text_is_withheld_on_screen_unless_shown() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let live = run
            .step(Labels::new("Checking", "checked"), async |step| {
                step.warn(
                    Line::new("bad token ")
                        .secret("hidden-secret-1")
                        .text(" in settings.yml"),
                );
                step.info(Line::new("password: ").secret("shown-to-the-user").shown());
                step.output(Line::sensitive("hidden-secret-2"));
                step.output(Line::sensitive("shown-output").shown());
                // Past the next frame, so the output tail is on screen.
                tokio::time::sleep(FRAME * 2).await;
                Ok(term.contents())
            })
            .await
            .expect("step");
        run.finish();
        assert!(!live.contains("hidden-secret"), "{live}");
        // Only the value is replaced, in its place.
        assert!(
            live.contains("! bad token [REDACTED] in settings.yml"),
            "{live}"
        );
        assert!(live.contains("│   [REDACTED]"), "{live}");
        assert!(live.contains("› password: shown-to-the-user"), "{live}");
        assert!(live.contains("│   shown-output"), "{live}");
        assert!(!term.contents().contains("hidden-secret"));
    }

    /// Off a terminal, shown sensitive text is withheld too: stderr there
    /// is a log. `shown` on a plain line changes nothing.
    #[test]
    fn shown_text_needs_a_terminal() {
        let shown = Line::sensitive("s").shown();
        assert_eq!(shown.on_screen(false, true), ["s"]);
        assert_eq!(shown.on_screen(false, false), [WITHHELD]);
        assert_eq!(Line::from("p").shown().on_screen(false, false), ["p"]);
        let owned = String::from("o");
        assert_eq!(Line::from(&owned).on_screen(false, false), ["o"]);
        assert_eq!(Line::from(owned).on_screen(false, false), ["o"]);
        let mixed = Line::new("token: ").secret("s").text(" (24 characters)");
        assert_eq!(
            mixed.on_screen(false, false),
            ["token: [REDACTED] (24 characters)"]
        );
    }

    /// A failure whose cause is sensitive shows the placeholder, never the
    /// secret (decision 8), even behind anyhow's context.
    #[tokio::test]
    async fn a_sensitive_cause_is_withheld_in_the_failure_block() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let _ = run
            .step(
                Labels::new("Checking", "checked"),
                async |_| -> anyhow::Result<()> {
                    let secret =
                        sb_cli_core::log::Sensitive::new(std::io::Error::other("token=hunter2"));
                    Err(anyhow::Error::new(secret).context("the token was refused"))
                },
            )
            .await;
        run.finish();
        let screen = term.contents();
        assert!(!screen.contains("hunter2"), "{screen}");
        assert!(
            screen.contains("    the token was refused\n    [REDACTED]"),
            "{screen}"
        );
    }

    /// A sub-step is a deeper branch; its parent ends after it.
    #[tokio::test]
    async fn sub_steps_nest_under_their_parent() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        run.step(Labels::new("Checking", "checked"), async |step| {
            step.step(Labels::new("Reading", "read"), async |_| Ok(()))
                .await?;
            step.step(Labels::new("Parsing", "parsed"), async |_| Ok(()))
                .await?;
            Ok(())
        })
        .await
        .expect("step");
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ╰── ✓ checked                                                 0s
            ├── ✓ read                                                0s
            ╰── ✓ parsed                                              0s
        ");
    }

    /// Concurrent siblings run in a group; their results come back in
    /// spawn order, whatever order they finish in.
    #[tokio::test(start_paused = true)]
    async fn a_group_returns_results_in_spawn_order() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let results = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group();
                group.spawn(Labels::new("Slow", "slow done"), |_| async {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(1)
                });
                group.spawn(Labels::new("Fast", "fast done"), |_| async { Ok(2) });
                Ok(group.join().await?)
            })
            .await
            .expect("step");
        assert_eq!(results, [1, 2]);
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ╰── ✓ checked                                                 0s
            ├── ✓ slow done                                           0s
            ╰── ✓ fast done                                           0s
        ");
    }

    /// A failing sibling stops the others ("stopped"), its parent reads
    /// "sub-step failed", and its `Exit` comes back (decision 7).
    #[tokio::test(start_paused = true)]
    async fn a_failing_sibling_stops_the_others() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let result: Result<(), Exit> = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group();
                group.spawn(Labels::new("Waiting", "waited"), |step| async move {
                    let token = step.token();
                    match token
                        .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                        .await
                    {
                        Some(()) => Ok(()),
                        None => anyhow::bail!("cancelled"),
                    }
                });
                group.spawn(
                    Labels::new("Failing", "never").failure("failed"),
                    |_| async {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        anyhow::bail!("refused")
                    },
                );
                group.join().await?;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(Exit(1))));
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ╰── ✗ Checking                              sub-step failed · 0s
            ├── ✗ Waiting                                   stopped · 0s
            ╰── ✗ failed                                              0s
                refused
        ");
    }

    /// Ctrl-C with two siblings running marks both and their parent
    /// interrupted (decision 7).
    #[tokio::test(start_paused = true)]
    async fn ctrl_c_interrupts_every_running_step() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let signal = shutdown.clone();
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group::<()>();
                for name in ["A", "B"] {
                    group.spawn(Labels::new(name, "done"), |step| async move {
                        let token = step.token();
                        match token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                            .await
                        {
                            Some(()) => Ok(()),
                            None => anyhow::bail!("cancelled"),
                        }
                    });
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                signal.signalled(2);
                group.join().await?;
                Ok(())
            })
            .await;
        run.finish();
        let screen = term.contents();
        // (Times read 0s: tokio's paused clock doesn't move std's Instant.)
        assert_eq!(screen.matches("interrupted · 0s").count(), 3, "{screen}");
    }

    /// A group dropped without `join` (a `?` before it) stops its steps at
    /// once, even one never run, and nothing is drawn after the run ends.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_group_stops_its_steps_at_once() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let _ = run
            .step(
                Labels::new("Checking", "checked"),
                async |step| -> anyhow::Result<()> {
                    let mut group = step.group::<()>();
                    group.spawn(Labels::new("Waiting", "waited"), |_| async {
                        tokio::time::sleep(Duration::from_secs(9)).await;
                        Ok(())
                    });
                    drop(group);
                    anyhow::bail!("gave up")
                },
            )
            .await;
        run.finish();
        let at_end = term.contents();
        insta::assert_snapshot!(at_end, @r"
        sb demo
        ╰── ✗ Checking                                                0s
            gave up
            ╰── ✗ Waiting                                   stopped · 0s
        ");
        // The aborted task's late drop draws nothing more.
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert_eq!(term.contents(), at_end);
    }

    /// A failing sibling stops the others at once, not when `join` is
    /// finally polled (errgroup's stop).
    #[tokio::test(start_paused = true)]
    async fn a_failure_stops_its_siblings_at_once() {
        let (_term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let start = tokio::time::Instant::now();
        let stopped_after = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&stopped_after);
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group::<()>();
                group.spawn(Labels::new("Waiting", "waited"), move |step| async move {
                    step.token().cancelled().await;
                    *seen.lock().expect("lock") = Some(start.elapsed());
                    anyhow::bail!("stopped")
                });
                group.spawn(Labels::new("Failing", "never"), |_| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    anyhow::bail!("refused")
                });
                // The parent is busy before it joins.
                tokio::time::sleep(Duration::from_secs(5)).await;
                group.join().await?;
                Ok(())
            })
            .await;
        run.finish();
        let after = stopped_after.lock().expect("lock").expect("stopped");
        assert!(after < Duration::from_secs(2), "stopped after {after:?}");
    }

    /// A sibling that keeps its `Step` past its end (a detached task) draws
    /// nothing more, and its sub-steps and groups don't start: tried while
    /// another sibling still runs, so the group's token isn't cancelled
    /// yet and only the step's own end decides.
    #[tokio::test(start_paused = true)]
    async fn a_detached_step_draws_nothing() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let observed = term.clone();
        run.step(Labels::new("Checking", "checked"), async |step| {
            let (kept_tx, kept_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let mut group = step.group();
            group.spawn(Labels::new("Leaking", "leaked"), move |step| async move {
                let _ = kept_tx.send(step);
                Ok(())
            });
            group.spawn(Labels::new("Holding", "held"), move |_| async move {
                let _ = release_rx.await;
                Ok(())
            });
            let mut kept = kept_rx.await.expect("the kept step");
            tokio::time::sleep(FRAME * 2).await;
            let before = observed.contents();
            kept.output("too late");
            kept.warn("too late");
            let late = kept
                .step(Labels::new("Late", "late"), async |_| Ok(()))
                .await;
            assert!(matches!(late, Err(Exit(1))));
            let mut inner = kept.group::<()>();
            inner.spawn(Labels::new("Late", "late"), |_| async { Ok(()) });
            assert!(matches!(inner.join().await, Err(Exit(1))));
            tokio::time::sleep(FRAME * 2).await;
            // The same screen, the spinner's frame aside.
            let still = |screen: String| -> String {
                screen
                    .chars()
                    .filter(|c| !('\u{2800}'..='\u{28FF}').contains(c))
                    .collect()
            };
            assert_eq!(still(observed.contents()), still(before));
            let _ = release_tx.send(());
            group.join().await?;
            Ok(())
        })
        .await
        .expect("step");
        run.finish();
        assert!(!term.contents().contains("too late") && !term.contents().contains("Late"));
    }

    /// A panicking sibling stops the others at once too, not when `join`
    /// is polled.
    #[tokio::test(start_paused = true)]
    async fn a_panic_stops_its_siblings_at_once() {
        let (_term, options) = screen();
        let start = tokio::time::Instant::now();
        let stopped_after = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&stopped_after);
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let joined = tokio::spawn(async move {
            let _ = run
                .step(Labels::new("Checking", "checked"), async |step| {
                    let mut group = step.group::<()>();
                    group.spawn(Labels::new("Waiting", "waited"), move |step| async move {
                        step.token().cancelled().await;
                        *seen.lock().expect("lock") = Some(start.elapsed());
                        anyhow::bail!("stopped")
                    });
                    group.spawn(Labels::new("Boom", "boom"), |_| async {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        panic!("a broken sibling")
                    });
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    group.join().await?;
                    Ok(())
                })
                .await;
        })
        .await;
        assert!(joined.is_err_and(|e| e.is_panic()));
        let after = stopped_after.lock().expect("lock").expect("stopped");
        assert!(after < Duration::from_secs(2), "stopped after {after:?}");
    }

    /// A parent that panics with a group not joined: its open siblings are
    /// dropped while the panic unwinds, and read ✗ (the endings table puts
    /// that row first).
    #[tokio::test(start_paused = true)]
    async fn siblings_dropped_by_a_panic_read_failed() {
        let (_term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let shared = Arc::clone(&run.shared);
        let joined = tokio::spawn(async move {
            let _ = run
                .step(
                    Labels::new("Checking", "checked"),
                    async |step| -> anyhow::Result<()> {
                        let mut group = step.group::<()>();
                        group.spawn(Labels::new("Waiting", "waited"), |_| async {
                            tokio::time::sleep(Duration::from_secs(9)).await;
                            Ok(())
                        });
                        panic!("the parent broke")
                    },
                )
                .await;
        })
        .await;
        assert!(joined.is_err_and(|e| e.is_panic()));
        assert_eq!(
            lock(&shared).tree.statuses(),
            [Status::Failed, Status::Failed]
        );
    }

    /// A sub-step inside a stopped sibling reads "stopped" too: one failure
    /// block, the failing sibling's.
    #[tokio::test(start_paused = true)]
    async fn a_sub_step_of_a_stopped_sibling_reads_stopped() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group::<()>();
                group.spawn(Labels::new("Outer", "outer done"), |mut step| async move {
                    let inner = Labels::new("Inner", "inner done");
                    step.step(inner, async |step| {
                        step.token().cancelled().await;
                        anyhow::bail!("cancelled")
                    })
                    .await?;
                    Ok(())
                });
                group.spawn(Labels::new("Failing", "never"), |_| async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    anyhow::bail!("refused")
                });
                group.join().await?;
                Ok(())
            })
            .await;
        run.finish();
        let screen = term.contents();
        assert!(
            screen.contains("✗ Inner") && !screen.contains("cancelled"),
            "{screen}"
        );
        assert_eq!(screen.matches("stopped · 0s").count(), 2, "{screen}");
        assert_eq!(screen.matches("refused").count(), 1, "{screen}");
    }

    /// A failed sibling's kept `Step` adds no line to its drawn tail.
    #[tokio::test(start_paused = true)]
    async fn a_failed_siblings_kept_step_adds_no_lines() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let (kept_tx, kept_rx) = tokio::sync::oneshot::channel();
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group::<()>();
                group.spawn(
                    Labels::new("Leaking", "leaked"),
                    move |mut step| async move {
                        step.output("a last line");
                        let _ = kept_tx.send(step);
                        anyhow::bail!("refused")
                    },
                );
                group.join().await?;
                Ok(())
            })
            .await;
        let mut kept = kept_rx.await.expect("the kept step");
        kept.output("too late");
        run.finish();
        let screen = term.contents();
        assert!(
            screen.contains("a last line") && !screen.contains("too late"),
            "{screen}"
        );
    }

    /// The command's own request during a group: siblings and parent read
    /// "stopped", not interrupted.
    #[tokio::test(start_paused = true)]
    async fn a_request_stops_a_group() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let request = shutdown.clone();
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group::<()>();
                group.spawn(Labels::new("Waiting", "waited"), |step| async move {
                    step.token().cancelled().await;
                    anyhow::bail!("stopped")
                });
                tokio::task::yield_now().await;
                request.request(0);
                group.join().await?;
                Ok(())
            })
            .await;
        run.finish();
        let screen = term.contents();
        assert_eq!(screen.matches("stopped · 0s").count(), 2, "{screen}");
        assert!(!screen.contains("interrupted"), "{screen}");
    }

    /// A panicking sibling: the others are stopped, then the panic goes on.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_sibling_resumes_its_panic() {
        let (_term, options) = screen();
        let waited = Arc::new(Mutex::new(false));
        let ended = Arc::clone(&waited);
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        // Read the tree itself: the in-memory screen drops draws closer
        // than its refresh rate allows, and nothing forces one after a panic.
        let shared = Arc::clone(&run.shared);
        let joined = tokio::spawn(async move {
            let _ = run
                .step(Labels::new("Checking", "checked"), async |step| {
                    let mut group = step.group::<()>();
                    group.spawn(Labels::new("Waiting", "waited"), move |step| async move {
                        step.token().cancelled().await;
                        *ended.lock().expect("lock") = true;
                        anyhow::bail!("stopped")
                    });
                    group.spawn(Labels::new("Boom", "boom"), |_| async {
                        tokio::task::yield_now().await;
                        panic!("a broken sibling")
                    });
                    group.join().await?;
                    Ok(())
                })
                .await;
        })
        .await;
        let panic = joined.expect_err("the panic").into_panic();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"a broken sibling"));
        // The other was stopped (at once) and waited for; the panicking one
        // and their parent read ✗.
        assert!(
            *waited.lock().expect("lock"),
            "the other sibling wasn't waited for"
        );
        let statuses = lock(&shared).tree.statuses();
        assert_eq!(statuses, [Status::Failed, Status::Stopped, Status::Failed]);
    }

    /// A group that lets every sibling finish (config decision 3): a
    /// failure stops no one, every ending is drawn, and every result comes
    /// back in spawn order; the parent reads "sub-step failed".
    #[tokio::test(start_paused = true)]
    async fn a_group_all_lets_every_sibling_finish() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let mut seen = Vec::new();
        let result: Result<(), Exit> = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group_all();
                group.spawn(
                    Labels::new("Failing", "never").failure("failed"),
                    |_| async { anyhow::bail!("refused") },
                );
                group.spawn(Labels::new("Waiting", "waited"), |step| async move {
                    let token = step.token();
                    match token
                        .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                        .await
                    {
                        Some(()) => Ok(2),
                        None => anyhow::bail!("cancelled"),
                    }
                });
                group.spawn(Labels::new("Quick", "quick"), |_| async { Ok(3) });
                let results = group.join().await;
                seen = results.iter().map(|r| r.as_ref().ok().copied()).collect();
                for result in results {
                    result?;
                }
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(Exit(1))));
        assert_eq!(seen, [None, Some(2), Some(3)]);
        run.finish();
        insta::assert_snapshot!(term.contents(), @r"
        sb demo
        ╰── ✗ Checking                              sub-step failed · 0s
            ├── ✗ failed                                              0s
            │   refused
            ├── ✓ waited                                              0s
            ╰── ✓ quick                                               0s
        ");
    }

    /// A group that lets every sibling finish, inside a stopping group's
    /// sibling: a failure in the outer group stops its steps, which read
    /// "stopped" (the endings table, at any depth).
    #[tokio::test(start_paused = true)]
    async fn a_group_all_under_a_failed_sibling_reads_stopped() {
        let (_term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let shared = Arc::clone(&run.shared);
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut outer = step.group::<()>();
                outer.spawn(Labels::new("Inner", "inner done"), |mut step| async move {
                    let mut inner = step.group_all::<()>();
                    inner.spawn(Labels::new("Leaf", "leaf done"), |step| async move {
                        let token = step.token();
                        match token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                            .await
                        {
                            Some(()) => Ok(()),
                            None => anyhow::bail!("cancelled"),
                        }
                    });
                    for result in inner.join().await {
                        result?;
                    }
                    Ok(())
                });
                outer.spawn(
                    Labels::new("Bad", "never").failure("bad failed"),
                    |_| async {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        anyhow::bail!("refused")
                    },
                );
                outer.join().await?;
                Ok(())
            })
            .await;
        run.finish();
        // The tree itself, in creation order (Checking, Inner, Bad, then
        // Leaf, created by Inner's task): the screen draws a nested
        // sibling's line out of tree order (carried in tasks/todo.md, as
        // with `Group`).
        let statuses = lock(&shared).tree.statuses();
        assert_eq!(
            statuses,
            [
                Status::SubStepFailed,
                Status::Stopped,
                Status::Failed,
                Status::Stopped
            ]
        );
    }

    /// In a group that lets every sibling finish, a panic stops no one:
    /// the others finish, then the panic resumes (the endings table).
    #[tokio::test(start_paused = true)]
    async fn a_group_all_panic_waits_for_the_others() {
        let (_term, options) = screen();
        let finished = Arc::new(Mutex::new(false));
        let ended = Arc::clone(&finished);
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let shared = Arc::clone(&run.shared);
        let joined = tokio::spawn(async move {
            let _ = run
                .step(Labels::new("Checking", "checked"), async |step| {
                    let mut group = step.group_all::<()>();
                    group.spawn(Labels::new("Waiting", "waited"), move |step| async move {
                        let token = step.token();
                        match token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                            .await
                        {
                            Some(()) => {
                                *ended.lock().expect("lock") = true;
                                Ok(())
                            }
                            None => anyhow::bail!("cancelled"),
                        }
                    });
                    group.spawn(Labels::new("Boom", "boom"), |_| async {
                        tokio::task::yield_now().await;
                        panic!("a broken sibling")
                    });
                    for result in group.join().await {
                        result?;
                    }
                    Ok(())
                })
                .await;
        })
        .await;
        let panic = joined.expect_err("the panic").into_panic();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"a broken sibling"));
        assert!(
            *finished.lock().expect("lock"),
            "the other sibling was stopped"
        );
        let statuses = lock(&shared).tree.statuses();
        assert_eq!(statuses, [Status::Failed, Status::Done, Status::Failed]);
    }

    /// Ctrl-C still interrupts every sibling of a group that lets them
    /// all finish, and their parent.
    #[tokio::test(start_paused = true)]
    async fn ctrl_c_interrupts_a_group_all() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let signal = shutdown.clone();
        let _ = run
            .step(Labels::new("Checking", "checked"), async |step| {
                let mut group = step.group_all::<()>();
                for name in ["A", "B"] {
                    group.spawn(Labels::new(name, "done"), |step| async move {
                        let token = step.token();
                        match token
                            .run_until_cancelled(tokio::time::sleep(Duration::from_secs(9)))
                            .await
                        {
                            Some(()) => Ok(()),
                            None => anyhow::bail!("cancelled"),
                        }
                    });
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                signal.signalled(2);
                for result in group.join().await {
                    result?;
                }
                Ok(())
            })
            .await;
        run.finish();
        let screen = term.contents();
        assert_eq!(screen.matches("interrupted · 0s").count(), 3, "{screen}");
    }

    /// Each ending's word in the run log.
    #[test]
    fn outcomes_name_every_ending() {
        let names = [
            (Status::Done, "done"),
            (Status::Failed, "failed"),
            (Status::Interrupted, "interrupted"),
            (Status::Stopped, "stopped"),
            (Status::SubStepFailed, "sub-step failed"),
            (Status::Running, "running"),
        ];
        for (status, name) in names {
            assert_eq!(outcome(status), name);
        }
    }

    /// Plain end lines: ✓ with the done label and time, ✗ with why.
    #[test]
    fn plain_end_lines_say_how_a_step_ended() {
        let labels = Labels::new("Pulling", "pulled").failure("pull failed");
        let two = Duration::from_secs(2);
        assert_eq!(
            end_line(&plain_theme(), &labels, Status::Done, two, false),
            "✓ pulled 2s"
        );
        assert_eq!(
            end_line(&plain_theme(), &labels, Status::Failed, two, false),
            "✗ pull failed 2s"
        );
        assert_eq!(
            end_line(&plain_theme(), &labels, Status::Interrupted, two, false),
            "✗ Pulling interrupted 2s"
        );
        assert_eq!(
            end_line(&plain_theme(), &labels, Status::Stopped, two, false),
            "✗ Pulling stopped 2s"
        );
        assert_eq!(
            end_line(&plain_theme(), &labels, Status::SubStepFailed, two, false),
            "✗ Pulling sub-step failed 2s"
        );
    }
}
