//! The log picker (tui spec, "The picker"): the Services and Containers
//! sections, one row per source, `name  <symbol> <status text>`, with a
//! selection that skips the sections' heading rows and the
//! Docker-unreachable notice row.
//!
//! The list is ratatui's `List` with a `ListState`
//! (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.List.html>,
//! <https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html>):
//! the state keeps the selection and scrolls the window to it. The
//! selection moves by rows the picker chooses itself (the next source
//! row), because `ListState`'s `select_next`, `select_previous`,
//! `select_first`, `select_last` and `scroll_down_by`/`scroll_up_by` are
//! index arithmetic that would stop on a heading (ratatui-widgets 0.3.2
//! has no non-selectable rows).
//!
//! The Containers section is kept live by the docker adapter's
//! [`watch`](crate::source::docker::watch): each new list replaces the
//! section, the selection staying on the same source, or on the row now
//! at its place when that source is gone (tui spec, "Live updates").
//!
//! Needs attention, the name and kind filters (`Tab`), the mouse and the
//! services' 5 s refresh come with the tui spec's slice 3.

use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use sb_cli_core::theme::{Role, Theme};
use sb_docker::ps::{Row as ContainerRow, picker_status};
use sb_host::systemd::{Service, format_duration};
use sb_term_ui::DisplayText;
use tokio::sync::watch;

use crate::source::docker::{Container, Snapshot};
use crate::status::{Status, service_status};
use crate::term::{App, Flow};
use crate::viewer::Kind;

/// The selection's mark, drawn before the selected row (proto-logs' look,
/// reviewed 2026-10-29); it shows the selection without colour too.
const MARK: &str = "› ";

/// The kinds the picker shows (tui spec, "Entry points": `sb logs` all,
/// `sb docker logs` containers; `Tab` comes with slice 3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Containers,
}

impl Filter {
    fn shows(self, kind: Kind) -> bool {
        match self {
            Filter::All => true,
            Filter::Containers => kind == Kind::Container,
        }
    }
}

/// What Enter chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A service, by its name without `.service`, as sb-host lists it.
    Service(String),
    Container(Container),
}

/// One source in the picker.
#[derive(Clone, Debug)]
struct Entry {
    kind: Kind,
    /// The service's name without `.service`, as sb-host lists it, or the
    /// container's as `ps` shows it.
    name: String,
    /// The name as the screen shows it (`DisplayText`, one line).
    shown: String,
    status: Status,
    /// A container's ID and creation.
    container: Option<Container>,
}

impl Entry {
    fn choice(&self) -> Choice {
        match &self.container {
            Some(container) => Choice::Container(container.clone()),
            None => Choice::Service(self.name.clone()),
        }
    }
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// A section's heading; the selection skips it.
    Heading(Kind),
    /// The source at this index of the entries.
    Source(usize),
    /// The Containers section's one row when Docker is unreachable; the
    /// selection skips it.
    Unreachable,
}

/// The picker: an [`App`] for [`crate::Tui::run`]. Enter on a source
/// quits the run with that source chosen ([`Picker::take_choice`]); `q`
/// and Esc quit it with none.
#[derive(Debug)]
pub struct Picker {
    /// The services, then the containers, each sorted by name.
    entries: Vec<Entry>,
    rows: Vec<Row>,
    state: ListState,
    theme: Theme,
    filter: Filter,
    /// The Containers section is shown (Docker was asked).
    docker: bool,
    /// Why Docker couldn't be reached, shown as the section's one row.
    unreachable: Option<String>,
    /// The Containers section's live list.
    updates: Option<watch::Receiver<Snapshot>>,
    /// Why the containers may be out of date, in the footer.
    trouble: Option<String>,
    /// The list's height at the last draw, for the page keys.
    height: usize,
    chosen: Option<usize>,
}

