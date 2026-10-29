//! Prototype: stream container and systemd logs into a ratatui viewer.
//! The view scrolls directly by screen rows (no selection cursor), long lines
//! wrap or scroll sideways, a right-click menu mirrors the keys, and container
//! restarts are followed instead of ending the stream. Throwaway; see FINDINGS.md.

mod sources;
mod theme;

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::{EventsOptionsBuilder, LogsOptions};
use clap::Parser;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use futures::{FutureExt, StreamExt};
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_textarea::{CursorMove, TextArea};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use sources::{Entry, Health, Section, Source};

// musl's allocator is much slower than glibc's for ratatui's per-frame
// allocations; static release builds use mimalloc instead.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const MAX_LINES: usize = 5000;
/// Ticks and frames per second, the ratatui recipe's defaults.
const TICK_RATE: f64 = 4.0;
const FRAME_RATE: f64 = 60.0;
/// Prefix marking a line written by the viewer itself (stopped/restarted notices).
const NOTICE: char = '\u{1}';
/// Columns moved per sideways scroll step.
const HSTEP: u16 = 8;
/// Services are re-read every this many ticks (5 s) while the picker shows.
const SERVICE_REFRESH_TICKS: u32 = 20;

#[derive(Parser)]
struct Cli {
    /// Open the picker already filtered to this text.
    #[arg(long, default_value = "")]
    filter: String,
    /// systemd unit prefix that counts as a Saltbox service.
    #[arg(long, default_value = "saltbox_managed_")]
    service_prefix: String,
    /// Skip the TUI: stream this source (container name or unit) to stdout.
    #[arg(long)]
    inline: Option<String>,
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

/// One picker line. Only `Item` rows can be selected; ratatui's List has no
/// notion of unselectable rows, so `App::step` skips headers and spacers.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Spacer,
    Header(Section),
    Item(Section, usize),
}

struct App {
    service_prefix: String,
    /// None until the first load finishes.
    entries: Option<Vec<Entry>>,
    rows: Vec<Row>,
    /// Live filter box while typing, and the filter text in effect.
    filter_box: Option<TextArea<'static>>,
    filter: String,
    /// The picker data is out of date and should be reloaded on the next tick.
    stale: bool,
    load_error: Option<String>,
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
    let docker = Docker::connect_with_local_defaults()?;

    if let Some(name) = cli.inline {
        let source = sources::load(&docker, &cli.service_prefix)
            .await?
            .into_iter()
            .map(|e| e.source)
            .find(|s| s.name() == name)
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

    let mut app = App::new(cli.service_prefix, cli.filter);
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app, docker).await;
    ratatui::restore();
    result
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

/// Events from the terminal reader task.
enum AppEvent {
    Input(Event),
    Tick,
    Render,
}

/// Reads terminal input on its own task and emits ticks and render requests,
/// following the ratatui async event handler recipe:
/// https://ratatui.rs/recipes/apps/terminal-and-event-handler/
/// (EventStream read via `next().fuse()` inside `tokio::select!`, forwarded
/// over an unbounded mpsc channel; tick and render on their own intervals).
fn spawn_event_task(tx: mpsc::UnboundedSender<AppEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / TICK_RATE));
        let mut render = tokio::time::interval(Duration::from_secs_f64(1.0 / FRAME_RATE));
        loop {
            let crossterm_event = reader.next().fuse();
            let event = tokio::select! {
                maybe_event = crossterm_event => match maybe_event {
                    Some(Ok(event)) => AppEvent::Input(event),
                    Some(Err(_)) | None => break,
                },
                _ = tick.tick() => AppEvent::Tick,
                _ = render.tick() => AppEvent::Render,
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    })
}

fn spawn_load(
    docker: Docker,
    prefix: String,
    tx: mpsc::UnboundedSender<Result<Vec<Entry>, String>>,
) {
    tokio::spawn(async move {
        let result = sources::load(&docker, &prefix)
            .await
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(result);
    });
}

