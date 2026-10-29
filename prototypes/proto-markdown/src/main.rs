//! Prototype: render Saltbox announcement Markdown with tui-markdown (ratatui)
//! and termimad (standalone). The full-screen viewer lists every announcement
//! that applies, new ones marked and older ones tinted, next to the selected
//! one. Throwaway; see FINDINGS.md.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, ValueEnum};
use proto_theme as theme;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Size;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Widget, Wrap};
use ratatui::{DefaultTerminal, Frame, Terminal, TerminalOptions, Viewport};
use serde::Deserialize;
use tui_scrollview::{ScrollView, ScrollViewState};

#[derive(Parser)]
struct Cli {
    /// announcements.yml as it is now (sb-go's `--after`).
    file: PathBuf,
    /// announcements.yml from before the update (sb-go's `--before`).
    /// Announcements not in it are new; without it, all are new, as in sb-go.
    #[arg(long)]
    before: Option<PathBuf>,
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

#[derive(Clone, Deserialize)]
struct Announcement {
    date: String,
    title: String,
    message: String,
    #[serde(default)]
    required_folder: Option<String>,
    #[serde(default)]
    required_file: Option<String>,
}

/// An announcement to show, and whether it arrived with this update.
struct Item {
    announcement: Announcement,
    new: bool,
}

fn load(path: &Path) -> Result<Vec<Announcement>> {
    let file: AnnouncementFile = serde_saphyr::from_str(&std::fs::read_to_string(path)?)?;
    Ok(file.announcements)
}

/// sb-go's rules (saltbox/announcements.go): an announcement is new when its
/// date and title weren't in the file before the update (no before file:
/// all are new), and it only applies when its required folder and file, if
/// set, exist. Unlike sb-go, older ones are kept, so missed announcements
/// can still be read. Newest first.
fn classify(after: Vec<Announcement>, before: Option<&[Announcement]>) -> Vec<Item> {
    let key = |a: &Announcement| format!("{}|{}", a.date, a.title);
    let seen: Option<HashSet<String>> = before.map(|b| b.iter().map(key).collect());
    let applies = |a: &Announcement| {
        a.required_folder
            .as_deref()
            .is_none_or(|p| Path::new(p).is_dir())
            && a.required_file
                .as_deref()
                .is_none_or(|p| Path::new(p).is_file())
    };
    let mut items: Vec<Item> = after
        .into_iter()
        .filter(applies)
        .map(|a| Item {
            new: seen.as_ref().is_none_or(|s| !s.contains(&key(&a))),
            announcement: a,
        })
        .collect();
    items.sort_by(|x, y| y.announcement.date.cmp(&x.announcement.date));
    items
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let before = cli.before.as_deref().map(load).transpose()?;
    let items = classify(load(&cli.file)?, before.as_deref());
    match (cli.renderer, cli.mode) {
        (Renderer::Termimad, Mode::Inline) => {
            let skin = termimad::MadSkin::default();
            let width = ratatui::crossterm::terminal::size().map_or(80, |(w, _)| w as usize);
            for item in items.iter().filter(|i| i.new) {
                let a = &item.announcement;
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
        (Renderer::TuiMarkdown, Mode::Inline) => inline(&items),
        (Renderer::TuiMarkdown, Mode::View) => view(items),
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

fn render(a: &Announcement, new: bool) -> Text<'_> {
    let mut title = vec![
        Span::styled(a.date.clone(), Style::new().fg(theme::MUTED)),
        Span::styled(" · ", Style::new().fg(theme::FAINT)),
        Span::styled(a.title.clone(), Style::new().fg(theme::ACCENT).bold()),
    ];
    if new {
        title.push(Span::raw("  "));
        title.push(Span::styled(
            " NEW ",
            Style::new().fg(theme::ON_ACCENT).bg(theme::OK).bold(),
        ));
    }
    let mut text = Text::from(Line::from(title));
    text.extend(tui_markdown::from_str_with_options(
        &a.message,
        &tui_markdown::Options::new(SaltboxSheet),
    ));
    text
}

/// Prints the new announcements into normal scrollback via an inline
/// viewport, no alternate screen (what `sb update` shows after updating).
fn inline(items: &[Item]) -> Result<()> {
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(0),
        },
    )?;
    let width = terminal.size()?.width;
    for item in items.iter().filter(|i| i.new) {
        let paragraph = Paragraph::new(render(&item.announcement, true))
            .style(Style::new().fg(theme::TEXT))
            .wrap(Wrap { trim: false });
        let height = u16::try_from(paragraph.line_count(width)).unwrap_or(u16::MAX);
        terminal.insert_before(height, |buf| paragraph.render(buf.area, buf))?;
    }
    Ok(())
}

/// Below this width the list goes above the announcement instead of beside it.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 70;

struct Viewer {
    items: Vec<Item>,
    list: ListState,
    /// Scroll position of the announcement. tui-scrollview clamps it while
    /// rendering: content that fits never scrolls, and the furthest position
    /// shows the last line at the bottom (tui-scrollview 0.6.8
    /// `scroll_view.rs`, `render`).
    article: ScrollViewState,
    quit: bool,
    // From the last draw, for mouse hits.
    list_area: Rect,
    /// The list's rows: its area inside the border.
    list_rows: Rect,
}

impl Viewer {
    fn new(items: Vec<Item>) -> Self {
        // Open on the newest new announcement, or the newest one.
        let first = items.iter().position(|i| i.new).unwrap_or(0);
        Self {
            list: ListState::default().with_selected((!items.is_empty()).then_some(first)),
            items,
            article: ScrollViewState::default(),
            quit: false,
            list_area: Rect::default(),
            list_rows: Rect::default(),
        }
    }

    /// Runs a list movement and starts the newly selected announcement at
    /// its top. ListState's own select_* methods do the moving; it clamps the
    /// selection to the list when rendering.
    fn move_list(&mut self, movement: impl FnOnce(&mut ListState)) {
        let before = self.list.selected();
        movement(&mut self.list);
        // ListState::select_next doesn't clamp; List clamps when rendering
        // (ratatui-widgets 0.3.2 list/state.rs, list/rendering.rs). Clamp here
        // too, so moving past the last item isn't seen as a change.
        let last = self.items.len().saturating_sub(1);
        if let Some(selected) = self.list.selected() {
            self.list.select(Some(selected.min(last)));
        }
        if self.list.selected() != before {
            self.article.scroll_to_top();
        }
    }

    fn on_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_list(ListState::select_next),
            KeyCode::Up | KeyCode::Char('k') => self.move_list(ListState::select_previous),
            KeyCode::PageDown | KeyCode::Char(' ') => self.article.scroll_page_down(),
            KeyCode::PageUp | KeyCode::Char('b') => self.article.scroll_page_up(),
            KeyCode::Home | KeyCode::Char('g') => self.article.scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => self.article.scroll_to_bottom(),
            _ => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        let in_list = self.list_area.contains(pos);
        match m.kind {
            // ratatui's List has no hit-testing (none in ratatui-widgets 0.3.2
            // list/), so rows are mapped here: only
            // clicks inside the block (Block::inner) count, offset by the
            // list's scroll.
            MouseEventKind::Down(MouseButton::Left) if self.list_rows.contains(pos) => {
                let row = self.list.offset() + usize::from(m.row - self.list_rows.y);
                if row < self.items.len() {
                    self.move_list(|l| l.select(Some(row)));
                }
            }
            MouseEventKind::ScrollDown if in_list => self.move_list(ListState::select_next),
            MouseEventKind::ScrollUp if in_list => self.move_list(ListState::select_previous),
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    self.article.scroll_down();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    self.article.scroll_up();
                }
            }
            _ => {}
        }
    }
}

