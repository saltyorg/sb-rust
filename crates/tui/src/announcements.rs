//! The announcement viewer (tui spec, "The announcement viewer"):
//! proto-markdown's reviewed design (prototypes/proto-markdown,
//! FINDINGS.md "Viewer redesign (maintainer review)"). A list of the
//! announcements, newest first, the new ones marked and the older ones
//! faint, beside the selected one (above it below 70 columns).
//!
//! - **The pane** is a `tui-scrollview` `ScrollView`
//!   (<https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/struct.ScrollView.html>):
//!   it clamps the offset while rendering, so content that fits never
//!   scrolls and the bottom position is the last full page, and it draws
//!   its scrollbar only when the text doesn't fit. Its `Scrollbar` has no
//!   style of its own, so it keeps the colour of the cells under it: the
//!   pane block's style.
//! - **The content's height** comes from `Paragraph::line_count`
//!   (ratatui's `unstable-rendered-line-info`, ADR-001,
//!   <https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.line_count>),
//!   measured again one column narrower when the scrollbar takes one.
//! - **The list and the layout** are [`crate::split`]'s, shared with
//!   restore's viewer: ratatui's `List` with a `ListState`, moved by
//!   `select_next` and `select_previous` and clamped at once, a click's
//!   row from `ListState::offset()`.
//! - **The text** is the inline form's ([`crate::markdown`]): sanitized
//!   before and after tui-markdown, in the palette's roles.
//! - **The keys** are the key map's (tui spec, "Keys: one map for every
//!   screen"): the footer is the shared fitted row, `?` or F1 the keys
//!   overlay ([`crate::hints`]).

use std::io;

use crossterm::event::{Event as Input, KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::{Role, Theme};
use tui_scrollview::ScrollViewState;

use crate::hints::{Group, Hint, Key, Keys, Meaning, Overlay};
use crate::term::{App, Ending, Flow, Tui};
use crate::{markdown, split};

/// Rows the wheel scrolls the announcement by.
const WHEEL_ROWS: usize = 3;

/// One announcement for the viewer (tui spec, "Input"): as the file wrote
/// it, and whether it is new. Which ones apply, and which are new, is the
/// caller's (ops' rules).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The repository's name (`Saltbox`, `Sandbox`).
    pub repo: String,
    /// As the file wrote it (`YYYY-MM-DD` by convention); the list sorts by
    /// its text. Sanitized here, as the title and repository.
    pub date: String,
    pub title: String,
    /// Markdown.
    pub message: String,
    /// Whether it is new: the file before the update lacked its
    /// `date|title`, or there was no file before (`sb update`'s rule);
    /// `sb announcements` always passes false.
    pub new: bool,
}

/// The viewer on stdout's terminal over `entries`, in `theme`, until `q`,
/// Esc, Ctrl-C or the run's token (a first SIGTERM or SIGHUP).
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored (reported whatever the ending, cli-L5).
pub async fn view(shutdown: &Shutdown, entries: Vec<Entry>, theme: Theme) -> io::Result<Ending> {
    let mut viewer = Viewer::new(entries, theme);
    tracing::info!(
        entries = viewer.entries.len(),
        new = viewer.new_count(),
        "announcement viewer opened"
    );
    let mut tui = Tui::enter(shutdown)?;
    let ending = tui.run(&mut viewer).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?ending.as_ref().ok(), "announcement viewer closed");
    crate::ended(ending, left)
}

/// An entry as drawn: its date, title and repository made safe for one
/// line, its message as given (rendered for the pane in
/// [`Viewer::text`]).
#[derive(Debug)]
struct Shown {
    repo: String,
    date: String,
    title: String,
    message: String,
    new: bool,
}

