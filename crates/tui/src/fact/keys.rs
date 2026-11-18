//! The fact editor's dialogs' keys (tui spec, editors, `sb fact`,
//! Screen): the edit box (Enter stages, Alt+Enter adds a line, Esc
//! cancels), the add boxes (the name, then the value), the save and quit
//! questions, the waiting, drift and outcome dialogs. The boxes are
//! ratatui-textarea's
//! (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html>),
//! their text shown as it is, a secret's too (tui spec, editors, Values).

use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType};
use ratatui_textarea::{CursorMove, TextArea};
use sb_cli_core::theme::Role;
use sb_config::facts::session::{Drift, DriftChoice};
use tui_scrollview::ScrollViewState;

use super::changes::{self, Pending, Target};
use super::dialog::Popup;
use super::{Page, Row, names, newline, one_line};
use crate::term::Flow;

impl Page {
    /// A paste (bracketed paste): into the box up, or the search box, whole
    /// (ratatui-textarea's `insert_str`): a value box keeps its lines, a
    /// one-line box (a name, the search) gets them as spaces, and a last
    /// line end goes; a newline in it never stages anything. Anywhere
    /// else it does nothing, so its letters aren't keys.
    pub(super) fn paste(&mut self, text: &str) -> Flow {
        if self.menu.is_some() {
            return Flow::Unchanged;
        }
        match &mut self.popup {
            Popup::Edit { line, .. } | Popup::AddValue { line, .. } => {
                line.insert_str(crate::paste::lines(text));
            }
            Popup::AddKey { line, .. } => {
                line.insert_str(crate::paste::one_line(text));
            }
            Popup::None if self.searching => {
                self.search.insert_str(crate::paste::one_line(text));
                self.searched();
            }
            _ => return Flow::Unchanged,
        }
        Flow::Redraw
    }

    /// `e`: the edit box for the selected setting.
    pub(super) fn start_edit(&mut self) {
        let Some(id) = self.selected_setting() else {
            "Pick a setting to edit".clone_into(&mut self.status);
            return;
        };
        let target = self.target(Row::Setting(id));
        if self.deleted(&target) {
            "Keep it first: it is to be deleted".clone_into(&mut self.status);
            return;
        }
        let setting = self.setting(id);
        // `split`, not `lines`: the value's lines exactly, so an edit that
        // changes nothing gives the value back.
        let mut line = TextArea::new(setting.value.split('\n').map(str::to_owned).collect());
        line.move_cursor(CursorMove::Bottom);
        line.move_cursor(CursorMove::End);
        let title = format!(" Edit {} ", one_line(&setting.key));
        let keys = " Enter keep · Alt+Enter new line · Esc cancel ";
        self.popup = Popup::Edit {
            line: Box::new(self.dialog_line(line, title, keys)),
            target,
        };
    }

    /// `a`: the name box for a setting added to the selected copy: the
    /// copy's row's or a setting's, or an app's only copy.
    pub(super) fn start_add(&mut self) {
        let copy = match self.selected() {
            Some(Row::Copy(r, i) | Row::Setting((r, i, _))) => Some((r, i)),
            Some(Row::App(r)) if self.roles[r].instances.len() == 1 => Some((r, 0)),
            _ => None,
        };
        let Some((r, i)) = copy else {
            "Pick a copy to add a setting to".clone_into(&mut self.status);
            return;
        };
        let target = Target::Copy(
            self.roles[r].name.clone(),
            self.roles[r].instances[i].name.clone(),
        );
        if self.deleted(&target) {
            "Keep it first: it is to be deleted".clone_into(&mut self.status);
            return;
        }
        let title = format!(" Add a setting to {} ", self.label(&target));
        let line = self.dialog_line(TextArea::default(), title, " Enter next · Esc cancel ");
        self.popup = Popup::AddKey {
            line: Box::new(line),
            copy: target,
        };
    }

