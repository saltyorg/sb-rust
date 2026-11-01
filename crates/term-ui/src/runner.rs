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

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use sb_cli_core::cancel::{CancellationToken, Shutdown};
use sb_cli_core::exit::{Exit, quiet_exit};
use sb_cli_core::log::RunLog;
use sb_cli_core::theme::{self, Role};

use crate::style::paint;
use crate::tree::{Draw, Status, Tree};

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
    /// The command's `-v` (sb has none yet): plain output even on a
    /// terminal.
    pub verbose: bool,
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
    tree: Tree,
    mode: Mode,
    colour: bool,
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
        self.ended = true;
        self.tree.end();
        if std::thread::panicking() {
            return;
        }
        self.redraw();
        if let Mode::Live(area) = &self.mode {
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
/// Live on a terminal; plain when the target isn't one or the command
/// asks for verbose output.
pub struct Runner {
    shared: Shared,
    shutdown: Shutdown,
    frames: Option<tokio::task::JoinHandle<()>>,
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
        // The failure block's pointer comes with Task 7.
        let _ = log;
        // The title goes out before the live area exists: a
        // `MultiProgress::println` before its bar first drew was lost
        // (seen on indicatif's in-memory terminal).
        let (mode, colour, size) = match options.target {
            Target::Stderr => {
                plain_line(title);
                // indicatif hides its stderr target when stderr isn't a
                // terminal or `TERM` is dumb or unset (draw_target.rs,
                // `ProgressDrawTarget::stderr`), so live follows it: a
                // hidden target would swallow every line.
                let target = ProgressDrawTarget::stderr();
                let mode = if options.verbose || target.is_hidden() {
                    Mode::Plain
                } else {
                    Mode::Live(Area::new(target))
                };
                (mode, crate::colour_on(crate::Stream::Stderr), Size::Stderr)
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
                (Mode::Live(Area::new(target)), colour, size)
            }
        };
        let plain = matches!(mode, Mode::Plain);
        let shared = Arc::new(Mutex::new(State {
            tree: Tree::default(),
            mode,
            colour,
            size,
            frame: 0,
            ended: false,
        }));
        lock(&shared).redraw();
        let frames = (!plain).then(|| tokio::spawn(frames(Arc::clone(&shared))));
        Runner {
            shared,
            shutdown: shutdown.clone(),
            frames,
        }
    }

    /// Run one top-level step to its end, after the previous one. The
    /// body's result decides the ending ("Step endings" in the spec).
    pub async fn step<T>(
        &mut self,
        labels: Labels,
        body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
    ) -> Result<T, Exit> {
        let token = self.shutdown.token();
        if token.is_cancelled() {
            return Err(Exit(1));
        }
        let started = Instant::now();
        let id = {
            let mut state = lock(&self.shared);
            let id = state.tree.add(None, labels.clone(), started);
            if matches!(state.mode, Mode::Plain) {
                plain_line(&format!("- {}", labels.running));
            }
            state.redraw();
            id
        };
        // Ends the step if this future is dropped before the body returns.
        let mut guard = Ending {
            shared: Arc::clone(&self.shared),
            id,
            labels,
            started,
            open: true,
        };
        let mut step = Step {
            shared: Arc::clone(&self.shared),
            id,
            token: token.child_token(),
        };
        let result = body(&mut step).await;
        let (status, outcome) = match result {
            Ok(value) => (Status::Done, Ok(value)),
            Err(err) => ending(&self.shutdown, &err),
        };
        guard.close(status);
        outcome
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
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
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
}

impl Ending {
    fn close(&mut self, status: Status) {
        self.open = false;
        let mut state = lock(&self.shared);
        state.tree.finish(self.id, status, Instant::now());
        // Plain lines go through anstream, so they still print while a
        // panic unwinds; the live area doesn't (see `State::end`).
        if matches!(state.mode, Mode::Plain) {
            let colour = state.colour;
            let line = end_line(
                theme::current(),
                &self.labels,
                status,
                self.started.elapsed(),
                colour,
            );
            plain_line(&line);
        }
        if !std::thread::panicking() {
            state.redraw();
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
            self.close(status);
        }
    }
}

/// How a failed body's step ends, and what comes back: interrupted,
/// stopped by the command's own request, a quiet exit passed up, or
/// failed.
/// A signal and the command's own request both cancel every step's token,
/// so the shutdown alone says which it was (a sibling's stop, Task 9, has
/// its own token).
fn ending<T>(shutdown: &Shutdown, err: &anyhow::Error) -> (Status, Result<T, Exit>) {
    if shutdown.interrupted() {
        (Status::Interrupted, Err(Exit(1)))
    } else if shutdown.requested().is_some() {
        (Status::Stopped, Err(Exit(1)))
    } else if let Some(code) = quiet_exit(err.as_ref()) {
        (Status::SubStepFailed, Err(Exit(code)))
    } else {
        (Status::Failed, Err(Exit(1)))
    }
}

/// A running step, lent to its body.
pub struct Step {
    shared: Shared,
    id: usize,
    token: CancellationToken,
}

impl Step {
    /// The token this step watches: a child of `shutdown.token()`,
    /// cancelled by Ctrl-C or the command's own `Shutdown::request`. A
    /// body stops early when it is cancelled and returns its error.
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
        let mut state = lock(&self.shared);
        if matches!(state.mode, Mode::Plain) {
            let (role, mark) = if warn {
                (Role::Warn, "!")
            } else {
                (Role::Muted, "›")
            };
            let colour = state.colour;
            plain_line(&format!(
                "{} {}",
                paint(theme::current(), role, colour, mark),
                line.text
            ));
        }
        state.tree.notice(self.id, warn, line.text);
        state.redraw();
    }

    /// Output: shown in the live tail (or plainly).
    pub fn output(&mut self, text: impl Into<Line>) {
        let line = text.into();
        let mut state = lock(&self.shared);
        if matches!(state.mode, Mode::Plain) {
            plain_line(&line.text);
        }
        state.tree.output(self.id, line.text);
        state.redraw();
    }
}