/// Signals on every container event (start, die, health change, ...), from
/// Docker's event stream: https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html#method.events
fn spawn_docker_events(docker: Docker, tx: mpsc::UnboundedSender<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let filters = HashMap::from([("type", vec!["container"])]);
        let opts = EventsOptionsBuilder::new().filters(&filters).build();
        let mut stream = docker.events(Some(opts));
        while let Some(Ok(_)) = stream.next().await {
            if tx.send(()).is_err() {
                return;
            }
        }
    })
}

async fn run(terminal: &mut DefaultTerminal, app: &mut App, docker: Docker) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(u64, String)>();
    let (event_tx, mut events) = mpsc::unbounded_channel::<AppEvent>();
    let event_task = spawn_event_task(event_tx);
    // Picker data loads in the background so the screen appears at once.
    // Docker's event stream marks the list stale when any container changes;
    // services have no such stream here, so they refresh on a slow timer.
    // Loads start on ticks, which also batches bursts of Docker events.
    let (load_tx, mut loads) = mpsc::unbounded_channel::<Result<Vec<Entry>, String>>();
    let (stale_tx, mut stale) = mpsc::unbounded_channel::<()>();
    let docker_events = spawn_docker_events(docker.clone(), stale_tx);
    let mut loading = false;
    let mut ticks: u32 = 0;
    app.stale = true;
    // Opt-in frame timing for performance work: PROTO_LOGS_TIMING=<file>.
    let mut timing =
        std::env::var_os("PROTO_LOGS_TIMING").and_then(|p| std::fs::File::create(p).ok());
    // Input and new lines only mark the screen dirty; drawing happens on the
    // next Render event, so bursts of events cost at most one frame each tick.
    let mut dirty = true;
    while !app.quit {
        tokio::select! {
            Some(event) = events.recv() => match event {
                AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    app.on_key(key, &docker, &tx);
                    dirty = true;
                }
                AppEvent::Input(Event::Mouse(mouse)) => {
                    // Plain pointer motion only matters while the menu is open.
                    if !matches!(mouse.kind, MouseEventKind::Moved) || app.menu.is_some() {
                        app.on_mouse(mouse, &docker, &tx);
                        dirty = true;
                    }
                }
                AppEvent::Input(Event::Resize(..)) => dirty = true,
                AppEvent::Tick => {
                    ticks = ticks.wrapping_add(1);
                    if ticks.is_multiple_of(SERVICE_REFRESH_TICKS) {
                        app.stale = true;
                    }
                    if app.stale && !loading && matches!(app.screen, Screen::Pick) {
                        app.stale = false;
                        loading = true;
                        spawn_load(docker.clone(), app.service_prefix.clone(), load_tx.clone());
                    }
                }
                AppEvent::Input(_) => {}
                AppEvent::Render if dirty => {
                    let started = std::time::Instant::now();
                    terminal.draw(|f| draw(f, app))?;
                    dirty = false;
                    if let Some(file) = timing.as_mut() {
                        use std::io::Write as _;
                        let _ = writeln!(file, "{}", started.elapsed().as_micros());
                    }
                }
                AppEvent::Render => {}
            },
            Some(line) = rx.recv() => {
                app.push(line);
                dirty = true;
            }
            Some(result) = loads.recv() => {
                loading = false;
                app.set_entries(result);
                dirty = true;
            }
            Some(()) = stale.recv() => app.stale = true,
        }
    }
    docker_events.abort();
    // Turn mouse reporting off, then keep discarding events from the same
    // EventStream for a moment, so mouse codes already in flight don't reach
    // the shell. crossterm forbids mixing EventStream with read()/poll()
    // (https://docs.rs/crossterm/0.29.0/crossterm/event/index.html), so the
    // drain goes through the event task rather than a separate read loop.
    // UNVERIFIED pattern: no doc covers draining; kept because it measurably
    // stopped the leak (FINDINGS.md).
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    let _ = tokio::time::timeout(Duration::from_millis(150), async {
        while events.recv().await.is_some() {}
    })
    .await;
    event_task.abort();
    if let Some(task) = app.stream.take() {
        task.abort();
    }
    Ok(())
}

