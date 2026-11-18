//! The fact editor's dialogs (tui spec, editors, `sb fact`, Screen and
//! Save): the edit and add boxes, the save and quit questions, the
//! "waiting for Saltbox" dialog, the drift dialog ("Before / On disk")
//! and the outcome. Each is a tui-popup `Popup`
//! (<https://docs.rs/tui-popup/0.7.7/tui_popup/>), drawn from a fresh
//! `PopupState` each frame so it stays centred and sized to its text, as
//! proto-facts does; the buttons go in the rows the popup reports
//! (`PopupState::area`). The drift dialog's text scrolls in a
//! tui-scrollview `ScrollView`
//! (<https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/struct.ScrollView.html>).

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect, Size};
use ratatui::style::Modifier;
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Clear, Paragraph, Wrap};
use ratatui_textarea::TextArea;
use sb_cli_core::theme::Role;
use sb_config::facts::Role as App;
use sb_config::facts::apply::RoleOutcome;
use sb_config::facts::session::{Drift, DriftChoice, LOCK_TIMEOUT};
use tui_popup::{KnownSizeWrapper, Popup as TuiPopup, PopupState};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use super::changes::{Pending, Target};
use super::{Page, REGENERATED, one_line};

/// The dialog up, if any.
pub(super) enum Popup {
    None,
    /// The edit box for `target`.
    Edit {
        line: Box<TextArea<'static>>,
        target: Target,
    },
    /// The name of a setting to add to `copy`.
    AddKey {
        line: Box<TextArea<'static>>,
        copy: Target,
    },
    /// The value of the setting `target` to add.
    AddValue {
        line: Box<TextArea<'static>>,
        target: Target,
    },
    ConfirmSave,
    /// Quit with staged changes: Save, Discard or Cancel.
    ConfirmQuit,
    /// The save has started; nothing to ask yet.
    Saving,
    /// Saltbox holds `role`'s lock; waited so long.
    Waiting {
        role: String,
        waited: Duration,
    },
    /// `role` changed on disk: Reload or Cancel, answered on `reply`.
    Drift {
        role: String,
        drift: Box<Drift>,
        reply: Sender<DriftChoice>,
        scroll: ScrollViewState,
    },
    /// The save's outcome, a line per role; `all_saved` when nothing is
    /// left staged.
    Outcome {
        lines: Vec<(String, Role)>,
        all_saved: bool,
    },
}

/// Each role's outcome as the outcome dialog says it, with its colour.
pub(super) fn outcome_lines(outcomes: &BTreeMap<String, RoleOutcome>) -> Vec<(String, Role)> {
    outcomes
        .iter()
        .map(|(role, outcome)| {
            let role = one_line(role);
            match outcome {
                RoleOutcome::Applied => (format!("{role}: saved"), Role::Ok),
                RoleOutcome::Unsynced(cause) => (
                    format!(
                        "{role}: saved, but the folder couldn't be synced, so it may not survive a crash ({})",
                        one_line(&cause.to_string())
                    ),
                    Role::Warn,
                ),
                RoleOutcome::Drifted { reloaded: true } => (
                    format!("{role}: changed on disk; reloaded, its changes dropped"),
                    Role::Warn,
                ),
                RoleOutcome::Drifted { reloaded: false } => (
                    format!("{role}: changed on disk; nothing written, its changes kept"),
                    Role::Warn,
                ),
                RoleOutcome::Failed(cause) => (
                    format!("{role}: failed: {}", one_line(&cause.to_string())),
                    Role::Problem,
                ),
                RoleOutcome::Unattempted => (
                    format!("{role}: not saved (the save stopped first); its changes kept"),
                    Role::Muted,
                ),
            }
        })
        .collect()
}

/// The dialog's width: 70 columns, or the screen's less two.
fn width(area: Rect) -> usize {
    usize::from(area.width.saturating_sub(2)).clamp(1, 70)
}

/// A dialog's text box, its title and keys on its own border: tui-popup
/// centres, sizes and clears; the popup draws no border of its own.
fn text_box(frame: &mut Frame<'_>, area: Rect, line: &TextArea<'static>) {
    let mut popup = TuiPopup::new(KnownSizeWrapper::new(line, width(area), 8));
    popup.borders = Borders::NONE;
    frame.render_widget(popup, area);
}

