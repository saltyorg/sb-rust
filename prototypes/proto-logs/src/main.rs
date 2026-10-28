//! Prototype: stream container and systemd logs into a ratatui viewer.
//! The view scrolls directly by screen rows (no selection cursor), long lines
//! wrap or scroll sideways, a right-click menu mirrors the keys, and container
//! restarts are followed instead of ending the stream. Throwaway; see FINDINGS.md.

use std::collections::VecDeque;
use std::time::Duration;

use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use clap::Parser;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use futures::{FutureExt, StreamExt};
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_textarea::TextArea;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// musl's allocator is much slower than glibc's for ratatui's per-frame
// allocations; static release builds use mimalloc instead.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MAX_LINES: usize = 5000;
/// Prefix marking a line written by the viewer itself (stopped/restarted notices).
const NOTICE: char = '\u{1}';
/// Columns moved per sideways scroll step.
const HSTEP: u16 = 8;

#[derive(Parser)]
struct Cli {
    /// Only list sources whose name contains this text.
    #[arg(long, default_value = "")]
    filter: String,
    /// systemd unit prefix that counts as a Saltbox service.
    #[arg(long, default_value = "saltbox_managed_")]
    service_prefix: String,
    /// Skip the TUI: stream this source (container name or unit) to stdout.
    #[arg(long)]
    inline: Option<String>,
    /// Mouse reporting: `buttons` reports clicks, wheel and drags only (what
    /// sb-go's bubbletea uses); `all` also reports every pointer movement.
    #[arg(long, value_enum, default_value = "buttons")]
    mouse: MouseMode,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum MouseMode {
    Buttons,
    All,
}

#[derive(Clone)]
enum Source {
    Container(String),
    Service(String),
}

impl Source {
    fn label(&self) -> String {
        match self {
            Self::Container(n) => format!("container  {n}"),
            Self::Service(n) => format!("service    {n}"),
        }
    }
}

enum Screen {
    Pick,
    View,
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Wrap,
    Follow,
}

type Term = ratatui::Terminal<CrosstermBackend<CountingWriter>>;

struct App {
    sources: Vec<Source>,
    pick: ListState,
    screen: Screen,
    current: Option<Source>,
    lines: VecDeque<String>,
    /// While paused, the view starts at row `skip` of line `top`.
    top: usize,
    skip: u16,
    follow: bool,
    wrap: bool,
    hscroll: u16,
    search: Option<TextArea<'static>>,
    query: String,
    /// Right-click menu position and highlighted item.
    menu: Option<(Position, usize)>,
    generation: u64,
    stream: Option<JoinHandle<()>>,
    quit: bool,
    // From the last draw, for mouse hits and paging.
    pick_area: Rect,
    body: Rect,
    menu_area: Rect,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cli_mouse = cli.mouse;
    let docker = Docker::connect_with_local_defaults()?;
    let sources = list_sources(&docker, &cli).await?;

    if let Some(name) = cli.inline {
        let source = sources
            .into_iter()
            .find(|s| matches!(s, Source::Container(n) | Source::Service(n) if *n == name))
            .context("no such source")?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = spawn_stream(docker, source, 0, tx);
        // Plain streaming: the terminal owns scrollback, search and copy.
        loop {
            tokio::select! {
                Some((_, line)) = rx.recv() => match line.strip_prefix(NOTICE) {
                    Some(notice) => println!("-- {notice} --"),
                    None => println!("{line}"),
                },
                _ = tokio::signal::ctrl_c() => break,
                else => break,
            }
        }
        task.abort();
        return Ok(());
    }