impl App {
    fn new(service_prefix: String, filter: String) -> Self {
        Self {
            service_prefix,
            entries: None,
            rows: Vec::new(),
            filter_box: None,
            filter,
            stale: true,
            load_error: None,
            pick: ListState::default(),
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
        }
    }

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
        // Typing in the picker filter narrows the list on every keystroke.
        if let Some(filter_box) = &mut self.filter_box {
            match key.code {
                KeyCode::Enter => self.open_selected(docker, tx),
                KeyCode::Esc => {
                    self.filter_box = None;
                    self.filter.clear();
                    self.rebuild_rows();
                }
                KeyCode::Down => self.step(1),
                KeyCode::Up => self.step(-1),
                _ => {
                    if filter_box.input(key) {
                        self.filter = filter_box.lines().join("");
                        self.rebuild_rows();
                    }
                }
            }
            return;
        }
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
            Screen::Pick => {
                let page = isize::try_from(self.pick_area.height.max(2) - 1).unwrap_or(1);
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => self.step(1),
                    KeyCode::Up | KeyCode::Char('k') => self.step(-1),
                    KeyCode::PageDown => self.step(page),
                    KeyCode::PageUp => self.step(-page),
                    KeyCode::End | KeyCode::Char('G') => self.step(isize::MAX),
                    KeyCode::Home | KeyCode::Char('g') => self.step(-isize::MAX),
                    KeyCode::Enter => self.open_selected(docker, tx),
                    KeyCode::Char('/') => self.open_filter(),
                    KeyCode::Esc if !self.filter.is_empty() => {
                        self.filter.clear();
                        self.rebuild_rows();
                    }
                    KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                    _ => {}
                }
            }
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
                // The list has no border: screen row = list row - offset.
                let row = self
                    .pick_area
                    .contains(pos)
                    .then(|| self.pick.offset() + usize::from(m.row - self.pick_area.y))
                    .filter(|r| matches!(self.rows.get(*r), Some(Row::Item(..))));
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
                    MouseEventKind::ScrollDown => self.step(1),
                    MouseEventKind::ScrollUp => self.step(-1),
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
        let Some(source) = self.selected_source() else {
            return;
        };
        self.filter_box = None;
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
        // Statuses may have changed while viewing.
        self.stale = true;
        self.rebuild_rows();
    }

    fn set_entries(&mut self, result: Result<Vec<Entry>, String>) {
        match result {
            Ok(entries) => {
                self.entries = Some(entries);
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(e),
        }
        self.rebuild_rows();
    }

    /// Rebuilds the picker rows after the data or filter
    /// changed, keeping the cursor on the same source when it is still shown.
    fn rebuild_rows(&mut self) {
        let before = self.pick.selected().and_then(|i| self.rows.get(i).copied());
        let before_source = self.selected_source();
        let entries = self.entries.as_deref().unwrap_or_default();
        self.rows.clear();
        for (section, items) in sources::sections(entries, &self.filter) {
            if !self.rows.is_empty() {
                self.rows.push(Row::Spacer);
            }
            self.rows.push(Row::Header(section));
            self.rows
                .extend(items.into_iter().map(|i| Row::Item(section, i)));
        }
        let same_row = |r: &Row| match (r, before) {
            (Row::Item(s, i), Some(Row::Item(bs, _))) => {
                *s == bs && Some(&entries[*i].source) == before_source.as_ref()
            }
            _ => false,
        };
        let same_source = |r: &Row| matches!(r, Row::Item(_, i) if Some(&entries[*i].source) == before_source.as_ref());
        let first_item = |r: &Row| matches!(r, Row::Item(..));
        let at = self
            .rows
            .iter()
            .position(same_row)
            .or_else(|| self.rows.iter().position(same_source))
            .or_else(|| self.rows.iter().position(first_item));
        self.pick.select(at);
    }

    fn open_filter(&mut self) {
        let mut t = TextArea::new(vec![self.filter.clone()]);
        t.move_cursor(CursorMove::End);
        self.filter_box = Some(t);
    }

    fn selected_source(&self) -> Option<Source> {
        let entries = self.entries.as_ref()?;
        match self.rows.get(self.pick.selected()?)? {
            Row::Item(_, i) => Some(entries[*i].source.clone()),
            _ => None,
        }
    }

    /// Moves the cursor `delta` selectable rows (headers and spacers are
    /// skipped), stopping at the first and last item.
    fn step(&mut self, delta: isize) {
        let Some(mut at) = self.pick.selected() else {
            return;
        };
        let mut left = delta.unsigned_abs();
        let mut i = at;
        while left > 0 {
            let next = if delta < 0 {
                i.checked_sub(1)
            } else {
                Some(i + 1).filter(|n| *n < self.rows.len())
            };
            let Some(n) = next else { break };
            i = n;
            if matches!(self.rows[i], Row::Item(..)) {
                at = i;
                left -= 1;
            }
        }
        self.pick.select(Some(at));
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
                    .fg(theme::WARN)
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
    // Base text colour for the whole screen; the background stays the
    // terminal's own, so transparent and themed terminals keep theirs.
    f.buffer_mut().set_style(area, Style::new().fg(theme::TEXT));
    match app.screen {
        Screen::Pick => draw_picker(f, app, header, body, footer),
        Screen::View => draw_view(f, app, header, body, footer),
    }
}

fn health_color(status: &sources::Status) -> Color {
    match (status.health, status.symbol) {
        // Restarting and leftover units need a look, but aren't broken.
        (Health::Problem, '↻' | '!') | (Health::Busy, _) => theme::WARN,
        (Health::Problem, _) => theme::PROBLEM,
        (Health::Ok, _) => theme::OK,
        (Health::Idle, _) => theme::IDLE,
    }
}

fn kind_color(source: &Source) -> Color {
    match source {
        Source::Service(_) => theme::SERVICE,
        Source::Container(_) => theme::CONTAINER,
    }
}

/// A section's title colour and the darker tone of its rule.
fn section_colors(section: Section) -> (Color, Color) {
    match section {
        Section::Attention => (theme::PROBLEM, theme::ATTENTION_RULE),
        Section::Services => (theme::SERVICE, theme::SERVICE_RULE),
        Section::Containers => (theme::CONTAINER, theme::CONTAINER_RULE),
    }
}

/// Footer key hints: keys in colour, what they do in the default colour.
fn key_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in hints {
        spans.push(Span::styled(
            (*key).to_string(),
            Style::new().fg(theme::ACCENT).bold(),
        ));
        spans.push(Span::styled(
            format!(" {what}   "),
            Style::new().fg(theme::MUTED),
        ));
    }
    Line::from(spans)
}

