//! The fact editor's page (tui spec, "Editors", `sb fact`, Screen; the
//! settings page of docs/ideas/fact-editor.md and proto-facts' reviewed
//! design). This is its read-only part (module build M54): the apps and
//! their settings, search, folding, masking, Show and Copy. Changes,
//! saving and the dialogs come with M55, the mouse and the menu with M56.
//!
//! - **The page** is ratatui's `List` with a `ListState`
//!   (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.List.html>):
//!   each app a heading, a heading per copy for apps with several (or one
//!   not named after the app), each setting with its value. The state's
//!   `select_next`, `select_previous`, `scroll_down_by` and `scroll_up_by`
//!   move the selection, clamped at once (ratatui-widgets 0.3.2
//!   `list/state.rs` clamps only at render), as proto-facts does. Why not
//!   tui-tree-widget's `Tree`: the page folds at one level only (apps), a
//!   search shows every match open whatever the folds, a folded app counts
//!   its settings, and values line up in one column across the levels;
//!   that is proto-facts' reviewed `List` page, and the crate would be a
//!   new dependency for a fraction of it.
//! - **Search** is a ratatui-textarea line
//!   (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html>):
//!   `/` opens it, typing narrows the page to the apps, copies and keys
//!   that match (never values: they can be secrets) and opens the matching
//!   apps (an app or copy whose name matches shows whole, even with no
//!   settings); Enter, Down or Tab keep it, Esc clears it
//!   (`TextArea::clear`), then leaves it.
//! - **Folding:** the page opens expanded when it fits, else folded to app
//!   names (proto-facts' rule; the threshold is the manual check's, tui
//!   spec, Testing Strategy). Enter on an app folds or opens it; Left
//!   folds the selected row's app, Right opens it. While a search shows
//!   every match open, these keys do nothing, so no fold changes unseen.
//! - **Secrets** ([`is_secret_key`]) show as dots. **Show** (`s`) reveals
//!   the selected one until the selection moves (on a value that isn't
//!   masked it has nothing to do, and is drawn faint); **Copy** (`c`) sends
//!   any value
//!   through crossterm's OSC 52 `CopyToClipboard`
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/clipboard/struct.CopyToClipboard.html>)
//!   without showing it. Values, keys and names reach the screen through
//!   term-ui's `DisplayText`, which drops control sequences.
//! - **The changes panel** is always there: on the right from 100 columns,
//!   at the bottom below that. It has nothing to list until M55.
//! - **Never logged:** a fact's value. The run log gets the role, copy and
//!   key of what is shown and copied, and whether the copy was sent.

use std::collections::BTreeSet;
use std::io::{self, Write};

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph};
use ratatui_textarea::TextArea;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::escape_controls;
use sb_cli_core::theme::{Role, Theme};
use sb_config::facts::Role as App;
use sb_config::facts::secret::is_secret_key;
use sb_term_ui::DisplayText;

use crate::term::{App as Screen, Ending, Flow, Tui};

/// The selection's mark, as the pickers draw it: it shows the selection
/// without colour too.
const MARK: &str = "› ";

/// A masked value: dots of a fixed length, whatever the value's.
const DOTS: &str = "••••••••";

/// The widest key column; longer keys push their value along.
const KEY_WIDTH_CAP: usize = 28;

/// From this width the changes panel sits on the right (tui spec, Screen).
const WIDE: u16 = 100;

/// The changes panel's width beside the page.
const PANEL_WIDTH: u16 = 36;

/// The status line's note after a copy: OSC 52 can't confirm it (tui
/// spec, Known limits).
/// Short enough for an 80-column status line with most keys (the
/// maintainer, 2026-11-16).
const COPIED: &str = "(if your terminal allows it; otherwise use Show)";

/// The fact editor's page over `roles` (sb-config's catalog), in `theme`,
/// on stdout's terminal, until `q`, Esc, Ctrl-C or the run's token (a
/// first SIGTERM or SIGHUP).
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored (reported whatever the ending, cli-L5).
pub async fn browse(shutdown: &Shutdown, roles: Vec<App>, theme: Theme) -> io::Result<Ending> {
    let mut page = Page::new(roles, theme, Box::new(io::stdout()));
    tracing::info!(roles = page.roles.len(), "fact editor opened");
    let mut tui = Tui::enter(shutdown)?;
    let ending = tui.run(&mut page).await;
    let left = tui.leave().await;
    tracing::info!(ending = ?ending.as_ref().ok(), "fact editor closed");
    crate::ended(ending, left)
}

