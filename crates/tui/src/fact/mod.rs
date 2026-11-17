//! The fact editor (tui spec, "Editors", `sb fact`, Screen; the settings
//! page of docs/ideas/fact-editor.md and proto-facts' reviewed design):
//! the apps and their settings, search, folding, masking, Show and Copy
//! (module build M54); the changes, the changes panel with undo, saving
//! and the dialogs (M55); the mouse and the right-click menu ([`mouse`],
//! M56).
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
//!   any value through crossterm's OSC 52 `CopyToClipboard`
//!   (<https://docs.rs/crossterm/0.29.0/crossterm/clipboard/struct.CopyToClipboard.html>)
//!   without showing it. Values, keys and names reach the screen through
//!   term-ui's `DisplayText`, which drops control sequences.
//! - **Changes** ([`changes`]): Edit (`e`, or Enter on a setting), Add a
//!   setting to the selected copy (`a`: its name, then its value), Delete
//!   or Keep a setting, a copy or an app (`d`; deleting an app or a copy
//!   replaces the changes inside it), Undo the selected row's change
//!   (`u`). The session checks every change before it is staged
//!   (`Session::check`), so a change the file can't take is refused at
//!   once, with its reason on the status line.
//! - **The changes panel** lists each staged change with its undo: on the
//!   right from 100 columns, a band at the bottom below that.
//! - **Save** (`v`) asks first, then saves on its own thread ([`save`]),
//!   with the "waiting for Saltbox", drift and outcome dialogs
//!   ([`dialog`]). Quit with staged changes asks Save, Discard or Cancel.
//!   Dialogs are tui-popup's (<https://docs.rs/tui-popup/0.7.7/tui_popup/>);
//!   the drift dialog's text scrolls in a tui-scrollview
//!   (<https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/>).
//! - **Never logged:** a fact's value. The run log gets the role, copy and
//!   key of what is shown, copied and staged, the change's kind, and
//!   whether the copy was sent; sb-config logs the save.

mod changes;
mod dialog;
mod keys;
mod mouse;
mod save;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::Arc;

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};
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
use sb_config::facts::apply::RoleOutcome;
use sb_config::facts::secret::is_secret_key;
use sb_config::facts::session::Session;
use sb_term_ui::DisplayText;
use tokio::sync::Notify;
use tui_scrollview::ScrollViewState;

use self::changes::{Pending, Target};
use self::dialog::Popup;
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

/// The most changes the bottom band lists before it scrolls no further
/// (proto-facts' four).
const BAND_ROWS: usize = 4;

/// The status line's note after a copy: OSC 52 can't confirm it (tui
/// spec, Known limits).
/// Short enough for an 80-column status line with most keys (the
/// maintainer, 2026-11-16).
const COPIED: &str = "(if your terminal allows it; otherwise use Show)";

/// How long Saltbox's lock is waited for before the waiting dialog shows:
/// a shorter hold would only flash it.
const WAIT_SHOWN: std::time::Duration = std::time::Duration::from_secs(1);

/// The status line's note after a Delete (tui spec, Screen), short
/// enough for an 80-column line after most labels (the maintainer,
/// 2026-11-16, as Copy's).
const DELETED: &str = "deleted on save; Saltbox recreates it next run";

/// What the save question says of deletions (tui spec, Screen).
const REGENERATED: &str = "Saltbox creates a new value next time it runs.";

/// The fact editor over `session`'s catalog, in `theme`, on stdout's
/// terminal, until it quits, Ctrl-C or the run's token (a first SIGTERM
/// or SIGHUP). A save under way when Ctrl-C or the token ends the screen
/// finishes the role it is committing, then stops; the session (and its
/// editor lock) goes when this returns.
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored (reported whatever the ending, cli-L5).
pub async fn browse(shutdown: &Shutdown, session: Session, theme: Theme) -> io::Result<Ending> {
    let mut page = Page::new(session, theme, Box::new(io::stdout()));
    tracing::info!(roles = page.roles.len(), "fact editor opened");
    let mut tui = Tui::enter(shutdown)?;
    page.wake = Some(tui.waker());
    let ending = tui.run(&mut page).await;
    // A save still running stops after its role; a drift question still
    // open is answered Cancel (its reply goes with the dialog).
    let running = page.saving.take();
    page.popup = Popup::None;
    if let Some(running) = &running {
        running.stop();
    }
    let left = tui.leave().await;
    if let Some(running) = running {
        // On a blocking thread of the pool: the save may be committing.
        let _ = sb_cli_core::spawn::blocking(move || running.finish()).await;
    }
    tracing::info!(ending = ?ending.as_ref().ok(), staged = page.pending.len(), "fact editor closed");
    crate::ended(ending, left)
}