/// The viewer's state (an [`App`] on [`Tui`]).
#[derive(Debug)]
pub(crate) struct Viewer {
    entries: Vec<Shown>,
    theme: Theme,
    list: ListState,
    /// The pane's scroll position; tui-scrollview clamps it when it
    /// renders.
    article: ScrollViewState,
    /// The selected entry's text, for each of the last two widths it was
    /// laid out at (a pane with and without its scrollbar), newest first;
    /// emptied when the selection changes.
    rendered: Vec<(u16, Text<'static>)>,
    /// The list's block and its rows inside the border, from the last
    /// draw, for the mouse.
    list_area: Rect,
    list_rows: Rect,
    overlay: Overlay,
}

/// The viewer's keys, for the `?` overlay.
pub(crate) const KEYS: Keys = Keys {
    screen: "announcements",
    groups: &[
        Group {
            title: "Move",
            keys: &[
                Key {
                    keys: &["↑", "↓", "k", "j"],
                    what: "the previous, next announcement",
                    meaning: Meaning::Move,
                },
                Key {
                    keys: &["←", "→", "h", "l"],
                    what: "the previous, next announcement",
                    meaning: Meaning::Direction,
                },
                Key {
                    keys: &["PgUp", "PgDn", "b", "Space"],
                    what: "a page of the text",
                    meaning: Meaning::Page,
                },
                Key {
                    keys: &["Home", "End", "g", "G"],
                    what: "the text's top, bottom",
                    meaning: Meaning::Ends,
                },
            ],
        },
        Group {
            title: "Leave",
            keys: &[
                Key {
                    keys: &["Esc"],
                    what: "close",
                    meaning: Meaning::Back,
                },
                Key {
                    keys: &["q"],
                    what: "close",
                    meaning: Meaning::Leave,
                },
                Key {
                    keys: &["Ctrl+C"],
                    what: "stop (exit 130)",
                    meaning: Meaning::Stop,
                },
                Key {
                    keys: &["?", "F1"],
                    what: "these keys",
                    meaning: Meaning::Help,
                },
            ],
        },
    ],
    mouse: "a click on the list chooses; the wheel moves the list or scrolls the text",
};

impl Viewer {
    /// The viewer over `entries`, newest first (a stable sort: equal dates
    /// keep the order given), on the newest new one, else the newest one.
    pub(crate) fn new(entries: Vec<Entry>, theme: Theme) -> Self {
        let mut entries: Vec<Shown> = entries
            .into_iter()
            .map(|e| Shown {
                repo: markdown::one_line(&e.repo),
                date: markdown::one_line(&e.date),
                title: markdown::one_line(&e.title),
                message: e.message,
                new: e.new,
            })
            .collect();
        entries.sort_by(|a, b| b.date.cmp(&a.date));
        let first = entries.iter().position(|e| e.new).unwrap_or(0);
        Self {
            list: ListState::default().with_selected((!entries.is_empty()).then_some(first)),
            entries,
            theme,
            article: ScrollViewState::default(),
            rendered: Vec::new(),
            list_area: Rect::default(),
            list_rows: Rect::default(),
            overlay: Overlay::default(),
        }
    }

