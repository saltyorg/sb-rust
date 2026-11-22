//! The runner's live block (term-ui spec, "Live block"; decision 15): a
//! command's own text drawn in place of the step tree, through
//! `console_static_text` 0.9.0
//! (<https://docs.rs/console_static_text/0.9.0/console_static_text/>),
//! the live text of Deno's CLI
//! (<https://docs.rs/crate/deno/2.8.3/source/util/draw_thread.rs>).
//!
//! The crate does the drawing: no line is padded, a size change wraps
//! the last frame at the new width to count the rows it takes before
//! moving up over them, a frame taller than the screen loses its top
//! rows, and a frame whose lines are the same as the last's writes
//! nothing. The size is passed in at every frame (`render_with_size`),
//! so its `sized` feature (atty, `terminal_size`) stays off. What is ours:
//! when a frame is drawn, the synchronized update around it (crossterm's
//! `BeginSynchronizedUpdate`, DEC 2026), the hand-over to stdout
//! ([`BlockArea::suspend`]) and the endings.
//!
//! Why not the tree's indicatif area: indicatif pads each line to the
//! width and moves up by its line count, so a narrowing on a terminal
//! that rewraps doubles its rows and leaves copies (measured in tmux
//! 3.2a), and it stops drawing at the screen's height, cutting the
//! bottom, where the row being measured is. Why not ratatui's inline
//! viewport (0.30.2): it clears the whole screen on a narrowing, and
//! crossterm's cursor query goes to stdout, into a pipe (term-ui spec,
//! decision 15).

use std::io::Write;
use std::time::Duration;

use console_static_text::{ConsoleSize, ConsoleStaticText};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use sb_cli_core::cancel::Shutdown;

/// How often a block's frame moves: `console_static_text`'s own example
/// and Deno's draw thread both draw every 120 ms (the tree's spinner
/// keeps its 80 ms). The bytes a run sends follow it, since a frame
/// whose lines didn't change writes nothing.
pub(crate) const BLOCK_FRAME: Duration = Duration::from_millis(120);

/// What a block's render gets at each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFrame {
    /// The screen's width, which the text should fit.
    pub width: usize,
    /// The screen's rows: a taller text loses its top rows.
    pub rows: usize,
    /// Whether colour is on (stderr's choice).
    pub colour: bool,
    /// The spinner's glyph for this frame, the tree's spinner.
    pub spinner: &'static str,
    /// Whether a signal interrupted the run: the last frame, kept on
    /// the screen, should say so.
    pub interrupted: bool,
}

type Render = Box<dyn FnMut(&BlockFrame) -> String + Send>;

/// A live block's text, rendered at each frame (term-ui decision 15):
/// for [`crate::Runner::start_block`].
pub struct Block {
    render: Render,
    #[cfg(any(test, feature = "testing"))]
    screen: Option<Screen>,
}

impl Block {
    /// A block whose text `render` gives at each frame, its lines split
    /// at `\n`, each fitting the frame's width.
    pub fn new(render: impl FnMut(&BlockFrame) -> String + Send + 'static) -> Self {
        Self {
            render: Box::new(render),
            #[cfg(any(test, feature = "testing"))]
            screen: None,
        }
    }

    /// Drawn on `screen` instead of stderr, always live, its spinner
    /// standing still (term-ui's `testing` feature).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn on(self, screen: Screen) -> Self {
        Self {
            screen: Some(screen),
            ..self
        }
    }

    /// Its test screen, if it has one.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block").finish_non_exhaustive()
    }
}

/// Where a block draws.
enum Sink {
    Stderr,
    #[cfg(any(test, feature = "testing"))]
    Screen(Screen),
}

/// A live block being drawn (the runner's block mode).
pub(crate) struct BlockArea {
    text: ConsoleStaticText,
    render: Render,
    sink: Sink,
    shutdown: Shutdown,
    colour: bool,
    /// Frame ticks so far: the spinner's glyph moves once a tick.
    ticks: usize,
    /// The size the last frame tick found: a tick that finds another
    /// waits for the next, as the crate's example and Deno debounce a
    /// resize.
    seen: Option<ConsoleSize>,
}

