//! Prototype: stream container and systemd logs into a ratatui viewer.
//! Throwaway; see FINDINGS.md.

use std::collections::VecDeque;
use std::time::Duration;

use ansi_to_tui::IntoText;
use anyhow::{Context, Result};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptions;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use futures::StreamExt;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const MAX_LINES: usize = 5000;

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

struct App {
    sources: Vec<Source>,
    pick: ListState,
    screen: Screen,
    current: Option<Source>,
    lines: VecDeque<String>,
    top: usize,
    height: usize,
    follow: bool,
    search: Option<TextArea<'static>>,
    query: String,
    generation: u64,
    stream: Option<JoinHandle<()>>,
    quit: bool,
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
                Some((_, line)) = rx.recv() => println!("{line}"),
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
        height: 0,
        follow: true,
        search: None,
        query: String::new(),
        generation: 0,
        stream: None,
        quit: false,
    };
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, docker).await;
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

/// Streams one source into the channel, tagged with a generation so lines from
/// a stream the user already left are dropped. Aborting the task kills a
/// journalctl child via kill_on_drop.
fn spawn_stream(
    docker: Docker,
    source: Source,
    generation: u64,
    tx: mpsc::UnboundedSender<(u64, String)>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        match source {
            Source::Container(name) => {
                let opts = LogsOptions {
                    follow: true,
                    stdout: true,
                    stderr: true,
                    tail: "500".into(),
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
                        if tx.send((generation, line.to_string())).is_err() {
                            return;
                        }
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
                    if tx.send((generation, line)).is_err() {
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
    while !app.quit {
        terminal.draw(|f| draw(f, app))?;
        tokio::select! {
            Some(Ok(event)) = events.next() => {
                if let Event::Key(key) = event && key.kind == KeyEventKind::Press {
                    app.on_key(key, &docker, &tx);
                }
            }
            Some(first) = rx.recv() => {
                // Batch everything already queued so a busy log costs one redraw.
                app.push(first);
                while let Ok(more) = rx.try_recv() {
                    app.push(more);
                }
            }
            _ = tick.tick() => {}
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
            self.top = self.top.saturating_sub(1);
        }
    }

    fn max_top(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
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
        match self.screen {
            Screen::Pick => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.pick.select_next(),
                KeyCode::Up | KeyCode::Char('k') => self.pick.select_previous(),
                KeyCode::Enter => {
                    if let Some(source) = self
                        .pick
                        .selected()
                        .and_then(|i| self.sources.get(i))
                        .cloned()
                    {
                        self.open(source, docker, tx);
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Screen::View => match key.code {
                KeyCode::Esc => {
                    if let Some(task) = self.stream.take() {
                        task.abort();
                    }
                    self.generation += 1;
                    self.screen = Screen::Pick;
                }
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('f') => self.follow = !self.follow,
                KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
                KeyCode::PageUp => self.scroll(-(self.height as isize)),
                KeyCode::PageDown => self.scroll(self.height as isize),
                KeyCode::Home | KeyCode::Char('g') => {
                    self.follow = false;
                    self.top = 0;
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

    fn open(&mut self, source: Source, docker: &Docker, tx: &mpsc::UnboundedSender<(u64, String)>) {
        if let Some(task) = self.stream.take() {
            task.abort();
        }
        self.generation += 1;
        self.lines.clear();
        self.top = 0;
        self.follow = true;
        self.stream = Some(spawn_stream(
            docker.clone(),
            source.clone(),
            self.generation,
            tx.clone(),
        ));
        self.current = Some(source);
        self.screen = Screen::View;
    }

    fn scroll(&mut self, delta: isize) {
        if self.follow {
            self.top = self.max_top();
        }
        self.follow = false;
        self.top = self.top.saturating_add_signed(delta).min(self.max_top());
    }

    /// Moves the view to the next (or previous) matching line and stops following.
    fn jump(&mut self, forward: bool) {
        if self.query.is_empty() {
            return;
        }
        let start = if self.follow {
            self.max_top()
        } else {
            self.top
        };
        let found = if forward {
            (start + 1..self.lines.len()).find(|&i| self.matches(&self.lines[i]))
        } else {
            (0..start).rev().find(|&i| self.matches(&self.lines[i]))
        };
        if let Some(i) = found {
            self.follow = false;
            self.top = i.min(self.max_top());
        }
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
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
            f.render_widget(Line::from(" ↑↓ move  Enter open  q quit".dim()), footer);
        }
        Screen::View => {
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
            app.height = inner.height as usize;
            if app.follow {
                app.top = app.max_top();
            }
            let end = (app.top + app.height).min(app.lines.len());
            let lines: Vec<Line> = app
                .lines
                .range(app.top..end)
                .map(|raw| {
                    // ANSI parsing is ansi-to-tui's job; fall back to raw text.
                    let line = raw
                        .as_bytes()
                        .into_text()
                        .ok()
                        .and_then(|t| t.lines.into_iter().next())
                        .unwrap_or_else(|| Line::raw(raw.clone()));
                    if app.matches(raw) {
                        line.style(Style::default().bg(Color::Rgb(70, 60, 0)))
                    } else {
                        line
                    }
                })
                .collect();
            f.render_widget(Paragraph::new(lines).block(block), body);
            if let Some(search) = &app.search {
                let [_, area] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(body);
                f.render_widget(ratatui::widgets::Clear, area);
                f.render_widget(search, area);
            }
            f.render_widget(
                Line::from(
                    " ↑↓/PgUp/PgDn scroll  f follow  / search  n/N next/prev  Esc sources  q quit"
                        .dim(),
                ),
                footer,
            );
        }
    }
}