    fn new_count(&self) -> usize {
        self.entries.iter().filter(|e| e.new).count()
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// Runs a list movement ([`split::move_list`]: clamped, a move past
    /// the last entry is no move), and starts a newly selected
    /// announcement at its top.
    fn move_list(&mut self, movement: impl FnOnce(&mut ListState)) -> Flow {
        if !split::move_list(&mut self.list, self.entries.len(), movement) {
            return Flow::Unchanged;
        }
        self.article.scroll_to_top();
        self.rendered.clear();
        Flow::Redraw
    }

    /// The pane's scroll moved by `scroll`.
    fn scroll(&mut self, scroll: impl FnOnce(&mut ScrollViewState)) -> Flow {
        scroll(&mut self.article);
        Flow::Redraw
    }

    fn key(&mut self, code: KeyCode) -> Flow {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => Flow::Quit,
            // ← → and `h` `l` too: sb-go's previous and next announcement
            // (tui spec, "Keys: one map for every screen", rule 7).
            KeyCode::Down | KeyCode::Right | KeyCode::Char('j' | 'l') => {
                self.move_list(ListState::select_next)
            }
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k' | 'h') => {
                self.move_list(ListState::select_previous)
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll(ScrollViewState::scroll_page_down)
            }
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll(ScrollViewState::scroll_page_up),
            KeyCode::Home | KeyCode::Char('g') => self.scroll(ScrollViewState::scroll_to_top),
            KeyCode::End | KeyCode::Char('G') => self.scroll(ScrollViewState::scroll_to_bottom),
            _ => Flow::Unchanged,
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        let at = Position::new(mouse.column, mouse.row);
        let in_list = self.list_area.contains(at);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if self.list_rows.contains(at) => {
                // A click under the last entry selects nothing.
                match split::clicked(&self.list, self.list_rows, at, self.entries.len()) {
                    Some(row) => self.move_list(|list| list.select(Some(row))),
                    None => Flow::Unchanged,
                }
            }
            MouseEventKind::ScrollDown if in_list => self.move_list(ListState::select_next),
            MouseEventKind::ScrollUp if in_list => self.move_list(ListState::select_previous),
            MouseEventKind::ScrollDown => self.scroll(|a| {
                for _ in 0..WHEEL_ROWS {
                    a.scroll_down();
                }
            }),
            MouseEventKind::ScrollUp => self.scroll(|a| {
                for _ in 0..WHEEL_ROWS {
                    a.scroll_up();
                }
            }),
            _ => Flow::Unchanged,
        }
    }

