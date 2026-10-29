//! Prototype: fact editor as a settings page (see docs/ideas/fact-editor.md).
//! Apps are headings, settings sit beneath them with values visible, a search
//! box narrows the page, buttons do everything, and unsaved changes are always
//! listed. Throwaway; see FINDINGS.md. Never point it at the live facts directory.

mod model;

use proto_theme as theme;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use model::{Change, FactId, Role, Target};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;

#[derive(Parser)]
struct Cli {
    /// Directory of app .ini files (use a copy).
    dir: PathBuf,
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    App(usize),
    Copy(usize, usize),
    Setting(FactId),
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Edit,
    Copy,
    Show,
    Delete,
    Undo,
    Save,
    Quit,
}

enum Popup {
    None,
    Edit(Box<TextArea<'static>>, FactId),
    ConfirmSave,
    ConfirmQuit,
    /// Right-click (or `m`) menu for one row; `sel` is the highlighted item.
    Menu {
        at: Position,
        sel: usize,
    },
}

struct App {
    roles: Vec<Role>,
    folded: BTreeSet<usize>,
    fold_decided: bool,
    list: ListState,
    /// Single-line search box; editing keys (Backspace, Ctrl+H, Ctrl+W, …)
    /// are the widget's job, not ours. `query` mirrors its text.
    search: TextArea<'static>,
    query: String,
    searching: bool,
    pending: BTreeMap<Target, Change>,
    shown: BTreeSet<FactId>,
    popup: Popup,
    status: String,
    quit: bool,
    // Areas from the last draw, for mouse hits.
    list_area: Rect,
    search_area: Rect,
    buttons: Vec<(Rect, Action)>,
    undo_targets: Vec<(Rect, Target)>,
    popup_buttons: Vec<(Rect, bool)>,
    menu_area: Rect,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if std::fs::canonicalize(&cli.dir)? == std::path::Path::new("/opt/saltbox") {
        bail!("refusing to edit the standard Saltbox facts directory; pass a copy");
    }
    let mut app = App::new(model::load(&cli.dir)?);
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app, &cli.dir);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    drain_input();
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, dir: &std::path::Path) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| draw(f, app))?;
        // Handle every event that is already waiting, then redraw once, so a
        // moving mouse can't queue up a redraw per motion event. Uses crossterm's
        // documented blocking read() plus poll() on one thread:
        // https://docs.rs/crossterm/0.29.0/crossterm/event/index.html
        let mut event = event::read()?;
        loop {
            let action = match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => None,
            };
            if let Some(action) = action {
                app.perform(action, dir)?;
            }
            if app.quit || !event::poll(std::time::Duration::ZERO)? {
                break;
            }
            event = event::read()?;
        }
    }
    Ok(())
}

/// Reads and discards input still in flight after mouse capture is turned
/// off, so stray mouse codes don't end up in the user's shell. Same thread as
/// the event loop, as crossterm requires for read()/poll().
/// UNVERIFIED pattern: no doc covers draining; kept because it measurably
/// stopped the leak (FINDINGS.md).
fn drain_input() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
    while std::time::Instant::now() < deadline {
        if let Ok(true) = event::poll(std::time::Duration::from_millis(20)) {
            let _ = event::read();
        }
    }
}

impl App {
    fn new(roles: Vec<Role>) -> Self {
        Self {
            roles,
            folded: BTreeSet::new(),
            fold_decided: false,
            list: ListState::default().with_selected(Some(0)),
            search: search_box(),
            query: String::new(),
            searching: false,
            pending: BTreeMap::new(),
            shown: BTreeSet::new(),
            popup: Popup::None,
            status: String::new(),
            quit: false,
            list_area: Rect::default(),
            search_area: Rect::default(),
            buttons: Vec::new(),
            undo_targets: Vec::new(),
            popup_buttons: Vec::new(),
            menu_area: Rect::default(),
        }
    }

    fn has_copies(&self, r: usize) -> bool {
        let role = &self.roles[r];
        role.instances.len() > 1 || role.instances.first().is_some_and(|i| i.name != role.name)
    }

    fn matches(&self, (r, i, f): FactId) -> bool {
        let q = self.query.to_lowercase();
        let role = &self.roles[r];
        q.is_empty()
            || role.name.to_lowercase().contains(&q)
            || role.instances[i].name.to_lowercase().contains(&q)
            || role.instances[i].facts[f].key.to_lowercase().contains(&q)
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (r, role) in self.roles.iter().enumerate() {
            let hits: Vec<FactId> = role
                .instances
                .iter()
                .enumerate()
                .flat_map(|(i, inst)| (0..inst.facts.len()).map(move |f| (r, i, f)))
                .filter(|id| self.matches(*id))
                .collect();
            if !self.query.is_empty() && hits.is_empty() {
                continue;
            }
            rows.push(Row::App(r));
            // Searching opens every matching group, whatever its fold state.
            if self.query.is_empty() && self.folded.contains(&r) {
                continue;
            }
            for (i, _) in role.instances.iter().enumerate() {
                let inst_hits: Vec<FactId> = hits.iter().copied().filter(|h| h.1 == i).collect();
                if inst_hits.is_empty() {
                    continue;
                }
                if self.has_copies(r) {
                    rows.push(Row::Copy(r, i));
                }
                rows.extend(inst_hits.into_iter().map(Row::Setting));
            }
        }
        rows
    }

    fn selected(&self) -> Option<Row> {
        self.rows().get(self.list.selected()?).copied()
    }

