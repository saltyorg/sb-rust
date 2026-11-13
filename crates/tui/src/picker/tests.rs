//! The picker's Services section (tui spec, Testing Strategy): rows
//! sorted by name with every status's words, the selection skipping the
//! heading and stopping at both ends, Enter choosing, `q` and Esc
//! quitting with no choice, the selected row one solid bar, and
//! `TestBackend` snapshots at 120×40 and 80×24.

use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sb_cli_core::theme::{Palette, Profile};
use sb_host::systemd::Timer;

use super::*;

/// 2026-11-13 10:00:00 UTC.
fn now() -> Timestamp {
    "2026-11-13T10:00:00Z".parse().expect("a time")
}

fn ago(secs: i64) -> Timestamp {
    now() - jiff::SignedDuration::from_secs(secs)
}

fn service(name: &str, active: &str, sub: &str) -> Service {
    Service {
        name: name.into(),
        active: active.into(),
        sub: sub.into(),
        unit_file_state: Some("enabled".into()),
        active_enter: None,
        state_change: None,
        timer: None,
    }
}

/// One service in each state of the status table, out of order.
fn services() -> Vec<Service> {
    let mut running = service("saltbox_managed_web", "active", "running");
    running.active_enter = Some(ago(2 * 86_400 + 3 * 3600));
    let mut failed = service("saltbox_managed_backup", "failed", "failed");
    failed.state_change = Some(ago(2 * 3600 + 5 * 60));
    let mut scheduled = service("saltbox_managed_cleanup", "inactive", "dead");
    scheduled.timer = Some(Timer {
        active: "active".into(),
        sub: "waiting".into(),
        next: Some(now() + jiff::SignedDuration::from_secs(3 * 3600)),
    });
    let mut leftover = service("saltbox_managed_old", "inactive", "dead");
    leftover.unit_file_state = Some("disabled".into());
    vec![
        running,
        service("docker", "active", "running"),
        failed,
        scheduled,
        leftover,
        service("saltbox_managed_mount", "active", "exited"),
        service("saltbox_managed_retry", "activating", "auto-restart"),
        service("saltbox_managed_stopped", "inactive", "dead"),
    ]
}

fn picker(services: &[Service]) -> Picker {
    Picker::new(
        services,
        now(),
        Theme::resolve(&Palette::default(), Profile::Ascii),
    )
}

fn press(picker: &mut Picker, code: KeyCode) -> Flow {
    picker.input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn drawn(picker: &mut Picker, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|f| picker.draw(f)).expect("drawn");
    terminal
}

/// The screen's rows, trailing spaces trimmed.
fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn selected(picker: &Picker) -> Option<String> {
    match picker.state.selected().map(|i| picker.rows[i]) {
        Some(Row::Service(i)) => Some(picker.entries[i].name.clone()),
        _ => None,
    }
}

#[test]
fn services_are_sorted_by_name_under_their_heading_with_their_status() {
    let mut p = picker(&services());
    let rows = screen(&drawn(&mut p, 80, 14));
    assert_eq!(rows[0], " ◆ Logs   8 services · 3 need attention");
    assert_eq!(rows[1], "");
    assert!(rows[2].starts_with("  Services (8) ───"), "{rows:?}");
    assert!(rows[2].ends_with('─'));
    assert_eq!(
        rows[3..11],
        [
            "› docker                   ✓ running",
            "  saltbox_managed_backup   ✗ failed 2h 5m ago",
            "  saltbox_managed_cleanup  ✓ scheduled · next run in 3h",
            "  saltbox_managed_mount    ✓ finished",
            "  saltbox_managed_old      ! disabled · not cleaned up",
            "  saltbox_managed_retry    ↻ restarting",
            "  saltbox_managed_stopped  ○ stopped",
            "  saltbox_managed_web      ✓ running · 2d 3h",
        ]
    );
    assert_eq!(rows[13], "↑↓ move · Enter open · q quit");
}

