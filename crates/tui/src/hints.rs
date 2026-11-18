//! The key hints every screen shares (tui spec, "Keys: one map for every
//! screen", Hints and The `?` overlay): each screen's keys as one table
//! ([`Keys`]), the footer row fitted to the width ([`footer`]), and the
//! `?` overlay listing every key of the screen ([`Overlay`]).
//!
//! - **The footer** is a `Line` of `Span`s, as every screen drew its hints
//!   before. Its fit is ours (domain logic; ratatui, tui-widgets and
//!   ratatui's third-party widgets have no key-hint widget): the screen's
//!   hints left-aligned in its order of importance, the last ones dropped
//!   whole when the row is too narrow, and a tail (`? keys · q quit`)
//!   right-aligned and never dropped. sb-go's hint row (bubbles `help`'s
//!   `ShortHelpView`) also dropped whole items from the end
//!   (<https://github.com/charmbracelet/bubbles/blob/v2.2.1/help/help.go>,
//!   `shouldAddItem`). Widths are display widths (`Span::width`).
//! - **The overlay** is ratatui's popup pattern: `Clear`, then a bordered
//!   `Block` over a centred `Rect`
//!   (<https://ratatui.rs/examples/apps/popup/>,
//!   <https://docs.rs/ratatui-core/0.1.2/ratatui_core/layout/struct.Rect.html#method.centered>),
//!   for the block's bottom title, which tui-popup's `Popup` has no field
//!   for. Its groups go in two columns when one column is taller than the
//!   screen and two fit; otherwise one column, scrolled in a tui-scrollview
//!   `ScrollView` ([`crate::split::render_scrolled`]) when taller than the
//!   screen. Any key that doesn't scroll it closes it and does nothing
//!   else, so `q` closes it without quitting.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use sb_cli_core::theme::{Role, Theme};
use tui_scrollview::ScrollViewState;

use crate::term::Flow;

/// One footer hint: its keys as shown, and what they do.
pub(crate) type Hint = (&'static str, &'static str);

/// What a key means (tui spec, "Keys: one map for every screen", rule 1):
/// a key has the same meaning on every screen it works on. `Direction`
/// is rule 7's one declared class: ← → and `h` `l` point at what the
/// screen holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Meaning {
    Move,
    Page,
    Ends,
    Direction,
    Open,
    Part,
    Find,
    Back,
    Leave,
    Stop,
    Help,
    Save,
    Copy,
    Edit,
    Add,
    Delete,
    Undo,
    Menu,
    NewLine,
}

/// One row of a screen's key table: its keys, each as the overlay shows
/// it, what they do there, and what they mean everywhere.
#[derive(Debug)]
pub(crate) struct Key {
    pub(crate) keys: &'static [&'static str],
    pub(crate) what: &'static str,
    pub(crate) meaning: Meaning,
}

/// A heading of the overlay and its rows.
#[derive(Debug)]
pub(crate) struct Group {
    pub(crate) title: &'static str,
    pub(crate) keys: &'static [Key],
}

/// A screen's keys: its name in the overlay's title, its groups, and a
/// last line on the mouse.
#[derive(Debug)]
pub(crate) struct Keys {
    pub(crate) screen: &'static str,
    pub(crate) groups: &'static [Group],
    pub(crate) mouse: &'static str,
}

impl Keys {
    /// The key that opens the overlay, as the footer's tail names it: `?`,
    /// or F1 where the table has no `?` (a form, whose fields take it).
    pub(crate) fn help_key(&self) -> &'static str {
        self.groups
            .iter()
            .flat_map(|g| g.keys)
            .find(|k| k.meaning == Meaning::Help)
            .and_then(|k| k.keys.first().copied())
            .unwrap_or("?")
    }
}

/// The separator between hints, and the least room before the tail.
const SEPARATOR: &str = " · ";
const BEFORE_TAIL: usize = 3;

fn width(text: &str) -> usize {
    Span::raw(text).width()
}

fn hint_width(&(keys, what): &Hint) -> usize {
    width(keys) + 1 + width(what)
}

fn hints_width(hints: &[Hint]) -> usize {
    hints.iter().map(hint_width).sum::<usize>() + width(SEPARATOR) * hints.len().saturating_sub(1)
}

/// `hints` as spans: each key in the accent, its words muted, a faint
/// separator between them.
fn spans(theme: &Theme, hints: &[Hint]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, (keys, what)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                SEPARATOR,
                Style::from(theme.style(Role::Faint)),
            ));
        }
        spans.push(Span::styled(*keys, Style::from(theme.style(Role::Accent))));
        spans.push(Span::styled(
            format!(" {what}"),
            Style::from(theme.style(Role::Muted)),
        ));
    }
    spans
}