    fn deleted(&self, row: Row) -> bool {
        let del = |t| matches!(self.pending.get(&t), Some(Change::Delete));
        match row {
            Row::App(r) => del(Target::App(r)),
            Row::Copy(r, i) => del(Target::App(r)) || del(Target::Copy(r, i)),
            Row::Setting((r, i, f)) => {
                del(Target::App(r)) || del(Target::Copy(r, i)) || del(Target::Setting((r, i, f)))
            }
        }
    }

    fn value(&self, id: FactId) -> String {
        match self.pending.get(&Target::Setting(id)) {
            Some(Change::Set(v)) => v.clone(),
            _ => self.roles[id.0].instances[id.1].facts[id.2].value.clone(),
        }
    }

    fn masked(&self, id: FactId) -> bool {
        model::looks_secret(&self.roles[id.0].instances[id.1].facts[id.2].key)
            && !self.shown.contains(&id)
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if let Popup::Menu { at, sel } = self.popup {
            return self.on_menu_key(key, at, sel);
        }
        match &mut self.popup {
            Popup::Edit(textarea, id) => {
                match key.code {
                    KeyCode::Esc => {
                        self.popup = Popup::None;
                        self.status = "Edit cancelled".into();
                    }
                    KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
                        let (value, id) = (textarea.lines().join("\n"), *id);
                        self.stage_edit(id, value);
                        self.popup = Popup::None;
                    }
                    _ => {
                        textarea.input(key);
                    }
                }
                return None;
            }
            Popup::ConfirmSave => {
                match key.code {
                    KeyCode::Enter | KeyCode::Char('y') => return Some(Action::Save),
                    _ => self.popup = Popup::None,
                }
                return None;
            }
            Popup::ConfirmQuit => {
                match key.code {
                    KeyCode::Enter | KeyCode::Char('y') => self.quit = true,
                    _ => self.popup = Popup::None,
                }
                return None;
            }
            // Handled by on_menu_key above.
            Popup::Menu { .. } | Popup::None => {}
        }
        if self.searching {
            match key.code {
                KeyCode::Esc if self.query.is_empty() => self.searching = false,
                KeyCode::Esc => self.search = search_box(),
                KeyCode::Enter | KeyCode::Down | KeyCode::Tab => self.searching = false,
                _ => {
                    self.search.input(key);
                }
            }
            self.query = self.search.lines().first().cloned().unwrap_or_default();
            self.list.select(Some(0));
            return None;
        }
        let len = self.rows().len();
        let sel = self.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.list.select(Some((sel + 1).min(len.saturating_sub(1))))
            }
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(sel.saturating_sub(1))),
            KeyCode::PageDown => self
                .list
                .select(Some((sel + 10).min(len.saturating_sub(1)))),
            KeyCode::PageUp => self.list.select(Some(sel.saturating_sub(10))),
            KeyCode::Enter => self.activate(),
            KeyCode::Left => self.fold_selected(true),
            KeyCode::Right => self.fold_selected(false),
            KeyCode::Char('e') => return Some(Action::Edit),
            KeyCode::Char('c') => return Some(Action::Copy),
            KeyCode::Char('s') => return Some(Action::Show),
            KeyCode::Char('d') => return Some(Action::Delete),
            KeyCode::Char('u') => return Some(Action::Undo),
            KeyCode::Char('m') | KeyCode::Menu => self.open_menu(None),
            KeyCode::Char('v') => return Some(Action::Save),
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    fn on_menu_key(&mut self, key: KeyEvent, at: Position, sel: usize) -> Option<Action> {
        let items = self
            .selected()
            .map(|r| self.menu_items(r))
            .unwrap_or_default();
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.popup = Popup::Menu {
                    at,
                    sel: sel.saturating_sub(1),
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.popup = Popup::Menu {
                    at,
                    sel: (sel + 1).min(last),
                }
            }
            KeyCode::Enter => {
                self.popup = Popup::None;
                return items.get(sel).map(|(_, _, a)| *a);
            }
            KeyCode::Esc | KeyCode::Char('m') | KeyCode::Menu => self.popup = Popup::None,
            code => {
                // An item's own shortcut works inside the menu too.
                let hit = items
                    .iter()
                    .find(|(_, k, _)| key_matches(k, code))
                    .map(|(_, _, a)| *a);
                if hit.is_some() {
                    self.popup = Popup::None;
                }
                return hit;
            }
        }
        None
    }

    /// Enter / second click: open or fold a heading, edit a setting.
    fn activate(&mut self) {
        match self.selected() {
            Some(Row::App(r)) => {
                if !self.folded.remove(&r) {
                    self.folded.insert(r);
                }
            }
            Some(Row::Setting(id)) => self.start_edit(id),
            _ => {}
        }
    }

    fn fold_selected(&mut self, fold: bool) {
        let r = match self.selected() {
            Some(Row::App(r) | Row::Copy(r, _) | Row::Setting((r, _, _))) => r,
            None => return,
        };
        if fold {
            self.folded.insert(r);
            let rows = self.rows();
            self.list
                .select(rows.iter().position(|row| *row == Row::App(r)));
        } else {
            self.folded.remove(&r);
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Option<Action> {
        let pos = Position::new(m.column, m.row);
        if let Popup::Menu { sel, .. } = &mut self.popup {
            let area = self.menu_area;
            let item =
                (area.contains(pos) && m.row > area.y && m.row < area.bottom().saturating_sub(1))
                    .then(|| usize::from(m.row - area.y - 1));
            match m.kind {
                MouseEventKind::Moved => {
                    if let Some(i) = item {
                        *sel = i;
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let action = item.and_then(|i| {
                        self.selected()
                            .and_then(|r| self.menu_items(r).get(i).map(|(_, _, a)| *a))
                    });
                    self.popup = Popup::None;
                    return action;
                }
                MouseEventKind::Down(_) => self.popup = Popup::None,
                _ => {}
            }
            return None;
        }
        if !matches!(self.popup, Popup::None) {
            if m.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, yes)) = self.popup_buttons.iter().find(|(r, _)| r.contains(pos))
            {
                let yes = *yes;
                return match (&self.popup, yes) {
                    (Popup::ConfirmSave, true) => Some(Action::Save),
                    (Popup::ConfirmQuit, true) => {
                        self.quit = true;
                        None
                    }
                    _ => {
                        self.popup = Popup::None;
                        None
                    }
                };
            }
            return None;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, action)) = self.buttons.iter().find(|(r, _)| r.contains(pos)) {
                    return Some(*action);
                }
                if let Some((_, target)) = self.undo_targets.iter().find(|(r, _)| r.contains(pos)) {
                    let t = *target;
                    self.undo(t);
                    return None;
                }
                if self.search_area.contains(pos) {
                    self.searching = true;
                    return None;
                }
                self.searching = false;
                if self.list_area.contains(pos) {
                    let row = self.list.offset() + usize::from(m.row - self.list_area.y);
                    if row < self.rows().len() {
                        if self.list.selected() == Some(row) {
                            self.activate();
                        } else {
                            self.list.select(Some(row));
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if self.list_area.contains(pos) => {
                let row = self.list.offset() + usize::from(m.row - self.list_area.y);
                if row < self.rows().len() {
                    self.list.select(Some(row));
                    self.open_menu(Some(pos));
                }
            }
            MouseEventKind::ScrollDown if self.list_area.contains(pos) => {
                let len = self.rows().len();
                let sel = self.list.selected().unwrap_or(0);
                self.list.select(Some((sel + 1).min(len.saturating_sub(1))));
            }
            MouseEventKind::ScrollUp if self.list_area.contains(pos) => {
                let sel = self.list.selected().unwrap_or(0);
                self.list.select(Some(sel.saturating_sub(1)));
            }
            _ => {}
        }
        None
    }

    fn perform(&mut self, action: Action, dir: &std::path::Path) -> Result<()> {
        let sel = self.selected();
        match action {
            Action::Edit => match sel {
                Some(Row::Setting(id)) if !self.deleted(Row::Setting(id)) => self.start_edit(id),
                _ => self.status = "Pick a setting to edit".into(),
            },
            Action::Copy => match sel {
                Some(Row::Setting(id)) => {
                    // OSC 52 via crossterm's clipboard command (feature "osc52"):
                    // https://docs.rs/crossterm/0.29.0/crossterm/clipboard/struct.CopyToClipboard.html
                    // Terminals that block clipboard access ignore it silently.
                    execute!(
                        std::io::stdout(),
                        crossterm::clipboard::CopyToClipboard::to_clipboard_from(self.value(id))
                    )?;
                    let key = &self.roles[id.0].instances[id.1].facts[id.2].key;
                    self.status = format!(
                        "Copied {key} to the clipboard (if your terminal allows it; otherwise use Show)"
                    );
                }
                _ => self.status = "Pick a setting to copy".into(),
            },
            Action::Show => match sel {
                Some(Row::Setting(id)) => {
                    if !self.shown.remove(&id) {
                        self.shown.insert(id);
                    }
                }
                _ => self.status = "Pick a setting to show".into(),
            },
            Action::Delete => {
                if let Some(row) = sel {
                    let t = target_of(row);
                    if matches!(self.pending.get(&t), Some(Change::Delete)) {
                        self.undo(t);
                    } else {
                        // Deleting an app or copy replaces any pending changes inside it.
                        self.pending.retain(|k, _| !inside(*k, t));
                        self.pending.insert(t, Change::Delete);
                        self.status = format!(
                            "{} will be deleted when you save. Saltbox creates a new value next time it runs.",
                            self.label(t)
                        );
                    }
                }
            }
            Action::Undo => {
                if let Some(row) = sel {
                    self.undo(target_of(row));
                }
            }
            Action::Save => {
                if self.pending.is_empty() {
                    self.status = "Nothing to save".into();
                } else if matches!(self.popup, Popup::ConfirmSave) {
                    let count = self.pending.len();
                    model::apply(&self.roles, &self.pending)?;
                    self.roles = model::load(dir)?;
                    self.pending.clear();
                    self.shown.clear();
                    self.popup = Popup::None;
                    self.list.select(Some(0));
                    self.status = format!("Saved {count} change(s)");
                } else {
                    self.popup = Popup::ConfirmSave;
                }
            }
            Action::Quit => {
                if self.pending.is_empty() {
                    self.quit = true;
                } else {
                    self.popup = Popup::ConfirmQuit;
                }
            }
        }
        Ok(())
    }

    /// Opens the action menu for the selected row, at the click position or
    /// next to the row when opened from the keyboard.
    fn open_menu(&mut self, at: Option<Position>) {
        let Some(sel) = self.list.selected() else {
            return;
        };
        if sel >= self.rows().len() {
            return;
        }
        let row_y =
            self.list_area.y + u16::try_from(sel.saturating_sub(self.list.offset())).unwrap_or(0);
        let at = at.unwrap_or(Position::new(self.list_area.x + 30, row_y));
        self.popup = Popup::Menu { at, sel: 0 };
    }

    /// The menu entries for a row: label, the key that does the same thing
    /// without the menu, and the action.
    fn menu_items(&self, row: Row) -> Vec<(String, &'static str, Action)> {
        let t = target_of(row);
        let marked_delete = matches!(self.pending.get(&t), Some(Change::Delete));
        let mut items = Vec::new();
        match row {
            Row::Setting(id) => {
                if model::looks_secret(&self.roles[id.0].instances[id.1].facts[id.2].key) {
                    let label = if self.shown.contains(&id) {
                        "Hide"
                    } else {
                        "Show"
                    };
                    items.push((label.to_string(), "s", Action::Show));
                }
                items.push(("Copy".to_string(), "c", Action::Copy));
                if !self.deleted(row) {
                    items.push(("Edit".to_string(), "e", Action::Edit));
                }
                if matches!(self.pending.get(&t), Some(Change::Set(_))) {
                    items.push(("Undo change".to_string(), "u", Action::Undo));
                }
                items.push((
                    if marked_delete { "Keep" } else { "Delete" }.to_string(),
                    "d",
                    Action::Delete,
                ));
            }
            Row::Copy(..) => {
                items.push((
                    if marked_delete {
                        "Keep copy"
                    } else {
                        "Delete copy"
                    }
                    .to_string(),
                    "d",
                    Action::Delete,
                ));
            }
            Row::App(_) => {
                items.push((
                    if marked_delete {
                        "Keep app"
                    } else {
                        "Delete app"
                    }
                    .to_string(),
                    "d",
                    Action::Delete,
                ));
            }
        }
        items
    }

    fn start_edit(&mut self, id: FactId) {
        let mut textarea = TextArea::new(self.value(id).lines().map(String::from).collect());
        let key = self.roles[id.0].instances[id.1].facts[id.2].key.clone();
        textarea.set_block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(theme::ACCENT))
                .title(Span::styled(
                    format!(" Edit {key} "),
                    Style::new().fg(theme::ACCENT).bold(),
                ))
                .title_bottom(Span::styled(
                    " Enter save · Alt+Enter new line · Esc cancel ",
                    Style::new().fg(theme::MUTED),
                )),
        );
        textarea.set_style(Style::new().fg(theme::TEXT));
        textarea.set_cursor_line_style(Style::default());
        textarea.move_cursor(ratatui_textarea::CursorMove::Bottom);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        self.popup = Popup::Edit(Box::new(textarea), id);
    }

    fn stage_edit(&mut self, id: FactId, value: String) {
        let original = &self.roles[id.0].instances[id.1].facts[id.2].value;
        if &value == original {
            self.pending.remove(&Target::Setting(id));
            self.status = "No change".into();
        } else {
            self.pending.insert(Target::Setting(id), Change::Set(value));
            self.status = format!(
                "{} changed (not saved yet)",
                self.label(Target::Setting(id))
            );
        }
    }

    fn undo(&mut self, t: Target) {
        if self.pending.remove(&t).is_some() {
            self.status = format!("Undid change to {}", self.label(t));
        }
    }

    fn label(&self, t: Target) -> String {
        match t {
            Target::App(r) => self.roles[r].name.clone(),
            Target::Copy(r, i) => format!(
                "{} · {}",
                self.roles[r].name, self.roles[r].instances[i].name
            ),
            Target::Setting((r, i, f)) => {
                let key = &self.roles[r].instances[i].facts[f].key;
                if self.has_copies(r) {
                    format!(
                        "{} · {} · {key}",
                        self.roles[r].name, self.roles[r].instances[i].name
                    )
                } else {
                    format!("{} · {key}", self.roles[r].name)
                }
            }
        }
    }
}

