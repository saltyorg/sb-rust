//! The fact editor's mouse and its menu (tui spec, editors, `sb fact`,
//! Screen; module build M56): a click selects a row and a second click on
//! it acts as Enter, the wheel moves the selection, a right click opens
//! the row's menu; a click on a bottom button, an undo or a dialog's
//! button does what its key does; a click on the search box gives it the
//! keys. Shift-drag is the terminal's own selection.
//!
//! Click targets are the rectangles the last frame drew them in
//! ([`Hits`]), recorded while drawing, so no hit-map is kept by hand
//! (proto-facts), tested with `Rect::contains`
//! (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Rect.html#method.contains>).
//! Why not a ratatui call for the rows: ratatui-widgets 0.3.2's `List`
//! keeps no row rects, only its offset
//! (<https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html#method.offset>),
//! and every row is one line, so the row under the pointer is the offset
//! plus its line (the pickers map clicks the same way). Hover is
//! crossterm's `MouseEventKind::Moved`, which its mouse capture reports
//! (<https://docs.rs/crossterm/0.29.0/crossterm/event/enum.MouseEventKind.html>).
//!
//! The menu is a tui-popup `Popup` (<https://docs.rs/tui-popup/0.7.7/tui_popup/>)
//! rendered without kept state into a rectangle of its own size just
//! under the row it acts on, from the pointer's column, or just over the
//! row when there is no room under it, then fitted to the screen
//! (`Rect::clamp`): a fresh state each frame centres it in that
//! rectangle, which is the rectangle itself, so it opens where it is put
//! and resizes with its labels. Under or over, the row's name stays in
//! sight (the maintainer, 2026-11-17; proto-facts opened it on the row,
//! and neither ratatui nor tui-popup places a popup by a row).
//! (Its `PopupState::move_to` only moves a popup already drawn, which is
//! why proto-facts drew the menu by hand; the stateless render doesn't
//! need it.) The body is a `KnownSize` newtype, as tui-popup's
//! `KnownSize` docs suggest, and its rows (`Rect::rows`) map clicks and
//! hovers back to items. Every item shows its key, and the key works with
//! the menu open or not; `m` or the Menu key opens it on the selected
//! row.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::Style;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{ListState, Widget};
use sb_cli_core::theme::Role;
use tui_popup::{KnownSize, Popup as TuiPopup};

use super::changes::{Pending, Target};
use super::dialog::Popup;
use super::{Action, Page, Row};
use crate::term::Flow;

/// Where the last frame drew what can be clicked.
#[derive(Debug, Default)]
pub(super) struct Hits {
    /// The page's list.
    pub(super) list: Rect,
    /// The search box.
    pub(super) search: Rect,
    /// Each bottom button that could act, and what it does.
    pub(super) buttons: Vec<(Rect, Action)>,
    /// Each change's undo in the changes panel.
    pub(super) undo: Vec<(Rect, Target)>,
    /// Each dialog button, and the key it stands for.
    pub(super) dialog: Vec<(Rect, KeyCode)>,
    /// The menu, when it is open.
    pub(super) menu: Rect,
}

/// The open menu: where it opened (the column, and its row's line), and
/// the item lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Menu {
    pub(super) at: Position,
    pub(super) lit: usize,
}

/// Where a menu opened from the keyboard starts, from the list's left
/// edge (proto-facts' 30 columns).
const KEYBOARD_MENU_COLUMN: u16 = 30;

/// One menu item: its label, its key, and what it does.
type Item = (String, char, Action);

/// The menu's rows inside its border.
fn rows(menu: Rect) -> impl Iterator<Item = Rect> {
    menu.inner(Margin::new(1, 1)).rows()
}