/// A setting: its role, copy and fact indexes in the catalog.
type Id = (usize, usize, usize);

/// One row of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    App(usize),
    Copy(usize, usize),
    Setting(Id),
}

/// What a bottom button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Copy,
    Show,
    Quit,
}

/// The page's state.
struct Page {
    roles: Vec<App>,
    theme: Theme,
    list: ListState,
    folded: BTreeSet<usize>,
    /// Whether the first draw has chosen expanded or folded.
    fold_decided: bool,
    /// The search line; its editing keys (Backspace, Ctrl-H, word delete)
    /// are the widget's. `query` is its text.
    search: TextArea<'static>,
    query: String,
    searching: bool,
    /// The one value Show reveals, until the selection moves.
    shown: Option<Id>,
    status: String,
    /// Where Copy's OSC 52 goes: the terminal (stdout), or a test's sink.
    out: Box<dyn Write + Send>,
    /// The list's height in the last frame (the page keys' step).
    height: u16,
}

impl std::fmt::Debug for Page {
    // No value in it: `Role`'s facts print their keys only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("roles", &self.roles)
            .field("selected", &self.list.selected())
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

/// The search line, empty: one line, its cursor shown only while it has
/// focus (set at each draw).
fn search_box() -> TextArea<'static> {
    let mut line = TextArea::default();
    line.set_cursor_line_style(Style::default());
    line.set_placeholder_text("press / to search");
    line
}

/// `text` as one line of plain screen text: control sequences dropped
/// (term-ui's `DisplayText`), only its first line.
fn one_line(text: &str) -> String {
    DisplayText::from_bytes(text.as_bytes(), false)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// A role's name as its heading: the first letter upper case (proto-facts'
/// look).
fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map_or_default(|first| first.to_uppercase().chain(chars).collect())
}