/// A notice or output line. From `&str`, `String` and `&String`.
pub struct Line {
    text: String,
}

impl From<&str> for Line {
    fn from(text: &str) -> Self {
        Line { text: text.into() }
    }
}

impl From<String> for Line {
    fn from(text: String) -> Self {
        Line { text }
    }
}

impl From<&String> for Line {
    fn from(text: &String) -> Self {
        Line { text: text.clone() }
    }
}

/// Move the spinner every [`FRAME`], skipping missed ticks
/// (https://docs.rs/tokio/1.54.1/tokio/time/enum.MissedTickBehavior.html).
async fn frames(shared: Shared) {
    let mut tick = tokio::time::interval(FRAME);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let mut state = lock(&shared);
        if state.ended {
            return;
        }
        state.frame += 1;
        state.redraw();
    }
}

/// A plain-mode line on stderr (sb-go's plain mode: `Verbose ||
/// !IsInteractive()`).
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
            verbose: false,
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

    /// While a step runs, the live area shows it with its spinner.
    #[tokio::test]
    async fn a_running_step_is_live() {
        let (term, options) = screen();
        let shutdown = Shutdown::new();
        let mut run = Runner::start("sb demo", &shutdown, None, options).await;
        let seen = run
            .step(
                Labels::new("Pulling images", "images pulled"),
                async |step| {
                    step.output("pulling sonarr");
                    Ok(term.contents())
                },
            )
            .await
            .expect("step");
        insta::assert_snapshot!(seen, @r"
        sb demo
        ├── ⠋ Pulling images                                          0s
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
    #[tokio::test]
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
                Ok(term.contents())
            })
            .await
            .expect("step");
        assert!(!during.contains("tail line"), "{during}");
        insta::assert_snapshot!(during, @r"
        sb demo
        ├── ✓ long done                                               0s
        ├── ⠋ Short                                                   0s
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
