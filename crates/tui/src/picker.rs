//! The log picker (tui spec, "The picker"): the Needs attention,
//! Services and Containers sections, one row per source,
//! `name  <symbol> <status text>`, with a selection that skips the
//! sections' heading rows and their notice rows.
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
//! - **Needs attention** heads the list: every shown source whose status
//!   has `attention`, also listed under its kind.
//! - **Filters:** `Tab` cycles the kind filter (all, services,
//!   containers); `/` opens the name filter, a ratatui-textarea line
//!   (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html>)
//!   that narrows the list as it is typed, smart-case substring; Enter
//!   keeps it, Esc clears it.
//! - **Live:** the Containers section follows the docker adapter's
//!   [`watch`](crate::source::docker::watch); the services are read again
//!   every 5 s while the picker shows (its tick), and every status is
//!   worked out again then, so times and the 10-minute restart rule move
//!   on. Across reloads and filtering the selection stays on the same
//!   source, or on the row now at its place when that source is gone
//!   (tui spec, "Live updates").
//! - **Mouse:** the wheel moves the selection; a click selects a row, a
//!   click on the selected row opens it; other rows ignore clicks. The
//!   row under the pointer is the list's area and `ListState::offset()`
//!   (tui spec, "Mouse").

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::future::BoxFuture;
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui_textarea::{CursorMove, TextArea};
use sb_cli_core::config::escape_controls;
use sb_cli_core::theme::{Role, Theme};
use sb_docker::ps::{Row as ContainerRow, picker_status};
use sb_host::systemd::{Service, format_duration};
use sb_term_ui::DisplayText;
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::Dispatch;
use tracing::instrument::WithSubscriber as _;
use tracing::subscriber::NoSubscriber;

use crate::source::docker::{Container, Snapshot};
use crate::status::{Status, service_status};
use crate::term::{App, Flow};
use crate::viewer::Kind;

/// The selection's mark, drawn before the selected row (proto-logs' look,
/// reviewed 2026-10-29); it shows the selection without colour too.
const MARK: &str = "› ";

/// How often the services are read again while the picker shows (tui
/// spec, "Live updates").
const REFRESH: Duration = Duration::from_secs(5);

/// The label before the name filter's line, and the hint after it.
const FILTER_LABEL: &str = " Filter: ";
const FILTER_HINT: &str = "Enter keeps · Esc clears ";

/// The kinds the picker shows (tui spec, "Entry points": `sb logs` opens
/// on all; `Tab` cycles them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    All,
    Services,
    Containers,
}

impl Filter {
    fn shows(self, kind: Kind) -> bool {
        match self {
            Filter::All => true,
            Filter::Services => kind == Kind::Service,
            Filter::Containers => kind == Kind::Container,
        }
    }

    /// The filter `Tab` moves to.
    fn next(self) -> Self {
        match self {
            Filter::All => Filter::Services,
            Filter::Services => Filter::Containers,
            Filter::Containers => Filter::All,
        }
    }
}

/// Smart case (decided.md A-Q5), as the viewer's search: a query with an
/// upper-case letter is matched exactly, any other ignoring case. `shown`
/// is the name as the screen shows it.
fn name_matches(shown: &str, query: &str) -> bool {
    if query.chars().any(char::is_uppercase) {
        shown.contains(query)
    } else {
        shown.to_lowercase().contains(&query.to_lowercase())
    }
}

/// What Enter chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// A service, by its name without `.service`, as sb-host lists it.
    Service(String),
    Container(Container),
}

/// Reads the service list again for the 5 s refresh: the cause as text
/// when it fails. Its records belong in the future it returns, which runs
/// silenced; the call itself isn't.
pub type ListServices =
    Box<dyn Fn() -> BoxFuture<'static, Result<Vec<Service>, String>> + Send + Sync>;

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

/// The list's sections, in their order (tui spec, "Sections").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Attention,
    Services,
    Containers,
}

/// One row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// A section's heading; the selection skips it.
    Heading(Section),
    /// The source at this index of the entries, in that section.
    Source(Section, usize),
    /// The Containers section's one row when Docker couldn't be reached;
    /// the selection skips it.
    Unreachable,
}

