//! Prototype: render Saltbox announcement Markdown with tui-markdown (ratatui)
//! and termimad (standalone). Throwaway; see FINDINGS.md.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use proto_theme as theme;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Paragraph, Widget, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
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

/// Saltbox look on the shared truecolor palette. Every colour tui-markdown
/// uses comes through this trait (tui-markdown 0.3.12 `style_sheet.rs`), so
/// overriding each styled element keeps the output truecolor-only. Heading
/// markers and code fences are dropped, closer to glamour's output.
#[derive(Clone)]
struct SaltboxSheet;

impl tui_markdown::StyleSheet for SaltboxSheet {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::new().fg(theme::ON_ACCENT).bg(theme::ACCENT).bold(),
            2 => Style::new().fg(theme::ACCENT).bold(),
            3 => Style::new().fg(theme::TEXT).bold(),
            _ => Style::new().fg(theme::MUTED).bold().italic(),
        }
    }

    fn code(&self) -> Style {
        Style::new().fg(theme::CODE).bg(theme::CODE_BG)
    }

    fn link(&self) -> Style {
        Style::new().fg(theme::LINK).underlined()
    }

    fn blockquote(&self) -> Style {
        Style::new().fg(theme::MUTED).italic()
    }

    fn heading_meta(&self) -> Style {
        Style::new().fg(theme::FAINT)
    }

    fn metadata_block(&self) -> Style {
        Style::new().fg(theme::MUTED)
    }

    fn html(&self) -> Style {
        Style::new().fg(theme::MUTED)
    }

    fn math_inline(&self) -> Style {
        self.code()
    }

    fn math_display(&self) -> Style {
        self.code()
    }

    fn footnote_ref(&self) -> Style {
        Style::new().fg(theme::LINK)
    }

    fn footnote_def(&self) -> Style {
        Style::new().fg(theme::MUTED)
    }

    fn definition_term(&self) -> Style {
        Style::new().fg(theme::TEXT).bold()
    }

    fn definition_description(&self) -> Style {
        Style::new().fg(theme::TEXT)
    }

    fn alert(&self, kind: tui_markdown::AlertKind) -> Style {
        use tui_markdown::AlertKind;
        let color = match kind {
            AlertKind::Note => theme::ACCENT,
            AlertKind::Tip => theme::OK,
            AlertKind::Important => theme::ACCENT_2,
            AlertKind::Warning => theme::WARN,
            AlertKind::Caution => theme::PROBLEM,
        };
        Style::new().fg(color)
    }

    fn table_header(&self) -> Style {
        Style::new().fg(theme::ACCENT).bold()
    }

    fn table_cell(&self) -> Style {
        Style::new().fg(theme::TEXT)
    }

    fn table_border(&self) -> Style {
        Style::new().fg(theme::BORDER)
    }

    fn list_marker(&self) -> Style {
        Style::new().fg(theme::ACCENT).bold()
    }

    fn image_alt(&self) -> Style {
        Style::new().fg(theme::MUTED).italic()
    }

    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }

    fn code_block_fence(&self) -> &str {
        ""
    }
}

fn render(a: &Announcement) -> Text<'_> {
    let mut text = Text::from(Line::from(vec![
        Span::styled(a.date.clone(), Style::new().fg(theme::MUTED)),
        Span::styled(" · ", Style::new().fg(theme::FAINT)),
        Span::styled(a.title.clone(), Style::new().fg(theme::ACCENT).bold()),
    ]));
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
        let paragraph = Paragraph::new(render(a))
            .style(Style::new().fg(theme::TEXT))
            .wrap(Wrap { trim: false });
        let height = u16::try_from(paragraph.line_count(width)).unwrap_or(u16::MAX);
        terminal.insert_before(height, |buf| paragraph.render(buf.area, buf))?;
    }
    Ok(())
}

fn draw(f: &mut Frame, file: &AnnouncementFile, index: usize, scroll: u16) {
    let a = &file.announcements[index];
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let title = format!(" Announcement {}/{} ", index + 1, file.announcements.len());
    let paragraph = Paragraph::new(render(a))
        .style(Style::new().fg(theme::TEXT))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(theme::BORDER))
                .title(Span::styled(title, Style::new().fg(theme::ACCENT).bold())),
        );
    f.render_widget(paragraph, body);
    f.render_widget(
        theme::key_hints(&[("↑↓", "scroll"), ("←→", "prev/next"), ("q", "quit")]),
        footer,
    );
}

fn view(file: &AnnouncementFile) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut index = 0usize;
    let mut scroll = 0u16;
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw(f, file, index, scroll))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn fixture() -> AnnouncementFile {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/announcements.yml");
        serde_saphyr::from_str(&std::fs::read_to_string(path).expect("fixture"))
            .expect("fixture parses")
    }

    /// Truecolor is the design target (capability map, decision 10): every
    /// coloured cell uses a palette RGB value, never a named ANSI colour.
    #[test]
    fn announcements_draw_only_truecolor() {
        let file = fixture();
        for index in 0..file.announcements.len() {
            let mut terminal = Terminal::new(TestBackend::new(80, 60)).expect("backend");
            terminal.draw(|f| draw(f, &file, index, 0)).expect("draw");
            for cell in terminal.backend().buffer().content() {
                for color in [cell.fg, cell.bg] {
                    assert!(
                        matches!(color, Color::Rgb(..) | Color::Reset),
                        "announcement {index}: {:?} drawn in {color:?}",
                        cell.symbol()
                    );
                }
            }
        }
    }
}