    let mut app = App {
        sources,
        pick: ListState::default().with_selected(Some(0)),
        screen: Screen::Pick,
        current: None,
        lines: VecDeque::new(),
        top: 0,
        skip: 0,
        follow: true,
        wrap: true,
        hscroll: 0,
        search: None,
        query: String::new(),
        menu: None,
        generation: 0,
        stream: None,
        quit: false,
        pick_area: Rect::default(),
        body: Rect::default(),
        menu_area: Rect::default(),
    };
    // ratatui::init sets up raw mode, the alternate screen and the panic hook;
    // the terminal we draw with wraps stdout to count bytes for the timing log.
    drop(ratatui::init());
    let mut terminal = Term::new(CrosstermBackend::new(CountingWriter))?;
    match cli_mouse {
        // crossterm's EnableMouseCapture always includes all-motion tracking
        // (1003), so button-only mode writes the modes itself: 1000 clicks and
        // wheel, 1002 drags, 1006 SGR encoding.
        MouseMode::Buttons => {
            use std::io::Write as _;
            let mut out = std::io::stdout();
            write!(out, "\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
            out.flush()?;
        }
        MouseMode::All => execute!(std::io::stdout(), EnableMouseCapture)?,
    }
    let result = run(&mut terminal, &mut app, docker).await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    drain_input();
    ratatui::restore();
    result
}

/// Bytes written to the terminal during the current frame, for the timing log.
static BYTES_WRITTEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// stdout wrapper that counts bytes written into `BYTES_WRITTEN`.
struct CountingWriter;

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = std::io::stdout().write(buf)?;
        BYTES_WRITTEN.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}

/// Reads and discards input still in flight after mouse capture is turned
/// off, so stray mouse codes don't end up in the user's shell.
fn drain_input() {
    let deadline = std::time::Instant::now() + Duration::from_millis(150);
    while std::time::Instant::now() < deadline {
        if let Ok(true) = crossterm::event::poll(Duration::from_millis(20)) {
            let _ = crossterm::event::read();
        }
    }
}

async fn list_sources(docker: &Docker, cli: &Cli) -> Result<Vec<Source>> {
    let mut sources: Vec<Source> = docker
        .list_containers(None::<bollard::query_parameters::ListContainersOptions>)
        .await?
        .into_iter()
        .filter_map(|c| {
            c.names?
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
        })
        .map(Source::Container)
        .collect();
    sources.sort_by_key(Source::label);
    let out = tokio::process::Command::new("systemctl")
        .args([
            "list-units",
            "--type=service",
            "--all",
            "--no-pager",
            "--plain",
            "--no-legend",
        ])
        .output()
        .await?;
    sources.extend(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|u| {
                u.starts_with(&cli.service_prefix)
                    || (!cli.filter.is_empty() && u.contains(&cli.filter))
            })
            .map(|u| Source::Service(u.to_string())),
    );
    sources.retain(
        |s| matches!(s, Source::Container(n) | Source::Service(n) if n.contains(&cli.filter)),
    );
    Ok(sources)
}

fn now_secs() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i32::try_from(secs).unwrap_or(i32::MAX)
}

/// Streams one source into the channel, tagged with a generation so lines from
/// a stream the user already left are dropped. Containers are followed across
/// restarts and re-creation; aborting the task kills a journalctl child via
/// kill_on_drop.
fn spawn_stream(
    docker: Docker,
    source: Source,
    generation: u64,
    tx: mpsc::UnboundedSender<(u64, String)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let send = |line: String| tx.send((generation, line)).is_ok();
        match source {
            Source::Container(name) => {
                let mut since = 0;
                loop {
                    let tail = if since == 0 { "500" } else { "all" };
                    let opts = LogsOptions {
                        follow: true,
                        stdout: true,
                        stderr: true,
                        tail: tail.into(),
                        since,
                        ..Default::default()
                    };
                    let mut stream = docker.logs(&name, Some(opts));
                    while let Some(Ok(chunk)) = stream.next().await {
                        let bytes = match &chunk {
                            LogOutput::StdOut { message }
                            | LogOutput::StdErr { message }
                            | LogOutput::Console { message } => message,
                            LogOutput::StdIn { .. } => continue,
                        };
                        for line in String::from_utf8_lossy(bytes).lines() {
                            if !send(line.to_string()) {
                                return;
                            }
                        }
                    }
                    // The follow stream ends when the container stops. Wait for it
                    // (or a re-created container with the same name) to run again.
                    since = now_secs();
                    if !send(format!(
                        "{NOTICE}container stopped, waiting for it to come back…"
                    )) {
                        return;
                    }
                    loop {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let running = docker
                            .inspect_container(&name, None)
                            .await
                            .ok()
                            .and_then(|i| i.state?.running)
                            .unwrap_or(false);
                        if running {
                            break;
                        }
                    }
                    if !send(format!("{NOTICE}container is running again")) {
                        return;
                    }
                }
            }
            Source::Service(unit) => {
                let Ok(mut child) = tokio::process::Command::new("journalctl")
                    .args([
                        "-u",
                        &unit,
                        "-n",
                        "500",
                        "-f",
                        "-o",
                        "short-iso",
                        "--no-pager",
                    ])
                    .stdout(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                else {
                    return;
                };
                let Some(stdout) = child.stdout.take() else {
                    return;
                };
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !send(line) {
                        return;
                    }
                }
            }
        }
    })
}