/// A setting: its role, copy and setting indexes (into
/// [`Page::settings`]).
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
    Edit,
    Add,
    Copy,
    Show,
    Delete,
    Save,
    Quit,
    /// Undo the selected row's change (the menu's Undo change).
    Undo,
}

/// A setting as the page shows it: the catalog's, with its staged value,
/// or one staged to be added.
#[derive(Clone)]
struct Setting {
    key: String,
    value: String,
    /// Whether it is staged to be added (not in the file yet).
    added: bool,
}

/// The page's state.
struct Page {
    roles: Vec<App>,
    /// The session the changes are checked and saved with; away while a
    /// save runs.
    session: Option<Session>,
    theme: Theme,
    list: ListState,
    /// The folded apps, by name, so a reload keeps them.
    folded: BTreeSet<String>,
    /// Whether the first draw has chosen expanded or folded.
    fold_decided: bool,
    /// The search line; its editing keys (Backspace, Ctrl-H, word delete)
    /// are the widget's. `query` is its text.
    search: TextArea<'static>,
    query: String,
    searching: bool,
    /// The one value Show reveals, until the selection moves, by name: an
    /// addition can move the rows under it.
    shown: Option<Target>,
    status: String,
    /// The staged changes, by what they are on.
    pending: BTreeMap<Target, Pending>,
    popup: Popup,
    /// A save under way.
    saving: Option<save::Running>,
    /// Quit once the save the quit dialog started has saved everything.
    quit_after_save: bool,
    /// Where Copy's OSC 52 goes: the terminal (stdout), or a test's sink.
    out: Box<dyn Write + Send>,
    /// What a save's thread wakes the screen with ([`Tui::waker`]).
    wake: Option<Arc<Notify>>,
    /// The list's height in the last frame (the page keys' step).
    height: u16,
    /// Where the last frame drew what can be clicked.
    hits: mouse::Hits,
    /// The right-click menu, when it is open.
    menu: Option<mouse::Menu>,
}

