//! A list beside a pane (above it when narrow): the announcement viewer's
//! layout and list handling (tui spec, "The announcement viewer"), shared
//! with restore's viewer (ops decision 32).
//!
//! - **The list** is ratatui's `List` with a `ListState`
//!   (<https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html>):
//!   its `select_next` and `select_previous` move it, clamped here at
//!   once, since `ListState` corrects them only when the list is drawn
//!   (ratatui-widgets 0.3.2 `list/state.rs`).
//! - **A click:** `List` has no hit-testing, so a click's row is
//!   `ListState::offset()` plus its row inside the block (`Block::inner`),
//!   as the picker maps it.
//! - **The pane** is a tui-scrollview `ScrollView`
//!   (<https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/struct.ScrollView.html>)
//!   of a wrapped `Paragraph`: it clamps the offset while rendering and
//!   draws its scrollbar only when the content doesn't fit.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{ListItem, ListState, Paragraph};
use sb_cli_core::theme::{Profile, Role, Theme};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

/// The most rows a pane holds: tui-scrollview draws the whole content
/// every frame, into a buffer of this many rows at most (tui spec,
/// "Scrolling").
pub(crate) const MAX_ROWS: u16 = 2000;

/// Below this width the list goes above the pane.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 70;

/// The list's narrowest width, its border included.
const LIST_MIN_WIDTH: u16 = 24;

/// The list's and the pane's areas in `body`: side by side from 70
/// columns, the list as wide as its widest row or its `title` plus its
/// border, from 24 columns up to 40% of the screen; else the list above,
/// as tall as its rows plus its border, up to a third of the screen.
pub(crate) fn areas(body: Rect, rows: &[ListItem<'_>], title: &Line<'_>) -> [Rect; 2] {
    if body.width >= SIDE_BY_SIDE_MIN_WIDTH {
        let widest = rows
            .iter()
            .map(ListItem::width)
            .chain([title.width()])
            .max()
            .unwrap_or(0);
        let width = u16::try_from(widest + 2)
            .unwrap_or(u16::MAX)
            .clamp(LIST_MIN_WIDTH, body.width * 2 / 5);
        Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(body)
    } else {
        let height = u16::try_from(rows.len() + 2)
            .unwrap_or(u16::MAX)
            .min(body.height / 3);
        Layout::vertical([Constraint::Length(height), Constraint::Fill(1)]).areas(body)
    }
}

/// `movement` run on `list` of `len` rows, the selection clamped to the
/// last row: whether the selection changed (a move past either end is no
/// move).
pub(crate) fn move_list(
    list: &mut ListState,
    len: usize,
    movement: impl FnOnce(&mut ListState),
) -> bool {
    if len == 0 {
        return false;
    }
    let before = list.selected();
    movement(list);
    if let Some(selected) = list.selected() {
        list.select(Some(selected.min(len - 1)));
    }
    list.selected() != before
}

/// The row of `len` a click at `at` lands on, `rows` being the list's
/// rows inside its border at the last draw; none outside them or under
/// the last row.
pub(crate) fn clicked(list: &ListState, rows: Rect, at: Position, len: usize) -> Option<usize> {
    if !rows.contains(at) {
        return None;
    }
    let row = list.offset() + usize::from(at.y - rows.y);
    (row < len).then_some(row)
}

/// The selected row's bar: the Selected roles, reverse video with colour
/// off (tui decision 23's rule).
pub(crate) fn selected(theme: &Theme) -> Style {
    let bar = Style::from(theme.style(Role::SelectedFg)).patch(Style::from(
        anstyle::Style::new().bg_color(theme.color(Role::SelectedBg)),
    ));
    reversed_without_colour(theme, bar)
}

/// `style`, in reverse video too when the theme has no colour, where its
/// background isn't drawn (tui decision 23's rule).
pub(crate) fn reversed_without_colour(theme: &Theme, style: Style) -> Style {
    if theme.profile() == Profile::Ascii {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

/// `content`, `width` columns by `height` rows (at most [`MAX_ROWS`]), in
/// a `ScrollView` drawn into `area` at `state`'s offset, with no
/// horizontal scrollbar.
pub(crate) fn render_scrolled(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut ScrollViewState,
    content: Paragraph<'static>,
    size: Size,
) {
    let mut view = ScrollView::new(Size::new(size.width, size.height.min(MAX_ROWS)))
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never);
    view.render_widget(content, view.area());
    frame.render_stateful_widget(view, area, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> Vec<ListItem<'static>> {
        (0..n).map(|i| ListItem::new(format!("row {i}"))).collect()
    }

    #[test]
    fn side_by_side_from_70_columns_else_above() {
        let title = Line::from("Files");
        let [list, pane] = areas(Rect::new(0, 0, 100, 30), &rows(3), &title);
        assert_eq!((list.width, pane.x), (24, 24), "the narrowest list");
        assert_eq!(list.height, 30);
        let [list, pane] = areas(Rect::new(0, 0, 60, 30), &rows(3), &title);
        assert_eq!((list.height, pane.y, list.width), (5, 5, 60));
        let wide = vec![ListItem::new("x".repeat(60))];
        let [list, _] = areas(Rect::new(0, 0, 100, 30), &wide, &title);
        assert_eq!(list.width, 40, "at most 40%");
    }

    #[test]
    fn moves_are_clamped_and_report_a_change() {
        let mut list = ListState::default().with_selected(Some(0));
        assert!(!move_list(&mut list, 2, ListState::select_previous));
        assert!(move_list(&mut list, 2, ListState::select_next));
        assert!(!move_list(&mut list, 2, ListState::select_next));
        assert_eq!(list.selected(), Some(1));
        assert!(!move_list(&mut list, 0, ListState::select_next));
    }

    #[test]
    fn a_click_maps_to_its_row() {
        let list = ListState::default();
        let rows = Rect::new(1, 1, 20, 5);
        assert_eq!(clicked(&list, rows, Position::new(2, 3), 9), Some(2));
        assert_eq!(
            clicked(&list, rows, Position::new(2, 3), 2),
            None,
            "under the last"
        );
        assert_eq!(
            clicked(&list, rows, Position::new(0, 3), 9),
            None,
            "on the border"
        );
    }

    #[test]
    fn the_bar_is_reversed_with_colour_off() {
        assert!(
            selected(&Theme::plain())
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}