fn draw(f: &mut Frame, v: &mut Viewer) {
    let [body, footer] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let new_count = v.items.iter().filter(|i| i.new).count();
    let rows: Vec<ListItem> = v
        .items
        .iter()
        .map(|i| {
            let a = &i.announcement;
            // New ones get a marker and full-strength text; ones seen before
            // are tinted, so the eye goes to what's new first.
            let line = if i.new {
                Line::from(vec![
                    Span::styled("● ", Style::new().fg(theme::OK)),
                    Span::styled(format!("{}  ", a.date), Style::new().fg(theme::MUTED)),
                    Span::styled(a.title.clone(), Style::new().fg(theme::TEXT).bold()),
                ])
            } else {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{}  {}", a.date, a.title),
                        Style::new().fg(theme::FAINT),
                    ),
                ])
            };
            ListItem::new(line)
        })
        .collect();
    // Wide enough for the widest row (ListItem::width) plus the borders, up
    // to 40% of the screen.
    let widest = rows.iter().map(ListItem::width).max().unwrap_or(0);
    let [list_area, article_area] = if body.width >= SIDE_BY_SIDE_MIN_WIDTH {
        let width = u16::try_from(widest + 2)
            .unwrap_or(u16::MAX)
            .clamp(24, body.width * 2 / 5);
        Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(body)
    } else {
        let height = u16::try_from(v.items.len() + 2)
            .unwrap_or(u16::MAX)
            .min(body.height / 3);
        Layout::vertical([Constraint::Length(height), Constraint::Fill(1)]).areas(body)
    };
    v.list_area = list_area;

    let list_title = if new_count == 0 {
        Line::from(Span::styled(
            " Announcements ",
            Style::new().fg(theme::ACCENT).bold(),
        ))
    } else {
        Line::from(vec![
            Span::styled(" Announcements ", Style::new().fg(theme::ACCENT).bold()),
            Span::styled(format!("· {new_count} new "), Style::new().fg(theme::OK)),
        ])
    };
    let list_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(list_title);
    v.list_rows = list_block.inner(list_area);
    let list = List::new(rows)
        .block(list_block)
        .highlight_style(theme::SELECTED);
    f.render_stateful_widget(list, list_area, &mut v.list);

    // tui-scrollview's scrollbar has no style of its own (Scrollbar::new's
    // styles are empty), so it keeps the colour already in its cells: the
    // block's style. The content itself is copied over from the scroll view's
    // buffer and keeps the paragraph's style.
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .style(Style::new().fg(theme::FAINT));
    let inner = block.inner(article_area);
    f.render_widget(block, article_area);
    if let Some(item) = v.list.selected().and_then(|i| v.items.get(i)) {
        let paragraph = Paragraph::new(render(&item.announcement, item.new))
            .style(Style::new().fg(theme::TEXT))
            .wrap(Wrap { trim: false });
        // The scroll view needs the content size up front. When the text is
        // taller than the view, a scrollbar takes one column, so measure again
        // at that width.
        let mut width = inner.width;
        let mut lines = paragraph.line_count(width);
        if lines > usize::from(inner.height) {
            width = width.saturating_sub(1);
            lines = paragraph.line_count(width);
        }
        let height = u16::try_from(lines).unwrap_or(u16::MAX);
        let mut scroll_view = ScrollView::new(Size::new(width, height));
        scroll_view.render_widget(paragraph, scroll_view.area());
        f.render_stateful_widget(scroll_view, inner, &mut v.article);
    } else {
        f.render_widget(
            Paragraph::new("No announcements apply to this system.")
                .style(Style::new().fg(theme::MUTED)),
            inner,
        );
    }

    let mut hints = theme::key_hints(&[
        ("↑↓", "announcement"),
        ("PgUp/PgDn", "scroll"),
        ("q", "close"),
    ]);
    if new_count > 0 {
        hints.push_span(Span::styled("● ", Style::new().fg(theme::OK)));
        hints.push_span(Span::styled("new", Style::new().fg(theme::MUTED)));
    }
    f.render_widget(hints, footer);
}

