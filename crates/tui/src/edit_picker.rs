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
//! - **Keys** (tui spec, "Keys: one map for every screen"): Up/Down and
//!   `k`/`j` move; PgUp/PgDn and `b`/Space a page; Home/End and `g`/`G`
//!   the first and last file; Enter opens; `/` filters by title or name
//!   (a ratatui-textarea line, smart case as the log picker's: Enter keeps
//!   it, Esc clears it); Esc clears a filter in effect, then leaves, as
//!   `q` does, with nothing chosen; `?` or F1 the keys overlay
//!   ([`crate::hints`]); Ctrl-C is the event loop's (130). The footer is
//!   the shared fitted row.
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
use ratatui_textarea::{CursorMove, TextArea};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::{Role, Theme};

use crate::hints::{Group, Hint, Key, Keys, Meaning, Overlay};
use crate::term::{App, Ending, Flow, Tui};

/// sb-go's prompt line (cmd/edit.go, `View`).
pub const PROMPT: &str = "Select a Saltbox configuration file to edit:";

/// The selection's mark, as the log picker draws it.
const MARK: &str = "› ";

/// The space between a title and its name on one row.
const GAP: usize = 2;

/// The label before the filter line, and the hint after it, as the log
/// picker's.
const FILTER_LABEL: &str = " Filter: ";
const FILTER_HINT: &str = "Enter keeps · Esc clears ";