impl std::fmt::Debug for Page {
    // No value in it: `Role`'s facts and `Pending` print no values.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("roles", &self.roles)
            .field("selected", &self.list.selected())
            .field("query", &self.query)
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

/// The search line, empty: one line, its cursor shown only while it has
/// focus (set at each draw).
fn search_box(theme: &Theme) -> TextArea<'static> {
    let mut line = TextArea::default();
    line.set_cursor_line_style(Style::default());
    line.set_placeholder_text("press / to search");
    line.set_style(Style::from(theme.style(Role::Text)));
    line.set_placeholder_style(Style::from(theme.style(Role::Faint)));
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

/// `target`'s role, copy and key for the run log, escaped (cli-core's
/// rule): never a value. A missing part is `-`.
fn names(target: &Target) -> (String, String, String) {
    let (role, copy, key) = match target {
        Target::App(role) => (role.as_str(), "-", "-"),
        Target::Copy(role, copy) => (role.as_str(), copy.as_str(), "-"),
        Target::Setting(role, copy, key) => (role.as_str(), copy.as_str(), key.as_str()),
    };
    (
        escape_controls(role),
        escape_controls(copy),
        escape_controls(key),
    )
}

/// Whether `key` is one of a text line's own: a newline (Enter, Ctrl-M,
/// `\n`, `\r`) that ratatui-textarea would add, as its single-line example
/// filters them
/// (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>).
fn newline(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Char('\n' | '\r') => true,
        KeyCode::Char('m') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

impl Page {
    fn new(session: Session, theme: Theme, out: Box<dyn Write + Send>) -> Self {
        let roles = session.catalog();
        Self {
            list: ListState::default().with_selected((!roles.is_empty()).then_some(0)),
            roles,
            session: Some(session),
            search: search_box(&theme),
            theme,
            folded: BTreeSet::new(),
            fold_decided: false,
            query: String::new(),
            searching: false,
            shown: None,
            status: String::new(),
            pending: BTreeMap::new(),
            popup: Popup::None,
            saving: None,
            quit_after_save: false,
            out,
            wake: None,
            height: 0,
            hits: mouse::Hits::default(),
            menu: None,
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

    /// Copy `i` of app `r`'s settings as the page shows them: the file's,
    /// each with its staged value, then the ones staged to be added, in
    /// key order.
    fn settings(&self, r: usize, i: usize) -> Vec<Setting> {
        let role = &self.roles[r];
        let instance = &role.instances[i];
        let staged = |key: &str| {
            self.pending.get(&Target::Setting(
                role.name.clone(),
                instance.name.clone(),
                key.to_owned(),
            ))
        };
        let mut settings: Vec<Setting> = instance
            .facts
            .iter()
            .map(|fact| Setting {
                key: fact.key.clone(),
                value: match staged(&fact.key) {
                    Some(Pending::Set(value)) => value.clone(),
                    _ => fact.value.clone(),
                },
                added: false,
            })
            .collect();
        let added = self
            .pending
            .iter()
            .filter_map(|(target, pending)| match (target, pending) {
                (Target::Setting(app, copy, key), Pending::Set(value))
                    if *app == role.name
                        && *copy == instance.name
                        && !instance.facts.iter().any(|fact| fact.key == *key) =>
                {
                    Some(Setting {
                        key: key.clone(),
                        value: value.clone(),
                        added: true,
                    })
                }
                _ => None,
            });
        settings.extend(added);
        settings
    }

    fn setting(&self, (r, i, f): Id) -> Setting {
        self.settings(r, i).swap_remove(f)
    }

    /// What `row` is, by name.
    fn target(&self, row: Row) -> Target {
        match row {
            Row::App(r) => Target::App(self.roles[r].name.clone()),
            Row::Copy(r, i) => Target::Copy(
                self.roles[r].name.clone(),
                self.roles[r].instances[i].name.clone(),
            ),
            Row::Setting((r, i, f)) => Target::Setting(
                self.roles[r].name.clone(),
                self.roles[r].instances[i].name.clone(),
                self.setting((r, i, f)).key,
            ),
        }
    }

    /// Whether `target`, or the app or copy it is in, is staged to be
    /// deleted.
    fn deleted(&self, target: &Target) -> bool {
        std::iter::once(target.clone())
            .chain(target.outer())
            .any(|t| self.pending.get(&t) == Some(&Pending::Delete))
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
                let settings: Vec<Row> = self
                    .settings(r, i)
                    .iter()
                    .enumerate()
                    .filter(|(_, setting)| copy_hit || self.matches(&setting.key))
                    .map(|(f, _)| Row::Setting((r, i, f)))
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
            if self.query.is_empty() && self.folded.contains(&role.name) {
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

    /// Whether setting `id`'s key names a secret: masked until Show.
    fn secret(&self, id: Id) -> bool {
        is_secret_key(&self.setting(id).key)
    }

    /// Whether Show has revealed setting `id`.
    fn is_shown(&self, id: Id) -> bool {
        self.shown.as_ref() == Some(&self.target(Row::Setting(id)))
    }

    /// Whether setting `id` shows as dots now.
    fn masked(&self, id: Id) -> bool {
        self.secret(id) && !self.is_shown(id)
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
        let name = self.roles[r].name.clone();
        if fold {
            self.folded.insert(name);
            let heading = self.rows().iter().position(|row| *row == Row::App(r));
            self.list.select(heading);
        } else {
            self.folded.remove(&name);
        }
    }

    /// Enter: an app's heading folds or opens it (not while searching, as
    /// [`Page::fold_selected`]); a setting opens its edit box.
    fn activate(&mut self) {
        match self.selected() {
            Some(Row::App(r)) if self.query.is_empty() => {
                let name = self.roles[r].name.clone();
                if !self.folded.remove(&name) {
                    self.folded.insert(name);
                }
            }
            Some(Row::Setting(_)) => self.start_edit(),
            _ => {}
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
            _ if newline(&key) => {}
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
        if let Some(menu) = self.menu {
            return self.menu_key(key, menu);
        }
        if !matches!(self.popup, Popup::None) {
            return self.popup_key(key);
        }
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
            KeyCode::Char('u') => self.undo_selected(),
            KeyCode::Char('m') | KeyCode::Menu => self.open_menu(None),
            KeyCode::Char('e') => return self.act(Action::Edit),
            KeyCode::Char('a') => return self.act(Action::Add),
            KeyCode::Char('c') => return self.act(Action::Copy),
            KeyCode::Char('s') => return self.act(Action::Show),
            KeyCode::Char('d') => return self.act(Action::Delete),
            KeyCode::Char('v') => return self.act(Action::Save),
            KeyCode::Char('q') | KeyCode::Esc => return self.act(Action::Quit),
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// A bottom button's action, by key or by click.
    fn act(&mut self, action: Action) -> Flow {
        match action {
            Action::Quit if self.pending.is_empty() => return Flow::Quit,
            Action::Quit => self.popup = Popup::ConfirmQuit,
            Action::Save if self.pending.is_empty() => {
                "Nothing to save".clone_into(&mut self.status);
            }
            Action::Save => self.popup = Popup::ConfirmSave,
            Action::Edit => self.start_edit(),
            Action::Undo => self.undo_selected(),
            Action::Add => self.start_add(),
            Action::Delete => self.toggle_delete(),
            Action::Show => match self.selected_setting() {
                Some(id) if self.is_shown(id) => self.shown = None,
                Some(id) if self.secret(id) => {
                    let target = self.target(Row::Setting(id));
                    let (role, copy, key) = names(&target);
                    self.shown = Some(target);
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
        let setting = self.setting(id);
        let copied = crossterm::execute!(
            self.out,
            CopyToClipboard::to_clipboard_from(setting.value.as_str())
        );
        let shown = one_line(&setting.key);
        let (role, copy, key) = names(&self.target(Row::Setting(id)));
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

    /// `target` as the changes panel and the status line name it:
    /// `app · copy · key`, the copy left out for an app with one copy
    /// named after it (proto-facts).
    fn label(&self, target: &Target) -> String {
        let app = |role: &str| self.roles.iter().position(|r| r.name == role);
        let with_copy = |role: &str| app(role).is_none_or(|r| self.has_copies(r));
        let text = match target {
            Target::App(role) => role.clone(),
            Target::Copy(role, copy) => format!("{role} · {copy}"),
            Target::Setting(role, copy, key) if with_copy(role) => {
                format!("{role} · {copy} · {key}")
            }
            Target::Setting(role, _, key) => format!("{role} · {key}"),
        };
        one_line(&text)
    }

    /// Stage `pending` on `target` once the session has checked it with
    /// every other staged change; `replacing` are the changes it replaces
    /// (an app's or copy's deletion replaces those inside it). A refusal
    /// stages nothing and says why on the status line.
    fn stage(&mut self, target: &Target, pending: &Pending) -> bool {
        let mut staged = self.pending.clone();
        staged.retain(|t, _| !t.inside(target));
        staged.insert(target.clone(), pending.clone());
        if let Some(session) = &self.session
            && let Err(refused) = session.check(&changes::to_changes(&staged))
        {
            self.status = format!("Can't stage that change: {refused}");
            return false;
        }
        let added = matches!(target, Target::Setting(..))
            && !self.exists(target)
            && matches!(pending, Pending::Set(_));
        let (role, copy, key) = names(target);
        tracing::info!(
            role = %role,
            copy = %copy,
            key = %key,
            change = changes::kind(target, pending, added),
            "change staged"
        );
        self.pending = staged;
        true
    }

    /// Whether `target` is in the catalog (not one staged to be added).
    fn exists(&self, target: &Target) -> bool {
        let Some(role) = self.roles.iter().find(|r| r.name == target.role()) else {
            return false;
        };
        match target {
            Target::App(_) => true,
            Target::Copy(_, copy) => role.instances.iter().any(|i| i.name == *copy),
            Target::Setting(_, copy, key) => role
                .instances
                .iter()
                .any(|i| i.name == *copy && i.facts.iter().any(|f| f.key == *key)),
        }
    }

    /// Drop `target`'s staged change, if it has one.
    fn undo(&mut self, target: &Target) -> bool {
        if self.pending.remove(target).is_none() {
            return false;
        }
        let (role, copy, key) = names(target);
        tracing::info!(role = %role, copy = %copy, key = %key, "change undone");
        self.status = format!("Undid the change to {}", self.label(target));
        self.clamp();
        true
    }

    /// `u`: undo the selected row's change.
    fn undo_selected(&mut self) {
        let Some(row) = self.selected() else {
            return;
        };
        let target = self.target(row);
        if !self.undo(&target) {
            "Nothing to undo here".clone_into(&mut self.status);
        }
    }

    /// The selection back on a row after rows went (an undone addition,
    /// a reload).
    fn clamp(&mut self) {
        self.move_selection(|_| {});
    }

    /// `d`: Delete the selected row's setting, copy or app, or Keep it
    /// again. Deleting a setting staged to be added drops the addition.
    fn toggle_delete(&mut self) {
        let Some(row) = self.selected() else {
            return;
        };
        let target = self.target(row);
        if self.pending.get(&target) == Some(&Pending::Delete) {
            self.undo(&target);
            return;
        }
        if target
            .outer()
            .iter()
            .any(|t| self.pending.get(t) == Some(&Pending::Delete))
        {
            "Its app or copy is already to be deleted".clone_into(&mut self.status);
            return;
        }
        if !self.exists(&target) {
            self.undo(&target);
            return;
        }
        if self.stage(&target, &Pending::Delete) {
            self.status = format!("{}: {DELETED}", self.label(&target));
        }
    }

    /// Save the staged changes: the session goes to a thread of its own
    /// ([`save::Running`]) until the save is over.
    fn start_save(&mut self) -> Flow {
        let Some(session) = self.session.take() else {
            "Can't save: the save before stopped unexpectedly; quit and open sb fact again"
                .clone_into(&mut self.status);
            return Flow::Redraw;
        };
        let changes = changes::to_changes(&self.pending);
        tracing::info!(changes = changes.len(), "save started");
        match save::Running::start(session, changes, self.wake.clone()) {
            Ok(running) => {
                self.saving = Some(running);
                self.popup = Popup::Saving;
            }
            Err((session, err)) => {
                self.session = Some(*session);
                self.status = format!("Couldn't start the save: {err}");
                self.popup = Popup::None;
                self.quit_after_save = false;
            }
        }
        Flow::Redraw
    }

    /// The save's news: the waiting dialog's time, a drift question, or
    /// its end.
    fn save_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(running) = &self.saving {
            let event = match running.next() {
                save::Next::Event(event) => event,
                save::Next::Quiet => break,
                save::Next::Gone => {
                    self.save_lost();
                    return true;
                }
            };
            changed = true;
            match event {
                // A wait shorter than a second goes unshown; a drift
                // question being answered stays up.
                save::Event::Waiting { role, waited } => {
                    if waited >= WAIT_SHOWN
                        && matches!(self.popup, Popup::Saving | Popup::Waiting { .. })
                    {
                        self.popup = Popup::Waiting { role, waited };
                    }
                }
                save::Event::Drift { role, drift, reply } => {
                    self.popup = Popup::Drift {
                        role,
                        drift,
                        reply,
                        scroll: ScrollViewState::default(),
                    };
                }
                save::Event::Done { session, outcomes } => {
                    self.saving = None;
                    self.saved(*session, &outcomes);
                }
            }
        }
        changed
    }

    /// The save's thread ended without handing the session back (it
    /// panicked): what was committed stays, nothing more can be saved.
    fn save_lost(&mut self) {
        self.saving = None;
        self.quit_after_save = false;
        tracing::error!("the save stopped unexpectedly");
        self.popup = Popup::Outcome {
            lines: vec![(
                "The save stopped unexpectedly. Some apps may be saved; quit and open sb fact again to see them.".to_owned(),
                Role::Problem,
            )],
            all_saved: false,
        };
    }

    /// The save is over: the session back, the catalog as it is now, the
    /// changes that were written (or dropped by a reload) gone from the
    /// panel, and the outcome dialog.
    fn saved(&mut self, session: Session, outcomes: &BTreeMap<String, RoleOutcome>) {
        for (role, outcome) in outcomes {
            if matches!(
                outcome,
                RoleOutcome::Applied
                    | RoleOutcome::Unsynced(_)
                    | RoleOutcome::Drifted { reloaded: true }
            ) {
                self.pending.retain(|target, _| target.role() != role);
            }
        }
        self.roles = session.catalog();
        self.session = Some(session);
        self.shown = None;
        self.status.clear();
        self.clamp();
        // Saved only when every app's changes are on disk: a reload dropped
        // its changes, which the user has to make again.
        let all_saved = outcomes
            .values()
            .all(|outcome| matches!(outcome, RoleOutcome::Applied | RoleOutcome::Unsynced(_)));
        self.popup = Popup::Outcome {
            lines: dialog::outcome_lines(outcomes),
            all_saved,
        };
    }

    /// Where values start, after the indent and the key: the same column
    /// for every setting, worked out from all of them, so values don't
    /// move while searching (proto-facts), the keys counted up to
    /// [`KEY_WIDTH_CAP`].
    fn key_width(&self) -> usize {
        let mut width = 0;
        for (r, role) in self.roles.iter().enumerate() {
            for i in 0..role.instances.len() {
                for setting in self.settings(r, i) {
                    let key = Span::raw(one_line(&setting.key)).width().min(KEY_WIDTH_CAP);
                    width = width.max(self.indent(r).len() + key);
                }
            }
        }
        width
    }

    /// A setting's indent: deeper under a copy's heading.
    fn indent(&self, r: usize) -> &'static str {
        if self.has_copies(r) { "    " } else { "  " }
    }

    /// The tag after a row with a staged change.
    fn tag(&self, target: &Target, added: bool) -> Option<Span<'static>> {
        match self.pending.get(target)? {
            Pending::Delete => Some(Span::styled("  will be deleted", self.style(Role::Problem))),
            Pending::Set(_) if added => Some(Span::styled("  new", self.style(Role::Ok))),
            Pending::Set(_) => Some(Span::styled("  changed", self.style(Role::Warn))),
        }
    }

    /// One row of the page; a row staged to be deleted is struck through.
    fn item(&self, row: Row, key_width: usize) -> ListItem<'static> {
        let target = self.target(row);
        let strike = if self.deleted(&target) {
            Modifier::CROSSED_OUT
        } else {
            Modifier::empty()
        };
        let mut spans = match row {
            Row::App(r) => {
                let role = &self.roles[r];
                let folded = self.query.is_empty() && self.folded.contains(&role.name);
                let mut spans = vec![
                    Span::styled(if folded { "▸ " } else { "▾ " }, self.style(Role::Faint)),
                    Span::styled(
                        title_case(&one_line(&role.name)),
                        self.style(Role::Text).add_modifier(Modifier::BOLD | strike),
                    ),
                ];
                if folded {
                    let count: usize = (0..role.instances.len())
                        .map(|i| self.settings(r, i).len())
                        .sum();
                    let settings = if count == 1 { "setting" } else { "settings" };
                    spans.push(Span::styled(
                        format!("  {count} {settings}"),
                        self.style(Role::Muted),
                    ));
                }
                spans
            }
            Row::Copy(r, i) => vec![
                Span::raw("  "),
                Span::styled(
                    one_line(&self.roles[r].instances[i].name),
                    self.style(Role::Accent).add_modifier(strike),
                ),
            ],
            Row::Setting(id) => {
                // Every value starts in the same column.
                let indent = self.indent(id.0);
                let setting = self.setting(id);
                let key = one_line(&setting.key);
                let pad = key_width.saturating_sub(indent.len() + Span::raw(key.as_str()).width());
                let value = if self.masked(id) {
                    Span::styled(DOTS, self.style(Role::Muted).add_modifier(strike))
                } else {
                    let shown = DisplayText::from_bytes(setting.value.as_bytes(), false);
                    let mut lines = shown.lines();
                    let mut first = lines.next().unwrap_or_default().to_owned();
                    if lines.next().is_some() {
                        first.push_str(" …");
                    }
                    Span::styled(first, self.style(Role::Text).add_modifier(strike))
                };
                vec![
                    Span::raw(indent),
                    Span::styled(key, self.style(Role::Text).add_modifier(strike)),
                    Span::raw(" ".repeat(pad + 2)),
                    value,
                ]
            }
        };
        let added = matches!(row, Row::Setting(id) if self.setting(id).added);
        spans.extend(self.tag(&target, added));
        ListItem::new(Line::from(spans))
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
    /// the selected row are drawn faint. "Save changes" is "Save" when
    /// the row is too narrow for every button's whole label.
    fn buttons(&self, width: u16) -> Vec<(Span<'static>, Action, bool)> {
        let row = self.selected();
        let target = row.map(|row| self.target(row));
        let on_setting = self.selected_setting();
        let live = target.as_ref().is_some_and(|t| !self.deleted(t));
        let show = if on_setting.is_some_and(|id| self.is_shown(id)) {
            "Hide"
        } else {
            "Show"
        };
        let delete = if target
            .as_ref()
            .is_some_and(|t| self.pending.get(t) == Some(&Pending::Delete))
        {
            "Keep"
        } else {
            "Delete"
        };
        // Inside a deleted app or copy only Keep on it acts.
        let can_delete = target
            .as_ref()
            .is_some_and(|t| self.pending.get(t) == Some(&Pending::Delete) || !self.deleted(t));
        let can_add = match row {
            Some(Row::Copy(..) | Row::Setting(_)) => live,
            Some(Row::App(r)) => live && self.roles[r].instances.len() == 1,
            None => false,
        };
        let specs = |save: &'static str| {
            [
                ("Edit", "e", Action::Edit, on_setting.is_some() && live),
                ("Add", "a", Action::Add, can_add),
                ("Copy", "c", Action::Copy, on_setting.is_some()),
                (
                    show,
                    "s",
                    Action::Show,
                    on_setting.is_some_and(|id| self.secret(id)),
                ),
                (delete, "d", Action::Delete, can_delete),
                (save, "v", Action::Save, !self.pending.is_empty()),
                ("Quit", "q", Action::Quit, true),
            ]
        };
        // Each label, its key, the spaces around it and one between.
        let needed = |save| -> usize {
            specs(save)
                .iter()
                .map(|(label, key, ..)| Span::raw(format!(" {label} ({key}) ")).width() + 1)
                .sum::<usize>()
                + 1
        };
        let save = if needed("Save changes") <= usize::from(width) {
            "Save changes"
        } else {
            "Save"
        };
        specs(save)
            .into_iter()
            .map(|(label, key, action, enabled)| {
                let style = match (enabled, action) {
                    (true, Action::Save) => self
                        .style(Role::OnAccent)
                        .patch(self.bg(Role::Ok))
                        .add_modifier(Modifier::BOLD),
                    (true, _) => self.style(Role::Text).patch(self.bg(Role::ButtonBg)),
                    (false, _) => self
                        .style(Role::Faint)
                        .patch(self.bg(Role::ButtonDisabledBg)),
                };
                (
                    Span::styled(format!(" {label} ({key}) "), style),
                    action,
                    enabled,
                )
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

    /// The changes panel: each staged change's verb, what it is on and its
    /// undo, one row each, laid out by ratatui (proto-facts); the undos'
    /// rectangles come back as their click targets.
    fn draw_changes(&self, frame: &mut Frame<'_>, area: Rect) -> Vec<(Rect, Target)> {
        let mut undos = Vec::new();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.style(Role::Border))
            .title(Span::styled(
                format!(" Unsaved changes ({}) ", self.pending.len()),
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area).inner(Margin::new(1, 0));
        frame.render_widget(block, area);
        if self.pending.is_empty() {
            frame.render_widget(
                Span::styled("Nothing changed yet", self.style(Role::Muted)),
                inner,
            );
            return undos;
        }
        let undo = Span::styled(
            " undo ",
            self.style(Role::Link).add_modifier(Modifier::UNDERLINED),
        );
        // Changes past the rows there are: the last row says how many.
        let rows: Vec<Rect> = inner.rows().collect();
        let shown = if self.pending.len() > rows.len() {
            let more = self.pending.len() - rows.len().saturating_sub(1);
            if let Some(last) = rows.last() {
                frame.render_widget(
                    Span::styled(format!("+{more} more"), self.style(Role::Muted)),
                    *last,
                );
            }
            rows.len().saturating_sub(1)
        } else {
            rows.len()
        };
        for ((target, pending), row) in self.pending.iter().zip(rows.into_iter().take(shown)) {
            let verb = match pending {
                Pending::Delete => Span::styled("Delete", self.style(Role::Problem)),
                Pending::Set(_) if !self.exists(target) => {
                    Span::styled("Add", self.style(Role::Ok))
                }
                Pending::Set(_) => Span::styled("Edit", self.style(Role::Warn)),
            };
            let [verb_area, label_area, undo_area] = row.layout(
                &Layout::horizontal([
                    Constraint::Length(6),
                    Constraint::Fill(1),
                    Constraint::Length(u16::try_from(undo.width()).unwrap_or(u16::MAX)),
                ])
                .spacing(1),
            );
            frame.render_widget(verb, verb_area);
            frame.render_widget(
                Span::styled(self.label(target), self.style(Role::Text)),
                label_area,
            );
            frame.render_widget(undo.clone(), undo_area);
            undos.push((undo_area, target.clone()));
        }
        undos
    }
}

impl Screen for Page {
    fn input(&mut self, input: Input) -> Flow {
        // The pointer moving is the menu's alone (its hover): nothing else
        // on the page follows it, and every move is reported.
        if let Input::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..
        }) = input
            && self.menu.is_none()
        {
            return Flow::Unchanged;
        }
        let before = self.selected().map(|row| self.target(row));
        let status = self.status.clone();
        let flow = match input {
            Input::Key(key) => self.key(key),
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Resize(..) => return Flow::Redraw,
            _ => return Flow::Unchanged,
        };
        // Show, and what the status line said, last until the selection
        // moves; a message this key set stays.
        if self.selected().map(|row| self.target(row)) != before {
            self.shown = None;
            if self.status == status {
                self.status.clear();
            }
        }
        flow
    }

    /// A running save's news.
    fn wake(&mut self) -> Flow {
        if self.save_events() {
            Flow::Redraw
        } else {
            Flow::Unchanged
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let wide = area.width >= WIDE;
        // ratatui's layout: the search box, the page, the status line and
        // the buttons
        // (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.vertical>).
        // Below 100 columns the changes panel is a band above the status
        // line: its border and a row per change, up to four.
        let band = u16::try_from(self.pending.len().clamp(1, BAND_ROWS)).unwrap_or(1) + 2;
        let bottom = if wide { 0 } else { band };
        let [search, body, changes_bottom, status, buttons] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Fill(1),
            Constraint::Length(bottom),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_search(frame, search);
        self.hits.search = search;
        let list_area = if wide {
            let [list, changes] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(PANEL_WIDTH)])
                    .areas(body);
            self.hits.undo = self.draw_changes(frame, changes);
            list
        } else {
            self.hits.undo = self.draw_changes(frame, changes_bottom);
            body
        };
        self.height = list_area.height;
        self.hits.list = list_area;
        if !self.fold_decided && list_area.height > 0 {
            // Expanded when it fits, else folded to app names.
            self.fold_decided = true;
            if self.rows().len() > usize::from(list_area.height) {
                self.folded = self.roles.iter().map(|role| role.name.clone()).collect();
                self.clamp();
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
        // rectangle that is its click target (proto-facts).
        let row = buttons.inner(Margin::new(1, 0));
        let spans = self.buttons(row.width);
        let areas = Layout::horizontal(spans.iter().map(|(span, _, _)| {
            Constraint::Length(u16::try_from(span.width()).unwrap_or(u16::MAX))
        }))
        .spacing(1)
        .split(row);
        self.hits.buttons.clear();
        for ((span, action, enabled), area) in spans.into_iter().zip(areas.iter()) {
            // A button the row is too narrow for isn't a target (proto-facts).
            if enabled && usize::from(area.width) == span.width() {
                self.hits.buttons.push((*area, action));
            }
            frame.render_widget(span, *area);
        }
        dialog::draw(self, frame, area);
        self.draw_menu(frame, area);
    }
}

#[cfg(test)]
mod tests;