impl Picker {
    /// The picker on `services`, their status as at `now`, sorted by name
    /// (tui spec, "Sections"), the first one selected; no Containers
    /// section until [`Picker::with_containers`] or
    /// [`Picker::with_unreachable`].
    #[must_use]
    pub fn new(services: &[Service], now: Timestamp, theme: Theme) -> Self {
        let mut entries: Vec<Entry> = services
            .iter()
            .map(|service| Entry {
                kind: Kind::Service,
                name: service.name.clone(),
                shown: one_line(&service.name),
                status: service_status(service, now),
                container: None,
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut picker = Self {
            entries,
            rows: Vec::new(),
            state: ListState::default(),
            theme,
            filter: Filter::All,
            docker: false,
            unreachable: None,
            updates: None,
            trouble: None,
            height: 0,
            chosen: None,
        };
        picker.lay_out();
        picker
    }

    /// The Containers section on `rows`, their status as at `now`.
    #[must_use]
    pub fn with_containers(mut self, rows: &[ContainerRow], now: Timestamp) -> Self {
        self.docker = true;
        self.set_containers(rows, now);
        self
    }

    /// The Containers section as one notice row: Docker couldn't be
    /// reached, for `cause` (tui spec, "First load before the screen").
    #[must_use]
    pub fn with_unreachable(mut self, cause: &str) -> Self {
        self.docker = true;
        self.unreachable = Some(one_line(cause));
        self.lay_out();
        self
    }

    /// Show only the kinds `filter` lets through.
    #[must_use]
    pub fn with_filter(mut self, filter: Filter) -> Self {
        self.filter = filter;
        self.lay_out();
        self
    }

    /// Keep the Containers section live from `updates` (the docker
    /// adapter's `watch`).
    #[must_use]
    pub fn with_updates(mut self, updates: watch::Receiver<Snapshot>) -> Self {
        self.updates = Some(updates);
        self
    }

    /// The source chosen with Enter, taken so that the next run of the
    /// picker starts with none.
    pub fn take_choice(&mut self) -> Option<Choice> {
        self.chosen
            .take()
            .and_then(|i| self.entries.get(i))
            .map(Entry::choice)
    }

    /// Replace the containers with `rows`, as at `now`, keeping the
    /// selection.
    fn set_containers(&mut self, rows: &[ContainerRow], now: Timestamp) {
        let selected = self.selected_entry().map(|e| (e.kind, e.name.clone()));
        let at = self.state.selected();
        self.entries.retain(|e| e.kind != Kind::Container);
        let mut containers: Vec<Entry> = rows
            .iter()
            .map(|row| {
                let status = picker_status(row, now, format_duration);
                Entry {
                    kind: Kind::Container,
                    name: row.name.clone(),
                    shown: one_line(&row.name),
                    status: Status {
                        symbol: status.symbol,
                        text: status.text,
                        role: status.role,
                        attention: status.attention,
                    },
                    container: Some(Container {
                        id: row.id.clone(),
                        name: row.name.clone(),
                        created: row.created,
                    }),
                }
            })
            .collect();
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        self.entries.extend(containers);
        self.unreachable = None;
        // A chosen index would point elsewhere now.
        self.chosen = None;
        self.lay_out();
        let same = selected.and_then(|(kind, name)| {
            self.rows.iter().position(|row| match row {
                Row::Source(i) => self.entries[*i].kind == kind && self.entries[*i].name == name,
                _ => false,
            })
        });
        let place = || {
            let at = at?.min(self.rows.len().checked_sub(1)?);
            self.next_from(at, 1).or_else(|| self.next_from(at, -1))
        };
        let selection = same.or_else(place).or_else(|| self.next_from(0, 1));
        self.state.select(selection);
    }

    /// The rows for the entries and the filter, the first source
    /// selected.
    fn lay_out(&mut self) {
        let mut rows = Vec::with_capacity(self.entries.len() + 2);
        for kind in [Kind::Service, Kind::Container] {
            if !self.filter.shows(kind) {
                continue;
            }
            let start = rows.len();
            rows.extend(
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.kind == kind)
                    .map(|(i, _)| Row::Source(i)),
            );
            if kind == Kind::Container && self.unreachable.is_some() {
                rows.push(Row::Unreachable);
            }
            if rows.len() > start {
                rows.insert(start, Row::Heading(kind));
            }
        }
        self.rows = rows;
        self.state.select(self.next_from(0, 1));
    }

    fn selected_entry(&self) -> Option<&Entry> {
        match self.state.selected().and_then(|i| self.rows.get(i)) {
            Some(Row::Source(i)) => self.entries.get(*i),
            _ => None,
        }
    }

    /// The first selectable row from `at` on, going by `step` (1 or -1).
    fn next_from(&self, at: usize, step: isize) -> Option<usize> {
        let mut i = isize::try_from(at).ok()?;
        while let Some(row) = usize::try_from(i).ok().and_then(|i| self.rows.get(i)) {
            if matches!(row, Row::Source(_)) {
                return usize::try_from(i).ok();
            }
            i += step;
        }
        None
    }

    /// Move the selection `by` source rows (negative: up), stopping at
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
                if let Some(&Row::Source(i)) = chosen {
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

    fn count(&self, kind: Kind) -> usize {
        self.entries.iter().filter(|e| e.kind == kind).count()
    }

    /// The title row: the screen's name and the counts of what is shown.
    fn title(&self) -> TextLine<'static> {
        let shown = |e: &&Entry| self.filter.shows(e.kind);
        let problems = self
            .entries
            .iter()
            .filter(shown)
            .filter(|e| e.status.attention)
            .count();
        let counted = |n: usize, one: &str, many: &str| match n {
            1 => format!("1 {one}"),
            n => format!("{n} {many}"),
        };
        let mut spans = vec![
            Span::styled(
                " ◆ Logs ",
                self.style(Role::OnAccent).patch(self.bg(Role::Accent)),
            ),
            Span::raw("  "),
        ];
        let faint = self.style(Role::Faint);
        if self.filter.shows(Kind::Service) {
            let services = counted(self.count(Kind::Service), "service", "services");
            spans.push(Span::styled(services, self.style(Role::Service)));
            spans.push(Span::styled(" · ", faint));
        }
        if self.docker && self.unreachable.is_none() && self.filter.shows(Kind::Container) {
            let containers = counted(self.count(Kind::Container), "container", "containers");
            spans.push(Span::styled(containers, self.style(Role::Container)));
            spans.push(Span::styled(" · ", faint));
        }
        spans.push(match problems {
            0 => Span::styled("all healthy", self.style(Role::Ok)),
            1 => Span::styled("1 needs attention", self.style(Role::Problem)),
            n => Span::styled(format!("{n} need attention"), self.style(Role::Problem)),
        });
        TextLine::from(spans)
    }

    /// `role`'s colour as a background (none under `ascii`), through
    /// ratatui-core's `anstyle` conversion, as the foreground roles.
    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// The list's items: each heading with its rule, then its rows.
    ///
    /// Why a `List` of padded rows, not ratatui's `Table`: the spec's
    /// mouse rows come from `ListState::offset()` (tui spec, Tech Stack),
    /// and a `Table` wouldn't remove the heading-skipping selection. The
    /// heading's rule is drawn as text: ratatui-widgets 0.3.2 has no rule
    /// widget, and a titled `Block` can't be a `ListItem` that scrolls
    /// with the rows (UNVERIFIED that no other ratatui-org crate has one;
    /// checked against ratatui-widgets, tui-popup and tui-scrollview).
    /// Widths are display widths (`Span::width`, unicode-width); names
    /// line up across the sections.
    fn items(&self, width: u16) -> Vec<ListItem<'static>> {
        let name_width = self
            .entries
            .iter()
            .filter(|e| self.filter.shows(e.kind))
            .map(|e| Span::raw(e.shown.as_str()).width())
            .max()
            .unwrap_or(0);
        // The rule runs to the right edge, less the selection's mark.
        let rule_width = usize::from(width).saturating_sub(Span::raw(MARK).width());
        self.rows
            .iter()
            .map(|row| match *row {
                Row::Heading(kind) => {
                    let (word, rule_role) = match kind {
                        Kind::Service => ("Services", Role::ServiceRule),
                        Kind::Container => ("Containers", Role::ContainerRule),
                    };
                    let title = if self.unreachable.is_some() && kind == Kind::Container {
                        format!("{word} ")
                    } else {
                        format!("{word} ({}) ", self.count(kind))
                    };
                    let rule = "─".repeat(rule_width.saturating_sub(Span::raw(&title).width()));
                    ListItem::new(TextLine::from(vec![
                        Span::styled(title, self.style(kind.role())),
                        Span::styled(rule, self.style(rule_role)),
                    ]))
                }
                Row::Source(i) => {
                    let entry = &self.entries[i];
                    let pad = name_width.saturating_sub(Span::raw(entry.shown.as_str()).width());
                    let status = self.style(entry.status.role);
                    ListItem::new(TextLine::from(vec![
                        Span::styled(entry.shown.clone(), self.style(entry.kind.role())),
                        Span::raw(" ".repeat(pad + 2)),
                        Span::styled(
                            format!("{} {}", entry.status.symbol, entry.status.text),
                            status,
                        ),
                    ]))
                }
                Row::Unreachable => ListItem::new(TextLine::styled(
                    format!(
                        "Docker unreachable: {}",
                        self.unreachable.as_deref().unwrap_or_default()
                    ),
                    self.style(Role::Problem),
                )),
            })
            .collect()
    }

    /// The key hints, then why the containers may be out of date.
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
        if let Some(trouble) = &self.trouble {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(one_line(trouble), self.style(Role::Warn)));
        }
        TextLine::from(spans)
    }
}

/// `name` as one line of plain screen text (sb's names, and Docker's
/// messages, reach the screen only through `DisplayText`).
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

    /// A new container list or a change in the live updates' trouble.
    fn wake(&mut self) -> Flow {
        let Some(updates) = self.updates.as_mut() else {
            return Flow::Unchanged;
        };
        if !updates.has_changed().unwrap_or(false) {
            return Flow::Unchanged;
        }
        let snapshot = updates.borrow_and_update().clone();
        self.trouble = snapshot.trouble;
        if let Some(rows) = snapshot.rows {
            self.set_containers(&rows, Timestamp::now());
        }
        Flow::Redraw
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