/// True when `k` is strictly inside `outer` (a setting in an app, a copy in an app, ...).
fn inside(k: Target, outer: Target) -> bool {
    match (k, outer) {
        (Target::Copy(r, _) | Target::Setting((r, _, _)), Target::App(a)) => r == a,
        (Target::Setting((r, i, _)), Target::Copy(a, b)) => (r, i) == (a, b),
        _ => false,
    }
}

fn key_matches(shortcut: &str, code: KeyCode) -> bool {
    match code {
        KeyCode::Char(c) => shortcut.len() == 1 && shortcut.starts_with(c),
        _ => false,
    }
}

fn search_box() -> TextArea<'static> {
    let mut t = TextArea::default();
    t.set_placeholder_text("type / or click here to search");
    t.set_placeholder_style(Style::new().fg(theme::FAINT));
    t.set_cursor_line_style(Style::default());
    t
}

fn target_of(row: Row) -> Target {
    match row {
        Row::App(r) => Target::App(r),
        Row::Copy(r, i) => Target::Copy(r, i),
        Row::Setting(id) => Target::Setting(id),
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // Base text colour for the whole screen; the background stays the
    // terminal's own.
    f.buffer_mut().set_style(area, Style::new().fg(theme::TEXT));
    let wide = area.width >= 100;
    let changes_height = if wide || app.pending.is_empty() {
        0
    } else {
        u16::try_from(app.pending.len()).unwrap_or(4).min(4) + 2
    };
    let [search, body, changes_bottom, status, buttons] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(changes_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let (list_area, changes_area) = if wide {
        let [l, c] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(36)]).areas(body);
        (l, Some(c))
    } else {
        (body, (changes_height > 0).then_some(changes_bottom))
    };

    // Search box.
    let border = if app.searching {
        theme::ACCENT
    } else {
        theme::BORDER
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            " Saltbox settings ",
            Style::new().fg(theme::ACCENT).bold(),
        ));
    let inner = block.inner(search);
    f.render_widget(block, search);
    let [label, field] =
        Layout::horizontal([Constraint::Length(9), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Line::from(" Search  ".bold()), label);
    // Only show the text cursor while the box has focus.
    app.search.set_cursor_style(if app.searching {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    });
    f.render_widget(&app.search, field);
    app.search_area = search;

    // Settings page.
    let rows = app.rows();
    if !app.fold_decided && list_area.height > 0 {
        // Open everything when it fits on screen, otherwise start folded to app names.
        app.fold_decided = true;
        if rows.len() > usize::from(list_area.height) {
            app.folded = (0..app.roles.len()).collect();
        }
    }
    let rows = app.rows();
    // Sized from every setting, not just visible ones, so values don't jump
    // sideways while searching.
    let key_width = app
        .roles
        .iter()
        .flat_map(|r| &r.instances)
        .flat_map(|i| &i.facts)
        .map(|f| f.key.chars().count())
        .max()
        .unwrap_or(10)
        .min(28);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| row_item(app, *row, key_width))
        .collect();
    let list = List::new(items).highlight_style(theme::SELECTED);
    let list_area = list_area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    if rows.is_empty() {
        let msg = if app.query.is_empty() {
            "No settings found".to_string()
        } else {
            format!(
                "No settings match \"{}\". Press Esc in the search box to clear it.",
                app.query
            )
        };
        f.render_widget(Line::from(msg).fg(theme::MUTED), list_area);
    }
    f.render_stateful_widget(list, list_area, &mut app.list);
    app.list_area = list_area;

    // Unsaved changes, always visible once there are any.
    let mut undo_targets = Vec::new();
    if let Some(ca) = changes_area {
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::BORDER))
            .title(Span::styled(
                format!(" Unsaved changes ({}) ", app.pending.len()),
                Style::new().fg(theme::ACCENT).bold(),
            ));
        let inner = block.inner(ca);
        let lines: Vec<Line> = if app.pending.is_empty() {
            vec![Line::from("Nothing changed yet").fg(theme::MUTED)]
        } else {
            app.pending
                .iter()
                .enumerate()
                .map(|(n, (t, c))| {
                    let (verb, colour) = match c {
                        Change::Set(_) => ("Edit  ", theme::WARN),
                        Change::Delete => ("Delete", theme::PROBLEM),
                    };
                    let undo = " undo ";
                    let label_width =
                        usize::from(inner.width).saturating_sub(verb.len() + undo.len() + 2);
                    let label: String = app.label(*t).chars().take(label_width).collect();
                    if let Ok(y) = u16::try_from(n)
                        && y < inner.height
                    {
                        let x = inner.right().saturating_sub(undo.len() as u16);
                        undo_targets.push((Rect::new(x, inner.y + y, undo.len() as u16, 1), *t));
                    }
                    let pad = usize::from(inner.width)
                        .saturating_sub(verb.len() + 1 + label.chars().count() + undo.len());
                    Line::from(vec![
                        Span::styled(verb, Style::default().fg(colour)),
                        Span::raw(format!(" {label}{}", " ".repeat(pad))),
                        Span::raw(undo).underlined().fg(theme::LINK),
                    ])
                })
                .collect()
        };
        f.render_widget(Paragraph::new(lines).block(block), ca);
    }
    app.undo_targets = undo_targets;

    f.render_widget(
        Line::from(format!(" {}", app.status)).fg(theme::MUTED),
        status,
    );

    // Buttons: every action is visible and clickable; the key is shown on it.
    let sel = app.selected();
    let on_setting = matches!(sel, Some(Row::Setting(_)));
    let specs = [
        ("Edit", "e", Action::Edit, on_setting),
        ("Copy", "c", Action::Copy, on_setting),
        (
            if matches!(sel, Some(Row::Setting(id)) if app.shown.contains(&id)) {
                "Hide"
            } else {
                "Show"
            },
            "s",
            Action::Show,
            on_setting,
        ),
        (
            if sel.is_some_and(|r| {
                app.pending
                    .get(&target_of(r))
                    .is_some_and(|c| matches!(c, Change::Delete))
            }) {
                "Keep"
            } else {
                "Delete"
            },
            "d",
            Action::Delete,
            sel.is_some(),
        ),
        ("Save changes", "v", Action::Save, !app.pending.is_empty()),
        ("Quit", "q", Action::Quit, true),
    ];
    app.buttons.clear();
    let mut x = buttons.x + 1;
    let mut spans = vec![Span::raw(" ")];
    for (label, key, action, enabled) in specs {
        let text = format!(" {label} ({key}) ");
        let width = u16::try_from(text.chars().count()).unwrap_or(0);
        let style = match (enabled, action) {
            (true, Action::Save) => Style::new().fg(theme::ON_ACCENT).bg(theme::OK).bold(),
            (true, _) => Style::new().fg(theme::TEXT).bg(theme::BUTTON_BG),
            (false, _) => Style::new().fg(theme::FAINT).bg(theme::BUTTON_DISABLED_BG),
        };
        if enabled && x + width <= buttons.right() {
            app.buttons
                .push((Rect::new(x, buttons.y, width, 1), action));
        }
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
        x += width + 1;
    }
    f.render_widget(Line::from(spans), buttons);

    // Popups.
    app.popup_buttons.clear();
    match &app.popup {
        Popup::Edit(textarea, _) => {
            // tui-popup centres, sizes and clears; the textarea keeps its own
            // titled border, so the popup draws none.
            // https://docs.rs/tui-popup/0.7.7/tui_popup/
            let width = usize::from(area.width) * 7 / 10;
            let mut popup =
                tui_popup::Popup::new(tui_popup::KnownSizeWrapper::new(&**textarea, width, 8));
            popup.borders = ratatui::widgets::Borders::NONE;
            f.render_widget(popup, area);
        }
        Popup::ConfirmSave => {
            let n = app.pending.len();
            let deletes = app
                .pending
                .values()
                .filter(|c| matches!(c, Change::Delete))
                .count();
            let mut text = vec![Line::from(format!(
                "Save {n} change(s) to the Saltbox settings?"
            ))];
            if deletes > 0 {
                text.push(Line::from(format!("{deletes} deleted value(s) will be created fresh the next time Saltbox runs.")).fg(theme::WARN));
            }
            confirm(f, app, " Save changes ", text, "Save (y)", "Cancel (n)");
        }
        Popup::ConfirmQuit => confirm(
            f,
            app,
            " Unsaved changes ",
            vec![Line::from(format!(
                "You have {} unsaved change(s). Quit and lose them?",
                app.pending.len()
            ))],
            "Quit (y)",
            "Keep editing (n)",
        ),
        // Context menu drawn here rather than with tui-popup: its PopupState::move_to
        // only works after a first render and the size is frozen afterwards
        // (tui-popup 0.7.7 popup_state.rs / popup.rs popup_area), which doesn't suit
        // a menu opened at the cursor with changing labels.
        Popup::Menu { at, sel } => {
            let items = app
                .selected()
                .map(|r| app.menu_items(r))
                .unwrap_or_default();
            let width = items
                .iter()
                .map(|(l, k, _)| l.chars().count() + k.len() + 5)
                .max()
                .unwrap_or(10) as u16;
            let height = u16::try_from(items.len()).unwrap_or(0) + 2;
            let x = at.x.min(area.right().saturating_sub(width));
            let y = at.y.min(area.bottom().saturating_sub(height));
            let menu = Rect::new(x, y, width.min(area.width), height.min(area.height));
            app.menu_area = menu;
            let inner_w = usize::from(menu.width.saturating_sub(2));
            let lines: Vec<Line> = items
                .iter()
                .enumerate()
                .map(|(i, (label, key, _))| {
                    let pad = inner_w.saturating_sub(label.chars().count() + key.len() + 2);
                    let text = format!(" {label}{}{key} ", " ".repeat(pad));
                    if i == *sel {
                        Line::from(text).style(theme::SELECTED)
                    } else {
                        Line::from(vec![
                            Span::raw(format!(" {label}{}", " ".repeat(pad))),
                            Span::raw(format!("{key} ")).fg(theme::MUTED),
                        ])
                    }
                })
                .collect();
            f.render_widget(Clear, menu);
            f.render_widget(
                Paragraph::new(lines).block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::new().fg(theme::BORDER)),
                ),
                menu,
            );
        }
        Popup::None => {}
    }
}