/// Whether `key` is `c` with no Ctrl or Alt.
fn plain_char(key: &KeyEvent, c: char) -> bool {
    key.code == KeyCode::Char(c)
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl Page {
    /// The menu's items for `row`, each with the key that does the same
    /// without the menu (proto-facts' menu: Show or Hide on a secret,
    /// Copy, Edit unless deleted, Undo change on an edited setting, and
    /// Delete or Keep; copies and apps have their Delete or Keep).
    pub(super) fn menu_items(&self, row: Row) -> Vec<Item> {
        let target = self.target(row);
        let staged_delete = self.pending.get(&target) == Some(&Pending::Delete);
        let keep = |what: &str| {
            if staged_delete {
                format!("Keep {what}")
            } else {
                format!("Delete {what}")
            }
        };
        let mut items = Vec::new();
        match row {
            Row::Setting(id) => {
                if self.secret(id) {
                    let label = if self.is_shown(id) { "Hide" } else { "Show" };
                    items.push((label.to_owned(), 's', Action::Show));
                }
                items.push(("Copy".to_owned(), 'c', Action::Copy));
                if !self.deleted(&target) {
                    items.push(("Edit".to_owned(), 'e', Action::Edit));
                }
                if matches!(self.pending.get(&target), Some(Pending::Set(_))) {
                    items.push(("Undo change".to_owned(), 'u', Action::Undo));
                }
                let label = if staged_delete { "Keep" } else { "Delete" };
                items.push((label.to_owned(), 'd', Action::Delete));
            }
            Row::Copy(..) => items.push((keep("copy"), 'd', Action::Delete)),
            Row::App(_) => items.push((keep("app"), 'd', Action::Delete)),
        }
        items
    }

    /// Open the menu on the selected row: from `at` (a right click), or
    /// from the row (`m`, the Menu key); [`Page::draw_menu`] puts it under
    /// or over that row.
    pub(super) fn open_menu(&mut self, at: Option<Position>) {
        let Some(selected) = self.list.selected() else {
            return;
        };
        if self.selected().is_none() {
            return;
        }
        let line = u16::try_from(selected.saturating_sub(self.list.offset())).unwrap_or(0);
        let list = self.hits.list;
        // From the keyboard: at the row, past the mark, the indent and the
        // start of a key (proto-facts' place).
        let at = at.unwrap_or(Position::new(
            list.x.saturating_add(KEYBOARD_MENU_COLUMN),
            list.y.saturating_add(line),
        ));
        self.menu = Some(Menu { at, lit: 0 });
    }

    /// A key while the menu is open: Up and Down light an item, Enter does
    /// it, an item's key does it, Esc (or `m`) closes the menu.
    pub(super) fn menu_key(&mut self, key: KeyEvent, menu: Menu) -> Flow {
        let items = self.selected().map_or_default(|row| self.menu_items(row));
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu = Some(Menu {
                    lit: menu.lit.saturating_sub(1),
                    ..menu
                });
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.menu = Some(Menu {
                    lit: (menu.lit + 1).min(last),
                    ..menu
                });
            }
            KeyCode::Enter => {
                self.menu = None;
                if let Some(&(_, _, action)) = items.get(menu.lit) {
                    return self.act(action);
                }
            }
            KeyCode::Esc | KeyCode::Char('m') | KeyCode::Menu => self.menu = None,
            _ => {
                // An item's own key works with the menu open too.
                if let Some(&(_, _, action)) = items.iter().find(|(_, k, _)| plain_char(&key, *k)) {
                    self.menu = None;
                    return self.act(action);
                }
                return Flow::Unchanged;
            }
        }
        Flow::Redraw
    }

    /// The row drawn at `at` in the last frame, if any.
    fn row_at(&self, at: Position) -> Option<usize> {
        if !self.hits.list.contains(at) {
            return None;
        }
        let row = self.list.offset() + usize::from(at.y - self.hits.list.y);
        (row < self.rows().len()).then_some(row)
    }

    /// A mouse event: the menu's, a dialog's, or the page's.
    pub(super) fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        let at = Position::new(mouse.column, mouse.row);
        if let Some(menu) = self.menu {
            return self.menu_mouse(mouse, at, menu);
        }
        if !matches!(self.popup, Popup::None) {
            // A dialog's buttons, and the drift dialog's wheel (its
            // arrow keys); nothing else under it.
            let key = match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => self
                    .hits
                    .dialog
                    .iter()
                    .find(|(r, _)| r.contains(at))
                    .map(|&(_, code)| code),
                MouseEventKind::ScrollDown if matches!(self.popup, Popup::Drift { .. }) => {
                    Some(KeyCode::Down)
                }
                MouseEventKind::ScrollUp if matches!(self.popup, Popup::Drift { .. }) => {
                    Some(KeyCode::Up)
                }
                _ => None,
            };
            return match key {
                Some(code) => self.popup_key(KeyEvent::new(code, KeyModifiers::NONE)),
                None => Flow::Unchanged,
            };
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(at),
            MouseEventKind::Down(MouseButton::Right) => {
                let Some(row) = self.row_at(at) else {
                    return Flow::Unchanged;
                };
                self.searching = false;
                self.list.select(Some(row));
                self.open_menu(Some(at));
                Flow::Redraw
            }
            MouseEventKind::ScrollDown if self.hits.list.contains(at) => {
                self.move_selection(ListState::select_next);
                Flow::Redraw
            }
            MouseEventKind::ScrollUp if self.hits.list.contains(at) => {
                self.move_selection(ListState::select_previous);
                Flow::Redraw
            }
            _ => Flow::Unchanged,
        }
    }

    /// A left click on the page: a button, an undo, the search box, or a
    /// row (a second click on the selected row is Enter).
    fn click(&mut self, at: Position) -> Flow {
        if self.hits.search.contains(at) {
            self.searching = true;
            return Flow::Redraw;
        }
        // Anywhere else the page has the keys again, as after Enter in
        // the search box.
        self.searching = false;
        if let Some(&(_, action)) = self.hits.buttons.iter().find(|(r, _)| r.contains(at)) {
            return self.act(action);
        }
        if let Some((_, target)) = self.hits.undo.iter().find(|(r, _)| r.contains(at)) {
            let target = target.clone();
            self.undo(&target);
            return Flow::Redraw;
        }
        let Some(row) = self.row_at(at) else {
            return Flow::Redraw;
        };
        if self.list.selected() == Some(row) {
            self.activate();
        } else {
            self.list.select(Some(row));
        }
        Flow::Redraw
    }

    /// A mouse event while the menu is open: a hover lights the item under
    /// the pointer, a left click on an item does it, any other click
    /// closes the menu.
    fn menu_mouse(&mut self, mouse: MouseEvent, at: Position, menu: Menu) -> Flow {
        let item = rows(self.hits.menu).position(|r| r.contains(at));
        match mouse.kind {
            MouseEventKind::Moved => match item {
                Some(lit) if lit != menu.lit => {
                    self.menu = Some(Menu { lit, ..menu });
                    Flow::Redraw
                }
                _ => Flow::Unchanged,
            },
            MouseEventKind::Down(MouseButton::Left) => {
                self.menu = None;
                let action = item.and_then(|i| {
                    self.selected()
                        .and_then(|row| self.menu_items(row).get(i).map(|&(_, _, a)| a))
                });
                match action {
                    Some(action) => self.act(action),
                    None => Flow::Redraw,
                }
            }
            MouseEventKind::Down(_) => {
                self.menu = None;
                Flow::Redraw
            }
            _ => Flow::Unchanged,
        }
    }

    /// The menu just under its row, or just over it when the screen has no
    /// room under it, fitted to the screen (`Rect::clamp`), the lit item one
    /// solid bar.
    pub(super) fn draw_menu(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(menu) = self.menu else {
            self.hits.menu = Rect::default();
            return;
        };
        let items = self.selected().map_or_default(|row| self.menu_items(row));
        let body = MenuBody {
            items: &items,
            lit: menu.lit,
            text: self.style(Role::Text),
            key: self.style(Role::Muted),
            lit_style: self
                .style(Role::SelectedFg)
                .patch(self.bg(Role::SelectedBg)),
        };
        // The body and its border, under the row (`at`'s line) or over it.
        let width = u16::try_from(body.width() + 2).unwrap_or(u16::MAX);
        let height = u16::try_from(body.height() + 2).unwrap_or(u16::MAX);
        let under = menu.at.y.saturating_add(1);
        let y = if under.saturating_add(height) <= area.bottom() {
            under
        } else {
            menu.at.y.saturating_sub(height)
        };
        let rect = Rect::new(menu.at.x, y, width, height).clamp(area);
        self.hits.menu = rect;
        let mut popup = TuiPopup::new(body);
        popup.border_set = border::ROUNDED;
        popup.border_style = self.style(Role::Border);
        frame.render_widget(popup, rect);
    }
}