async fn run(terminal: &mut Term, app: &mut App, docker: Docker) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(u64, String)>();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    // Opt-in frame timing for performance work: PROTO_LOGS_TIMING=<file>.
    let mut timing =
        std::env::var_os("PROTO_LOGS_TIMING").and_then(|p| std::fs::File::create(p).ok());
    let mut cause = "start";
    while !app.quit {
        let started = std::time::Instant::now();
        BYTES_WRITTEN.store(0, std::sync::atomic::Ordering::Relaxed);
        terminal.draw(|f| draw(f, app))?;
        if let Some(file) = timing.as_mut() {
            use std::io::Write as _;
            let bytes = BYTES_WRITTEN.load(std::sync::atomic::Ordering::Relaxed);
            let mode = match (app.menu.is_some(), app.follow) {
                (true, _) => "menu",
                (false, true) => "follow",
                (false, false) => "paused",
            };
            let _ = writeln!(
                file,
                "{} {mode} {cause} {bytes}",
                started.elapsed().as_micros()
            );
        }
        tokio::select! {
            Some(Ok(first)) = events.next() => {
                // Handle every event that is already waiting, then redraw once.
                // Redrawing per event let a moving mouse queue seconds of frames.
                let mut next = Some(first);
                while let Some(event) = next.take() {
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            cause = "key";
                            app.on_key(key, &docker, &tx);
                        }
                        Event::Mouse(mouse) => {
                            cause = if matches!(mouse.kind, MouseEventKind::Moved) { "mouse-move" } else { "mouse" };
                            app.on_mouse(mouse, &docker, &tx);
                        }
                        _ => cause = "other",
                    }
                    if app.quit {
                        break;
                    }
                    next = events.next().now_or_never().flatten().and_then(Result::ok);
                }
            },
            Some(first) = rx.recv() => {
                cause = "lines";
                // Batch everything already queued so a busy log costs one redraw.
                app.push(first);
                while let Ok(more) = rx.try_recv() {
                    app.push(more);
                }
            }
            _ = tick.tick() => cause = "tick",
        }
    }
    if let Some(task) = app.stream.take() {
        task.abort();
    }
    Ok(())
}

impl App {
    fn push(&mut self, (generation, line): (u64, String)) {
        if generation != self.generation {
            return;
        }
        self.lines.push_back(line);
        if self.lines.len() > MAX_LINES {
            self.lines.pop_front();
            if self.top == 0 {
                self.skip = 0;
            }
            self.top = self.top.saturating_sub(1);
        }
    }

    fn matches(&self, line: &str) -> bool {
        !self.query.is_empty() && line.contains(&self.query)
    }

    /// Screen rows one log line takes at the current width and wrap setting.
    fn height(&self, i: usize) -> u16 {
        row_height(&render_line(&self.lines[i]), self.body.width, self.wrap)
    }