/// A centred confirmation dialog with two clickable buttons, drawn with
/// tui-popup (https://docs.rs/tui-popup/0.7.7/tui_popup/). A fresh
/// PopupState each frame keeps it centred and sized to its content; the
/// rendered area it reports is used to hit-test the buttons.
fn confirm(
    f: &mut Frame,
    app: &mut App,
    title: &str,
    mut text: Vec<Line<'static>>,
    yes: &str,
    no: &str,
) {
    let width = 70usize.min(usize::from(f.area().width.saturating_sub(2)));
    let width = width.max(1);
    // Count wrapped lines with ratatui itself (Paragraph::line_count, behind
    // the unstable-rendered-line-info feature, capability map decision 9), so
    // the buttons are never cut off. Dividing by the width undercounts when
    // whole words move to the next line.
    let text_lines = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(u16::try_from(width).unwrap_or(u16::MAX));
    let yes_text = format!(" {yes} ");
    let no_text = format!(" {no} ");
    text.push(Line::raw(""));
    text.push(Line::from(vec![
        Span::styled(
            yes_text.clone(),
            Style::new().fg(theme::ON_ACCENT).bg(theme::OK).bold(),
        ),
        Span::raw("  "),
        Span::styled(
            no_text.clone(),
            Style::new().fg(theme::TEXT).bg(theme::BUTTON_BG),
        ),
    ]));
    let body = Paragraph::new(text).wrap(Wrap { trim: false });
    let mut popup = tui_popup::Popup::new(tui_popup::KnownSizeWrapper::new(
        body,
        width,
        text_lines + 2,
    ));
    popup.title = Line::from(Span::styled(
        title.to_string(),
        Style::new().fg(theme::ACCENT).bold(),
    ));
    popup.border_set = ratatui::symbols::border::ROUNDED;
    popup.border_style = Style::new().fg(theme::BORDER);
    popup.style = Style::new().fg(theme::TEXT);
    let mut state = tui_popup::PopupState::default();
    f.render_stateful_widget(popup, f.area(), &mut state);
    if let Some(a) = *state.area() {
        let y = a.y + 1 + u16::try_from(text_lines + 1).unwrap_or(0);
        let yes_w = u16::try_from(yes_text.chars().count()).unwrap_or(0);
        let no_w = u16::try_from(no_text.chars().count()).unwrap_or(0);
        app.popup_buttons
            .push((Rect::new(a.x + 1, y, yes_w, 1), true));
        app.popup_buttons
            .push((Rect::new(a.x + 1 + yes_w + 2, y, no_w, 1), false));
    }
}