/// A `ConsoleStaticText` that is always given its size.
fn static_text() -> ConsoleStaticText {
    ConsoleStaticText::new(|| ConsoleSize {
        cols: None,
        rows: None,
    })
}

/// The spinner's glyph after `ticks` frame ticks: the tree's spinner,
/// a glyph a tick.
fn spinner(ticks: usize) -> &'static str {
    crate::tree::SPINNER[ticks % crate::tree::SPINNER.len()]
}

/// `command`'s escape sequence.
fn ansi(command: &impl crossterm::Command) -> String {
    let mut text = String::new();
    // Writing to a String can't fail.
    let _ = command.write_ansi(&mut text);
    text
}

impl BlockArea {
    /// `block` drawn on stderr (or its test screen), colour as `colour`
    /// says, the interruption read from `shutdown`.
    pub(crate) fn new(block: Block, shutdown: &Shutdown, colour: bool) -> Self {
        #[cfg(any(test, feature = "testing"))]
        let sink = block.screen.map_or(Sink::Stderr, Sink::Screen);
        #[cfg(not(any(test, feature = "testing")))]
        let sink = Sink::Stderr;
        Self {
            text: static_text(),
            render: block.render,
            sink,
            shutdown: shutdown.clone(),
            colour,
            ticks: 0,
            seen: None,
        }
    }

    /// Whether it draws on stderr (else on a test screen).
    pub(crate) fn on_stderr(&self) -> bool {
        matches!(self.sink, Sink::Stderr)
    }

    /// The screen's size now: stderr's (console's `size_checked`, as
    /// the tree's), else 24 x 80; or the test screen's.
    fn size(&self) -> ConsoleSize {
        let (rows, cols) = match &self.sink {
            Sink::Stderr => console::Term::stderr().size_checked().unwrap_or((24, 80)),
            #[cfg(any(test, feature = "testing"))]
            Sink::Screen(screen) => screen.size(),
        };
        ConsoleSize {
            cols: Some(cols),
            rows: Some(rows),
        }
    }

    /// Whether a signal interrupted the run.
    pub(crate) fn interrupted(&self) -> bool {
        self.shutdown.interrupted()
    }

    /// The frame's text at `size`.
    fn render(&mut self, size: ConsoleSize) -> String {
        let spinner = match self.sink {
            Sink::Stderr => spinner(self.ticks),
            #[cfg(any(test, feature = "testing"))]
            Sink::Screen(_) => crate::tree::SPINNER[0],
        };
        let frame = BlockFrame {
            width: size.cols.map_or(80, usize::from),
            rows: size.rows.map_or(24, usize::from),
            colour: self.colour,
            spinner,
            interrupted: self.interrupted(),
        };
        // As every line term-ui draws from text it is given (term-ui spec,
        // "Escape handling"): only SGR kept, and only when the profile
        // keeps codes; console_static_text writes what it gets.
        let text = (self.render)(&frame);
        let codes = crate::keeps_codes(self.colour, sb_cli_core::theme::current().profile());
        crate::DisplayText::from_bytes(text.as_bytes(), codes)
            .as_str()
            .to_owned()
    }

    /// What to write for the frame at `size`: none when its lines are
    /// the last frame's.
    fn frame(&mut self, size: ConsoleSize) -> Option<String> {
        let text = self.render(size);
        self.text.render_with_size(&text, size)
    }

    /// Draw a frame now.
    pub(crate) fn draw(&mut self) {
        let size = self.size();
        if let Some(frame) = self.frame(size) {
            self.write(&synchronized(&frame));
        }
    }