/// The menu's items as a popup body: a row each, the label on the left,
/// its key on the right, the lit one a solid bar.
struct MenuBody<'a> {
    items: &'a [Item],
    lit: usize,
    text: Style,
    key: Style,
    lit_style: Style,
}

/// The least space between a label and its key.
const MENU_GAP: usize = 1;

impl KnownSize for MenuBody<'_> {
    /// The widest label, the gap, the widest key, and a column either
    /// side (the rows' margin).
    fn width(&self) -> usize {
        let widest = |part: &dyn Fn(&Item) -> usize| self.items.iter().map(part).max().unwrap_or(0);
        widest(&|(label, _, _)| Span::raw(label.as_str()).width())
            + MENU_GAP
            + widest(&|(_, key, _)| Span::raw(key.to_string()).width())
            + 2
    }

    fn height(&self) -> usize {
        self.items.len()
    }
}

impl Widget for MenuBody<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for (i, ((label, key, _), row)) in self.items.iter().zip(area.rows()).enumerate() {
            let key = Span::styled(key.to_string(), self.key);
            let [label_area, key_area] =
                row.inner(Margin::new(1, 0)).layout(&Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(u16::try_from(key.width()).unwrap_or(u16::MAX)),
                ]));
            Line::styled(label.as_str(), self.text).render(label_area, buf);
            key.render(key_area, buf);
            if i == self.lit {
                buf.set_style(row, self.lit_style);
            }
        }
    }
}
