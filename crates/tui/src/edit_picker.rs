//! The edit picker (tui spec, "Editors", `sb edit`): the six Saltbox
//! configuration files under sb-go's prompt line, "Select a Saltbox
//! configuration file to edit:", each row the file's title and shown
//! name, and a key hint line. It returns the file chosen once the
//! terminal is restored; sb's `edit.rs` runs the editor, so `sb edit` and
//! `sb edit <file>` share one path and sb-tui never starts a process.
//!
//! The list is ratatui's `List` with a `ListState`
//! (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.List.html>,
//! <https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html>),
//! every row selectable, so the state's own `select_previous` and
//! `select_next` move it, clamped to the last row at once (as the
//! announcement viewer's list).
//!
//! - **Keys:** Up/Down and `k`/`j` move, Enter opens, `q` and Esc leave
//!   with nothing chosen; Ctrl-C is the event loop's (130).
//! - **Mouse:** the wheel moves; a click selects the row under it, and a
//!   second click on that row within [`DOUBLE_CLICK`] opens it. The row
//!   under the pointer comes from the list's area and its rows' heights as
//!   last drawn, from `ListState::offset()` on (tui spec, "Mouse").
//! - **Narrow terminals:** when a title and its name don't fit on one row,
//!   each file takes two, the name under its title, so neither is cut down
//!   to the widest title's width (23 columns with the mark).

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span, Text};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::{Role, Theme};

use crate::term::{App, Ending, Flow, Tui};

/// sb-go's prompt line (cmd/edit.go, `View`).
pub const PROMPT: &str = "Select a Saltbox configuration file to edit:";

/// The selection's mark, as the log picker draws it.
const MARK: &str = "› ";

/// The space between a title and its name on one row.
const GAP: usize = 2;

/// How soon a second click on the same row opens it. crossterm reports
/// presses only (no double click,
/// <https://docs.rs/crossterm/0.29.0/crossterm/event/enum.MouseEventKind.html>),
/// so the picker times them itself. UNVERIFIED as a terminal convention:
/// 500 ms is the Windows default
/// (<https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdoubleclicktime>);
/// GTK's default is 400 ms.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// One file the picker offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// The picker's title ("Accounts").
    pub title: &'static str,
    /// The file's shown name ("accounts.yml").
    pub shown: &'static str,
}

/// The picker on stdout's terminal over `items`, in `theme`, until a file
/// is chosen, `q`, Esc, Ctrl-C or the run's token (a first SIGTERM or
/// SIGHUP). With the ending, the value given with the file chosen. The
/// terminal is restored when this returns, the input drained
/// ([`Tui::leave`]), so the editor gets a clean terminal.
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored (reported whatever the ending, cli-L5).
pub async fn pick<T: Copy>(
    shutdown: &Shutdown,
    items: &[(Item, T)],
    theme: Theme,
) -> io::Result<(Ending, Option<T>)> {
    let shown: Vec<Item> = items.iter().map(|&(item, _)| item).collect();
    let mut picker = EditPicker::new(&shown, theme);
    let mut tui = Tui::enter(shutdown)?;
    let ending = tui.run(&mut picker).await;
    let left = tui.leave().await;
    let ending = crate::ended(ending, left)?;
    let chosen = match ending {
        Ending::Quit => picker.chosen.and_then(|i| items.get(i)),
        Ending::Interrupted | Ending::Cancelled => None,
    };
    tracing::info!(
        ending = ?ending,
        chosen = chosen.map(|(item, _)| item.shown),
        "edit picker closed"
    );
    Ok((ending, chosen.map(|&(_, value)| value)))
}

/// The picker's state: the files, the selection, and what the last frame
/// drew, for the mouse.
#[derive(Debug)]
struct EditPicker {
    items: Vec<Item>,
    theme: Theme,
    state: ListState,
    /// The list's area in the last frame.
    area: Rect,
    /// Each row's height in the last frame (1, or 2 on a narrow terminal).
    height: u16,
    /// The last click: the row, and when.
    clicked: Option<(usize, Instant)>,
    chosen: Option<usize>,
}