    /// A frame tick: a frame, unless the size changed since the last
    /// tick (then the next tick draws, at the size it settled on).
    pub(crate) fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        let size = self.size();
        let settled = self.seen.is_none_or(|seen| seen == size);
        self.seen = Some(size);
        if settled {
            self.draw();
        }
    }

    /// Clear the block, run `f` (which writes on stdout), and draw the
    /// block again, in one synchronized update, so the hand-over never
    /// shows a blank frame: the crate's own example of text logged above
    /// it, `render_clear_with_size`, the text, then `render_with_size`
    /// (<https://docs.rs/crate/console_static_text/0.9.0/source/examples/logging_text_above.rs>);
    /// indicatif's `MultiProgress::suspend` for the tree. Should `f`
    /// panic, the update is still ended.
    pub(crate) fn suspend<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let size = self.size();
        let mut text = ansi(&BeginSynchronizedUpdate);
        text.push_str(&self.text.render_clear_with_size(size).unwrap_or_default());
        self.write(&text);
        let result = {
            let _end = EndOnPanic(&self.sink);
            f()
        };
        // What `f` wrote reaches the terminal before the block again.
        let _ = std::io::stdout().flush();
        let mut text = self.frame(size).unwrap_or_default();
        text.push_str(&ansi(&EndSynchronizedUpdate));
        self.write(&text);
        result
    }

    /// Clear the block (a pause below, the happy end).
    pub(crate) fn clear(&mut self) {
        if let Some(text) = self.text.render_clear_with_size(self.size()) {
            self.write(&text);
        }
    }

    /// Draw the last frame and leave it, the cursor on the line below
    /// it (an interrupted end). Recorded for the blank line at
    /// the end (cli-core's `tail`).
    pub(crate) fn keep(&mut self) {
        let size = self.size();
        let text = self.render(size);
        let mut out = self.text.render_with_size(&text, size).unwrap_or_default();
        if !text.is_empty() {
            out.push_str("\r\n");
            let mut kept = text.trim_end_matches('\n').to_owned();
            kept.push('\n');
            sb_cli_core::tail::note(sb_cli_core::tail::Stream::Stderr, kept.as_bytes());
        }
        self.write(&synchronized(&out));
        self.text = static_text();
    }

    /// Lines printed below what is drawn (the tree of a failed run), as
    /// the runner prints a plain line: through anstream, recorded for the
    /// blank line at the end.
    pub(crate) fn print(&mut self, lines: &[String]) {
        for line in lines {
            match &self.sink {
                Sink::Stderr => {
                    let _ = writeln!(sb_cli_core::tail::stderr(), "{line}");
                }
                #[cfg(any(test, feature = "testing"))]
                Sink::Screen(screen) => screen.feed(format!("{line}\r\n").as_bytes(), false),
            }
        }
    }

    fn write(&mut self, text: &str) {
        self.sink.write(text);
    }
}

impl Sink {
    fn write(&self, text: &str) {
        match self {
            Sink::Stderr => {
                // Nothing better to do if stderr fails; the run goes on.
                let mut err = std::io::stderr().lock();
                let _ = err.write_all(text.as_bytes());
                let _ = err.flush();
            }
            #[cfg(any(test, feature = "testing"))]
            Sink::Screen(screen) => screen.feed(text.as_bytes(), true),
        }
    }
}

/// Ends the synchronized update [`BlockArea::suspend`] began when its
/// closure panics, so the terminal isn't left waiting for its end.
struct EndOnPanic<'a>(&'a Sink);

impl Drop for EndOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.write(&ansi(&EndSynchronizedUpdate));
        }
    }
}

/// `frame` in one synchronized update, so a terminal that knows DEC
/// 2026 shows it whole (crossterm's commands,
/// <https://docs.rs/crossterm/0.29.0/crossterm/terminal/struct.BeginSynchronizedUpdate.html>).
fn synchronized(frame: &str) -> String {
    format!(
        "{}{frame}{}",
        ansi(&BeginSynchronizedUpdate),
        ansi(&EndSynchronizedUpdate)
    )
}

/// A test screen for a live block (term-ui's `testing` feature): vt100,
/// a terminal that cuts its lines on a resize rather than rewrapping
/// them, as xterm and the Linux console do. Other output (a command's
/// stdout) is written to it as `Write`. Why not indicatif's
/// `InMemoryTerm`, already the tree's test screen: it can't be resized
/// and keeps no scrollback (indicatif 0.18.6 `in_memory.rs` makes its
/// parser with none), and the resize and copy tests need both.
#[cfg(any(test, feature = "testing"))]
#[derive(Clone)]
pub struct Screen(std::sync::Arc<std::sync::Mutex<ScreenState>>);