fn row_item(app: &App, row: Row, key_width: usize) -> ListItem<'static> {
    let deleted = app.deleted(row);
    let strike = if deleted {
        Modifier::CROSSED_OUT
    } else {
        Modifier::empty()
    };
    let tag = |t: Target| match app.pending.get(&t) {
        Some(Change::Delete) => Span::raw("  will be deleted").fg(theme::PROBLEM),
        Some(Change::Set(_)) => Span::raw("  changed").fg(theme::WARN),
        None => Span::raw(""),
    };
    match row {
        Row::App(r) => {
            let role = &app.roles[r];
            let folded = app.query.is_empty() && app.folded.contains(&r);
            let count: usize = role.instances.iter().map(|i| i.facts.len()).sum();
            let mut spans = vec![
                Span::raw(if folded { "▸ " } else { "▾ " }).fg(theme::FAINT),
                Span::styled(
                    title_case(&role.name),
                    Style::default().bold().add_modifier(strike),
                ),
            ];
            if folded {
                spans.push(Span::raw(format!("  {count} setting(s)")).fg(theme::MUTED));
            }
            spans.push(tag(Target::App(r)));
            ListItem::new(Line::from(spans))
        }
        Row::Copy(r, i) => ListItem::new(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                app.roles[r].instances[i].name.clone(),
                Style::default().fg(theme::ACCENT).add_modifier(strike),
            ),
            tag(Target::Copy(r, i)),
        ])),
        Row::Setting(id) => {
            // Deeper rows get a narrower key column so every value starts in
            // the same screen column.
            let (indent, key_width) = if app.has_copies(id.0) {
                ("      ", key_width)
            } else {
                ("    ", key_width + 2)
            };
            let key = app.roles[id.0].instances[id.1].facts[id.2].key.clone();
            let value = app.value(id);
            let mut shown = if app.masked(id) {
                "••••••••".to_string()
            } else {
                value.lines().next().unwrap_or("").to_string()
            };
            if !app.masked(id) && value.lines().count() > 1 {
                shown.push_str(" …");
            }
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(
                    format!("{key:<key_width$}  "),
                    Style::default().add_modifier(strike),
                ),
                Span::styled(
                    shown,
                    Style::default()
                        .fg(if app.masked(id) {
                            theme::MUTED
                        } else {
                            theme::TEXT
                        })
                        .add_modifier(strike),
                ),
                tag(Target::Setting(id)),
            ]))
        }
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn type_into_search(app: &mut App, text: &str) {
        app.searching = true;
        for c in text.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Regression: many terminals send Backspace as Ctrl+H, which the old
    /// hand-rolled search box typed as a literal "h".
    #[test]
    fn backspace_and_ctrl_h_delete_in_search() {
        let mut app = App::new(Vec::new());
        type_into_search(&mut app, "plexx");
        app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "plex");
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.query, "ple");
    }

    #[test]
    fn alt_and_ctrl_combos_do_not_type_letters() {
        let mut app = App::new(Vec::new());
        type_into_search(&mut app, "plex");
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT));
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "plex");
    }

    /// Renders the save dialog into ratatui's in-memory TestBackend (no
    /// terminal emulator involved) and checks title and buttons.
    #[test]
    fn save_dialog_renders_title_and_buttons() {
        let mut app = App::new(Vec::new());
        app.pending.insert(Target::App(0), Change::Delete);
        app.popup = Popup::ConfirmSave;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        app.roles = vec![Role {
            name: "demo".into(),
            path: "demo.ini".into(),
            instances: Vec::new(),
        }];
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows: Vec<String> = (0..20)
            .map(|y| {
                (0..100)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        let screen = rows.join("\n");
        assert!(screen.contains("Save changes"), "title missing:\n{screen}");
        assert!(
            screen.contains("Save (y)") && screen.contains("Cancel (n)"),
            "buttons missing:\n{screen}"
        );
        assert_eq!(app.popup_buttons.len(), 2);
    }

    fn fixture_app() -> App {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"));
        App::new(model::load(dir).expect("fixtures load"))
    }

    fn draw_app(
        app: &mut App,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                .expect("backend");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal
    }

    /// Truecolor is the design target (capability map, decision 10): every
    /// coloured cell on every screen state uses a palette RGB value, never a
    /// named ANSI colour.
    #[test]
    fn screens_draw_only_truecolor() {
        let check = |app: &mut App, what: &str| {
            for (width, height) in [(110, 30), (80, 30)] {
                let terminal = draw_app(app, width, height);
                for cell in terminal.backend().buffer().content() {
                    for color in [cell.fg, cell.bg] {
                        assert!(
                            matches!(color, Color::Rgb(..) | Color::Reset),
                            "{what} at {width}: {:?} drawn in {color:?}",
                            cell.symbol()
                        );
                    }
                }
            }
        };
        let mut app = fixture_app();
        check(&mut app, "page");
        // A pending edit and a pending delete fill the changes panel.
        app.pending.insert(Target::App(0), Change::Delete);
        app.pending
            .insert(Target::Setting((1, 0, 0)), Change::Set("changed".into()));
        check(&mut app, "page with changes");
        app.popup = Popup::Menu {
            at: Position::new(10, 5),
            sel: 0,
        };
        check(&mut app, "menu");
        app.popup = Popup::ConfirmSave;
        check(&mut app, "save dialog");
        app.popup = Popup::ConfirmQuit;
        check(&mut app, "quit dialog");
        app.start_edit((1, 0, 0));
        check(&mut app, "edit popup");
        app.popup = Popup::None;
        type_into_search(&mut app, "zzz-no-match");
        check(&mut app, "search with no match");
    }

    /// The selected row is one solid bar, not a background behind spans that
    /// keep their own colours (review of the log picker).
    #[test]
    fn selected_row_is_one_uniform_bar() {
        let mut app = fixture_app();
        // A masked setting with a pending change mixes several colours.
        let rows = app.rows();
        let (index, id) = rows
            .iter()
            .enumerate()
            .find_map(|(i, row)| match *row {
                Row::Setting(id) if app.masked(id) => Some((i, id)),
                _ => None,
            })
            .expect("a masked setting in the fixtures");
        app.pending
            .insert(Target::Setting(id), Change::Set("new-secret".into()));
        app.list.select(Some(index));
        let terminal = draw_app(&mut app, 110, 30);
        let buffer = terminal.backend().buffer();
        let at = app.list.selected().unwrap_or_default() - app.list.offset();
        let y = app.list_area.y + u16::try_from(at).unwrap_or(0);
        let cells: Vec<_> = (app.list_area.x..app.list_area.right())
            .map(|x| &buffer[(x, y)])
            .collect();
        let (fg, bg) = (cells[0].fg, cells[0].bg);
        assert_ne!(bg, Color::Reset, "selection needs a background");
        for c in &cells {
            assert_eq!((c.fg, c.bg), (fg, bg), "cell {:?} differs", c.symbol());
        }
    }

    /// Word wrap can need more lines than width / dialog width, so the
    /// dialog height must come from the real wrapped line count, or its
    /// buttons get cut off at some widths.
    #[test]
    fn save_dialog_buttons_stay_visible_at_every_width() {
        let mut app = fixture_app();
        app.pending.insert(Target::App(0), Change::Delete);
        app.pending
            .insert(Target::Setting((1, 0, 0)), Change::Set("changed".into()));
        app.popup = Popup::ConfirmSave;
        for width in 30..=100 {
            let terminal = draw_app(&mut app, width, 30);
            let buffer = terminal.backend().buffer();
            let screen: String = (0..30)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| buffer[(x, y)].symbol().to_string())
                .collect();
            assert!(
                screen.contains("Save (y)") && screen.contains("Cancel (n)"),
                "buttons cut off at width {width}"
            );
        }
    }

    /// A line built so the old estimate is wrong: 69 characters estimate to
    /// 2 lines in a 38-column dialog, but word wrap needs 3.
    #[test]
    fn dialog_height_uses_the_wrapped_line_count() {
        let mut app = fixture_app();
        let text = ["wordsmith"; 7].join(" ");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20)).expect("backend");
        terminal
            .draw(|f| {
                confirm(
                    f,
                    &mut app,
                    " Test ",
                    vec![Line::from(text.clone())],
                    "Yes (y)",
                    "No (n)",
                )
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let screen: String = (0..20)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(screen.contains("Yes (y)"), "buttons cut off");
    }

    /// Screen position of the first cell of `text` (ASCII labels only).
    fn find(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, text: &str) -> Position {
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            // Byte offset to cell column: border glyphs are multi-byte.
            if let Some(byte) = row.find(text) {
                let x = row[..byte].chars().count();
                return Position::new(u16::try_from(x).expect("x"), y);
            }
        }
        panic!("{text:?} not on screen");
    }

    fn click(app: &mut App, at: Position) -> Option<Action> {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: at.x,
            row: at.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Each bottom button is clickable across its whole label, and nowhere
    /// beside it.
    #[test]
    fn bottom_buttons_click_where_they_are_drawn() {
        let mut app = fixture_app();
        app.pending
            .insert(Target::Setting((1, 0, 0)), Change::Set("changed".into()));
        let rows = app.rows();
        let setting = rows
            .iter()
            .position(|r| matches!(r, Row::Setting(_)))
            .expect("setting");
        app.list.select(Some(setting));
        let terminal = draw_app(&mut app, 110, 30);
        for (label, action) in [
            (" Edit (e) ", Action::Edit),
            (" Copy (c) ", Action::Copy),
            (" Show (s) ", Action::Show),
            (" Delete (d) ", Action::Delete),
            (" Save changes (v) ", Action::Save),
            (" Quit (q) ", Action::Quit),
        ] {
            let at = find(&terminal, label);
            let last = Position::new(at.x + u16::try_from(label.len()).expect("len") - 1, at.y);
            for p in [at, last] {
                assert!(click(&mut app, p) == Some(action), "{label:?} at {p:?}");
            }
            let after = Position::new(last.x + 1, at.y);
            assert!(
                click(&mut app, after).is_none(),
                "gap after {label:?} is clickable"
            );
        }
    }

    #[test]
    fn undo_link_removes_its_change() {
        let mut app = fixture_app();
        app.pending
            .insert(Target::Setting((1, 0, 0)), Change::Set("changed".into()));
        let terminal = draw_app(&mut app, 110, 30);
        let at = find(&terminal, " undo ");
        click(&mut app, Position::new(at.x + 2, at.y));
        assert!(app.pending.is_empty());
    }

    #[test]
    fn dialog_buttons_click_where_they_are_drawn() {
        let mut app = fixture_app();
        app.pending.insert(Target::App(0), Change::Delete);
        app.popup = Popup::ConfirmSave;
        let terminal = draw_app(&mut app, 110, 30);
        let save = find(&terminal, " Save (y) ");
        let cancel = find(&terminal, " Cancel (n) ");
        assert!(click(&mut app, Position::new(save.x + 9, save.y)) == Some(Action::Save));
        assert!(click(&mut app, Position::new(cancel.x + 1, cancel.y)).is_none());
        assert!(matches!(app.popup, Popup::None), "cancel closes the dialog");
    }

    #[test]
    fn esc_clears_then_leaves_search() {
        let mut app = App::new(Vec::new());
        type_into_search(&mut app, "plex");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.query, "");
        assert!(app.searching);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.searching);
    }
}