/// A centred question with its buttons, each a label in its colour:
/// tui-popup sized to the text (its wrapped rows counted by ratatui's
/// `Paragraph::line_count`, the `unstable-rendered-line-info` feature),
/// the buttons drawn in the row after a blank one (proto-facts' `confirm`).
/// Each button's rectangle comes back with the key it stands for, the
/// click target.
fn confirm(
    page: &Page,
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    mut text: Vec<Line<'static>>,
    buttons: &[(&str, bool, KeyCode)],
) -> Vec<(Rect, KeyCode)> {
    let width = width(area);
    let rows = Paragraph::new(text.clone())
        .wrap(Wrap { trim: false })
        .line_count(u16::try_from(width).unwrap_or(u16::MAX));
    text.push(Line::raw(""));
    text.push(Line::raw(""));
    let body = Paragraph::new(text).wrap(Wrap { trim: false });
    let mut popup = TuiPopup::new(KnownSizeWrapper::new(body, width, rows + 2));
    popup.title = Line::from(Span::styled(
        title.to_owned(),
        page.style(Role::Accent).add_modifier(Modifier::BOLD),
    ));
    popup.border_set = border::ROUNDED;
    popup.border_style = page.style(Role::Border);
    popup.style = page.style(Role::Text);
    let mut state = PopupState::default();
    frame.render_stateful_widget(popup, area, &mut state);
    let Some(row) = state
        .area()
        .map(|a| a.inner(Margin::new(1, 1)))
        .and_then(|inner| inner.rows().nth(rows + 1))
    else {
        return Vec::new();
    };
    let spans: Vec<Span<'static>> = buttons
        .iter()
        .map(|(label, main, _)| {
            let style = if *main {
                page.style(Role::OnAccent)
                    .patch(page.bg(Role::Ok))
                    .add_modifier(Modifier::BOLD)
            } else {
                page.style(Role::Text).patch(page.bg(Role::ButtonBg))
            };
            Span::styled(format!(" {label} "), style)
        })
        .collect();
    let areas = Layout::horizontal(
        spans
            .iter()
            .map(|span| Constraint::Length(u16::try_from(span.width()).unwrap_or(u16::MAX))),
    )
    .spacing(2)
    .split(row);
    let mut targets = Vec::new();
    for ((span, area), &(_, _, code)) in spans.into_iter().zip(areas.iter()).zip(buttons) {
        frame.render_widget(span, *area);
        targets.push((*area, code));
    }
    targets
}

/// `role`'s facts as the drift dialog lists them, values as they are.
fn facts(page: &Page, role: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for instance in &role.instances {
        lines.push(Line::styled(
            format!("  [{}]", one_line(&instance.name)),
            page.style(Role::Accent),
        ));
        for fact in &instance.facts {
            let shown = one_line(&fact.value);
            let value = if fact.value.contains('\n') {
                format!("{shown} …")
            } else {
                shown
            };
            lines.push(Line::from(format!("    {} = {value}", one_line(&fact.key))));
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled("  (no settings)", page.style(Role::Muted)));
    }
    lines
}

/// The drift dialog: the role as it was read and as it is on disk now,
/// in a scroll view, then Reload and Cancel.
fn drift(
    page: &Page,
    frame: &mut Frame<'_>,
    area: Rect,
    role: &str,
    drift: &Drift,
    scroll: &mut ScrollViewState,
) -> Vec<(Rect, KeyCode)> {
    let bold = page.style(Role::Text).add_modifier(Modifier::BOLD);
    let mut text = vec![
        Line::from(format!(
            "{} changed on disk since sb fact read it. Reload drops its changes here for you to make again; Cancel keeps them and writes nothing for it.",
            one_line(role)
        )),
        Line::raw(""),
        Line::styled("Before", bold),
    ];
    text.extend(facts(page, &drift.before));
    text.push(Line::raw(""));
    text.push(Line::styled("On disk", bold));
    match &drift.after {
        Some(after) => text.extend(facts(page, after)),
        None => text.push(Line::styled(
            "  (the file is gone)",
            page.style(Role::Muted),
        )),
    }
    let width = width(area);
    let height = usize::from(area.height.saturating_sub(2)).clamp(3, 20);
    let mut popup = TuiPopup::new(KnownSizeWrapper::new(Clear, width, height));
    popup.title = Line::from(Span::styled(
        " Changed on disk ",
        page.style(Role::Warn).add_modifier(Modifier::BOLD),
    ));
    popup.border_set = border::ROUNDED;
    popup.border_style = page.style(Role::Border);
    popup.style = page.style(Role::Text);
    let mut state = PopupState::default();
    frame.render_stateful_widget(popup, area, &mut state);
    let Some(inner) = state.area().map(|a| a.inner(Margin::new(1, 1))) else {
        return Vec::new();
    };
    let [view, _, buttons] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    // A screen too small for the text: tui-scrollview's scrollbars need
    // an area to draw in (its own comment on zero-sized views).
    if view.height == 0 || view.width < 2 {
        return Vec::new();
    }
    let body = Paragraph::new(text).wrap(Wrap { trim: false });
    let lines = u16::try_from(body.line_count(view.width.saturating_sub(1))).unwrap_or(u16::MAX);
    let mut scroll_view = ScrollView::new(Size::new(view.width.saturating_sub(1), lines))
        .horizontal_scrollbar_visibility(ScrollbarVisibility::Never);
    scroll_view.render_widget(body, Rect::new(0, 0, view.width.saturating_sub(1), lines));
    frame.render_stateful_widget(scroll_view, view, scroll);
    let reload = Span::styled(
        " Reload (r) ",
        page.style(Role::Text).patch(page.bg(Role::ButtonBg)),
    );
    let cancel = Span::styled(
        " Cancel (c) ",
        page.style(Role::Text).patch(page.bg(Role::ButtonBg)),
    );
    let [reload_area, cancel_area] = buttons.layout(
        &Layout::horizontal([
            Constraint::Length(u16::try_from(reload.width()).unwrap_or(u16::MAX)),
            Constraint::Length(u16::try_from(cancel.width()).unwrap_or(u16::MAX)),
        ])
        .spacing(2),
    );
    frame.render_widget(reload, reload_area);
    frame.render_widget(cancel, cancel_area);
    vec![
        (reload_area, KeyCode::Char('r')),
        (cancel_area, KeyCode::Char('c')),
    ]
}