fn view(items: Vec<Item>) -> Result<()> {
    let mut viewer = Viewer::new(items);
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut viewer);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    drain_input();
    ratatui::restore();
    result
}

/// Same loop as proto-facts: handle every waiting event, then redraw once,
/// using crossterm's documented blocking read() plus poll() on one thread:
/// https://docs.rs/crossterm/0.29.0/crossterm/event/index.html
fn run(terminal: &mut DefaultTerminal, v: &mut Viewer) -> Result<()> {
    while !v.quit {
        terminal.draw(|f| draw(f, v))?;
        let mut event = event::read()?;
        loop {
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => v.on_key(key.code),
                Event::Mouse(mouse) => v.on_mouse(mouse),
                _ => {}
            }
            if v.quit || !event::poll(std::time::Duration::ZERO)? {
                break;
            }
            event = event::read()?;
        }
    }
    Ok(())
}

/// Reads and discards input still in flight after mouse capture is turned
/// off, so stray mouse codes don't reach the shell (as in proto-facts).
/// UNVERIFIED pattern: no doc covers draining; it measurably stopped the leak
/// in proto-facts.
fn drain_input() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
    while std::time::Instant::now() < deadline {
        if let Ok(true) = event::poll(std::time::Duration::from_millis(20)) {
            let _ = event::read();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn fixture(name: &str) -> Vec<Announcement> {
        load(Path::new(&format!(
            "{}/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        )))
        .expect("fixture")
    }

    fn announcement(date: &str, title: &str, message: &str) -> Announcement {
        Announcement {
            date: date.into(),
            title: title.into(),
            message: message.into(),
            required_folder: None,
            required_file: None,
        }
    }

    fn draw_at(v: &mut Viewer, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
        terminal.draw(|f| draw(f, v)).expect("draw");
        terminal
    }

    fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn new_means_not_in_the_before_file_by_date_and_title() {
        let before = vec![announcement("2026-01-01", "Old", "")];
        let after = vec![
            announcement("2026-01-01", "Old", ""),
            announcement("2026-01-01", "Same day, new title", ""),
            announcement("2026-02-01", "Newer", ""),
        ];
        let items = classify(after.clone(), Some(&before));
        let got: Vec<(&str, bool)> = items
            .iter()
            .map(|i| (i.announcement.title.as_str(), i.new))
            .collect();
        // Newest first; within a day, file order.
        assert_eq!(
            got,
            vec![
                ("Newer", true),
                ("Old", false),
                ("Same day, new title", true)
            ]
        );
        // No before file: everything is new, as in sb-go.
        assert!(classify(after, None).iter().all(|i| i.new));
    }

    #[test]
    fn announcements_for_missing_folders_or_files_do_not_apply() {
        let mut a = announcement("2026-01-01", "Needs folder", "");
        a.required_folder = Some("/nonexistent-sb-rust-test".into());
        let mut b = announcement("2026-01-01", "Needs file", "");
        b.required_file = Some("/".into()); // a directory, not a file
        let mut c = announcement("2026-01-01", "Applies", "");
        c.required_folder = Some("/".into());
        let items = classify(vec![a, b, c], None);
        let titles: Vec<&str> = items
            .iter()
            .map(|i| i.announcement.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Applies"]);
    }

    #[test]
    fn a_short_announcement_does_not_scroll() {
        let mut v = Viewer::new(classify(
            vec![announcement("2026-01-01", "Short", "One line.")],
            None,
        ));
        draw_at(&mut v, 100, 30);
        v.on_key(KeyCode::PageDown);
        v.on_key(KeyCode::Char('G'));
        v.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 80,
            row: 10,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        let terminal = draw_at(&mut v, 100, 30);
        assert_eq!(v.article.offset().y, 0);
        assert!(
            screen(&terminal)[1].contains("Short"),
            "title stays on the top row"
        );
    }

    #[test]
    fn a_long_announcement_scrolls_only_to_its_last_line() {
        let long: String = (1..=60).map(|n| format!("Paragraph {n}.\n\n")).collect();
        let mut v = Viewer::new(classify(
            vec![announcement("2026-01-01", "Long", &long)],
            None,
        ));
        draw_at(&mut v, 100, 30);
        for _ in 0..50 {
            v.on_key(KeyCode::PageDown);
        }
        let terminal = draw_at(&mut v, 100, 30);
        assert!(v.article.offset().y > 0);
        let rows = screen(&terminal);
        // The last line sits on the last row inside the border.
        assert!(rows[27].contains("Paragraph 60."), "{:?}", &rows[25..29]);
    }

    #[test]
    fn switching_announcement_starts_at_its_top() {
        let long: String = (1..=60).map(|n| format!("Paragraph {n}.\n\n")).collect();
        let mut v = Viewer::new(classify(
            vec![
                announcement("2026-02-01", "First", &long),
                announcement("2026-01-01", "Second", &long),
            ],
            None,
        ));
        draw_at(&mut v, 100, 30);
        v.on_key(KeyCode::PageDown);
        draw_at(&mut v, 100, 30);
        assert!(v.article.offset().y > 0);
        v.on_key(KeyCode::Down);
        draw_at(&mut v, 100, 30);
        assert_eq!(v.article.offset().y, 0);
    }

    fn long_announcements(count: usize) -> Vec<Announcement> {
        let long: String = (1..=60).map(|n| format!("Paragraph {n}.\n\n")).collect();
        (0..count)
            .map(|i| {
                announcement(
                    &format!("2026-01-{:02}", 28 - i),
                    &format!("Entry {i}"),
                    &long,
                )
            })
            .collect()
    }

    #[test]
    fn moving_past_the_last_announcement_keeps_the_scroll_position() {
        let mut v = Viewer::new(classify(long_announcements(2), None));
        draw_at(&mut v, 100, 30);
        v.on_key(KeyCode::Down); // the last (oldest) one
        draw_at(&mut v, 100, 30);
        v.on_key(KeyCode::PageDown);
        draw_at(&mut v, 100, 30);
        let scrolled = v.article.offset().y;
        assert!(scrolled > 0);
        v.on_key(KeyCode::Down); // nothing below: the selection stays
        draw_at(&mut v, 100, 30);
        assert_eq!(v.article.offset().y, scrolled);
    }

    #[test]
    fn clicks_on_the_list_border_select_nothing() {
        // Narrow layout with more announcements than rows, so a click on the
        // bottom border would land on a hidden item if borders counted.
        let mut v = Viewer::new(classify(long_announcements(10), None));
        draw_at(&mut v, 60, 20);
        let before = v.list.selected();
        let area = v.list_area;
        for (column, row) in [
            (area.x + 3, area.bottom() - 1),
            (area.x, area.y + 2),
            (area.right() - 1, area.y + 2),
        ] {
            v.on_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
            });
        }
        assert_eq!(v.list.selected(), before);
    }

    #[test]
    fn list_is_wide_enough_for_the_longest_title() {
        let title = "Example migration notice"; // fits under the 40% cap
        let mut v = Viewer::new(classify(
            vec![
                announcement("2026-01-02", "Short", "Body."),
                announcement("2026-01-01", title, "Body."),
            ],
            None,
        ));
        let terminal = draw_at(&mut v, 120, 20);
        // The whole title, then the list's right border.
        let row = &screen(&terminal)[2];
        assert!(row.contains(&format!("{title}│")), "{row:?}");
    }

    #[test]
    fn scrollbar_uses_the_palette() {
        let mut v = Viewer::new(classify(long_announcements(1), None));
        let terminal = draw_at(&mut v, 100, 30);
        let buffer = terminal.backend().buffer();
        // The scrollbar sits in the last column inside the article border.
        let x = v.list_area.right() + (100 - v.list_area.right()) - 2;
        let cells: Vec<_> = (2..27).map(|y| &buffer[(x, y)]).collect();
        assert!(
            cells.iter().any(|c| c.symbol() == "█"),
            "no thumb at column {x}"
        );
        assert!(cells.iter().all(|c| c.fg == theme::FAINT));
    }

    #[test]
    fn list_marks_new_announcements_and_tints_older_ones() {
        let mut v = Viewer::new(classify(
            fixture("announcements.yml"),
            Some(&fixture("announcements-before.yml")),
        ));
        let terminal = draw_at(&mut v, 100, 24);
        let rows = screen(&terminal);
        let buffer = terminal.backend().buffer();
        let new_row = rows
            .iter()
            .position(|r| r.contains("Example Feature Update"))
            .expect("new row");
        let old_row = rows
            .iter()
            .position(|r| r.contains("Second Entry"))
            .expect("old row");
        assert!(rows[new_row].contains('●'));
        assert!(!rows[old_row].contains('●'));
        assert!(rows[0].contains("1 new"));
        // The old row's title is drawn in the faint tint; it isn't selected.
        let x = u16::try_from(rows[old_row].find("Second").expect("title")).expect("x");
        let x = u16::try_from(rows[old_row][..usize::from(x)].chars().count()).expect("col");
        assert_eq!(
            buffer[(x, u16::try_from(old_row).expect("y"))].fg,
            theme::FAINT
        );
        // It opens on the new announcement.
        assert!(v.items[v.list.selected().unwrap_or_default()].new);
    }

    #[test]
    fn clicking_a_list_row_selects_it() {
        let mut v = Viewer::new(classify(
            fixture("announcements.yml"),
            Some(&fixture("announcements-before.yml")),
        ));
        draw_at(&mut v, 100, 24);
        v.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: v.list_area.x + 2,
            row: v.list_area.y + 3,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(v.list.selected(), Some(2));
    }

    /// Truecolor is the design target (capability map, decision 10): every
    /// coloured cell uses a palette RGB value, never a named ANSI colour.
    #[test]
    fn viewer_draws_only_truecolor() {
        let items = classify(
            fixture("announcements.yml"),
            Some(&fixture("announcements-before.yml")),
        );
        let mut v = Viewer::new(items);
        for index in 0..v.items.len() {
            v.move_list(|l| l.select(Some(index)));
            for (width, height) in [(100, 60), (60, 40)] {
                let terminal = draw_at(&mut v, width, height);
                for cell in terminal.backend().buffer().content() {
                    for color in [cell.fg, cell.bg] {
                        assert!(
                            matches!(color, Color::Rgb(..) | Color::Reset),
                            "announcement {index} at {width}: {:?} drawn in {color:?}",
                            cell.symbol()
                        );
                    }
                }
            }
        }
    }
}