fn draw_picker(f: &mut Frame, app: &mut App, header: Rect, body: Rect, footer: Rect) {
    let entries = app.entries.as_deref().unwrap_or_default();
    let count = |f: &dyn Fn(&Entry) -> bool| entries.iter().filter(|e| f(e)).count();
    let services = count(&|e| matches!(e.source, Source::Service(_)));
    let problems = count(&|e| e.status.health == Health::Problem);
    let mut title = vec![
        " ◆ Logs ".bold().fg(theme::ON_ACCENT).bg(theme::ACCENT),
        Span::raw("  "),
    ];
    if app.entries.is_some() {
        title.push(format!("{services} services").fg(theme::SERVICE));
        title.push(" · ".fg(theme::FAINT));
        title.push(format!("{} containers", entries.len() - services).fg(theme::CONTAINER));
        title.push(" · ".fg(theme::FAINT));
        title.push(match problems {
            0 => "all healthy".fg(theme::OK),
            1 => "1 needs attention".fg(theme::PROBLEM).bold(),
            n => format!("{n} need attention").fg(theme::PROBLEM).bold(),
        });
    }
    f.render_widget(Line::from(title), header);

    // One blank row above the list, one column in from the edge.
    let area = body.inner(ratatui::layout::Margin::new(1, 0));
    let area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    app.pick_area = area;
    let message = if let Some(e) = &app.load_error {
        Some(Line::from(
            format!("Could not list sources: {e}").fg(theme::PROBLEM),
        ))
    } else if app.entries.is_none() {
        Some(Line::from("Loading sources…".fg(theme::MUTED)))
    } else if app.rows.is_empty() && !app.filter.is_empty() {
        Some(Line::from(format!(
            "Nothing matches \"{}\". Esc clears the filter.",
            app.filter
        )))
    } else if app.rows.is_empty() {
        Some(Line::from("No Saltbox services or containers found."))
    } else {
        None
    };
    if let Some(message) = message {
        f.render_widget(Paragraph::new(message), area);
    } else {
        let name_width = entries
            .iter()
            .map(|e| e.source.name().chars().count())
            .max()
            .unwrap_or(0);
        let status_width = entries
            .iter()
            .map(|e| e.status.text.chars().count())
            .max()
            .unwrap_or(0);
        let in_section = |section: Section| {
            app.rows
                .iter()
                .filter(|r| matches!(r, Row::Item(s, _) if *s == section))
                .count()
        };
        // Header rules run to the right edge (less the highlight column).
        let rule_width = usize::from(area.width.saturating_sub(1));
        let items: Vec<ListItem> = app
            .rows
            .iter()
            .map(|row| match *row {
                Row::Spacer => ListItem::new(""),
                Row::Header(section) => {
                    let (color, rule_color) = section_colors(section);
                    let title = format!("{} ({}) ", section.title(), in_section(section));
                    let rule = "─".repeat(rule_width.saturating_sub(title.chars().count()));
                    ListItem::new(Line::from(vec![
                        title.bold().fg(color),
                        Span::styled(rule, Style::new().fg(rule_color)),
                    ]))
                }
                Row::Item(section, i) => {
                    let e = &entries[i];
                    let color = health_color(&e.status);
                    let name = e.source.name();
                    // The shared Saltbox prefix is dimmed so the part that
                    // differs stands out; the full name still filters.
                    let (prefix, rest) = match name.strip_prefix(app.service_prefix.as_str()) {
                        Some(rest) if matches!(e.source, Source::Service(_)) => {
                            (app.service_prefix.as_str(), rest)
                        }
                        _ => ("", name),
                    };
                    let pad = name_width.saturating_sub(name.chars().count());
                    // "running · 2d": the state word in the health colour,
                    // the detail after it in the default colour.
                    let (word, detail) = match e.status.text.split_once(" · ") {
                        Some((word, detail)) => (word, format!(" · {detail}")),
                        None => (e.status.text.as_str(), String::new()),
                    };
                    let mut spans = vec![
                        Span::raw("  "),
                        Span::styled(e.status.symbol.to_string(), Style::new().fg(color).bold()),
                        Span::raw(" "),
                        Span::styled(prefix.to_string(), Style::new().fg(theme::FAINT)),
                        Span::styled(rest.to_string(), Style::new().bold()),
                        Span::raw(" ".repeat(pad + 3)),
                        Span::styled(word.to_string(), Style::new().fg(color)),
                        Span::raw(detail),
                    ];
                    // Needs attention mixes kinds, so say which one this is.
                    if section == Section::Attention {
                        let pad = status_width.saturating_sub(e.status.text.chars().count());
                        spans.push(Span::raw(" ".repeat(pad + 2)));
                        spans.push(e.source.kind().fg(kind_color(&e.source)));
                    }
                    ListItem::new(Line::from(spans))
                }
            })
            .collect();
        let list = List::new(items)
            .highlight_symbol("›")
            .highlight_style(theme::SELECTED);
        f.render_stateful_widget(list, area, &mut app.pick);
    }

    if let Some(filter_box) = &app.filter_box {
        let [label, input] =
            Layout::horizontal([Constraint::Length(9), Constraint::Fill(1)]).areas(footer);
        f.render_widget(Line::from(" Filter: ".bold().fg(theme::ACCENT)), label);
        f.render_widget(filter_box, input);
    } else {
        let mut keys = key_hints(&[
            ("↑↓", "move"),
            ("Enter", "or click again to open"),
            ("/", "filter"),
            ("q", "quit"),
        ]);
        if !app.filter.is_empty() {
            keys.push_span(format!("filter \"{}\" · Esc clears", app.filter).fg(theme::HIGHLIGHT));
        }
        f.render_widget(keys, footer);
    }
}