/// The dialog up, over the page.
pub(super) fn draw(page: &mut Page, frame: &mut Frame<'_>, area: Rect) {
    let mut popup = std::mem::replace(&mut page.popup, Popup::None);
    // The buttons drawn are the dialog's click targets (M56).
    page.hits.dialog = match &mut popup {
        Popup::None => Vec::new(),
        Popup::Edit { line, .. } | Popup::AddKey { line, .. } | Popup::AddValue { line, .. } => {
            text_box(frame, area, line);
            Vec::new()
        }
        Popup::Drift {
            role,
            drift: changed,
            scroll,
            ..
        } => drift(page, frame, area, role, changed, scroll),
        question => draw_question(page, frame, area, question),
    };
    page.popup = popup;
}

/// The dialogs that ask or tell, with buttons and no box: save, quit,
/// saving, waiting and the outcome.
fn draw_question(
    page: &Page,
    frame: &mut Frame<'_>,
    area: Rect,
    popup: &Popup,
) -> Vec<(Rect, KeyCode)> {
    let plural = |count: usize| if count == 1 { "change" } else { "changes" };
    match popup {
        Popup::ConfirmSave => {
            let count = page.pending.len();
            let deletes = page
                .pending
                .values()
                .filter(|p| **p == Pending::Delete)
                .count();
            let mut text = vec![Line::from(format!(
                "Save {count} {} to the Saltbox settings?",
                plural(count)
            ))];
            if deletes > 0 {
                text.push(Line::styled(
                    format!("Deleted values: {REGENERATED}"),
                    page.style(Role::Warn),
                ));
            }
            let buttons = [
                ("Save (y)", true, KeyCode::Char('y')),
                ("Cancel (n)", false, KeyCode::Char('n')),
            ];
            confirm(page, frame, area, " Save changes ", text, &buttons)
        }
        Popup::ConfirmQuit => {
            let count = page.pending.len();
            let text = vec![Line::from(format!(
                "You have {count} unsaved {}. Save them before you quit?",
                plural(count)
            ))];
            let buttons = [
                ("Save (y)", true, KeyCode::Char('y')),
                ("Discard (d)", false, KeyCode::Char('d')),
                ("Cancel (n)", false, KeyCode::Char('n')),
            ];
            confirm(page, frame, area, " Unsaved changes ", text, &buttons)
        }
        Popup::Saving => {
            let text = vec![Line::from("Saving the changes…")];
            confirm(page, frame, area, " Saving ", text, &[])
        }
        Popup::Waiting { role, waited } => {
            let text = vec![Line::from(format!(
                "Saltbox is changing {}'s settings. Waiting for it to finish: {} s of {} s.",
                one_line(role),
                waited.as_secs(),
                LOCK_TIMEOUT.as_secs()
            ))];
            let buttons = [("Cancel (c)", false, KeyCode::Char('c'))];
            confirm(page, frame, area, " Waiting for Saltbox ", text, &buttons)
        }
        Popup::Outcome { lines, all_saved } => {
            // As many lines as the screen holds with the borders, a blank
            // row and the button; the rest counted on the last.
            let room = usize::from(area.height.saturating_sub(6)).max(1);
            let mut text: Vec<Line<'static>> = lines
                .iter()
                .map(|(line, role)| Line::styled(line.clone(), page.style(*role)))
                .collect();
            if text.len() > room {
                let more = text.len() - (room - 1);
                text.truncate(room - 1);
                text.push(Line::styled(
                    format!("… and {more} more"),
                    page.style(Role::Muted),
                ));
            }
            let title = if *all_saved {
                " Saved "
            } else {
                " Not everything was saved "
            };
            confirm(
                page,
                frame,
                area,
                title,
                text,
                &[("OK (Enter)", true, KeyCode::Enter)],
            )
        }
        Popup::None
        | Popup::Edit { .. }
        | Popup::AddKey { .. }
        | Popup::AddValue { .. }
        | Popup::Drift { .. } => Vec::new(),
    }
}