/// A refresh that keeps failing (maintainer, 2026-11-14; tui spec,
/// "Verification notes"): the cause last logged and how many reads have
/// failed since the last good one.
#[derive(Debug)]
struct Failing {
    cause: String,
    failed_reads: u64,
}

/// A failed read: logged when the failure starts or its cause changes,
/// not each 5 s while it keeps failing.
fn log_failure(failing: &mut Option<Failing>, cause: &str) {
    match failing {
        Some(last) if last.cause == cause => last.failed_reads += 1,
        _ => {
            tracing::warn!(error = %escape_controls(cause), "service refresh failed");
            let failed_reads = failing.as_ref().map_or(0, |f| f.failed_reads) + 1;
            *failing = Some(Failing {
                cause: cause.to_owned(),
                failed_reads,
            });
        }
    }
}

/// A good read: after failed ones, one record with how many failed.
fn log_recovery(failing: &mut Option<Failing>) {
    if let Some(Failing { failed_reads, .. }) = failing.take() {
        tracing::info!(failed_reads, "service refresh recovered");
    }
}

/// The 5 s refresh of the services.
struct Refresh {
    list: ListServices,
    /// What wakes the screen when a read ends ([`crate::Tui::waker`]).
    waker: Arc<Notify>,
    /// The subscriber the reads run under: none at all.
    silence: Dispatch,
    /// The read under way, if any.
    pending: Option<Pending>,
}

impl fmt::Debug for Refresh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Refresh")
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}

/// A read of the services under way; dropping it stops the read. Why not
/// tokio-util's `AbortOnDropHandle`: see [`crate::source::SourceHandle`]'s
/// why-not (the `rt` feature it needs).
#[derive(Debug)]
struct Pending {
    result: oneshot::Receiver<Result<Vec<Service>, String>>,
    task: JoinHandle<()>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The picker: an [`App`] for [`crate::Tui::run`]. Enter on a source
/// quits the run with that source chosen ([`Picker::take_choice`]); `q`
/// and Esc quit it with none.
#[derive(Debug)]
pub struct Picker {
    /// The services and containers as last listed, for working out their
    /// status again.
    services: Vec<Service>,
    containers: Vec<ContainerRow>,
    /// The services, then the containers, each sorted by name.
    entries: Vec<Entry>,
    rows: Vec<Row>,
    state: ListState,
    theme: Theme,
    filter: Filter,
    /// The name filter in effect.
    query: String,
    /// The name filter's line while it is open.
    editing: Option<TextArea<'static>>,
    /// The Containers section is shown (Docker was asked).
    docker: bool,
    /// Why Docker couldn't be reached, shown as the section's one row.
    unreachable: Option<String>,
    /// The Containers section's live list.
    updates: Option<watch::Receiver<Snapshot>>,
    /// Why the containers may be out of date, in the footer.
    trouble: Option<String>,
    refresh: Option<Refresh>,
    /// Why the services may be out of date, in the footer.
    services_trouble: Option<String>,
    /// The refresh's failure since its last good read, for the run log.
    failing: Option<Failing>,
    /// When the statuses were last worked out (and the services last
    /// asked for).
    restated: Instant,
    /// The list's area at the last draw, for the mouse and the page keys.
    area: Rect,
    chosen: Option<usize>,
}

impl Picker {
    /// The picker on `services`, their status as at `now`, sorted by name
    /// (tui spec, "Sections"), the first row selected; no Containers
    /// section until [`Picker::with_containers`] or
    /// [`Picker::with_unreachable`]. [`Picker::with_containers`] and
    /// [`Picker::with_unreachable`] select the first row again: they build
    /// the picker the screen opens on.
    #[must_use]
    pub fn new(services: &[Service], now: Timestamp, theme: Theme) -> Self {
        let mut picker = Self {
            services: services.to_vec(),
            containers: Vec::new(),
            entries: Vec::new(),
            rows: Vec::new(),
            state: ListState::default(),
            theme,
            filter: Filter::All,
            query: String::new(),
            editing: None,
            docker: false,
            unreachable: None,
            updates: None,
            trouble: None,
            refresh: None,
            services_trouble: None,
            failing: None,
            restated: Instant::now(),
            area: Rect::default(),
            chosen: None,
        };
        picker.restate(now);
        picker
    }