    /// A list row: date, repository, title; new ones marked and at full
    /// strength, older ones faint without the marker; with none marked
    /// (`sb announcements`), every row at full strength.
    fn row(&self, entry: &Shown, marked: bool) -> ListItem<'static> {
        // Each row names its repository between the date and the title:
        // both repositories share the list (the maintainer, 2026-11-17).
        let line = if !marked {
            Line::from(vec![
                Span::styled(format!(" {}  ", entry.date), self.style(Role::Text)),
                Span::styled(entry.repo.clone(), self.style(Role::Muted)),
                Span::styled(format!("  {}", entry.title), self.style(Role::Text)),
            ])
        } else if entry.new {
            Line::from(vec![
                Span::styled("● ", self.style(Role::Ok)),
                Span::styled(format!("{}  ", entry.date), self.style(Role::Muted)),
                Span::styled(entry.repo.clone(), self.style(Role::Muted)),
                Span::raw("  "),
                Span::styled(
                    entry.title.clone(),
                    self.style(Role::Text).add_modifier(Modifier::BOLD),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{}  {}  {}", entry.date, entry.repo, entry.title),
                    self.style(Role::Faint),
                ),
            ])
        };
        ListItem::new(line)
    }

    /// The list's title: its name, and the count of new ones.
    fn list_title(&self) -> Line<'static> {
        let mut spans = vec![Span::styled(
            " Announcements ",
            self.style(Role::Accent).add_modifier(Modifier::BOLD),
        )];
        let new = self.new_count();
        if new > 0 {
            spans.push(Span::styled(format!("· {new} new "), self.style(Role::Ok)));
        }
        Line::from(spans)
    }

    /// The selected announcement's first line: date, title and the NEW
    /// badge.
    fn heading(&self, entry: &Shown) -> Line<'static> {
        let mut spans = vec![
            Span::styled(entry.date.clone(), self.style(Role::Muted)),
            Span::styled(" · ", self.style(Role::Faint)),
            Span::styled(
                entry.title.clone(),
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ),
        ];
        if entry.new {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                " NEW ",
                self.style(Role::OnAccent)
                    .patch(self.bg(Role::Ok))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    }

    /// The selected entry's text for a pane `width` wide: its heading, then
    /// its message, rendered again only for a width not among the last
    /// two (tables are laid out to the width; a pane that needs its
    /// scrollbar is measured at two widths each frame).
    fn text(&mut self, index: usize, width: u16) -> Text<'static> {
        if let Some((_, text)) = self.rendered.iter().find(|(w, _)| *w == width) {
            return text.clone();
        }
        let entry = &self.entries[index];
        let mut text = Text::from(self.heading(entry));
        text.extend(markdown::screen_text(&entry.message, &self.theme, width));
        self.rendered.insert(0, (width, text.clone()));
        self.rendered.truncate(2);
        text
    }

    /// The key hints, fitted to `width`, then the `● new` legend when any
    /// is new, its marker in the list's colour; the legend goes first when
    /// narrow (tui spec, "Keys", Hints).
    fn footer(&self, width: u16) -> Line<'static> {
        let mut hints: Vec<Hint> = vec![("↑↓", "choose"), ("PgUp/PgDn", "scroll")];
        if self.new_count() > 0 {
            hints.push(("●", "new"));
        }
        let mut line = crate::hints::footer(
            &self.theme,
            None,
            &hints,
            &[(KEYS.help_key(), "keys"), ("q", "close")],
            width,
        );
        for span in &mut line.spans {
            if span.content == "●" {
                span.style = self.style(Role::Ok);
            }
        }
        line
    }

    fn block(&self) -> Block<'static> {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.style(Role::Border))
    }

    fn selected(&self) -> Style {
        split::selected(&self.theme)
    }

    fn draw_pane(&mut self, frame: &mut Frame<'_>, area: Rect) {
        // The selection is always an entry's (`move_list` clamps it).
        let selected = self
            .list
            .selected()
            .and_then(|index| Some((index, self.entries.get(index)?)));
        let mut block = self.block().style(self.style(Role::Faint));
        if let Some((_, entry)) = selected {
            block = block.title(Span::styled(
                format!(" {} ", entry.repo),
                self.style(Role::Muted),
            ));
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let Some((index, _)) = selected else {
            frame.render_widget(
                Paragraph::new("No announcements apply to this system.")
                    .style(self.style(Role::Muted)),
                inner,
            );
            return;
        };
        // The scroll view needs the content's size first; a scrollbar
        // takes one column, so the text is measured again without it.
        let mut width = inner.width;
        let mut paragraph = self.paragraph(index, width);
        if paragraph.line_count(width) > usize::from(inner.height) {
            width = width.saturating_sub(1);
            paragraph = self.paragraph(index, width);
        }
        let height = u16::try_from(paragraph.line_count(width)).unwrap_or(u16::MAX);
        split::render_scrolled(
            frame,
            inner,
            &mut self.article,
            paragraph,
            Size::new(width, height),
        );
    }

    fn paragraph(&mut self, index: usize, width: u16) -> Paragraph<'static> {
        Paragraph::new(self.text(index, width))
            .style(self.style(Role::Text))
            .wrap(Wrap { trim: false })
    }
}

impl App for Viewer {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) if self.overlay.is_open() => self.overlay.key(key),
            Input::Mouse(mouse) if self.overlay.is_open() => self.overlay.mouse(mouse),
            Input::Key(key) if Overlay::opens(&key, false) => {
                self.overlay.open();
                Flow::Redraw
            }
            Input::Key(key) => self.key(key.code),
            Input::Mouse(mouse) => self.mouse(mouse),
            _ => Flow::Unchanged,
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
        let marked = self.new_count() > 0;
        let rows: Vec<ListItem<'static>> =
            self.entries.iter().map(|e| self.row(e, marked)).collect();
        let title = self.list_title();
        let [list_area, pane] = split::areas(body, &rows, &title);
        self.list_area = list_area;
        let list_block = self.block().title(title);
        self.list_rows = list_block.inner(list_area);
        let list = List::new(rows)
            .block(list_block)
            .highlight_style(self.selected());
        frame.render_stateful_widget(list, list_area, &mut self.list);
        self.draw_pane(frame, pane);
        frame.render_widget(Paragraph::new(self.footer(footer.width)), footer);
        self.overlay.draw(frame, &KEYS, &self.theme);
    }
}

#[cfg(test)]
mod tests;