/// How soon a second click on the same row opens it. crossterm reports
/// presses only (no double click,
/// <https://docs.rs/crossterm/0.29.0/crossterm/event/enum.MouseEventKind.html>),
/// so the picker times them itself. UNVERIFIED as a terminal convention:
/// 500 ms is the Windows default
/// (<https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdoubleclicktime>);
/// GTK's default is 400 ms.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// The picker's keys, for the `?` overlay (tui spec, "Keys: one map for
/// every screen").
pub(crate) const KEYS: Keys = Keys {
    screen: "edit picker",
    groups: &[
        Group {
            title: "Move",
            keys: &[
                Key {
                    keys: &["↑", "↓", "k", "j"],
                    what: "a file",
                    meaning: Meaning::Move,
                },
                Key {
                    keys: &["PgUp", "PgDn", "b", "Space"],
                    what: "a page",
                    meaning: Meaning::Page,
                },
                Key {
                    keys: &["Home", "End", "g", "G"],
                    what: "the first, the last",
                    meaning: Meaning::Ends,
                },
            ],
        },
        Group {
            title: "Find",
            keys: &[Key {
                keys: &["/"],
                what: "filter by title or name",
                meaning: Meaning::Find,
            }],
        },
        Group {
            title: "Leave",
            keys: &[
                Key {
                    keys: &["Enter"],
                    what: "edit the file",
                    meaning: Meaning::Open,
                },
                Key {
                    keys: &["Esc"],
                    what: "clear the filter, or leave",
                    meaning: Meaning::Back,
                },
                Key {
                    keys: &["q"],
                    what: "quit",
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
    mouse: "a click selects, a second click edits; the wheel moves",
};

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
    /// The filter in effect, and the files it shows, by index into `items`.
    query: String,
    shown: Vec<usize>,
    /// The filter line while it is open.
    editing: Option<TextArea<'static>>,
    overlay: Overlay,
    /// The selection, a row of `shown`.
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
            query: String::new(),
            shown: (0..items.len()).collect(),
            editing: None,
            overlay: Overlay::default(),
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
    /// always a row shown: [`EditPicker::step`] clamps it, clicks map to
    /// rows, a filter selects its first).
    fn choose(&mut self) -> Flow {
        let Some(&i) = self.state.selected().and_then(|row| self.shown.get(row)) else {
            return Flow::Unchanged;
        };
        self.chosen = Some(i);
        Flow::Quit
    }

    /// The filter is `query` now: the files whose title or name match it
    /// (the viewer's smart case, `search::matches`). The selection stays
    /// on its file while it is shown, as the log picker's does; else the
    /// first is selected.
    fn filter(&mut self, query: String) {
        let selected = self
            .state
            .selected()
            .and_then(|row| self.shown.get(row))
            .copied();
        let hit = |text: &str| !crate::search::matches(text, &query).is_empty();
        self.shown = (0..self.items.len())
            .filter(|&i| query.is_empty() || hit(self.items[i].title) || hit(self.items[i].shown))
            .collect();
        self.query = query;
        let row = selected.and_then(|i| self.shown.iter().position(|&s| s == i));
        self.state
            .select(row.or((!self.shown.is_empty()).then_some(0)));
    }

    /// Move the selection with `ListState`'s own `movement`, then clamp it
    /// to the rows shown at once (ratatui-widgets 0.3.2 `list/state.rs`
    /// clamps only at render), as the fact editor does.
    fn move_selection(&mut self, movement: impl FnOnce(&mut ListState)) {
        let Some(last) = self.shown.len().checked_sub(1) else {
            return;
        };
        movement(&mut self.state);
        let at = self.state.selected().map_or(0, |at| at.min(last));
        self.state.select(Some(at));
    }

    /// A page: the rows the list showed in the last frame.
    fn page(&self) -> usize {
        usize::from(self.area.height / self.height.max(1)).max(1)
    }

    /// Move the selection one row, stopping at either end (sb-go's list
    /// doesn't wrap): `ListState::select_next` and `select_previous`,
    /// clamped at once ([`EditPicker::move_selection`]), so an Enter or a
    /// double click before the next render never sees a row past the end.
    fn step(&mut self, down: bool) {
        if down {
            self.move_selection(ListState::select_next);
        } else {
            self.move_selection(ListState::select_previous);
        }
    }

    /// The keys that move the selection, with the filter line open or not.
    fn movement(&mut self, code: KeyCode) -> bool {
        let page = u16::try_from(self.page()).unwrap_or(u16::MAX);
        match code {
            KeyCode::Up => self.step(false),
            KeyCode::Down => self.step(true),
            KeyCode::PageUp => self.move_selection(|l| l.scroll_up_by(page)),
            KeyCode::PageDown => self.move_selection(|l| l.scroll_down_by(page)),
            _ => return false,
        }
        true
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if self.editing.is_some() {
            return self.edit_key(key);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Flow::Unchanged;
        }
        if self.movement(key.code) {
            return Flow::Redraw;
        }
        // The aliases every screen shares (tui spec, "Keys: one map for
        // every screen").
        match key.code {
            KeyCode::Char('k') => self.step(false),
            KeyCode::Char('j') => self.step(true),
            KeyCode::Char('b') => _ = self.movement(KeyCode::PageUp),
            KeyCode::Char(' ') => _ = self.movement(KeyCode::PageDown),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(ListState::select_first),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(ListState::select_last),
            KeyCode::Enter => return self.choose(),
            KeyCode::Char('/') => {
                // The line opens on the filter in effect, the cursor at its
                // end (ratatui-textarea's `TextArea::new` and
                // `move_cursor`), as the log picker's.
                let mut line = TextArea::new(vec![self.query.clone()]);
                line.move_cursor(CursorMove::End);
                line.set_cursor_line_style(Style::default());
                line.set_style(self.style(Role::Text));
                self.editing = Some(line);
            }
            KeyCode::Esc if !self.query.is_empty() => self.filter(String::new()),
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// A key while the filter line is open: the list narrows as it is
    /// typed; Enter keeps the filter, Esc clears it, the arrows and page
    /// keys still move. Every other key is the line's
    /// (ratatui-textarea's `TextArea::input`). Enter and Ctrl-M end it, as
    /// ratatui-textarea's single-line example does
    /// (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>).
    fn edit_key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n' | '\r') => self.editing = None,
            KeyCode::Char('m') if ctrl => self.editing = None,
            KeyCode::Esc => {
                self.editing = None;
                self.filter(String::new());
            }
            code if self.movement(code) => {}
            _ => {
                let Some(line) = self.editing.as_mut() else {
                    return Flow::Unchanged;
                };
                if !line.input(key) {
                    return Flow::Redraw;
                }
                let query = line.lines().join("");
                self.filter(query);
            }
        }
        Flow::Redraw
    }

    /// A paste (bracketed paste): into the filter line whole, as one line
    /// (ratatui-textarea's `insert_str`), as the log picker's; outside the
    /// line it does nothing, so its letters aren't keys.
    fn paste(&mut self, text: &str) -> Flow {
        let Some(line) = self.editing.as_mut() else {
            return Flow::Unchanged;
        };
        line.insert_str(crate::paste::one_line(text));
        let query = line.lines().join("");
        self.filter(query);
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
        (row < self.shown.len()).then_some(row)
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
            .shown
            .iter()
            .map(|&i| &self.items[i])
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

    /// The key hints, fitted to `width` (tui spec, "Keys", Hints).
    fn footer(&self, width: u16) -> TextLine<'static> {
        let mut hints: Vec<Hint> = vec![("Enter", "edit")];
        if !self.query.is_empty() {
            hints.push(("Esc", "clear"));
        }
        hints.extend([("/", "filter"), ("↑↓", "move")]);
        let tail = [(KEYS.help_key(), "keys"), ("q", "quit")];
        crate::hints::footer(&self.theme, &hints, &tail, width)
    }
}

impl App for EditPicker {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) if self.overlay.is_open() => self.overlay.key(key),
            Input::Mouse(mouse) if self.overlay.is_open() => self.overlay.mouse(mouse),
            Input::Key(key) if Overlay::opens(&key, self.editing.is_some()) => {
                self.overlay.open();
                Flow::Redraw
            }
            Input::Key(key) => self.key(key),
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Paste(_) if self.overlay.is_open() => Flow::Unchanged,
            Input::Paste(text) => self.paste(&text),
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
        if self.shown.is_empty() && !self.items.is_empty() {
            // The log picker's line for a filter that matches nothing, the
            // query through term-ui's `DisplayText` as the log picker's.
            let query = sb_term_ui::DisplayText::from_bytes(self.query.as_bytes(), false);
            let text = format!(
                "No files match \"{}\". Esc clears the filter.",
                query.lines().next().unwrap_or_default()
            );
            frame.render_widget(
                Paragraph::new(Span::styled(text, self.style(Role::Muted))),
                list,
            );
        } else {
            let widget = List::new(rows)
                .highlight_symbol(MARK)
                .highlight_style(selected);
            frame.render_stateful_widget(widget, list, &mut self.state);
        }
        if let Some(line) = &self.editing {
            // The log picker's filter line: its label, the line, its hint.
            let width = |text| u16::try_from(Span::raw(text).width()).unwrap_or(u16::MAX);
            let [label_area, line_area, hint_area] = Layout::horizontal([
                Constraint::Length(width(FILTER_LABEL)),
                Constraint::Fill(1),
                Constraint::Length(width(FILTER_HINT)),
            ])
            .areas(footer);
            frame.render_widget(
                Paragraph::new(Span::styled(FILTER_LABEL, self.style(Role::Accent))),
                label_area,
            );
            frame.render_widget(
                Paragraph::new(Span::styled(FILTER_HINT, self.style(Role::Muted))),
                hint_area,
            );
            // ratatui-textarea draws itself as a widget
            // (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html#impl-Widget-for-%26TextArea%3C'_%3E>).
            frame.render_widget(line, line_area);
        } else {
            frame.render_widget(Paragraph::new(self.footer(footer.width)), footer);
        }
        self.overlay.draw(frame, &KEYS, &self.theme);
    }
}

#[cfg(test)]
mod tests;