impl Page {
    fn new(roles: Vec<App>, theme: Theme, out: Box<dyn Write + Send>) -> Self {
        let mut search = search_box();
        search.set_style(Style::from(theme.style(Role::Text)));
        search.set_placeholder_style(Style::from(theme.style(Role::Faint)));
        Self {
            list: ListState::default().with_selected((!roles.is_empty()).then_some(0)),
            roles,
            theme,
            folded: BTreeSet::new(),
            fold_decided: false,
            search,
            query: String::new(),
            searching: false,
            shown: None,
            status: String::new(),
            out,
            height: 0,
        }
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    /// `role`'s colour as a background (none under `ascii`), as the
    /// pickers'.
    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// Whether app `r` has a heading per copy: several copies, or one not
    /// named after the app.
    fn has_copies(&self, r: usize) -> bool {
        let role = &self.roles[r];
        role.instances.len() > 1 || role.instances.first().is_some_and(|i| i.name != role.name)
    }

    /// Whether `name` holds the query, in any case. Values are never
    /// searched.
    fn matches(&self, name: &str) -> bool {
        name.to_lowercase().contains(&self.query.to_lowercase())
    }

    /// The rows shown: every app with a match, open unless folded (a
    /// search opens them all), its copies' headings (an empty copy's too),
    /// its settings. An app or copy whose name matches shows whole.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (r, role) in self.roles.iter().enumerate() {
            let role_hit = self.query.is_empty() || self.matches(&role.name);
            let mut below = Vec::new();
            for (i, instance) in role.instances.iter().enumerate() {
                let copy_hit = role_hit || self.matches(&instance.name);
                let settings: Vec<Row> = (0..instance.facts.len())
                    .filter(|&f| copy_hit || self.matches(&instance.facts[f].key))
                    .map(|f| Row::Setting((r, i, f)))
                    .collect();
                if !copy_hit && settings.is_empty() {
                    continue;
                }
                if self.has_copies(r) {
                    below.push(Row::Copy(r, i));
                }
                below.extend(settings);
            }
            if !role_hit && below.is_empty() {
                continue;
            }
            rows.push(Row::App(r));
            if self.query.is_empty() && self.folded.contains(&r) {
                continue;
            }
            rows.extend(below);
        }
        rows
    }

    fn selected(&self) -> Option<Row> {
        self.rows().get(self.list.selected()?).copied()
    }

    fn selected_setting(&self) -> Option<Id> {
        match self.selected()? {
            Row::Setting(id) => Some(id),
            Row::App(_) | Row::Copy(..) => None,
        }
    }

    /// Whether setting `id` shows as dots now.
    fn masked(&self, id: Id) -> bool {
        self.secret(id) && self.shown != Some(id)
    }

    /// Move the selection with `ListState`'s own `movement`, then clamp it
    /// to the rows at once. With nothing selected it starts from the first
    /// row: `select_previous` would wrap to the last.
    fn move_selection(&mut self, movement: impl FnOnce(&mut ListState)) {
        let count = self.rows().len();
        if count == 0 {
            self.list.select(None);
            return;
        }
        if self.list.selected().is_none() {
            self.list.select(Some(0));
        }
        movement(&mut self.list);
        if let Some(selected) = self.list.selected() {
            self.list.select(Some(selected.min(count - 1)));
        }
    }

    /// Fold (or open) the app of the selected row; folding selects its
    /// heading, so the selection stays on a row that is shown. Not while a
    /// search shows every match open.
    fn fold_selected(&mut self, fold: bool) {
        if !self.query.is_empty() {
            return;
        }
        let Some(Row::App(r) | Row::Copy(r, _) | Row::Setting((r, _, _))) = self.selected() else {
            return;
        };
        if fold {
            self.folded.insert(r);
            let heading = self.rows().iter().position(|row| *row == Row::App(r));
            self.list.select(heading);
        } else {
            self.folded.remove(&r);
        }
    }

    /// Enter: an app's heading folds or opens it (not while searching, as
    /// [`Page::fold_selected`]).
    fn activate(&mut self) {
        if self.query.is_empty()
            && let Some(Row::App(r)) = self.selected()
            && !self.folded.remove(&r)
        {
            self.folded.insert(r);
        }
    }

    /// A key while the search line has focus.
    fn search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc if self.query.is_empty() => self.searching = false,
            KeyCode::Esc => {
                self.search.clear();
                self.searched();
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => self.searching = false,
            // One line: Ctrl-M and the newline characters would add one,
            // as ratatui-textarea's single-line example filters them
            // (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>).
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Char('\n' | '\r') => {}
            // `input` says whether the text changed.
            _ => {
                if self.search.input(key) {
                    self.searched();
                }
            }
        }
    }

    /// The query is the search line's text now: the selection goes to the
    /// first row shown.
    fn searched(&mut self) {
        self.search
            .lines()
            .first()
            .cloned()
            .unwrap_or_default()
            .clone_into(&mut self.query);
        self.list.select((!self.rows().is_empty()).then_some(0));
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if self.searching {
            self.search_key(key);
            return Flow::Redraw;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Flow::Unchanged;
        }
        let page = self.height.max(1);
        match key.code {
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(ListState::select_next),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(ListState::select_previous),
            KeyCode::PageDown => self.move_selection(|l| l.scroll_down_by(page)),
            KeyCode::PageUp => self.move_selection(|l| l.scroll_up_by(page)),
            KeyCode::Home => self.move_selection(ListState::select_first),
            KeyCode::End => self.move_selection(ListState::select_last),
            KeyCode::Enter => self.activate(),
            KeyCode::Left => self.fold_selected(true),
            KeyCode::Right => self.fold_selected(false),
            KeyCode::Char('c') => return self.act(Action::Copy),
            KeyCode::Char('s') => return self.act(Action::Show),
            KeyCode::Char('q') | KeyCode::Esc => return self.act(Action::Quit),
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// A bottom button's action, by key (and by click with M56).
    fn act(&mut self, action: Action) -> Flow {
        match action {
            Action::Quit => return Flow::Quit,
            Action::Show => match self.selected_setting() {
                Some(id) if self.shown == Some(id) => self.shown = None,
                Some(id) if self.secret(id) => {
                    self.shown = Some(id);
                    let (role, copy, key) = self.names(id);
                    tracing::info!(role = %role, copy = %copy, key = %key, "fact shown");
                }
                Some(_) => "This value isn't hidden".clone_into(&mut self.status),
                None => "Pick a setting to show".clone_into(&mut self.status),
            },
            Action::Copy => match self.selected_setting() {
                Some(id) => self.copy(id),
                None => "Pick a setting to copy".clone_into(&mut self.status),
            },
        }
        Flow::Redraw
    }

    /// Copy setting `id`'s value to the clipboard through OSC 52, without
    /// showing it.
    fn copy(&mut self, id: Id) {
        let fact = &self.roles[id.0].instances[id.1].facts[id.2];
        let copied = crossterm::execute!(
            self.out,
            CopyToClipboard::to_clipboard_from(fact.value.as_str())
        );
        let shown = one_line(&fact.key);
        let (role, copy, key) = self.names(id);
        self.status = match copied {
            Ok(()) => {
                tracing::info!(role = %role, copy = %copy, key = %key, "fact copied");
                format!("Copied {shown} {COPIED}")
            }
            Err(err) => {
                tracing::warn!(role = %role, copy = %copy, key = %key, error = %err, "fact not copied");
                format!("Couldn't copy {shown}: {err}")
            }
        };
    }

    /// Setting `id`'s role, copy and key for the run log, escaped (cli-core's
    /// rule): never its value.
    fn names(&self, (r, i, f): Id) -> (String, String, String) {
        let role = &self.roles[r];
        let instance = &role.instances[i];
        (
            escape_controls(&role.name),
            escape_controls(&instance.name),
            escape_controls(&instance.facts[f].key),
        )
    }

    /// Whether setting `id`'s key names a secret: masked until Show.
    fn secret(&self, (r, i, f): Id) -> bool {
        is_secret_key(&self.roles[r].instances[i].facts[f].key)
    }

    /// A setting's indent: deeper under a copy's heading.
    fn indent(&self, r: usize) -> &'static str {
        if self.has_copies(r) { "    " } else { "  " }
    }

    /// Where values start, after the indent and the key: the same column
    /// for every setting, worked out from all of them, so values don't
    /// move while searching (proto-facts), the keys counted up to
    /// [`KEY_WIDTH_CAP`].
    fn key_width(&self) -> usize {
        self.roles
            .iter()
            .enumerate()
            .flat_map(|(r, role)| role.instances.iter().map(move |i| (r, i)))
            .flat_map(|(r, instance)| instance.facts.iter().map(move |f| (r, f)))
            .map(|(r, fact)| {
                self.indent(r).len() + Span::raw(one_line(&fact.key)).width().min(KEY_WIDTH_CAP)
            })
            .max()
            .unwrap_or(0)
    }

    /// One row of the page.
    fn item(&self, row: Row, key_width: usize) -> ListItem<'static> {
        match row {
            Row::App(r) => {
                let role = &self.roles[r];
                let folded = self.query.is_empty() && self.folded.contains(&r);
                let mut spans = vec![
                    Span::styled(if folded { "▸ " } else { "▾ " }, self.style(Role::Faint)),
                    Span::styled(
                        title_case(&one_line(&role.name)),
                        self.style(Role::Text).add_modifier(Modifier::BOLD),
                    ),
                ];
                if folded {
                    let count: usize = role.instances.iter().map(|i| i.facts.len()).sum();
                    let settings = if count == 1 { "setting" } else { "settings" };
                    spans.push(Span::styled(
                        format!("  {count} {settings}"),
                        self.style(Role::Muted),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
            Row::Copy(r, i) => ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    one_line(&self.roles[r].instances[i].name),
                    self.style(Role::Accent),
                ),
            ])),
            Row::Setting(id) => {
                // Every value starts in the same column.
                let indent = self.indent(id.0);
                let fact = &self.roles[id.0].instances[id.1].facts[id.2];
                let key = one_line(&fact.key);
                let pad = key_width.saturating_sub(indent.len() + Span::raw(key.as_str()).width());
                let value = if self.masked(id) {
                    Span::styled(DOTS, self.style(Role::Muted))
                } else {
                    let shown = DisplayText::from_bytes(fact.value.as_bytes(), false);
                    let mut lines = shown.lines();
                    let mut first = lines.next().unwrap_or_default().to_owned();
                    if lines.next().is_some() {
                        first.push_str(" …");
                    }
                    Span::styled(first, self.style(Role::Text))
                };
                ListItem::new(Line::from(vec![
                    Span::raw(indent),
                    Span::styled(key, self.style(Role::Text)),
                    Span::raw(" ".repeat(pad + 2)),
                    value,
                ]))
            }
        }
    }

    /// The page's one line when no row is shown.
    fn nothing_shown(&self) -> String {
        if self.query.is_empty() {
            "No settings found".to_owned()
        } else {
            format!(
                "No settings match \"{}\". Press Esc in the search box to clear it.",
                one_line(&self.query)
            )
        }
    }

    /// The bottom buttons: each with its key; the ones that can't act on
    /// the selected row are drawn faint.
    fn buttons(&self) -> Vec<(Span<'static>, Action)> {
        let on_setting = self.selected_setting();
        let show = if on_setting.is_some() && on_setting == self.shown {
            "Hide"
        } else {
            "Show"
        };
        [
            ("Copy", "c", Action::Copy, on_setting.is_some()),
            (
                show,
                "s",
                Action::Show,
                on_setting.is_some_and(|id| self.secret(id)),
            ),
            ("Quit", "q", Action::Quit, true),
        ]
        .into_iter()
        .map(|(label, key, action, enabled)| {
            let style = if enabled {
                self.style(Role::Text).patch(self.bg(Role::ButtonBg))
            } else {
                self.style(Role::Faint)
                    .patch(self.bg(Role::ButtonDisabledBg))
            };
            (Span::styled(format!(" {label} ({key}) "), style), action)
        })
        .collect()
    }

    /// The search box: the screen's title on its border, then the line.
    fn draw_search(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let border = if self.searching {
            Role::Accent
        } else {
            Role::Border
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.style(border))
            .title(Span::styled(
                " Saltbox settings ",
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [label, field] =
            Layout::horizontal([Constraint::Length(9), Constraint::Fill(1)]).areas(inner);
        frame.render_widget(
            Span::styled(
                " Search  ",
                self.style(Role::Text).add_modifier(Modifier::BOLD),
            ),
            label,
        );
        // The text cursor only while the line has focus.
        self.search.set_cursor_style(if self.searching {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        });
        frame.render_widget(&self.search, field);
    }

    /// The changes panel: none to list on the read-only page.
    fn draw_changes(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.style(Role::Border))
            .title(Span::styled(
                " Unsaved changes (0) ",
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area).inner(Margin::new(1, 0));
        frame.render_widget(block, area);
        frame.render_widget(
            Span::styled("Nothing changed yet", self.style(Role::Muted)),
            inner,
        );
    }
}

impl Screen for Page {
    fn input(&mut self, input: Input) -> Flow {
        let Input::Key(key) = input else {
            return match input {
                Input::Resize(..) => Flow::Redraw,
                _ => Flow::Unchanged,
            };
        };
        let before = self.selected();
        let flow = self.key(key);
        // Show, and what the status line said, last until the selection
        // moves.
        if self.selected() != before {
            self.shown = None;
            self.status.clear();
        }
        flow
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let wide = area.width >= WIDE;
        // ratatui's layout: the search box, the page, the status line and
        // the buttons
        // (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.vertical>).
        // Below 100 columns the changes panel is a band above the status
        // line: its border and its one line.
        let bottom = if wide { 0 } else { 3 };
        let [search, body, changes_bottom, status, buttons] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(bottom),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_search(frame, search);
        let list_area = if wide {
            let [list, changes] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(PANEL_WIDTH)])
                    .areas(body);
            self.draw_changes(frame, changes);
            list
        } else {
            self.draw_changes(frame, changes_bottom);
            body
        };
        self.height = list_area.height;
        if !self.fold_decided && list_area.height > 0 {
            // Expanded when it fits, else folded to app names.
            self.fold_decided = true;
            if self.rows().len() > usize::from(list_area.height) {
                self.folded = (0..self.roles.len()).collect();
                self.move_selection(|_| {});
            }
        }
        let rows = self.rows();
        if rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(self.nothing_shown(), self.style(Role::Muted))),
                list_area,
            );
        } else {
            let key_width = self.key_width();
            let items: Vec<ListItem<'static>> =
                rows.iter().map(|&row| self.item(row, key_width)).collect();
            // The selected row is one solid bar, as in the pickers.
            let selected = self
                .style(Role::SelectedFg)
                .patch(self.bg(Role::SelectedBg));
            let list = List::new(items)
                .highlight_symbol(MARK)
                .highlight_style(selected);
            frame.render_stateful_widget(list, list_area, &mut self.list);
        }
        frame.render_widget(
            Span::styled(
                format!(" {}", one_line(&self.status)),
                self.style(Role::Muted),
            ),
            status,
        );
        // The buttons laid out by ratatui, one column apart, each in the
        // rectangle that is its click target (proto-facts; the clicks come
        // with M56).
        let spans = self.buttons();
        let row = buttons.inner(Margin::new(1, 0));
        let areas =
            Layout::horizontal(spans.iter().map(|(span, _)| {
                Constraint::Length(u16::try_from(span.width()).unwrap_or(u16::MAX))
            }))
            .spacing(1)
            .split(row);
        for ((span, _), area) in spans.into_iter().zip(areas.iter()) {
            frame.render_widget(span, *area);
        }
    }
}

#[cfg(test)]
mod tests;