/// `text` cut to `room` columns, an `…` at the cut.
fn cut(text: &str, room: usize) -> String {
    if width(text) <= room {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if width(&out) + width(&c.to_string()) + 1 > room {
            break;
        }
        out.push(c);
    }
    if room > 0 {
        out.push('…');
    }
    out
}

/// The footer row, `width` columns wide: `lead` first (a trouble text,
/// never dropped, cut with `…` only when it alone is wider than the room
/// the tail leaves), then as many of `hints` as fit, in order, whole,
/// then `tail` right-aligned. When even the tail doesn't fit, its first
/// hint alone (`? keys`).
pub(crate) fn footer(
    theme: &Theme,
    lead: Option<Span<'static>>,
    hints: &[Hint],
    tail: &[Hint],
    width: u16,
) -> Line<'static> {
    let width = usize::from(width);
    let tail = if hints_width(tail) <= width {
        tail
    } else {
        &tail[..tail.len().min(1)]
    };
    let tail_width = hints_width(tail);
    let mut left: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    if let Some(lead) = lead {
        let room = width.saturating_sub(tail_width + BEFORE_TAIL);
        let text = cut(&lead.content, room);
        used = self::width(&text);
        left.push(Span::styled(text, lead.style));
    }
    let lead_gap = if used > 0 { BEFORE_TAIL } else { 0 };
    let mut shown = 0;
    while shown < hints.len() {
        let next = hints_width(&hints[..=shown]);
        if used + lead_gap + next + BEFORE_TAIL + tail_width > width {
            break;
        }
        shown += 1;
    }
    if shown > 0 {
        if used > 0 {
            left.push(Span::raw(" ".repeat(BEFORE_TAIL)));
            used += BEFORE_TAIL;
        }
        left.extend(spans(theme, &hints[..shown]));
        used += hints_width(&hints[..shown]);
    }
    let pad = width.saturating_sub(used + tail_width);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(spans(theme, tail));
    Line::from(left)
}

/// The `?` overlay's state: whether it is open, its scroll, and its area
/// in the last frame (for a click outside it).
#[derive(Debug, Default)]
pub(crate) struct Overlay {
    open: bool,
    scroll: ScrollViewState,
    area: Rect,
}

/// Whether the scroll keys can move the overlay's text: it is taller than
/// its pane.
fn scrolls(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('k' | 'j' | 'b' | ' ' | 'g' | 'G')
    )
}

impl Overlay {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Whether `key` opens the overlay: `?` (plain, or with Shift as
    /// terminals send it) unless a text box takes it (`typing`), F1
    /// always (htop's help key, for text boxes).
    pub(crate) fn opens(key: &KeyEvent, typing: bool) -> bool {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::F(1) => true,
            KeyCode::Char('?') => plain && !typing,
            _ => false,
        }
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.scroll = ScrollViewState::default();
    }

