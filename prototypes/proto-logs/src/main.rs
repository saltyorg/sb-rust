//! Prototype: stream container and systemd logs into a ratatui viewer.
//! Long lines wrap (toggle), any line can be opened in a popup (JSON is
//! pretty-printed), a right-click menu mirrors the keys, and container
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
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use futures::StreamExt;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const MAX_LINES: usize = 5000;
/// Prefix marking a line written by the viewer itself (stopped/restarted notices).
const NOTICE: char = '\u{1}';

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
    Open,
    Wrap,
    Follow,
}

enum Popup {
    None,
    /// The full text of one line; `scroll` is in rows.
    Open {
        line: usize,
        scroll: u16,
    },
    Menu {
        at: Position,
        sel: usize,
    },
}

struct App {
    sources: Vec<Source>,
    pick: ListState,
    screen: Screen,
    current: Option<Source>,
    lines: VecDeque<String>,
    /// Highlighted line while paused; the newest line while following.
    cursor: usize,
    top: usize,
    follow: bool,
    wrap: bool,
    search: Option<TextArea<'static>>,
    query: String,
    popup: Popup,
    generation: u64,
    stream: Option<JoinHandle<()>>,
    quit: bool,
    // From the last draw, for mouse hits and paging.
    pick_area: Rect,
    body: Rect,
    rows: Vec<(Rect, usize)>,
    menu_area: Rect,
    open_area: Rect,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
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
        cursor: 0,
        top: 0,
        follow: true,
        wrap: true,
        search: None,
        query: String::new(),
        popup: Popup::None,
        generation: 0,
        stream: None,
        quit: false,
        pick_area: Rect::default(),
        body: Rect::default(),
        rows: Vec::new(),
        menu_area: Rect::default(),
        open_area: Rect::default(),
    };
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app, docker).await;
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
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