#[test]
fn a_healthy_list_says_so_and_one_service_is_singular() {
    let mut p = picker(&[service("docker", "active", "running")]);
    let rows = screen(&drawn(&mut p, 60, 6));
    assert_eq!(rows[0], " ◆ Logs   1 service · all healthy");
    let mut failed = service("docker", "failed", "failed");
    failed.state_change = None;
    let mut p = picker(&[failed]);
    assert_eq!(
        screen(&drawn(&mut p, 60, 6))[0],
        " ◆ Logs   1 service · 1 needs attention"
    );
}

#[test]
fn the_selection_skips_the_heading_and_stops_at_both_ends() {
    let mut p = picker(&services());
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    assert_eq!(press(&mut p, KeyCode::Up), Flow::Redraw);
    assert_eq!(selected(&p).as_deref(), Some("docker"), "the heading");
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
    press(&mut p, KeyCode::Char('k'));
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    press(&mut p, KeyCode::Down);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    press(&mut p, KeyCode::Home);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
}

#[test]
fn page_keys_move_by_the_lists_height() {
    let mut p = picker(&services());
    // A list of 3 rows: the title, a blank row and the footer take 3.
    drawn(&mut p, 60, 6);
    press(&mut p, KeyCode::PageDown);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_mount"));
    press(&mut p, KeyCode::PageDown);
    press(&mut p, KeyCode::PageDown);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    press(&mut p, KeyCode::PageUp);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_old"));
    // The window follows the selection.
    let rows = screen(&drawn(&mut p, 60, 6));
    assert!(
        rows.iter().any(|r| r.starts_with("› saltbox_managed_old")),
        "{rows:?}"
    );
}

#[test]
fn enter_chooses_the_selected_service_once() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Down);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Quit);
    assert_eq!(p.take_choice().as_deref(), Some("saltbox_managed_backup"));
    assert_eq!(p.take_choice(), None, "taken");
    // The selection stays for the picker's next run.
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
}

#[test]
fn q_and_esc_quit_with_no_choice() {
    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        let mut p = picker(&services());
        assert_eq!(press(&mut p, code), Flow::Quit);
        assert_eq!(p.take_choice(), None);
    }
}

#[test]
fn other_keys_and_events_change_nothing() {
    let mut p = picker(&services());
    assert_eq!(press(&mut p, KeyCode::Char('x')), Flow::Unchanged);
    let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert_eq!(p.input(Input::Key(ctrl_j)), Flow::Unchanged);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    assert_eq!(p.input(Input::FocusGained), Flow::Unchanged);
}

#[test]
fn an_empty_list_has_no_selection_and_enter_does_nothing() {
    let mut p = picker(&[]);
    assert_eq!(p.state.selected(), None);
    press(&mut p, KeyCode::Down);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Unchanged);
    assert_eq!(p.take_choice(), None);
}

/// A name with control characters is one line of plain text.
#[test]
fn names_reach_the_screen_through_display_text() {
    let mut p = picker(&[service("bad\u{1b}]0;x\u{7}\nname", "active", "exited")]);
    let rows = screen(&drawn(&mut p, 60, 6));
    assert!(!rows.concat().contains('\u{1b}'), "{rows:?}");
    assert!(rows[3].starts_with("› bad"), "{rows:?}");
    assert!(rows[3].ends_with("✓ finished"), "{rows:?}");
}

/// The selected row's every cell has the selection's background, the
/// mark's included (proto-logs' test).
#[test]
fn the_selected_row_is_one_solid_bar() {
    let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    let mut p = Picker::new(&services(), now(), theme.clone());
    let terminal = drawn(&mut p, 80, 14);
    let buffer = terminal.backend().buffer();
    let want = Style::from(anstyle::Style::new().bg_color(theme.color(Role::SelectedBg))).bg;
    assert!(want.is_some());
    for x in 0..80 {
        assert_eq!(buffer[(x, 3)].bg, want.expect("a colour"), "column {x}");
    }
    // Rows that aren't selected have none.
    assert_ne!(buffer[(0, 4)].bg, want.expect("a colour"));
}

#[test]
fn snapshot_services_120x40() {
    let mut p = picker(&services());
    assert_snapshot!(drawn(&mut p, 120, 40).backend());
}

#[test]
fn snapshot_services_80x24() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Down);
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}