    /// A dialog's text box in the page's colours under `title`, its keys
    /// on the bottom border (proto-facts' edit box).
    fn dialog_line(
        &self,
        mut line: TextArea<'static>,
        title: String,
        keys: &str,
    ) -> TextArea<'static> {
        line.set_block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(self.style(Role::Accent))
                .title(Span::styled(
                    title,
                    self.style(Role::Accent).add_modifier(Modifier::BOLD),
                ))
                .title_bottom(Span::styled(keys.to_owned(), self.style(Role::Muted))),
        );
        line.set_style(self.style(Role::Text));
        line.set_cursor_line_style(Style::default());
        line.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        line
    }

    /// The edit box's Enter: stage the value, or drop the change when it
    /// is the file's again; whether the box is done.
    fn finish_edit(&mut self, target: &Target, value: &str) -> bool {
        if self.original(target).as_deref() == Some(value) {
            if self.pending.remove(target).is_some() {
                let (role, copy, key) = names(target);
                tracing::info!(role = %role, copy = %copy, key = %key, "change undone");
            }
            "No change".clone_into(&mut self.status);
            return true;
        }
        if self.stage(target, &Pending::Set(value.to_owned())) {
            self.status = format!("{} changed (not saved yet)", self.label(target));
            return true;
        }
        false
    }

    /// The file's value of setting `target`, if it is in the catalog.
    fn original(&self, target: &Target) -> Option<String> {
        let Target::Setting(role, copy, key) = target else {
            return None;
        };
        self.roles
            .iter()
            .find(|r| r.name == *role)?
            .instances
            .iter()
            .find(|i| i.name == *copy)?
            .facts
            .iter()
            .find(|f| f.key == *key)
            .map(|f| f.value.clone())
    }

    /// The name box's Enter: a name not in the copy yet, that the file can
    /// take (sb-config's rules, checked with an empty value), gives the
    /// value box.
    fn finish_key(&mut self, copy: &Target, key: &str) -> Option<Popup> {
        let Target::Copy(role, instance) = copy else {
            return None;
        };
        let target = Target::Setting(role.clone(), instance.clone(), key.to_owned());
        if key.is_empty() {
            "Type the new setting's name".clone_into(&mut self.status);
            return None;
        }
        if self.exists(&target) || self.pending.contains_key(&target) {
            self.status = format!("{} is already there", one_line(key));
            return None;
        }
        let mut staged = self.pending.clone();
        staged.insert(target.clone(), Pending::Set(String::new()));
        if let Some(session) = &self.session
            && let Err(refused) = session.check(&changes::to_changes(&staged))
        {
            self.status = format!("Can't add that name: {refused}");
            return None;
        }
        self.status.clear();
        let title = format!(" Value for {} ", self.label(&target));
        let keys = " Enter add · Alt+Enter new line · Esc cancel ";
        let line = self.dialog_line(TextArea::default(), title, keys);
        Some(Popup::AddValue {
            line: Box::new(line),
            target,
        })
    }

    /// A key while a dialog is up: the dialog's.
    pub(super) fn popup_key(&mut self, key: KeyEvent) -> Flow {
        let popup = std::mem::replace(&mut self.popup, Popup::None);
        let (popup, flow) = match popup {
            Popup::None => (Popup::None, Flow::Unchanged),
            Popup::Edit { line, target } => self.edit_key(key, line, target),
            Popup::AddKey { line, copy } => self.add_key_key(key, line, copy),
            Popup::AddValue { line, target } => self.add_value_key(key, line, target),
            Popup::ConfirmSave => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => return self.start_save(),
                KeyCode::Esc | KeyCode::Char('n') => (Popup::None, Flow::Redraw),
                _ => (Popup::ConfirmSave, Flow::Unchanged),
            },
            Popup::ConfirmQuit => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    self.quit_after_save = true;
                    return self.start_save();
                }
                KeyCode::Char('d') => return Flow::Quit,
                KeyCode::Esc | KeyCode::Char('n') => (Popup::None, Flow::Redraw),
                _ => (Popup::ConfirmQuit, Flow::Unchanged),
            },
            Popup::Saving => (Popup::Saving, Flow::Unchanged),
            Popup::Waiting { role, waited } => {
                if key.code == KeyCode::Esc
                    && let Some(running) = &self.saving
                {
                    running.cancel_wait();
                    "Cancelling the save…".clone_into(&mut self.status);
                }
                (Popup::Waiting { role, waited }, Flow::Redraw)
            }
            Popup::Drift {
                role,
                drift,
                reply,
                scroll,
            } => Self::drift_key(key, role, drift, reply, scroll),
            Popup::Outcome { lines, all_saved } => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => {
                    if self.quit_after_save && all_saved {
                        return Flow::Quit;
                    }
                    self.quit_after_save = false;
                    (Popup::None, Flow::Redraw)
                }
                _ => (Popup::Outcome { lines, all_saved }, Flow::Unchanged),
            },
        };
        // A handler that set a dialog of its own (a refusal keeps the box)
        // keeps it.
        if matches!(self.popup, Popup::None) {
            self.popup = popup;
        }
        flow
    }

    /// A key in the edit box: Enter stages, Alt+Enter adds a line, Esc
    /// cancels; the rest is the box's (ratatui-textarea's `input`).
    fn edit_key(
        &mut self,
        key: KeyEvent,
        mut line: Box<TextArea<'static>>,
        target: Target,
    ) -> (Popup, Flow) {
        match key.code {
            KeyCode::Esc => {
                "Edit cancelled".clone_into(&mut self.status);
                return (Popup::None, Flow::Redraw);
            }
            _ if newline(&key) && !key.modifiers.contains(KeyModifiers::ALT) => {
                let value = line.lines().join("\n");
                if self.finish_edit(&target, &value) {
                    return (Popup::None, Flow::Redraw);
                }
            }
            _ => {
                line.input(key);
            }
        }
        (Popup::Edit { line, target }, Flow::Redraw)
    }

    /// A key in the name box: Enter goes on to the value box, Esc cancels.
    fn add_key_key(
        &mut self,
        key: KeyEvent,
        mut line: Box<TextArea<'static>>,
        copy: Target,
    ) -> (Popup, Flow) {
        match key.code {
            KeyCode::Esc => return (Popup::None, Flow::Redraw),
            _ if newline(&key) => {
                let name = line.lines().join("");
                if let Some(value_box) = self.finish_key(&copy, &name) {
                    return (value_box, Flow::Redraw);
                }
            }
            _ => {
                line.input(key);
            }
        }
        (Popup::AddKey { line, copy }, Flow::Redraw)
    }

    /// A key in the value box: Enter stages the new setting, Alt+Enter adds
    /// a line, Esc cancels.
    fn add_value_key(
        &mut self,
        key: KeyEvent,
        mut line: Box<TextArea<'static>>,
        target: Target,
    ) -> (Popup, Flow) {
        match key.code {
            KeyCode::Esc => return (Popup::None, Flow::Redraw),
            _ if newline(&key) && !key.modifiers.contains(KeyModifiers::ALT) => {
                let value = line.lines().join("\n");
                if self.stage(&target, &Pending::Set(value)) {
                    self.status = format!("{} added (not saved yet)", self.label(&target));
                    return (Popup::None, Flow::Redraw);
                }
            }
            _ => {
                line.input(key);
            }
        }
        (Popup::AddValue { line, target }, Flow::Redraw)
    }

    /// A key in the drift dialog: `r` reloads, Esc cancels (the answer goes
    /// to the save; `c` copies on the page, so it doesn't cancel here), the
    /// arrows and page keys scroll.
    fn drift_key(
        key: KeyEvent,
        role: String,
        drift: Box<Drift>,
        reply: Sender<DriftChoice>,
        mut scroll: ScrollViewState,
    ) -> (Popup, Flow) {
        let answer = match key.code {
            KeyCode::Char('r') => Some(DriftChoice::Reload),
            KeyCode::Esc => Some(DriftChoice::Cancel),
            KeyCode::Down | KeyCode::Char('j') => {
                scroll.scroll_down();
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                scroll.scroll_up();
                None
            }
            KeyCode::PageDown => {
                scroll.scroll_page_down();
                None
            }
            KeyCode::PageUp => {
                scroll.scroll_page_up();
                None
            }
            _ => {
                return (
                    Popup::Drift {
                        role,
                        drift,
                        reply,
                        scroll,
                    },
                    Flow::Unchanged,
                );
            }
        };
        match answer {
            Some(answer) => {
                // The save may be gone (stopped): nothing waits then.
                let _ = reply.send(answer);
                (Popup::Saving, Flow::Redraw)
            }
            None => (
                Popup::Drift {
                    role,
                    drift,
                    reply,
                    scroll,
                },
                Flow::Redraw,
            ),
        }
    }
}