#[cfg(any(test, feature = "testing"))]
struct ScreenState {
    parser: vt100::Parser,
    /// What the block wrote.
    block: Vec<u8>,
    colour: bool,
}

#[cfg(any(test, feature = "testing"))]
impl Screen {
    /// A screen `rows` high and `cols` wide, colour off, with 1000 rows
    /// of scrollback.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(ScreenState {
            parser: vt100::Parser::new(rows, cols, 1000),
            block: Vec::new(),
            colour: false,
        })))
    }

    /// The same, colour on.
    #[must_use]
    pub fn coloured(self) -> Self {
        self.state().colour = true;
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ScreenState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether colour is on.
    pub(crate) fn colour(&self) -> bool {
        self.state().colour
    }

    /// The screen's rows and columns.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.state().parser.screen().size()
    }

    /// Resize it, as a terminal resized: vt100 keeps each row's cells,
    /// cutting them at a narrower width.
    pub fn resize(&self, rows: u16, cols: u16) {
        self.state().parser.screen_mut().set_size(rows, cols);
    }

    fn feed(&self, bytes: &[u8], block: bool) {
        let mut state = self.state();
        state.parser.process(bytes);
        if block {
            state.block.extend_from_slice(bytes);
        }
    }

    /// The bytes the block wrote so far.
    #[must_use]
    pub fn block_bytes(&self) -> usize {
        self.state().block.len()
    }

    /// What the block wrote so far.
    #[must_use]
    pub fn block_written(&self) -> Vec<u8> {
        self.state().block.clone()
    }

    /// The visible rows, each without its trailing spaces.
    #[must_use]
    pub fn rows(&self) -> Vec<String> {
        let state = self.state();
        let (_, cols) = state.parser.screen().size();
        state
            .parser
            .screen()
            .rows(0, cols)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// The scrollback and the visible rows, oldest first, each without
    /// its trailing spaces.
    #[must_use]
    pub fn history(&self) -> Vec<String> {
        let mut state = self.state();
        let (_, cols) = state.parser.screen().size();
        state.parser.screen_mut().set_scrollback(usize::MAX);
        let mut lines = Vec::new();
        for back in (1..=state.parser.screen().scrollback()).rev() {
            state.parser.screen_mut().set_scrollback(back);
            if let Some(row) = state.parser.screen().rows(0, cols).next() {
                lines.push(row.trim_end().to_owned());
            }
        }
        state.parser.screen_mut().set_scrollback(0);
        lines.extend(
            state
                .parser
                .screen()
                .rows(0, cols)
                .map(|row| row.trim_end().to_owned()),
        );
        lines
    }

    /// The cursor's row and column.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        self.state().parser.screen().cursor_position()
    }
}

#[cfg(any(test, feature = "testing"))]
impl Write for Screen {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // A terminal's own line discipline turns `\n` into `\r\n`.
        let text = String::from_utf8_lossy(buf).replace('\n', "\r\n");
        self.feed(text.as_bytes(), false);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Screen").field(&self.size()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sb_cli_core::cancel::Shutdown;
    use sb_cli_core::exit::Exit;

    use super::*;
    use crate::{Labels, Options, Runner};

    /// What the block shows (shared with its render), and the frames
    /// it was rendered for.
    #[derive(Clone, Default)]
    struct Shown {
        text: Arc<Mutex<String>>,
        frames: Arc<Mutex<Vec<BlockFrame>>>,
    }

    impl Shown {
        fn set(&self, text: &str) {
            *self.text.lock().expect("text") = text.to_owned();
        }

        fn frames(&self) -> Vec<BlockFrame> {
            self.frames.lock().expect("frames").clone()
        }

        /// A block on `screen` drawing the text, `interrupted` in place
        /// of `{spinner}` once a signal interrupted the run, and `{rule}`
        /// as a rule the frame's width.
        fn block(&self, screen: &Screen) -> Block {
            let shown = self.clone();
            Block::new(move |frame| {
                shown.frames.lock().expect("frames").push(*frame);
                let mark = if frame.interrupted {
                    "interrupted"
                } else {
                    frame.spinner
                };
                shown
                    .text
                    .lock()
                    .expect("text")
                    .replace("{spinner}", mark)
                    .replace("{rule}", &"-".repeat(frame.width))
            })
            .on(screen.clone())
        }
    }