    /// A key while open: the scroll keys scroll, any other closes it and
    /// does nothing else.
    pub(crate) fn key(&mut self, key: KeyEvent) -> Flow {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if plain && scrolls(key.code) {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.scroll.scroll_up(),
                KeyCode::Down | KeyCode::Char('j') => self.scroll.scroll_down(),
                KeyCode::PageUp | KeyCode::Char('b') => self.scroll.scroll_page_up(),
                KeyCode::PageDown | KeyCode::Char(' ') => self.scroll.scroll_page_down(),
                KeyCode::Home | KeyCode::Char('g') => self.scroll.scroll_to_top(),
                _ => self.scroll.scroll_to_bottom(),
            }
        } else {
            self.open = false;
        }
        Flow::Redraw
    }

    /// The mouse while open: the wheel scrolls; a click outside closes.
    pub(crate) fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll.scroll_down(),
            MouseEventKind::ScrollUp => self.scroll.scroll_up(),
            MouseEventKind::Down(MouseButton::Left)
                if !self.area.contains(Position::new(mouse.column, mouse.row)) =>
            {
                self.open = false;
            }
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// The overlay over the screen, when open.
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>, keys: &Keys, theme: &Theme) {
        if !self.open {
            return;
        }
        let screen = frame.area();
        let room = screen.inner(Margin::new(1, 1));
        let style = |role| Style::from(theme.style(role));
        let mouse = Line::styled(format!(" Mouse: {}", keys.mouse), style(Role::Muted));
        let mut one = column(keys, keys.groups, theme);
        one.push(Line::raw(""));
        one.push(mouse.clone());
        let two = (rows(one.len()) > room.height)
            .then(|| two_columns(keys, theme, room))
            .flatten();
        let content_width = match &two {
            Some((left, right)) => line_width(left) + 3 + line_width(right),
            None => line_width(&one),
        };
        let width = u16::try_from(content_width + 3)
            .unwrap_or(u16::MAX)
            .max(u16::try_from(width(keys.screen) + 12).unwrap_or(u16::MAX))
            .min(room.width);
        // One column wraps to the box (`Paragraph::line_count`, ratatui's
        // `unstable-rendered-line-info`, ADR-001), a column narrower when
        // its scrollbar takes one.
        let wrapped = |lines: &[Line<'static>], width: u16| {
            Paragraph::new(lines.to_vec())
                .wrap(Wrap { trim: false })
                .line_count(width)
        };
        let content_rows = match &two {
            Some((left, right)) => left.len().max(right.len()) + 2,
            None => wrapped(&one, width.saturating_sub(2)),
        };
        let fits = rows(content_rows) <= room.height;
        let height = rows(content_rows).min(room.height);
        let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
        self.area = area;
        let closes = if fits {
            " any key closes "
        } else {
            " ↑↓ scroll · other keys close "
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(style(Role::Border))
            .title(Span::styled(
                format!(" Keys: {} ", keys.screen),
                style(Role::Accent).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(closes, style(Role::Muted)))
            .style(style(Role::Text));
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        match two {
            Some((left, right)) => {
                // The columns, a blank row, the mouse's line.
                let [columns, _, mouse_row] = Layout::vertical([
                    Constraint::Fill(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(inner);
                let [left_area, right_area] = Layout::horizontal([
                    Constraint::Length(u16::try_from(line_width(&left)).unwrap_or(u16::MAX)),
                    Constraint::Fill(1),
                ])
                .spacing(3)
                .areas(columns);
                frame.render_widget(Paragraph::new(left), left_area);
                frame.render_widget(Paragraph::new(right), right_area);
                frame.render_widget(Paragraph::new(mouse), mouse_row);
            }
            None if fits => {
                frame.render_widget(Paragraph::new(one).wrap(Wrap { trim: false }), inner);
            }
            None => {
                let width = inner.width.saturating_sub(1);
                let size = Size::new(
                    width,
                    u16::try_from(wrapped(&one, width)).unwrap_or(u16::MAX),
                );
                crate::split::render_scrolled(
                    frame,
                    inner,
                    &mut self.scroll,
                    Paragraph::new(one).wrap(Wrap { trim: false }),
                    size,
                );
            }
        }
    }
}

/// A box of `n` lines' rows with its borders.
fn rows(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX).saturating_add(2)
}

fn line_width(lines: &[Line<'_>]) -> usize {
    lines.iter().map(Line::width).max().unwrap_or(0)
}

/// `groups` as the overlay's lines: each heading, then its keys, the
/// keys as wide as the widest of the whole table, then what they do.
fn column(keys: &Keys, groups: &[Group], theme: &Theme) -> Vec<Line<'static>> {
    let style = |role| Style::from(theme.style(role));
    let keys_width = keys
        .groups
        .iter()
        .flat_map(|g| g.keys)
        .map(|k| width(&k.keys.join(" ")))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!(" {}", group.title),
            style(Role::Accent).add_modifier(Modifier::BOLD),
        ));
        for key in group.keys {
            let shown = key.keys.join(" ");
            let pad = keys_width - width(&shown);
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(shown, style(Role::Accent)),
                Span::raw(" ".repeat(pad + 2)),
                Span::styled(key.what, style(Role::Text)),
            ]));
        }
    }
    lines
}

/// The groups in two columns, when two fit in `room` with the mouse's
/// line under them: the split that makes the taller column shortest.
fn two_columns(
    keys: &Keys,
    theme: &Theme,
    room: Rect,
) -> Option<(Vec<Line<'static>>, Vec<Line<'static>>)> {
    (1..keys.groups.len())
        .map(|at| {
            (
                column(keys, &keys.groups[..at], theme),
                column(keys, &keys.groups[at..], theme),
            )
        })
        .min_by_key(|(left, right)| left.len().max(right.len()))
        .filter(|(left, right)| {
            line_width(left) + 3 + line_width(right) + 3 <= usize::from(room.width)
                && rows(left.len().max(right.len()) + 2) <= room.height
        })
}

#[cfg(test)]
mod tests;