    /// Scrolls the view by `delta` screen rows. Scrolling up pauses; reaching
    /// the bottom again resumes following.
    fn scroll(&mut self, delta: i32) {
        if self.lines.is_empty() {
            return;
        }
        if self.follow {
            // Start from the position the follow view is showing.
            let (top, skip) = self.bottom_anchor();
            self.top = top;
            self.skip = skip;
            self.follow = false;
        }
        if delta < 0 {
            for _ in 0..delta.unsigned_abs() {
                if self.skip > 0 {
                    self.skip -= 1;
                } else if self.top > 0 {
                    self.top -= 1;
                    self.skip = self.height(self.top).saturating_sub(1);
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..delta {
                if self.at_bottom() {
                    break;
                }
                if self.skip + 1 < self.height(self.top) {
                    self.skip += 1;
                } else {
                    self.top += 1;
                    self.skip = 0;
                }
            }
            if self.at_bottom() {
                self.follow = true;
            }
        }
    }

    /// True when the rows from (top, skip) to the end fit on screen.
    fn at_bottom(&self) -> bool {
        let mut rows = self.height(self.top).saturating_sub(self.skip);
        for i in self.top + 1..self.lines.len() {
            rows = rows.saturating_add(self.height(i));
            if rows > self.body.height {
                return false;
            }
        }
        rows <= self.body.height
    }

    /// (top, skip) that puts the newest line on the bottom row.
    fn bottom_anchor(&self) -> (usize, u16) {
        let mut used = 0u16;
        let mut i = self.lines.len();
        while i > 0 {
            let h = self.height(i - 1);
            if used + h >= self.body.height {
                return (i - 1, h - (self.body.height - used));
            }
            used += h;
            i -= 1;
        }
        (0, 0)
    }

    fn on_key(
        &mut self,
        key: KeyEvent,
        docker: &Docker,
        tx: &mpsc::UnboundedSender<(u64, String)>,
    ) {
        if let Some(search) = &mut self.search {
            match key.code {
                KeyCode::Enter => {
                    self.query = search.lines().join("");
                    self.search = None;
                    self.jump(true);
                }
                KeyCode::Esc => self.search = None,
                _ => {
                    search.input(key);
                }
            }
            return;
        }
        if let Some((at, sel)) = self.menu {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.menu = Some((at, sel.saturating_sub(1))),
                KeyCode::Down | KeyCode::Char('j') => self.menu = Some((at, (sel + 1).min(1))),
                KeyCode::Enter => {
                    self.menu = None;
                    self.perform(self.menu_items()[sel].2);
                }
                KeyCode::Char('w') => {
                    self.menu = None;
                    self.perform(Action::Wrap);
                }
                KeyCode::Char('f') => {
                    self.menu = None;
                    self.perform(Action::Follow);
                }
                _ => self.menu = None,
            }
            return;
        }
        match self.screen {
            Screen::Pick => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.pick.select_next(),
                KeyCode::Up | KeyCode::Char('k') => self.pick.select_previous(),
                KeyCode::Enter => self.open_selected(docker, tx),
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Screen::View => match key.code {
                KeyCode::Esc => self.back_to_picker(),
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('f') => self.perform(Action::Follow),
                KeyCode::Char('w') => self.perform(Action::Wrap),
                KeyCode::Char('m') | KeyCode::Menu => {
                    self.menu = Some((Position::new(self.body.x + 20, self.body.y + 1), 0));
                }
                KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
                KeyCode::PageUp => self.scroll(-i32::from(self.body.height.max(2) - 1)),
                KeyCode::PageDown => self.scroll(i32::from(self.body.height.max(2) - 1)),
                KeyCode::Left | KeyCode::Char('h') if !self.wrap => {
                    self.hscroll = self.hscroll.saturating_sub(HSTEP)
                }
                KeyCode::Right | KeyCode::Char('l') if !self.wrap => {
                    self.hscroll = self.hscroll.saturating_add(HSTEP)
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    self.follow = false;
                    self.top = 0;
                    self.skip = 0;
                    self.hscroll = 0;
                }
                KeyCode::End | KeyCode::Char('G') => self.follow = true,
                KeyCode::Char('/') => {
                    let mut t = TextArea::default();
                    t.set_block(Block::bordered().title(" Search · Enter find · Esc cancel "));
                    self.search = Some(t);
                }
                KeyCode::Char('n') => self.jump(true),
                KeyCode::Char('N') => self.jump(false),
                _ => {}
            },
        }
    }

    fn on_mouse(
        &mut self,
        m: MouseEvent,
        docker: &Docker,
        tx: &mpsc::UnboundedSender<(u64, String)>,
    ) {
        let pos = Position::new(m.column, m.row);
        if let Some((at, _)) = self.menu {
            let area = self.menu_area;
            let item =
                (area.contains(pos) && m.row > area.y && m.row < area.bottom().saturating_sub(1))
                    .then(|| usize::from(m.row - area.y - 1));
            match m.kind {
                MouseEventKind::Moved => {
                    if let Some(sel) = item {
                        self.menu = Some((at, sel));
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.menu = None;
                    if let Some(action) =
                        item.and_then(|i| self.menu_items().get(i).map(|(_, _, a)| *a))
                    {
                        self.perform(action);
                    }
                }
                MouseEventKind::Down(_) => self.menu = None,
                _ => {}
            }
            return;
        }
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match self.screen {
            Screen::Pick => {
                let row = (self.pick_area.contains(pos) && m.row > self.pick_area.y)
                    .then(|| self.pick.offset() + usize::from(m.row - self.pick_area.y - 1))
                    .filter(|r| *r < self.sources.len());
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(r) = row {
                            if self.pick.selected() == Some(r) {
                                self.open_selected(docker, tx);
                            } else {
                                self.pick.select(Some(r));
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => self.pick.select_next(),
                    MouseEventKind::ScrollUp => self.pick.select_previous(),
                    _ => {}
                }
            }
            Screen::View => match m.kind {
                MouseEventKind::Down(MouseButton::Right) if self.body.contains(pos) => {
                    self.menu = Some((pos, 0))
                }
                // Shift+wheel and horizontal wheels scroll sideways when not wrapping.
                MouseEventKind::ScrollRight if !self.wrap => {
                    self.hscroll = self.hscroll.saturating_add(HSTEP)
                }
                MouseEventKind::ScrollLeft if !self.wrap => {
                    self.hscroll = self.hscroll.saturating_sub(HSTEP)
                }
                MouseEventKind::ScrollDown if shift && !self.wrap => {
                    self.hscroll = self.hscroll.saturating_add(HSTEP)
                }
                MouseEventKind::ScrollUp if shift && !self.wrap => {
                    self.hscroll = self.hscroll.saturating_sub(HSTEP)
                }
                MouseEventKind::ScrollDown if self.body.contains(pos) => self.scroll(3),
                MouseEventKind::ScrollUp if self.body.contains(pos) => self.scroll(-3),
                _ => {}
            },
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Wrap => {
                self.wrap = !self.wrap;
                self.hscroll = 0;
                self.skip = 0;
            }
            Action::Follow => {
                if self.follow {
                    self.scroll(-1);
                } else {
                    self.follow = true;
                }
            }
        }
    }

    fn menu_items(&self) -> [(String, &'static str, Action); 2] {
        [
            (
                format!("Wrap long lines: {}", if self.wrap { "on" } else { "off" }),
                "w",
                Action::Wrap,
            ),
            (
                format!("Follow: {}", if self.follow { "on" } else { "off" }),
                "f",
                Action::Follow,
            ),
        ]
    }

    fn open_selected(&mut self, docker: &Docker, tx: &mpsc::UnboundedSender<(u64, String)>) {
        let Some(source) = self
            .pick
            .selected()
            .and_then(|i| self.sources.get(i))
            .cloned()
        else {
            return;
        };
        if let Some(task) = self.stream.take() {
            task.abort();
        }
        self.generation += 1;
        self.lines.clear();
        self.top = 0;
        self.skip = 0;
        self.hscroll = 0;
        self.follow = true;
        self.query.clear();
        self.stream = Some(spawn_stream(
            docker.clone(),
            source.clone(),
            self.generation,
            tx.clone(),
        ));
        self.current = Some(source);
        self.screen = Screen::View;
    }

    fn back_to_picker(&mut self) {
        if let Some(task) = self.stream.take() {
            task.abort();
        }
        self.generation += 1;
        self.screen = Screen::Pick;
    }

    /// Scrolls the next (or previous) matching line to the top of the view.
    fn jump(&mut self, forward: bool) {
        if self.query.is_empty() {
            return;
        }
        // While following there is nothing after the newest line, so the
        // first search jumps back to the most recent match instead.
        let found = if self.follow {
            (0..self.lines.len())
                .rev()
                .find(|&i| self.matches(&self.lines[i]))
        } else if forward {
            (self.top + 1..self.lines.len()).find(|&i| self.matches(&self.lines[i]))
        } else {
            (0..self.top).rev().find(|&i| self.matches(&self.lines[i]))
        };
        if let Some(i) = found {
            self.follow = false;
            self.top = i;
            self.skip = 0;
        }
    }
}

/// One log line as styled text: ANSI parsed by ansi-to-tui, viewer notices dimmed.
fn render_line(raw: &str) -> Text<'static> {
    if let Some(notice) = raw.strip_prefix(NOTICE) {
        return Text::from(
            Line::from(format!("── {notice} ──")).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            ),
        );
    }
    raw.as_bytes()
        .into_text()
        .unwrap_or_else(|_| Text::raw(raw.to_string()))
}

fn row_height(text: &Text<'static>, width: u16, wrap: bool) -> u16 {
    if !wrap || width == 0 {
        return 1;
    }
    let lines = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(width);
    u16::try_from(lines.max(1)).unwrap_or(u16::MAX)
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    match app.screen {
        Screen::Pick => {
            f.render_widget(Line::from(" ◆ Logs · pick a source ".bold().cyan()), header);
            let items: Vec<ListItem> = app
                .sources
                .iter()
                .map(|s| ListItem::new(s.label()))
                .collect();
            let list = List::new(items)
                .block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .title(" Sources "),
                )
                .highlight_symbol("› ")
                .highlight_style(Style::default().reversed());
            f.render_stateful_widget(list, body, &mut app.pick);
            app.pick_area = body;
            f.render_widget(
                Line::from(" ↑↓ or wheel move   Enter or click again to open   q quit".dim()),
                footer,
            );
        }
        Screen::View => draw_view(f, app, header, body, footer),
    }
}

fn draw_view(f: &mut Frame, app: &mut App, header: Rect, body: Rect, footer: Rect) {
    let name = app.current.as_ref().map(Source::label).unwrap_or_default();
    let hits = app.lines.iter().filter(|l| app.matches(l)).count();
    let block = Block::bordered().border_type(BorderType::Rounded);
    let inner = block.inner(body);
    f.render_widget(block, body);
    app.body = inner;

    // A paused view that would leave rows empty below the newest line (e.g.
    // after turning wrap off) is at the bottom already: follow, which fills it.
    if !app.follow && !app.lines.is_empty() && app.at_bottom() {
        app.follow = true;
    }
    if app.follow {
        let (top, skip) = app.bottom_anchor();
        app.top = top;
        app.skip = skip;
    }
    let mut y = inner.y;
    let mut i = app.top;
    let mut skip = app.skip;
    while i < app.lines.len() && y < inner.bottom() {
        let mut text = render_line(&app.lines[i]);
        let full = row_height(&text, inner.width, app.wrap);
        let h = full.saturating_sub(skip).min(inner.bottom() - y);
        if app.matches(&app.lines[i]) {
            text = text.style(Style::default().bg(Color::Rgb(70, 60, 0)));
        }
        let p = Paragraph::new(text);
        let p = if app.wrap {
            p.wrap(Wrap { trim: false }).scroll((skip, 0))
        } else {
            p.scroll((0, app.hscroll))
        };
        f.render_widget(p, Rect::new(inner.x, y, inner.width, h));
        y += h;
        i += 1;
        skip = 0;
    }

    f.render_widget(
        Line::from(vec![
            format!(" ◆ {name} ").bold().cyan(),
            Span::raw(format!(" {} lines ", app.lines.len())).dim(),
            if app.follow {
                " FOLLOW ".black().on_green()
            } else {
                " PAUSED ".black().on_yellow()
            },
            Span::raw(if app.wrap {
                "  wrap on".to_string()
            } else {
                format!("  wrap off · col {}", app.hscroll + 1)
            })
            .dim(),
            if app.query.is_empty() {
                Span::raw("")
            } else {
                Span::raw(format!("  /{} · {hits} hits", app.query)).yellow()
            },
        ]),
        header,
    );

    if let Some(search) = &app.search {
        let [_, area] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(body);
        f.render_widget(Clear, area);
        f.render_widget(search, area);
    }

    if let Some((at, sel)) = app.menu {
        let items = app.menu_items();
        let width = items
            .iter()
            .map(|(l, k, _)| l.chars().count() + k.len() + 5)
            .max()
            .unwrap_or(10) as u16;
        let height = u16::try_from(items.len()).unwrap_or(0) + 2;
        let area = f.area();
        let menu = Rect::new(
            at.x.min(area.right().saturating_sub(width)),
            at.y.min(area.bottom().saturating_sub(height)),
            width,
            height,
        );
        app.menu_area = menu;
        let inner_w = usize::from(width.saturating_sub(2));
        let lines: Vec<Line> = items
            .iter()
            .enumerate()
            .map(|(i, (label, key, _))| {
                let pad = inner_w.saturating_sub(label.chars().count() + key.len() + 2);
                let line = Line::from(vec![
                    Span::raw(format!(" {label}{}", " ".repeat(pad))),
                    Span::raw(format!("{key} ")).dim(),
                ]);
                if i == sel {
                    line.style(Style::default().fg(Color::Black).bg(Color::Cyan))
                } else {
                    line
                }
            })
            .collect();
        f.render_widget(Clear, menu);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded)),
            menu,
        );
    }

    let hint = if app.wrap {
        " ↑↓/wheel scroll  w wrap off  f follow  / search  n/N next/prev  m or right-click menu  Esc sources  q quit"
    } else {
        " ↑↓/wheel scroll  ←→/shift+wheel sideways  w wrap on  f follow  / search  n/N  m or right-click menu  Esc  q"
    };
    f.render_widget(Line::from(hint.dim()), footer);
}
