//! Prototype: render Saltbox announcement Markdown with tui-markdown (ratatui)
//! and termimad (standalone). Throwaway; see FINDINGS.md.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};
use serde::Deserialize;

#[derive(Parser)]
struct Cli {
    file: PathBuf,
    #[arg(long, value_enum, default_value = "tui-markdown")]
    renderer: Renderer,
    #[arg(long, value_enum, default_value = "inline")]
    mode: Mode,
}

#[derive(Clone, Copy, ValueEnum)]
enum Renderer {
    TuiMarkdown,
    Termimad,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Inline,
    View,
}

#[derive(Deserialize)]
struct AnnouncementFile {
    announcements: Vec<Announcement>,
}

#[derive(Deserialize)]
struct Announcement {
    date: String,
    title: String,
    message: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let file: AnnouncementFile = serde_saphyr::from_str(&std::fs::read_to_string(&cli.file)?)?;
    match (cli.renderer, cli.mode) {
        (Renderer::Termimad, Mode::Inline) => {
            let skin = termimad::MadSkin::default();
            let width = ratatui::crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
            for a in &file.announcements {
                println!("{} · {}", a.date, a.title);
                print!("{}", skin.text(&a.message, Some(width)));
            }
            Ok(())
        }
        (Renderer::Termimad, Mode::View) => {
            anyhow::bail!(
                "not built: termimad's MadView is a second TUI stack next to ratatui (see FINDINGS)"
            )
        }
        (Renderer::TuiMarkdown, Mode::Inline) => inline(&file),
        (Renderer::TuiMarkdown, Mode::View) => view(&file),
    }
}

/// Saltbox look: keep tui-markdown's default styles but drop the literal
/// `##` markers and code fences, closer to glamour's output.
#[derive(Clone)]
struct SaltboxSheet;

impl tui_markdown::StyleSheet for SaltboxSheet {
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code_block_fence(&self) -> &str {
        ""
    }
}

fn render(a: &Announcement) -> Text<'_> {
    let mut text = Text::from(
        Line::from(format!("{} · {}", a.date, a.title))
            .bold()
            .cyan(),
    );
    text.extend(tui_markdown::from_str_with_options(
        &a.message,
        &tui_markdown::Options::new(SaltboxSheet),
    ));
    text
}

/// Prints into normal scrollback via an inline viewport, no alternate screen.
fn inline(file: &AnnouncementFile) -> Result<()> {
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(0),
        },
    )?;
    let width = terminal.size()?.width;
    for a in &file.announcements {
        let paragraph = Paragraph::new(render(a)).wrap(Wrap { trim: false });
        let height = u16::try_from(paragraph.line_count(width)).unwrap_or(u16::MAX);
        terminal.insert_before(height, |buf| paragraph.render(buf.area, buf))?;
    }
    Ok(())
}

fn view(file: &AnnouncementFile) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut index = 0usize;
    let mut scroll = 0u16;
    let result = (|| -> Result<()> {
        loop {
            let a = &file.announcements[index];
            terminal.draw(|f| {
                let [body, footer] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
                let title = format!(" Announcement {}/{} ", index + 1, file.announcements.len());
                let paragraph = Paragraph::new(render(a))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0))
                    .block(
                        Block::bordered()
                            .border_type(BorderType::Rounded)
                            .title(title),
                    );
                f.render_widget(paragraph, body);
                f.render_widget(
                    Line::from(" ↑↓ scroll  ←/→ prev/next  q quit".dim()),
                    footer,
                );
            })?;
            if let Event::Key(k) = event::read()?
                && k.kind == KeyEventKind::Press
            {
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::Right | KeyCode::Char('n') if index + 1 < file.announcements.len() => {
                        index += 1;
                        scroll = 0;
                    }
                    KeyCode::Left | KeyCode::Char('p') if index > 0 => {
                        index -= 1;
                        scroll = 0;
                    }
                    _ => {}
                }
            }
        }
    })();
    ratatui::restore();
    result
}
