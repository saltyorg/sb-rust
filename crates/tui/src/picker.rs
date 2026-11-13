//! The log picker (tui spec, "The picker"): its Services section, one row
//! per service, `name  <symbol> <status text>`, with a selection that
//! skips the section's heading row.
//!
//! The list is ratatui's `List` with a `ListState`
//! (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.List.html>,
//! <https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html>):
//! the state keeps the selection and scrolls the window to it. The
//! selection moves by rows the picker chooses itself (the next service
//! row), because `ListState`'s `select_next`, `select_previous`,
//! `select_first`, `select_last` and `scroll_down_by`/`scroll_up_by` are
//! index arithmetic that would stop on a heading (ratatui-widgets 0.3.2
//! has no non-selectable rows).
//!
//! Needs attention, the Containers section, the name and kind filters,
//! the mouse and the 5 s refresh come with the later slices (tui spec,
//! "Suggested slices", 2 and 3).

use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use sb_cli_core::theme::{Role, Theme};
use sb_host::systemd::Service;
use sb_term_ui::DisplayText;

use crate::status::{Status, service_status};
use crate::term::{App, Flow};

/// The selection's mark, drawn before the selected row (proto-logs' look,
/// reviewed 2026-10-29); it shows the selection without colour too.
const MARK: &str = "› ";

/// One service in the picker.
#[derive(Clone, Debug)]
struct Entry {
    /// The unit's name without `.service`, as sb-host lists it.
    name: String,
    /// The name as the screen shows it (`DisplayText`, one line).
    shown: String,
    status: Status,
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// The Services heading; the selection skips it.
    Services,
    /// The service at this index of the entries.
    Service(usize),
}

/// The picker: an [`App`] for [`crate::Tui::run`]. Enter on a service
/// quits the run with that service chosen ([`Picker::take_choice`]); `q`
/// and Esc quit it with none.
#[derive(Debug)]
pub struct Picker {
    entries: Vec<Entry>,
    rows: Vec<Row>,
    state: ListState,
    theme: Theme,
    /// The list's height at the last draw, for the page keys.
    height: usize,
    chosen: Option<usize>,
}