    /// The Containers section on `rows`, their status as at `now`.
    #[must_use]
    pub fn with_containers(mut self, rows: &[ContainerRow], now: Timestamp) -> Self {
        self.docker = true;
        self.containers = rows.to_vec();
        self.unreachable = None;
        self.state.select(None);
        self.restate(now);
        self
    }

    /// The Containers section as one notice row: Docker couldn't be
    /// reached, for `cause` (tui spec, "First load before the screen").
    #[must_use]
    pub fn with_unreachable(mut self, cause: &str) -> Self {
        self.docker = true;
        self.unreachable = Some(one_line(cause));
        self.state.select(None);
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

    /// Read the services again with `list` every 5 s while the picker
    /// shows, `waker` woken when a read ends. The reads run on the tokio
    /// runtime the picker's tick runs on.
    #[must_use]
    pub fn with_refresh(mut self, list: ListServices, waker: Arc<Notify>) -> Self {
        self.refresh = Some(Refresh {
            list,
            waker,
            // Why a registered `Dispatch` of a `NoSubscriber`, not
            // `Dispatch::none()`: `none()` doesn't register with
            // tracing's callsites, so a callsite first hit inside a read
            // would be cached as never enabled for the whole process, its
            // records lost after the read too (tracing-core 0.1.36
            // `callsite.rs`, `Rebuilder::JustOne`). Built once: each new
            // `Dispatch` rebuilds every callsite's interest.
            silence: Dispatch::new(NoSubscriber::new()),
            pending: None,
        });
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

    /// The entries from the lists, their status as at `now`, keeping the
    /// selection.
    fn restate(&mut self, now: Timestamp) {
        let selected = self.selected_key();
        let mut services: Vec<Entry> = self
            .services
            .iter()
            .map(|service| Entry {
                kind: Kind::Service,
                name: service.name.clone(),
                shown: one_line(&service.name),
                status: service_status(service, now),
                container: None,
            })
            .collect();
        let mut containers: Vec<Entry> = self
            .containers
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
        services.sort_by(|a, b| a.name.cmp(&b.name));
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        services.extend(containers);
        self.entries = services;
        // A chosen index would point elsewhere now.
        self.chosen = None;
        self.lay_out_keeping(selected);
    }

    /// The rows for the entries and the filters, keeping the selection.
    fn lay_out(&mut self) {
        let selected = self.selected_key();
        self.lay_out_keeping(selected);
    }

    /// The rows for the entries and the filters; the selection on
    /// `selected` (section, kind and name) if it is still listed, else on
    /// that source in another section, else on the row now at its place.
    fn lay_out_keeping(&mut self, selected: Option<(Section, Kind, String)>) {
        let at = self.state.selected();
        self.rows = self.rows_shown();
        let row_of = |want: &dyn Fn(Section, &Entry) -> bool| {
            self.rows.iter().position(|row| match *row {
                Row::Source(section, i) => want(section, &self.entries[i]),
                _ => false,
            })
        };
        let same = selected.and_then(|(section, kind, name)| {
            row_of(&|s, e| s == section && e.kind == kind && e.name == name)
                .or_else(|| row_of(&|_, e| e.kind == kind && e.name == name))
        });
        let place = || {
            let at = at?.min(self.rows.len().checked_sub(1)?);
            self.next_from(at, 1).or_else(|| self.next_from(at, -1))
        };
        let selection = same.or_else(place).or_else(|| self.next_from(0, 1));
        self.state.select(selection);
    }

    /// The rows the filters let through: Needs attention, then each kind's
    /// section; empty sections are left out (tui spec, "Sections").
    fn rows_shown(&self) -> Vec<Row> {
        let shown = |e: &Entry| self.filter.shows(e.kind) && name_matches(&e.shown, &self.query);
        let mut rows = Vec::with_capacity(self.entries.len() + 4);
        let mut attention: Vec<usize> = (0..self.entries.len())
            .filter(|&i| shown(&self.entries[i]) && self.entries[i].status.attention)
            .collect();
        // A stable sort: a service and a container of one name keep the
        // entries' order, the service first.
        attention.sort_by(|&a, &b| self.entries[a].name.cmp(&self.entries[b].name));
        if !attention.is_empty() {
            rows.push(Row::Heading(Section::Attention));
            rows.extend(
                attention
                    .into_iter()
                    .map(|i| Row::Source(Section::Attention, i)),
            );
        }
        for (section, kind) in [
            (Section::Services, Kind::Service),
            (Section::Containers, Kind::Container),
        ] {
            if !self.filter.shows(kind) {
                continue;
            }
            let start = rows.len();
            rows.extend(
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.kind == kind && shown(e))
                    .map(|(i, _)| Row::Source(section, i)),
            );
            // A name filter hides the notice row too: it names no source.
            if kind == Kind::Container && self.unreachable.is_some() && self.query.is_empty() {
                rows.push(Row::Unreachable);
            }
            if rows.len() > start {
                rows.insert(start, Row::Heading(section));
            }
        }
        rows
    }

    /// The selected row's section, kind and name.
    fn selected_key(&self) -> Option<(Section, Kind, String)> {
        match self.state.selected().and_then(|i| self.rows.get(i)) {
            Some(&Row::Source(section, i)) => {
                let entry = self.entries.get(i)?;
                Some((section, entry.kind, entry.name.clone()))
            }
            _ => None,
        }
    }

    /// The first selectable row from `at` on, going by `step` (1 or -1).
    fn next_from(&self, at: usize, step: isize) -> Option<usize> {
        let mut i = isize::try_from(at).ok()?;
        while let Some(row) = usize::try_from(i).ok().and_then(|i| self.rows.get(i)) {
            if matches!(row, Row::Source(..)) {
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

    /// Choose the selected source, if a source is selected.
    fn choose(&mut self) -> Flow {
        let chosen = self.state.selected().and_then(|i| self.rows.get(i));
        if let Some(&Row::Source(_, i)) = chosen {
            self.chosen = Some(i);
            return Flow::Quit;
        }
        Flow::Unchanged
    }

    /// The keys that move the selection, wherever the picker is.
    fn movement(&mut self, code: KeyCode) -> bool {
        let page = isize::try_from(usize::from(self.area.height).max(1)).unwrap_or(isize::MAX);
        match code {
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            _ => return false,
        }
        true
    }

    fn next_filter(&mut self) {
        self.filter = self.filter.next();
        self.lay_out();
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
        match key.code {
            KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Char('j') => self.move_by(1),
            KeyCode::Home => self.state.select(self.next_from(0, 1)),
            KeyCode::End => {
                let last = self.rows.len().saturating_sub(1);
                self.state.select(self.next_from(last, -1));
            }
            KeyCode::Enter => return self.choose(),
            KeyCode::Tab => self.next_filter(),
            KeyCode::Char('/') => {
                // The line opens on the filter in effect, the cursor at
                // its end (ratatui-textarea's `TextArea::new` and
                // `move_cursor`).
                let mut line = TextArea::new(vec![self.query.clone()]);
                line.move_cursor(CursorMove::End);
                line.set_cursor_line_style(Style::default());
                line.set_style(self.style(Role::Text));
                self.editing = Some(line);
            }
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.lay_out();
            }
            KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// A key while the name filter's line is open: the list narrows as it
    /// is typed; Enter keeps the filter, Esc clears it, the arrows and
    /// the page keys still move and `Tab` still cycles the kinds. Every
    /// other key is the line's (ratatui-textarea's `TextArea::input`,
    /// which edits and moves the cursor). Enter and Ctrl-M, which the
    /// line would take as a new line, end it, as ratatui-textarea's
    /// single-line example does
    /// (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>).
    fn edit_key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n' | '\r') => self.editing = None,
            KeyCode::Char('m') if ctrl => self.editing = None,
            KeyCode::Esc => {
                self.editing = None;
                self.query.clear();
                self.lay_out();
            }
            KeyCode::Tab => self.next_filter(),
            code if self.movement(code) => {}
            _ => {
                let Some(line) = self.editing.as_mut() else {
                    return Flow::Unchanged;
                };
                if !line.input(key) {
                    return Flow::Redraw;
                }
                self.query = line.lines().join("");
                self.lay_out();
            }
        }
        Flow::Redraw
    }

    /// A paste (bracketed paste): into the name filter's line whole, as
    /// one line (ratatui-textarea's `insert_str`); outside the line it
    /// does nothing, so its letters aren't keys.
    fn paste(&mut self, text: &str) -> Flow {
        let Some(line) = self.editing.as_mut() else {
            return Flow::Unchanged;
        };
        line.insert_str(crate::paste::one_line(text));
        self.query = line.lines().join("");
        self.lay_out();
        Flow::Redraw
    }

    fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.move_by(-1),
            MouseEventKind::ScrollDown => self.move_by(1),
            MouseEventKind::Down(MouseButton::Left) => {
                let at = Position::new(mouse.column, mouse.row);
                if !self.area.contains(at) {
                    return Flow::Unchanged;
                }
                // The list has no border: its first row on the screen is
                // the state's offset.
                let row = self.state.offset() + usize::from(mouse.row - self.area.y);
                if !matches!(self.rows.get(row), Some(Row::Source(..))) {
                    return Flow::Unchanged;
                }
                if self.state.selected() == Some(row) {
                    self.editing = None;
                    return self.choose();
                }
                self.state.select(Some(row));
            }
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// Start a read of the services, if the picker has a refresh and no
    /// read is under way.
    fn start_refresh(&mut self) {
        let Some(refresh) = self.refresh.as_mut() else {
            return;
        };
        if refresh.pending.is_some() {
            return;
        }
        let (send, result) = oneshot::channel();
        // The read runs with no subscriber (tracing's
        // `WithSubscriber::with_subscriber` with a `NoSubscriber`,
        // <https://docs.rs/tracing/0.1.44/tracing/instrument/trait.WithSubscriber.html#method.with_subscriber>,
        // <https://docs.rs/tracing/0.1.44/tracing/subscriber/struct.NoSubscriber.html>),
        // so a refresh writes none of its read's records, sb-exec's and
        // sb-host's alike; the picker logs a failure and each change
        // itself (tui spec, "Verification notes": the refresh's run log).
        // The first load before the screen is logged in full. A task the
        // read spawned wouldn't inherit the silence; sb-host's list spawns
        // none.
        let read = (refresh.list)().with_subscriber(refresh.silence.clone());
        let waker = Arc::clone(&refresh.waker);
        // Why a oneshot beside the `JoinHandle`, which carries a result
        // too: the result is sent before the wake, so `wake` always finds
        // it; the task's own result could still be pending at the wake.
        let task = tokio::spawn(async move {
            // The picker may be gone; its drop aborts this task first.
            let _ = send.send(read.await);
            waker.notify_one();
        });
        refresh.pending = Some(Pending { result, task });
    }

    /// The result of the services' read, if it has ended: `true` when it
    /// changed what the picker shows.
    fn refreshed(&mut self) -> bool {
        let Some(refresh) = self.refresh.as_mut() else {
            return false;
        };
        let Some(pending) = refresh.pending.as_mut() else {
            return false;
        };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => return false,
            // The task ended without a result (a panic): try again later.
            Err(oneshot::error::TryRecvError::Closed) => {
                refresh.pending = None;
                return false;
            }
        };
        refresh.pending = None;
        match result {
            Ok(services) => {
                self.services_trouble = None;
                log_recovery(&mut self.failing);
                log_changes(&self.services, &services);
                self.services = services;
                self.restate(Timestamp::now());
            }
            Err(cause) => {
                log_failure(&mut self.failing, &cause);
                self.services_trouble = Some(format!("couldn't refresh services: {cause}"));
            }
        }
        true
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    fn count(&self, kind: Kind) -> usize {
        self.entries.iter().filter(|e| e.kind == kind).count()
    }

    /// The title row: the screen's name, the counts of the kinds shown,
    /// and the name filter in effect.
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
        if !self.query.is_empty() && self.editing.is_none() {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(
                format!("filter \"{}\"", one_line(&self.query)),
                self.style(Role::Highlight),
            ));
        }
        TextLine::from(spans)
    }

    /// `role`'s colour as a background (none under `ascii`), through
    /// ratatui-core's `anstyle` conversion, as the foreground roles.
    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// A section's heading: its word and count in its role, and its rule.
    fn heading(&self, section: Section, rule_width: usize) -> ListItem<'static> {
        let (word, word_role, rule_role) = match section {
            Section::Attention => ("Needs attention", Role::Problem, Role::AttentionRule),
            Section::Services => ("Services", Role::Service, Role::ServiceRule),
            Section::Containers => ("Containers", Role::Container, Role::ContainerRule),
        };
        let rows = self
            .rows
            .iter()
            .filter(|row| matches!(row, Row::Source(s, _) if *s == section))
            .count();
        let title = if rows == 0 {
            format!("{word} ")
        } else {
            format!("{word} ({rows}) ")
        };
        let rule = "─".repeat(rule_width.saturating_sub(Span::raw(&title).width()));
        ListItem::new(TextLine::from(vec![
            Span::styled(title, self.style(word_role)),
            Span::styled(rule, self.style(rule_role)),
        ]))
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
    /// line up across the sections. Needs attention mixes the kinds, so
    /// its rows end with the kind's word (proto-logs' look): the words
    /// tell them apart without colour.
    fn items(&self, width: u16) -> Vec<ListItem<'static>> {
        let shown = |row: &&Row| match **row {
            Row::Source(_, i) => Some(&self.entries[i]),
            _ => None,
        };
        let name_width = self
            .rows
            .iter()
            .filter_map(|row| shown(&row))
            .map(|e| Span::raw(e.shown.as_str()).width())
            .max()
            .unwrap_or(0);
        let status_text = |e: &Entry| format!("{} {}", e.status.symbol, e.status.text);
        let status_width = self
            .rows
            .iter()
            .filter(|row| matches!(row, Row::Source(Section::Attention, _)))
            .filter_map(|row| shown(&row))
            .map(|e| Span::raw(status_text(e)).width())
            .max()
            .unwrap_or(0);
        // The rule runs to the right edge, less the selection's mark.
        let rule_width = usize::from(width).saturating_sub(Span::raw(MARK).width());
        self.rows
            .iter()
            .map(|row| match *row {
                Row::Heading(section) => self.heading(section, rule_width),
                Row::Source(section, i) => {
                    let entry = &self.entries[i];
                    let pad = name_width.saturating_sub(Span::raw(entry.shown.as_str()).width());
                    let status = status_text(entry);
                    let mut spans = vec![
                        Span::styled(entry.shown.clone(), self.style(entry.kind.role())),
                        Span::raw(" ".repeat(pad + 2)),
                    ];
                    if section == Section::Attention {
                        let pad = status_width.saturating_sub(Span::raw(&status).width());
                        spans.push(Span::styled(status, self.style(entry.status.role)));
                        spans.push(Span::raw(" ".repeat(pad + 2)));
                        spans.push(Span::styled(entry.kind.word(), self.style(Role::Muted)));
                    } else {
                        spans.push(Span::styled(status, self.style(entry.status.role)));
                    }
                    ListItem::new(TextLine::from(spans))
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

    /// The list's one line when no row is shown.
    fn nothing_shown(&self) -> String {
        if !self.query.is_empty() {
            return format!(
                "Nothing matches \"{}\". Esc clears the filter.",
                one_line(&self.query)
            );
        }
        match self.filter {
            Filter::All => "No matching systemd services or Docker containers found.",
            Filter::Services => "No matching systemd services found.",
            Filter::Containers => "No containers found.",
        }
        .to_owned()
    }

    /// Why the lists may be out of date, then the key hints: the trouble
    /// first, so an 80-column terminal shows it whole.
    fn footer(&self) -> TextLine<'static> {
        let key = self.style(Role::Accent);
        let muted = self.style(Role::Muted);
        let mut spans = Vec::new();
        for trouble in [&self.trouble, &self.services_trouble]
            .into_iter()
            .flatten()
        {
            spans.push(Span::styled(one_line(trouble), self.style(Role::Warn)));
            spans.push(Span::raw("   "));
        }
        let mut hints = vec![
            ("↑↓", "move"),
            ("Enter", "open"),
            ("/", "filter"),
            ("Tab", "kind"),
        ];
        if !self.query.is_empty() {
            hints.push(("Esc", "clear filter"));
        }
        hints.push(("q", "quit"));
        for (i, (k, what)) in hints.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", self.style(Role::Faint)));
            }
            spans.push(Span::styled(k, key));
            spans.push(Span::styled(format!(" {what}"), muted));
        }
        TextLine::from(spans)
    }
}

/// Whether a service's state changed between two reads, as the run log
/// tells: what systemd says, not the times the screen works out from it.
/// A state-change time that one of the reads couldn't get (sb-host's
/// `show` failed or timed out, leaving it `None`) is unknown, not a
/// change, so one failed `show` doesn't log every service twice.
fn changed(before: &Service, after: &Service) -> bool {
    let times_differ = match (before.state_change, after.state_change) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    };
    before.active != after.active
        || before.sub != after.sub
        || before.unit_file_state != after.unit_file_state
        || times_differ
}

/// The run-log records of a refresh: each service whose state changed,
/// was added or went (nothing when none did).
fn log_changes(before: &[Service], after: &[Service]) {
    for service in after {
        match before.iter().find(|old| old.name == service.name) {
            Some(old) if !changed(old, service) => {}
            Some(_) => tracing::info!(
                service = %escape_controls(&service.name),
                active = %escape_controls(&service.active),
                sub = %escape_controls(&service.sub),
                unit_file_state = %escape_controls(service.unit_file_state.as_deref().unwrap_or("-")),
                since = %service.state_change.map_or_else(|| "-".to_owned(), |at| at.to_string()),
                "service state changed"
            ),
            None => tracing::info!(service = %escape_controls(&service.name), "service listed"),
        }
    }
    for old in before {
        if !after.iter().any(|service| service.name == old.name) {
            tracing::info!(service = %escape_controls(&old.name), "service no longer listed");
        }
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
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Paste(text) => self.paste(&text),
            _ => Flow::Unchanged,
        }
    }