impl EditPicker {
    fn new(items: &[Item], theme: Theme) -> Self {
        Self {
            items: items.to_vec(),
            theme,
            state: ListState::default().with_selected((!items.is_empty()).then_some(0)),
            area: Rect::default(),
            height: 1,
            clicked: None,
            chosen: None,
        }
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    /// `role`'s colour as a background (none under `ascii`), as the log
    /// picker's.
    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// Open the selected file, if one is selected (the selection is
    /// always a row: [`EditPicker::step`] clamps it, clicks map to rows).
    fn choose(&mut self) -> Flow {
        let Some(i) = self.state.selected() else {
            return Flow::Unchanged;
        };
        self.chosen = Some(i);
        Flow::Quit
    }

    /// Move the selection one row, stopping at either end (sb-go's list
    /// doesn't wrap): `ListState::select_next` and `select_previous`, then
    /// the clamp they leave to the next render (ratatui-widgets 0.3.2
    /// `list/state.rs`), done now, so an Enter or a double click before
    /// that render never sees a row past the end.
    fn step(&mut self, down: bool) {
        if self.items.is_empty() {
            return;
        }
        if down {
            self.state.select_next();
        } else {
            self.state.select_previous();
        }
        let last = self.items.len() - 1;
        if let Some(i) = self.state.selected() {
            self.state.select(Some(i.min(last)));
        }
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Flow::Unchanged;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.step(false),
            KeyCode::Down | KeyCode::Char('j') => self.step(true),
            KeyCode::Enter => return self.choose(),
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.step(false),
            MouseEventKind::ScrollDown => self.step(true),
            MouseEventKind::Down(MouseButton::Left) => return self.click(mouse, Instant::now()),
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// A left click at `now`: it selects the row under it; a second one on
    /// that row within [`DOUBLE_CLICK`] opens it.
    fn click(&mut self, mouse: MouseEvent, now: Instant) -> Flow {
        let Some(row) = self.row_at(Position::new(mouse.column, mouse.row)) else {
            self.clicked = None;
            return Flow::Unchanged;
        };
        let double = self.clicked.is_some_and(|(last, at)| {
            last == row && now.saturating_duration_since(at) <= DOUBLE_CLICK
        });
        if double {
            self.clicked = None;
            self.state.select(Some(row));
            return self.choose();
        }
        self.clicked = Some((row, now));
        self.state.select(Some(row));
        Flow::Redraw
    }

    /// The file drawn at `at` in the last frame, if any. The list has no
    /// border: its first row on the screen is the state's offset.
    fn row_at(&self, at: Position) -> Option<usize> {
        if !self.area.contains(at) {
            return None;
        }
        // Why not a ratatui call: ratatui-widgets 0.3.2's `List` keeps no
        // row rects, only the offset (the log picker maps clicks so too).
        let row = self.state.offset() + usize::from((at.y - self.area.y) / self.height.max(1));
        (row < self.items.len()).then_some(row)
    }

    /// The title row: the screen's name and sb-go's prompt, wrapped on a
    /// narrow terminal.
    fn title(&self) -> Paragraph<'static> {
        Paragraph::new(TextLine::from(vec![
            Span::styled(
                " ◆ Edit ",
                self.style(Role::OnAccent).patch(self.bg(Role::Accent)),
            ),
            Span::raw("  "),
            Span::styled(PROMPT, self.style(Role::Text)),
        ]))
        .wrap(Wrap { trim: false })
    }

    /// The rows: each title, padded so the names line up, then its name;
    /// or, when `width` can't hold both, the name on a row of its own
    /// under its title. Widths are display widths (`Span::width`).
    fn rows(&self, width: u16) -> (Vec<ListItem<'static>>, u16) {
        let title_width = self
            .items
            .iter()
            .map(|item| Span::raw(item.title).width())
            .max()
            .unwrap_or(0);
        let name_width = self
            .items
            .iter()
            .map(|item| Span::raw(item.shown).width())
            .max()
            .unwrap_or(0);
        let one_row = Span::raw(MARK).width() + title_width + GAP + name_width;
        let narrow = one_row > usize::from(width);
        let rows = self
            .items
            .iter()
            .map(|item| {
                let title = Span::styled(item.title, self.style(Role::Text));
                let name = Span::styled(item.shown, self.style(Role::Muted));
                if narrow {
                    ListItem::new(Text::from(vec![
                        TextLine::from(title),
                        TextLine::from(vec![Span::raw("  "), name]),
                    ]))
                } else {
                    let pad = title_width - Span::raw(item.title).width() + GAP;
                    ListItem::new(TextLine::from(vec![
                        title,
                        Span::raw(" ".repeat(pad)),
                        name,
                    ]))
                }
            })
            .collect();
        (rows, if narrow { 2 } else { 1 })
    }

    /// The key hints.
    fn footer(&self) -> TextLine<'static> {
        let key = self.style(Role::Accent);
        let muted = self.style(Role::Muted);
        let mut spans = Vec::new();
        for (i, (k, what)) in [("↑↓", "move"), ("Enter", "open"), ("q", "quit")]
            .into_iter()
            .enumerate()
        {
            if i > 0 {
                spans.push(Span::styled(" · ", self.style(Role::Faint)));
            }
            spans.push(Span::styled(k, key));
            spans.push(Span::styled(format!(" {what}"), muted));
        }
        TextLine::from(spans)
    }
}

impl App for EditPicker {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) => self.key(key),
            Input::Mouse(mouse) => self.mouse(mouse),
            // A resize redraws: the rows may change between one and two
            // lines.
            Input::Resize(..) => Flow::Redraw,
            _ => Flow::Unchanged,
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let title = self.title();
        // The prompt's rows at this width: ratatui's `line_count`
        // (the `unstable-rendered-line-info` feature, ADR-001,
        // <https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.line_count>).
        let title_rows = u16::try_from(title.line_count(area.width)).unwrap_or(u16::MAX);
        // ratatui's layout: the title, a blank row, the list, the footer
        // (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.vertical>).
        let [title_area, _, list, footer] = Layout::vertical([
            Constraint::Length(title_rows),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);
        frame.render_widget(title, title_area);
        let (rows, height) = self.rows(list.width);
        self.area = list;
        self.height = height;
        // The selected row is one solid bar, as in the log picker.
        let selected = self
            .style(Role::SelectedFg)
            .patch(self.bg(Role::SelectedBg));
        let widget = List::new(rows)
            .highlight_symbol(MARK)
            .highlight_style(selected);
        frame.render_stateful_widget(widget, list, &mut self.state);
        frame.render_widget(Paragraph::new(self.footer()), footer);
    }
}

#[cfg(test)]
mod tests;