impl Picker {
    /// The picker on `services`, their status as at `now`, sorted by name
    /// (tui spec, "Sections"), the first one selected.
    #[must_use]
    pub fn new(services: &[Service], now: Timestamp, theme: Theme) -> Self {
        let mut entries: Vec<Entry> = services
            .iter()
            .map(|service| Entry {
                name: service.name.clone(),
                shown: one_line(&service.name),
                status: service_status(service, now),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut rows = Vec::with_capacity(entries.len() + 1);
        if !entries.is_empty() {
            rows.push(Row::Services);
            rows.extend((0..entries.len()).map(Row::Service));
        }
        let mut picker = Self {
            entries,
            rows,
            state: ListState::default(),
            theme,
            height: 0,
            chosen: None,
        };
        picker.state.select(picker.next_from(0, 1));
        picker
    }

    /// The service chosen with Enter (its name without `.service`), taken
    /// so that the next run of the picker starts with none.
    pub fn take_choice(&mut self) -> Option<String> {
        self.chosen
            .take()
            .and_then(|i| self.entries.get(i))
            .map(|entry| entry.name.clone())
    }

    /// The first selectable row from `at` on, going by `step` (1 or -1).
    fn next_from(&self, at: usize, step: isize) -> Option<usize> {
        let mut i = isize::try_from(at).ok()?;
        while let Some(row) = usize::try_from(i).ok().and_then(|i| self.rows.get(i)) {
            if matches!(row, Row::Service(_)) {
                return usize::try_from(i).ok();
            }
            i += step;
        }
        None
    }

    /// Move the selection `by` service rows (negative: up), stopping at
    /// either end.
    fn move_by(&mut self, by: isize) {
        let Some(mut at) = self.state.selected() else {
            return;
        };
        let step = by.signum();
        for _ in 0..by.unsigned_abs() {
            let Some(next) = at
                .checked_add_signed(step)
                .and_then(|from| self.next_from(from, step))
            else {
                break;
            };
            at = next;
        }
        self.state.select(Some(at));
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Flow::Unchanged;
        }
        let page = isize::try_from(self.height.max(1)).unwrap_or(isize::MAX);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Home => self.state.select(self.next_from(0, 1)),
            KeyCode::End => {
                let last = self.rows.len().saturating_sub(1);
                self.state.select(self.next_from(last, -1));
            }
            KeyCode::Enter => {
                let chosen = self.state.selected().and_then(|i| self.rows.get(i));
                if let Some(&Row::Service(i)) = chosen {
                    self.chosen = Some(i);
                    return Flow::Quit;
                }
                return Flow::Unchanged;
            }
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    /// The title row: the screen's name and the counts.
    fn title(&self) -> TextLine<'static> {
        let problems = self.entries.iter().filter(|e| e.status.attention).count();
        let services = match self.entries.len() {
            1 => "1 service".to_owned(),
            n => format!("{n} services"),
        };
        let health = match problems {
            0 => Span::styled("all healthy", self.style(Role::Ok)),
            1 => Span::styled("1 needs attention", self.style(Role::Problem)),
            n => Span::styled(format!("{n} need attention"), self.style(Role::Problem)),
        };
        TextLine::from(vec![
            Span::styled(
                " ◆ Logs ",
                self.style(Role::OnAccent).patch(self.bg(Role::Accent)),
            ),
            Span::raw("  "),
            Span::styled(services, self.style(Role::Service)),
            Span::styled(" · ", self.style(Role::Faint)),
            health,
        ])
    }

    /// `role`'s colour as a background (none under `ascii`), through
    /// ratatui-core's `anstyle` conversion, as the foreground roles.
    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// The list's items: the heading with its rule, then the rows.
    ///
    /// Why a `List` of padded rows, not ratatui's `Table`: the spec's
    /// mouse rows come from `ListState::offset()` (tui spec, Tech Stack),
    /// and a `Table` wouldn't remove the heading-skipping selection. The
    /// heading's rule is drawn as text: ratatui-widgets 0.3.2 has no rule
    /// widget, and a titled `Block` can't be a `ListItem` that scrolls
    /// with the rows (UNVERIFIED that no other ratatui-org crate has one;
    /// checked against ratatui-widgets, tui-popup and tui-scrollview).
    /// Widths are display widths (`Span::width`, unicode-width).
    fn items(&self, width: u16) -> Vec<ListItem<'static>> {
        let name_width = self
            .entries
            .iter()
            .map(|e| Span::raw(e.shown.as_str()).width())
            .max()
            .unwrap_or(0);
        // The rule runs to the right edge, less the selection's mark.
        let rule_width = usize::from(width).saturating_sub(Span::raw(MARK).width());
        self.rows
            .iter()
            .map(|row| match *row {
                Row::Services => {
                    let title = format!("Services ({}) ", self.entries.len());
                    let rule = "─".repeat(rule_width.saturating_sub(Span::raw(&title).width()));
                    ListItem::new(TextLine::from(vec![
                        Span::styled(title, self.style(Role::Service)),
                        Span::styled(rule, self.style(Role::ServiceRule)),
                    ]))
                }
                Row::Service(i) => {
                    let entry = &self.entries[i];
                    let pad = name_width.saturating_sub(Span::raw(entry.shown.as_str()).width());
                    let status = self.style(entry.status.role);
                    ListItem::new(TextLine::from(vec![
                        Span::styled(entry.shown.clone(), self.style(Role::Service)),
                        Span::raw(" ".repeat(pad + 2)),
                        Span::styled(
                            format!("{} {}", entry.status.symbol, entry.status.text),
                            status,
                        ),
                    ]))
                }
            })
            .collect()
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

/// `name` as one line of plain screen text (sb's names reach the screen
/// only through `DisplayText`).
fn one_line(name: &str) -> String {
    DisplayText::from_bytes(name.as_bytes(), false)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

impl App for Picker {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) => self.key(key),
            _ => Flow::Unchanged,
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        // ratatui's layout: the title, a blank row, the list, the footer
        // (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.vertical>).
        let [title, _, list, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(Paragraph::new(self.title()), title);
        self.height = usize::from(list.height);
        // The selected row is one solid bar: the highlight style patches
        // every span of it (proto-logs' `selected_row_is_one_uniform_bar`).
        let selected = self
            .style(Role::SelectedFg)
            .patch(self.bg(Role::SelectedBg));
        let widget = List::new(self.items(list.width))
            .highlight_symbol(MARK)
            .highlight_style(selected);
        frame.render_stateful_widget(widget, list, &mut self.state);
        frame.render_widget(Paragraph::new(self.footer()), footer);
    }
}

#[cfg(test)]
mod tests;