fn draw_view(f: &mut Frame, app: &mut App, header: Rect, body: Rect, footer: Rect) {
    let name = app.current.as_ref().map(Source::label).unwrap_or_default();
    let hits = app.lines.iter().filter(|l| app.matches(l)).count();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER));
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
            text = text.style(Style::default().bg(theme::SEARCH_HIT));
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
            format!(" ◆ {name} ")
                .bold()
                .fg(theme::ON_ACCENT)
                .bg(theme::ACCENT),
            Span::raw(format!("  {} lines ", app.lines.len())).fg(theme::MUTED),
            if app.follow {
                " FOLLOW ".bold().fg(theme::ON_ACCENT).bg(theme::OK)
            } else {
                " PAUSED ".bold().fg(theme::ON_ACCENT).bg(theme::WARN)
            },
            Span::raw(if app.wrap {
                "  wrap on".to_string()
            } else {
                format!("  wrap off · col {}", app.hscroll + 1)
            })
            .fg(theme::MUTED),
            if app.query.is_empty() {
                Span::raw("")
            } else {
                Span::raw(format!("  /{} · {hits} hits", app.query)).fg(theme::HIGHLIGHT)
            },
        ]),
        header,
    );

    if let Some(search) = &app.search {
        let [_, area] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(body);
        f.render_widget(Clear, area);
        f.render_widget(search, area);
    }

    // Context menu drawn here rather than with tui-popup: its PopupState::move_to
    // only works after a first render and the size is frozen afterwards
    // (tui-popup 0.7.7 popup_state.rs / popup.rs popup_area), which doesn't suit
    // a menu opened at the cursor with changing labels.
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
                    Span::raw(format!("{key} ")).fg(theme::MUTED),
                ]);
                if i == sel {
                    line.style(theme::SELECTED)
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

    let hints = if app.wrap {
        key_hints(&[
            ("↑↓", "scroll"),
            ("w", "wrap off"),
            ("f", "follow"),
            ("/", "search"),
            ("n/N", "next/prev"),
            ("m", "or right-click menu"),
            ("Esc", "sources"),
            ("q", "quit"),
        ])
    } else {
        key_hints(&[
            ("↑↓", "scroll"),
            ("←→", "sideways"),
            ("w", "wrap on"),
            ("f", "follow"),
            ("/", "search"),
            ("n/N", "next/prev"),
            ("m", "or right-click menu"),
            ("Esc", "sources"),
        ])
    };
    f.render_widget(hints, footer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use sources::{Status, classify_container, classify_unit};

    fn demo_app() -> App {
        let now = 1_000_000;
        let unit = |name: &str, active: &str, sub: &str, changed, timer| sources::Entry {
            source: Source::Service(name.into()),
            status: classify_unit(
                &sources::UnitFacts {
                    active: active.into(),
                    sub: sub.into(),
                    changed,
                    active_since: Some(now - 12 * 86400),
                    next_timer: timer,
                    file_state: None,
                },
                now,
            ),
        };
        let container = |name: &str, state: &str, exit_code, restart_count| sources::Entry {
            source: Source::Container(name.into()),
            status: classify_container(
                &sources::ContainerFacts {
                    state: state.into(),
                    exit_code,
                    restart_count,
                    started: Some(now - 2 * 86400),
                    finished: Some(now - 3 * 3600),
                    ..Default::default()
                },
                now,
            ),
        };
        let mut app = App::new("saltbox_managed_".into(), String::new());
        app.set_entries(Ok(vec![
            unit("docker", "active", "running", None, None),
            unit(
                "saltbox_managed_backup",
                "failed",
                "failed",
                Some(now - 7200),
                None,
            ),
            unit("saltbox_managed_mount", "active", "running", None, None),
            unit(
                "saltbox_managed_update",
                "inactive",
                "dead",
                None,
                Some(now + 10_800),
            ),
            container("plex", "running", 0, 0),
            container("radarr", "restarting", 1, 3),
            container("sonarr", "running", 0, 0),
            container("tdarr", "exited", 0, 0),
        ]));
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        terminal.draw(|f| draw(f, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn picker_groups_sources_with_status() {
        let mut app = demo_app();
        let screen = render(&mut app, 80, 24);
        println!("{screen}");
        assert!(screen.contains("2 need attention"));
        for title in ["NEEDS ATTENTION", "SERVICES", "CONTAINERS"] {
            assert!(screen.contains(title), "missing {title}");
        }
        assert!(screen.contains("failed 2h ago"));
        assert!(screen.contains("restarting (3×)"));
        assert!(screen.contains("scheduled · next run in 3h"));
        assert!(screen.contains("stopped 3h ago"));
        // The cursor starts on the first item, not on a header.
        assert_eq!(
            app.selected_source(),
            Some(Source::Service("saltbox_managed_backup".into()))
        );
    }

    #[test]
    fn cursor_skips_headers_and_survives_reloads_and_filtering() {
        let mut app = demo_app();
        render(&mut app, 80, 24);
        app.step(2); // backup -> radarr -> (spacer and header skipped) docker
        assert_eq!(
            app.selected_source(),
            Some(Source::Service("docker".into()))
        );
        assert!(matches!(
            app.rows[app.pick.selected().unwrap_or_default()],
            Row::Item(Section::Services, _)
        ));
        app.step(-isize::MAX);
        assert_eq!(app.pick.selected(), Some(1));
        app.step(isize::MAX);
        assert_eq!(
            app.selected_source(),
            Some(Source::Container("tdarr".into()))
        );
        // A reload with the same sources keeps the cursor where it was.
        let entries = app.entries.clone().unwrap_or_default();
        app.set_entries(Ok(entries));
        assert_eq!(
            app.selected_source(),
            Some(Source::Container("tdarr".into()))
        );
        // Filtering keeps it when the source is still shown, else moves to the first item.
        app.filter = "son".into();
        app.rebuild_rows();
        assert_eq!(
            app.selected_source(),
            Some(Source::Container("sonarr".into()))
        );
        app.filter = "zzz".into();
        app.rebuild_rows();
        assert_eq!(app.selected_source(), None);
        assert!(render(&mut app, 80, 24).contains("Nothing matches \"zzz\""));
    }

    #[test]
    fn selected_row_is_one_uniform_bar() {
        let mut app = demo_app();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("backend");
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let row = app.pick_area.y
            + u16::try_from(app.pick.selected().unwrap_or_default()).unwrap_or(0)
            - u16::try_from(app.pick.offset()).unwrap_or(0);
        let cells: Vec<_> = (app.pick_area.x..app.pick_area.right())
            .map(|x| &buffer[(x, row)])
            .collect();
        assert!(
            cells[3..].iter().any(|c| c.symbol() == "✗"),
            "not the selected row"
        );
        let (fg, bg) = (cells[0].fg, cells[0].bg);
        assert_ne!(bg, Color::Reset, "selection needs a background");
        for c in &cells {
            assert_eq!((c.fg, c.bg), (fg, bg), "cell {:?} differs", c.symbol());
            assert!(!c.modifier.contains(Modifier::DIM));
            assert!(!c.modifier.contains(Modifier::REVERSED));
        }
    }

    #[test]
    fn status_colours_follow_health() {
        let s = |health, symbol| Status {
            health,
            symbol,
            text: String::new(),
        };
        assert_eq!(health_color(&s(Health::Problem, '✗')), theme::PROBLEM);
        assert_eq!(health_color(&s(Health::Problem, '↻')), theme::WARN);
        assert_eq!(health_color(&s(Health::Ok, '✓')), theme::OK);
        assert_eq!(health_color(&s(Health::Idle, '○')), theme::IDLE);
        assert_eq!(health_color(&s(Health::Problem, '!')), theme::WARN);
    }

    /// Truecolor is the design target (capability map, decision 10): every
    /// cell the screens colour uses a palette RGB value, never a named ANSI
    /// colour whose look depends on the terminal theme.
    #[test]
    fn screens_draw_only_truecolor() {
        let mut app = demo_app();
        let check = |app: &mut App, what: &str| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("backend");
            terminal.draw(|f| draw(f, app)).expect("draw");
            for cell in terminal.backend().buffer().content() {
                for color in [cell.fg, cell.bg] {
                    assert!(
                        matches!(color, Color::Rgb(..) | Color::Reset),
                        "{what}: {:?} drawn in {color:?}",
                        cell.symbol()
                    );
                }
            }
        };
        check(&mut app, "picker");
        app.lines.extend([
            "plain line".to_string(),
            format!("{NOTICE}container stopped"),
        ]);
        app.query = "plain".into();
        app.screen = Screen::View;
        app.menu = Some((Position::new(5, 5), 0));
        check(&mut app, "viewer with menu and search hit");
    }
}