    /// Every 5 s: the statuses as at now, and a read of the services.
    fn tick(&mut self) -> Flow {
        if self.restated.elapsed() < REFRESH {
            return Flow::Unchanged;
        }
        self.restated = Instant::now();
        self.restate(Timestamp::now());
        // Hidden services (the containers filter) aren't read again
        // until `Tab` shows them.
        if self.filter.shows(Kind::Service) {
            self.start_refresh();
        }
        Flow::Redraw
    }

    /// A new container list, a change in the live updates' trouble, or
    /// the services read again.
    fn wake(&mut self) -> Flow {
        let mut changed = self.refreshed();
        if let Some(updates) = self.updates.as_mut()
            && updates.has_changed().unwrap_or(false)
        {
            let snapshot = updates.borrow_and_update().clone();
            self.trouble = snapshot.trouble;
            if let Some(rows) = snapshot.rows {
                self.containers = rows;
                self.unreachable = None;
                self.restate(Timestamp::now());
            }
            changed = true;
        }
        if changed {
            Flow::Redraw
        } else {
            Flow::Unchanged
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
        self.area = list;
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(TextLine::styled(
                    self.nothing_shown(),
                    self.style(Role::Muted),
                )),
                list,
            );
        } else {
            // The selected row is one solid bar: the highlight style
            // patches every span of it (proto-logs'
            // `selected_row_is_one_uniform_bar`).
            let selected = self
                .style(Role::SelectedFg)
                .patch(self.bg(Role::SelectedBg));
            let widget = List::new(self.items(list.width))
                .highlight_symbol(MARK)
                .highlight_style(selected);
            frame.render_stateful_widget(widget, list, &mut self.state);
        }
        if let Some(line) = &self.editing {
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
            frame.render_widget(Paragraph::new(self.footer()), footer);
        }
    }
}

#[cfg(test)]
mod tests;