async fn run(terminal: &mut DefaultTerminal, app: &mut App, docker: Docker) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(u64, String)>();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    // Opt-in frame timing for performance work: PROTO_LOGS_TIMING=<file>.
    let mut timing =
        std::env::var_os("PROTO_LOGS_TIMING").and_then(|p| std::fs::File::create(p).ok());
    let mut cause = "start";
    while !app.quit {
        let started = std::time::Instant::now();
        terminal.draw(|f| draw(f, app))?;
        if let Some(file) = timing.as_mut() {
            use std::io::Write as _;
            let mode = match (&app.popup, app.follow) {
                (Popup::Open { .. }, _) => "popup",
                (Popup::Menu { .. }, _) => "menu",
                (Popup::None, true) => "follow",
                (Popup::None, false) => "paused",
            };
            let _ = writeln!(file, "{} {mode} {cause}", started.elapsed().as_micros());
        }
        tokio::select! {
            Some(Ok(event)) = events.next() => match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    cause = "key";
                    app.on_key(key, &docker, &tx);
                }
                Event::Mouse(mouse) => {
                    cause = if matches!(mouse.kind, MouseEventKind::Moved) { "mouse-move" } else { "mouse" };
                    app.on_mouse(mouse, &docker, &tx);
                }
                _ => cause = "other",
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
            self.cursor = self.cursor.saturating_sub(1);
            self.top = self.top.saturating_sub(1);
        }
    }

    fn last(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }

    fn matches(&self, line: &str) -> bool {
        !self.query.is_empty() && line.contains(&self.query)
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
        match self.popup {
            Popup::Open { line, scroll } => {
                let scroll = match key.code {
                    KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll.saturating_add(10),
                    KeyCode::PageUp => scroll.saturating_sub(10),
                    _ => {
                        self.popup = Popup::None;
                        return;
                    }
                };
                self.popup = Popup::Open { line, scroll };
                return;
            }
            Popup::Menu { at, sel } => {
                let items = self.menu_items();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.popup = Popup::Menu {
                            at,
                            sel: sel.saturating_sub(1),
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.popup = Popup::Menu {
                            at,
                            sel: (sel + 1).min(items.len() - 1),
                        }
                    }
                    KeyCode::Enter => {
                        self.popup = Popup::None;
                        self.perform(items[sel].2);
                    }
                    KeyCode::Char('w') => {
                        self.popup = Popup::None;
                        self.perform(Action::Wrap);
                    }
                    KeyCode::Char('f') => {
                        self.popup = Popup::None;
                        self.perform(Action::Follow);
                    }
                    _ => self.popup = Popup::None,
                }
                return;
            }
            Popup::None => {}
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
                KeyCode::Enter => self.perform(Action::Open),
                KeyCode::Char('m') | KeyCode::Menu => self.open_menu(None),
                KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
                KeyCode::PageUp => self.move_cursor(-(self.body.height.max(1) as isize)),
                KeyCode::PageDown => self.move_cursor(self.body.height.max(1) as isize),
                KeyCode::Home | KeyCode::Char('g') => {
                    self.follow = false;
                    self.cursor = 0;
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
        match self.popup {
            Popup::Open { line, scroll } => {
                match m.kind {
                    MouseEventKind::ScrollDown => {
                        self.popup = Popup::Open {
                            line,
                            scroll: scroll.saturating_add(3),
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        self.popup = Popup::Open {
                            line,
                            scroll: scroll.saturating_sub(3),
                        }
                    }
                    MouseEventKind::Down(_) if !self.open_area.contains(pos) => {
                        self.popup = Popup::None
                    }
                    _ => {}
                }
                return;
            }
            Popup::Menu { at, .. } => {
                let area = self.menu_area;
                let item = (area.contains(pos)
                    && m.row > area.y
                    && m.row < area.bottom().saturating_sub(1))
                .then(|| usize::from(m.row - area.y - 1));
                match m.kind {
                    MouseEventKind::Moved => {
                        if let Some(sel) = item {
                            self.popup = Popup::Menu { at, sel };
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.popup = Popup::None;
                        if let Some(action) =
                            item.and_then(|i| self.menu_items().get(i).map(|(_, _, a)| *a))
                        {
                            self.perform(action);
                        }
                    }
                    MouseEventKind::Down(_) => self.popup = Popup::None,
                    _ => {}
                }
                return;
            }
            Popup::None => {}
        }
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
            Screen::View => {
                let hit = self
                    .rows
                    .iter()
                    .find(|(r, _)| r.contains(pos))
                    .map(|(_, i)| *i);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(i) = hit {
                            if !self.follow && self.cursor == i {
                                self.perform(Action::Open);
                            } else {
                                self.follow = false;
                                self.cursor = i;
                            }
                        }
                    }
                    MouseEventKind::Down(MouseButton::Right) => {
                        if let Some(i) = hit {
                            self.follow = false;
                            self.cursor = i;
                            self.open_menu(Some(pos));
                        }
                    }
                    MouseEventKind::ScrollDown if self.body.contains(pos) => self.move_cursor(3),
                    MouseEventKind::ScrollUp if self.body.contains(pos) => self.move_cursor(-3),
                    _ => {}
                }
            }
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Open => {
                if !self.lines.is_empty() {
                    let line = if self.follow {
                        self.last()
                    } else {
                        self.cursor
                    };
                    self.follow = false;
                    self.cursor = line;
                    self.popup = Popup::Open { line, scroll: 0 };
                }
            }
            Action::Wrap => self.wrap = !self.wrap,
            Action::Follow => {
                self.follow = !self.follow;
                if !self.follow {
                    self.cursor = self.last();
                }
            }
        }
    }

    fn menu_items(&self) -> Vec<(String, &'static str, Action)> {
        vec![
            ("Open line".into(), "Enter", Action::Open),
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

    fn open_menu(&mut self, at: Option<Position>) {
        let at = at
            .or_else(|| {
                self.rows
                    .iter()
                    .find(|(_, i)| *i == self.cursor)
                    .map(|(r, _)| Position::new(r.x + 20, r.y))
            })
            .unwrap_or(Position::new(self.body.x + 20, self.body.y + 1));
        self.popup = Popup::Menu { at, sel: 0 };
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
        self.cursor = 0;
        self.top = 0;
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

    fn move_cursor(&mut self, delta: isize) {
        if self.follow {
            self.cursor = self.last();
        }
        self.follow = false;
        self.cursor = self.cursor.saturating_add_signed(delta).min(self.last());
    }

    /// Moves the highlight to the next (or previous) matching line.
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
            (self.cursor + 1..self.lines.len()).find(|&i| self.matches(&self.lines[i]))
        } else {
            (0..self.cursor)
                .rev()
                .find(|&i| self.matches(&self.lines[i]))
        };
        if let Some(i) = found {
            self.follow = false;
            self.cursor = i;
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

fn plain(raw: &str) -> String {
    render_line(raw)
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect()
}

/// The popup body: the line as-is, plus any JSON in it pretty-printed.
fn open_text(raw: &str) -> Text<'static> {
    let text = plain(raw);
    let json_start = text.find(['{', '[']);
    let pretty = json_start.and_then(|i| {
        let value: serde_json::Value = serde_json::from_str(text[i..].trim()).ok()?;
        Some((i, serde_json::to_string_pretty(&value).ok()?))
    });
    match pretty {
        Some((i, json)) => {
            let mut out = Text::default();
            let prefix = text[..i].trim();
            if !prefix.is_empty() {
                out.lines.push(Line::from(prefix.to_string()).bold());
                out.lines.push(Line::raw(""));
            }
            out.lines
                .extend(json.lines().map(|l| Line::raw(l.to_string())));
            out
        }
        None => render_line(raw),
    }
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
    f.render_widget(
        Line::from(vec![
            format!(" ◆ {name} ").bold().cyan(),
            Span::raw(format!(" {} lines ", app.lines.len())).dim(),
            if app.follow {
                " FOLLOW ".black().on_green()
            } else {
                " PAUSED ".black().on_yellow()
            },
            Span::raw(if app.wrap { "  wrap on" } else { "  wrap off" }).dim(),
            if app.query.is_empty() {
                Span::raw("")
            } else {
                Span::raw(format!("  /{} · {hits} hits", app.query)).yellow()
            },
        ]),
        header,
    );
    let block = Block::bordered().border_type(BorderType::Rounded);
    let inner = block.inner(body);
    f.render_widget(block, body);
    app.body = inner;
    app.rows.clear();

    // Work out which lines fit: from the newest backwards while following, or
    // from `top` (kept so the highlighted line stays visible) while paused.
    let height = inner.height;
    let texts = |i: usize| render_line(&app.lines[i]);
    let mut start_y = inner.y;
    if !app.lines.is_empty() {
        if app.follow {
            let mut used = 0u16;
            let mut top = app.lines.len();
            while top > 0 {
                let h = row_height(&texts(top - 1), inner.width, app.wrap);
                if used + h > height && top < app.lines.len() {
                    break;
                }
                used = used.saturating_add(h);
                top -= 1;
            }
            app.top = top;
            // Sit against the bottom edge like `tail -f`, no gap under the newest line.
            start_y = inner.y + height.saturating_sub(used);
        } else {
            app.cursor = app.cursor.min(app.last());
            if app.cursor < app.top {
                app.top = app.cursor;
            }
            // If the highlight is below the fold, pull `top` down until it fits.
            let mut used: u16 = (app.top..=app.cursor)
                .map(|i| row_height(&texts(i), inner.width, app.wrap))
                .sum();
            while used > height && app.top < app.cursor {
                used -= row_height(&texts(app.top), inner.width, app.wrap);
                app.top += 1;
            }
            // If the lines from `top` to the end leave space (e.g. after turning
            // wrap off), pull older lines in so the screen stays full.
            // Only measure until the screen is full: measuring every line to the
            // end of the buffer made each redraw cost O(buffer) (70 ms in debug
            // builds with 500 long lines, and growing with the buffer).
            let mut below = 0u16;
            for i in app.top..app.lines.len() {
                below = below.saturating_add(row_height(&texts(i), inner.width, app.wrap));
                if below >= height {
                    break;
                }
            }
            while app.top > 0 && below < height {
                let h = row_height(&texts(app.top - 1), inner.width, app.wrap);
                if below + h > height {
                    break;
                }
                below += h;
                app.top -= 1;
            }
        }
        let mut y = start_y;
        let mut i = app.top;
        while i < app.lines.len() && y < inner.bottom() {
            let mut text = texts(i);
            let h = row_height(&text, inner.width, app.wrap).min(inner.bottom() - y);
            if !app.follow && i == app.cursor {
                text = text.style(Style::default().bg(Color::Rgb(40, 60, 90)));
            } else if app.matches(&app.lines[i]) {
                text = text.style(Style::default().bg(Color::Rgb(70, 60, 0)));
            }
            let rect = Rect::new(inner.x, y, inner.width, h);
            let p = Paragraph::new(text);
            f.render_widget(
                if app.wrap {
                    p.wrap(Wrap { trim: false })
                } else {
                    p
                },
                rect,
            );
            app.rows.push((rect, i));
            y += h;
            i += 1;
        }
    }

    if let Some(search) = &app.search {
        let [_, area] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(body);
        f.render_widget(Clear, area);
        f.render_widget(search, area);
    }

    match app.popup {
        Popup::Open { line, scroll } => {
            let [a] = Layout::vertical([Constraint::Percentage(80)])
                .flex(Flex::Center)
                .areas(f.area());
            let [a] = Layout::horizontal([Constraint::Percentage(85)])
                .flex(Flex::Center)
                .areas(a);
            app.open_area = a;
            let text = app
                .lines
                .get(line)
                .map(|l| open_text(l))
                .unwrap_or_default();
            f.render_widget(Clear, a);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0))
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .title(format!(" Line {} of {} ", line + 1, app.lines.len()))
                            .title_bottom(
                                " ↑↓ or wheel scroll · any other key or click outside closes ",
                            ),
                    ),
                a,
            );
        }
        Popup::Menu { at, sel } => {
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
        Popup::None => {}
    }

    f.render_widget(
        Line::from(" ↑↓/wheel move  Enter/click open line  w wrap  f follow  / search  n/N next/prev  m or right-click menu  Esc sources  q quit".dim()),
        footer,
    );
}