    fn start(shown: &Shown, screen: &Screen, shutdown: &Shutdown) -> Runner {
        Runner::start_block(
            "sb demo",
            shutdown,
            None,
            Options::default(),
            shown.block(screen),
        )
    }

    /// Each line at most once in `lines`, blank lines aside.
    fn once_each(lines: &[String]) -> bool {
        let mut seen = std::collections::HashSet::new();
        lines
            .iter()
            .filter(|line| !line.is_empty())
            .all(|line| seen.insert(line))
    }

    /// The block is drawn in place as its text changes, under what was
    /// on the screen, and leaves nothing at a happy end: no title, no
    /// tree, the earlier lines kept.
    #[tokio::test]
    async fn a_block_is_drawn_in_place_and_cleared_at_the_end() {
        let screen = Screen::new(12, 40);
        let mut other = screen.clone();
        other.write_all(b"$ earlier\n").expect("write");
        let shown = Shown::default();
        shown.set("Disk write\n1st run  {spinner} running");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("Run 1", "Ran 1").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert_eq!(
            screen.rows()[..3],
            ["$ earlier", "Disk write", "1st run  ⠋ running"],
        );
        shown.set("Disk write\n1st run  800.0 MB/s\n2nd run  {spinner} running");
        run.step(Labels::new("Run 2", "Ran 2").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert_eq!(
            screen.rows()[..4],
            [
                "$ earlier",
                "Disk write",
                "1st run  800.0 MB/s",
                "2nd run  ⠋ running"
            ],
        );
        run.finish();
        assert_eq!(screen.rows()[0], "$ earlier");
        assert!(
            screen.rows()[1..].iter().all(String::is_empty),
            "{:#?}",
            screen.rows()
        );
        assert_eq!(screen.cursor(), (1, 0));
    }

    /// The render gets the screen's size, its colour (none on a plain
    /// screen), the tree's spinner standing still on a test screen, and no
    /// interruption.
    #[tokio::test]
    async fn the_render_gets_the_frame() {
        for colour in [true, false] {
            let screen = Screen::new(30, 100);
            let screen = if colour { screen.coloured() } else { screen };
            let shown = Shown::default();
            shown.set("x");
            let run = start(&shown, &screen, &Shutdown::new());
            run.finish();
            let frames = shown.frames();
            assert_ne!(frames, []);
            assert!(frames.iter().all(|frame| *frame
                == BlockFrame {
                    width: 100,
                    rows: 30,
                    colour,
                    spinner: "⠋",
                    interrupted: false,
                }));
        }
    }

    /// `suspend` writes what it is given above the block, which is then
    /// drawn again below it: no line twice, no blank frame between.
    #[tokio::test]
    async fn suspend_prints_above_the_block() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("System\nCPU  {spinner} checking");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("Read", "Read").transient(), async |_| Ok(()))
            .await
            .expect("step");
        let mut out = screen.clone();
        let before = screen.block_bytes();
        run.suspend(|| {
            shown.set("\nDisk write\n1st run  {spinner} running");
            out.write_all(b"System\nCPU  AMD\n").expect("write");
        });
        assert_eq!(
            screen.rows()[..5],
            ["System", "CPU  AMD", "", "Disk write", "1st run  ⠋ running"],
        );
        assert!(screen.block_bytes() > before);
        run.finish();
        assert_eq!(screen.rows()[..2], ["System", "CPU  AMD"]);
        assert!(screen.rows()[2..].iter().all(String::is_empty));
    }

    /// A frame whose lines didn't change writes nothing
    /// (`console_static_text` compares them), so the bytes follow what
    /// changes.
    #[tokio::test]
    async fn an_unchanged_frame_writes_nothing() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("Speedtest\nTokyo  {spinner} testing");
        let mut run = start(&shown, &screen, &Shutdown::new());
        let mut drawn = 0;
        run.step(Labels::new("A", "A").transient(), async |_| {
            drawn = screen.block_bytes();
            Ok(())
        })
        .await
        .expect("step");
        run.step(Labels::new("B", "B").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert!(drawn > 0);
        assert_eq!(screen.block_bytes(), drawn, "the same lines, again");
        run.finish();
    }

    /// Ctrl-C: the last frame stays, drawn with the interruption, the
    /// cursor below it, and no tree is printed (cli-core's block says
    /// "interrupted").
    #[tokio::test]
    async fn an_interrupted_run_keeps_its_last_frame() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("Disk write\n1st run  {spinner}");
        let shutdown = Shutdown::new();
        let mut run = start(&shown, &screen, &shutdown);
        let result: Result<(), Exit> = run
            .step(Labels::new("Run 1", "Ran 1").transient(), async |_| {
                shutdown.signalled(2);
                anyhow::bail!("interrupted")
            })
            .await;
        assert!(result.is_err());
        drop(run);
        assert_eq!(
            screen.rows()[..3],
            ["Disk write", "1st run  interrupted", ""],
        );
        assert!(!screen.rows().iter().any(|row| row.contains("sb demo")));
        assert_eq!(screen.cursor(), (2, 0));
    }

    /// A failed step: the block is cleared (its spinner would say the
    /// measurement goes on) and the tree is printed, as any failed run's,
    /// with the error.
    #[tokio::test]
    async fn a_failed_run_prints_the_tree_in_place_of_the_block() {
        let screen = Screen::new(16, 60);
        let shown = Shown::default();
        shown.set("System\nCPU  {spinner} checking");
        let mut run = start(&shown, &screen, &Shutdown::new());
        let result: Result<(), Exit> = run
            .step(
                Labels::new("Reading system information", "Read").transient(),
                async |_| anyhow::bail!("/proc/meminfo unreadable"),
            )
            .await;
        assert!(result.is_err());
        run.finish();
        let rows = screen.rows();
        assert_eq!(rows[0], "sb demo", "{rows:#?}");
        assert!(
            rows[1].contains("✗ Reading system information"),
            "{rows:#?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("checking")),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("/proc/meminfo unreadable"))
        );
    }

    /// Widened, the block is drawn again at the new width in place; the
    /// lines above it stay, each once.
    #[tokio::test]
    async fn a_widened_screen_redraws_the_block_in_place() {
        let screen = Screen::new(12, 40);
        let mut other = screen.clone();
        other.write_all(b"one\ntwo\n").expect("write");
        let shown = Shown::default();
        shown.set("Speedtest\n{rule}\nTokyo  {spinner}");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
            .await
            .expect("step");
        screen.resize(12, 70);
        run.step(Labels::new("B", "B").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert_eq!(
            screen.rows()[..5],
            [
                "one",
                "two",
                "Speedtest",
                "-".repeat(70).as_str(),
                "Tokyo  ⠋"
            ],
        );
        let history = screen.history();
        assert!(once_each(&history), "{history:#?}");
        assert_eq!(history[..3], ["one", "two", "Speedtest"], "{history:#?}");
        run.finish();
    }

    /// Narrowed on a screen that cuts its lines (vt100, as xterm), the
    /// block is drawn again at the new width with no copy left. Its lines
    /// within the new width: nothing above it is touched. A line wider
    /// than the new width counts as two rows when the crate wraps the last
    /// frame again, so the move up overshoots by one row a wide line, and
    /// that many rows above the block are cleared (term-ui spec, "Live
    /// block", the known limit).
    #[tokio::test]
    async fn a_narrowed_screen_that_cuts_lines_keeps_no_copy() {
        for (text, kept) in [
            (
                "Speedtest\n{rule}\nTokyo  {spinner}",
                ["one", "two", "three"].as_slice(),
            ),
            (
                "Speedtest\n------------------------------\nTokyo  {spinner}",
                &["one", "two", "three"],
            ),
        ] {
            let screen = Screen::new(12, 40);
            let mut other = screen.clone();
            other.write_all(b"one\ntwo\nthree\n").expect("write");
            let shown = Shown::default();
            shown.set(text);
            let mut run = start(&shown, &screen, &Shutdown::new());
            run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
                .await
                .expect("step");
            screen.resize(12, 32);
            run.step(Labels::new("B", "B").transient(), async |_| Ok(()))
                .await
                .expect("step");
            let rows = screen.rows();
            let at = rows
                .iter()
                .position(|row| row == "Speedtest")
                .expect("heading");
            assert!(once_each(&screen.history()), "{rows:#?}");
            assert_eq!(rows[at + 2], "Tokyo  ⠋", "{rows:#?}");
            let above: Vec<&str> = rows[..at].iter().map(String::as_str).collect();
            let lost = usize::from(text.contains("{rule}"));
            assert_eq!(above, kept[..kept.len() - lost], "{rows:#?}");
            run.finish();
        }
    }

    /// Taller than the screen, only its bottom rows are drawn (the crate
    /// cuts from the top), so the row being measured stays in view.
    #[tokio::test]
    async fn a_block_taller_than_the_screen_shows_its_bottom_rows() {
        let screen = Screen::new(4, 40);
        let shown = Shown::default();
        shown.set("Speedtest\nnode 1\nnode 2\nnode 3\nnode 4\nnode 5  {spinner}");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert_eq!(screen.rows(), ["node 2", "node 3", "node 4", "node 5  ⠋"]);
        run.finish();
    }

    /// A frame tick that finds the size changed draws nothing; the next
    /// tick draws at the size it settled on (the crate's example and
    /// Deno debounce a resize).
    #[tokio::test]
    async fn a_tick_waits_while_the_size_changes() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("{rule}");
        let mut area = BlockArea::new(shown.block(&screen), &Shutdown::new(), false);
        area.tick();
        assert_eq!(screen.rows()[0], "-".repeat(40));
        screen.resize(12, 50);
        let before = screen.block_bytes();
        area.tick();
        assert_eq!(screen.block_bytes(), before, "a changed size: no frame");
        area.tick();
        assert_eq!(screen.rows()[0], "-".repeat(50));
    }

    /// Paused below, the block is cleared; the next step draws it again.
    #[tokio::test]
    async fn a_pause_below_clears_the_block() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("Block {spinner}");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
            .await
            .expect("step");
        run.pause_below();
        assert!(screen.rows().iter().all(String::is_empty));
        run.step(Labels::new("B", "B").transient(), async |_| Ok(()))
            .await
            .expect("step");
        assert_eq!(screen.rows()[0], "Block ⠋");
        run.finish();
    }

    /// A panic in `suspend`'s closure still ends the synchronized
    /// update, so the terminal isn't left holding its frames.
    #[test]
    fn a_panic_while_suspended_ends_the_update() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("x");
        let mut area = BlockArea::new(shown.block(&screen), &Shutdown::new(), false);
        area.draw();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            area.suspend(|| panic!("in the closure"));
        }));
        assert!(panicked.is_err());
        let written = String::from_utf8(screen.block_written()).expect("utf-8");
        // DEC 2026's begin and end, as crossterm writes them.
        let (_, after) = written.rsplit_once("\x1b[?2026h").expect("a begin");
        assert!(after.ends_with("\x1b[?2026l"), "{after:?}");
    }

    /// The render's text is sanitised as every line term-ui draws:
    /// a title, a clipboard write and a screen clear never reach the
    /// terminal; SGR does when colour is on and the profile keeps codes.
    #[tokio::test]
    async fn the_blocks_text_is_sanitised() {
        for colour in [false, true] {
            let screen = Screen::new(12, 60);
            let screen = if colour { screen.coloured() } else { screen };
            let shown = Shown::default();
            shown.set("\x1b]0;title\x07\x1b]52;c;aGk=\x07\x1b[2J\x1b[31mred\x1b[0m\nok");
            let run = start(&shown, &screen, &Shutdown::new());
            let written = String::from_utf8(screen.block_written()).expect("utf-8");
            for bad in ["\x1b]0;", "\x1b]52;", "\x1b[2J"] {
                assert!(!written.contains(bad), "{bad:?} in {written:?}");
            }
            let codes = crate::keeps_codes(colour, sb_cli_core::theme::current().profile());
            assert_eq!(written.contains("\x1b[31m"), codes, "{written:?}");
            assert_eq!(screen.rows()[..2], ["red", "ok"]);
            run.finish();
        }
    }

    /// Paused, `suspend` runs its closure alone: the block, cleared by
    /// the pause, isn't drawn again until the next step.
    #[tokio::test]
    async fn suspend_while_paused_runs_the_closure_alone() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("Block {spinner}");
        let mut run = start(&shown, &screen, &Shutdown::new());
        run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
            .await
            .expect("step");
        run.pause_below();
        let before = screen.block_bytes();
        let mut out = screen.clone();
        run.suspend(|| out.write_all(b"printed\n").expect("write"));
        assert_eq!(screen.block_bytes(), before);
        assert_eq!(screen.rows()[0], "printed");
        run.finish();
    }

    /// A tree runner's `suspend` is indicatif's: the live area cleared,
    /// the closure run, and the live tree drawn again after it.
    #[tokio::test]
    async fn a_tree_runners_suspend_runs_the_closure() {
        let term = indicatif::InMemoryTerm::new(12, 60);
        let options = Options {
            target: crate::Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let mut run = Runner::start("sb demo", &Shutdown::new(), None, options);
        run.step(Labels::new("Reading", "Read"), async |_| Ok(()))
            .await
            .expect("step");
        let _ = term.moves_since_last_check();
        let mut ran = false;
        run.suspend(|| ran = true);
        assert!(ran);
        // indicatif cleared the live area and drew it again.
        assert_ne!(term.moves_since_last_check(), "");
        assert!(term.contents().contains("Read"), "{}", term.contents());
        run.finish();
    }

    /// The spinner moves a glyph a frame tick, round the tree's ten.
    #[test]
    fn the_spinner_moves_a_glyph_a_tick() {
        assert_eq!(spinner(0), "⠋");
        assert_eq!(spinner(3), "⠸");
        assert_eq!(spinner(13), "⠸");
    }

    /// A block draws on stderr unless it has a test screen; it debugs
    /// without its render.
    #[test]
    fn a_block_draws_on_stderr_without_a_screen() {
        let shutdown = Shutdown::new();
        let on_stderr = BlockArea::new(Block::new(|_| String::new()), &shutdown, false);
        assert!(on_stderr.on_stderr());
        let screen = Screen::new(4, 20);
        let on_screen = BlockArea::new(Block::new(|_| String::new()).on(screen), &shutdown, false);
        assert!(!on_screen.on_stderr());
        assert_eq!(
            format!("{:?}", Block::new(|_| String::new())),
            "Block { .. }"
        );
        assert_eq!(format!("{:?}", Screen::new(4, 20)), "Screen((4, 20))");
    }

    /// A block's frames move at its own pace, 120 ms, not the tree's.
    #[tokio::test(start_paused = true)]
    async fn the_frames_move_every_120_ms() {
        let screen = Screen::new(12, 40);
        let shown = Shown::default();
        shown.set("x");
        let run = start(&shown, &screen, &Shutdown::new());
        let at_start = shown.frames().len();
        tokio::time::sleep(BLOCK_FRAME * 10 + BLOCK_FRAME / 2).await;
        // The interval's first tick is at once, then one a frame.
        assert_eq!(shown.frames().len() - at_start, 11);
        run.finish();
    }

    /// Plain (stderr not a terminal, as under the test runner), the
    /// block is never rendered.
    #[tokio::test]
    async fn a_plain_run_never_renders_the_block() {
        let shown = Shown::default();
        let rendered = Arc::clone(&shown.frames);
        let block = Block::new(move |frame| {
            rendered.lock().expect("frames").push(*frame);
            String::new()
        });
        let hidden = indicatif::ProgressDrawTarget::stderr().is_hidden();
        let mut run =
            Runner::start_block("sb demo", &Shutdown::new(), None, Options::default(), block);
        // Transient, so a plain run prints nothing either.
        run.step(Labels::new("A", "A").transient(), async |_| Ok(()))
            .await
            .expect("step");
        run.finish();
        assert_eq!(shown.frames().is_empty(), hidden);
    }
}
