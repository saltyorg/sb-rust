//! The picker (tui spec, Testing Strategy): the Services section's rows
//! sorted by name with every status's words, the selection skipping the
//! headings and stopping at both ends, Enter choosing, `q` and Esc
//! quitting with no choice, the selected row one solid bar; the
//! Containers section with sb-docker's picker status, its filter, the
//! Docker-unreachable row and its live reloads; and `TestBackend`
//! snapshots at 120×40 and 80×24.

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
        Some(Row::Source(i)) => Some(picker.entries[i].name.clone()),
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
    assert_eq!(
        p.take_choice(),
        Some(Choice::Service("saltbox_managed_backup".into()))
    );
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

// --- the Containers section (M69) ---

/// sb-docker's fixtures, sorted by name, with times: authelia up 2h,
/// radarr restarted 3× and up 4m, sonarr exited 2h 5m ago.
fn containers() -> Vec<ContainerRow> {
    let mut rows = sb_docker::testing::fixture_rows();
    for row in &mut rows {
        match row.name.as_str() {
            "authelia" => row.started_at = Some(ago(2 * 3600)),
            "radarr" => {
                row.started_at = Some(ago(4 * 60));
                row.restart_count = 3;
            }
            "sonarr" => row.finished_at = Some(ago(2 * 3600 + 5 * 60)),
            _ => {}
        }
        row.created = Some(ago(86_400));
    }
    rows
}

fn both() -> Picker {
    picker(&services()[..2]).with_containers(&containers(), now())
}

fn names(p: &Picker) -> Vec<String> {
    p.rows
        .iter()
        .filter_map(|row| match row {
            Row::Source(i) => Some(p.entries[*i].name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn containers_follow_the_services_with_their_picker_status() {
    let mut p = both();
    let rows = screen(&drawn(&mut p, 80, 14));
    assert_eq!(
        rows[0],
        " ◆ Logs   2 services · 6 containers · 3 need attention"
    );
    assert!(rows[2].starts_with("  Services (2) ───"), "{rows:?}");
    assert_eq!(rows[3], "› docker               ✓ running");
    assert_eq!(rows[4], "  saltbox_managed_web  ✓ running · 2d 3h");
    assert!(rows[5].starts_with("  Containers (6) ───"), "{rows:?}");
    assert_eq!(
        rows[6..12],
        [
            "  0123456789ab         ✗ created (failed to start)",
            "  authelia             ✓ running · up 2h",
            "  lidarr               ◌ unknown",
            "  plex                 ✓ running (healthy)",
            "  radarr               ↻ running · restarted 3× · up 4m",
            "  sonarr               ✗ exited (code 1) · 2h 5m ago",
        ]
    );
}

#[test]
fn the_containers_filter_shows_only_containers() {
    let mut p = both().with_filter(Filter::Containers);
    let rows = screen(&drawn(&mut p, 80, 12));
    assert_eq!(rows[0], " ◆ Logs   6 containers · 3 need attention");
    assert!(rows[2].starts_with("  Containers (6) ───"), "{rows:?}");
    assert_eq!(selected(&p).as_deref(), Some("0123456789ab"));
    assert_eq!(names(&p).len(), 6);
}

#[test]
fn enter_on_a_container_chooses_it_by_id_and_creation() {
    let mut p = both();
    press(&mut p, KeyCode::End);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Quit);
    let sonarr = containers().pop().expect("sonarr");
    assert_eq!(
        p.take_choice(),
        Some(Choice::Container(Container {
            id: sonarr.id,
            name: "sonarr".into(),
            created: Some(ago(86_400)),
        }))
    );
}

/// Docker unreachable: the section's one row, which the selection
/// skips; no container count in the title.
#[test]
fn an_unreachable_docker_is_the_sections_one_row() {
    let mut p = picker(&services()[1..2]).with_unreachable("couldn't reach\u{1b}[31m Docker\nat x");
    let rows = screen(&drawn(&mut p, 80, 8));
    assert_eq!(rows[0], " ◆ Logs   1 service · all healthy");
    assert!(rows[4].starts_with("  Containers ───"), "{rows:?}");
    assert_eq!(rows[5], "  Docker unreachable: couldn't reach Docker");
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    // With no services, nothing to choose.
    let mut p = picker(&[]).with_unreachable("down");
    assert_eq!(p.state.selected(), None);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Unchanged);
    let rows = screen(&drawn(&mut p, 60, 6));
    assert_eq!(rows[3], "Docker unreachable: down");
}

/// A new list replaces the section; the selection stays on its
/// container, or on the row now at its place when it's gone; the
/// trouble shows in the footer.
#[tokio::test]
async fn the_section_reloads_keeping_the_selection() {
    let (tx, rx) = watch::channel(Snapshot::default());
    let mut p = both().with_updates(rx);
    assert_eq!(p.wake(), Flow::Unchanged, "nothing new");
    press(&mut p, KeyCode::End);
    press(&mut p, KeyCode::Up);
    assert_eq!(selected(&p).as_deref(), Some("radarr"));
    let mut rows = containers();
    rows.retain(|r| r.name != "plex");
    tx.send_replace(Snapshot {
        rows: Some(rows.clone()),
        trouble: None,
    });
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(selected(&p).as_deref(), Some("radarr"));
    assert!(!names(&p).contains(&"plex".to_owned()));
    // radarr goes: the row at its place is sonarr's.
    rows.retain(|r| r.name != "radarr");
    tx.send_replace(Snapshot {
        rows: Some(rows.clone()),
        trouble: Some("Docker events stopped; retrying".into()),
    });
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(selected(&p).as_deref(), Some("sonarr"));
    let screen_rows = screen(&drawn(&mut p, 80, 12));
    assert_eq!(
        screen_rows[11],
        "↑↓ move · Enter open · q quit   Docker events stopped; retrying"
    );
    // The last one goes: the row before.
    rows.retain(|r| r.name != "sonarr");
    tx.send_replace(Snapshot {
        rows: Some(rows),
        trouble: None,
    });
    p.wake();
    assert_eq!(selected(&p).as_deref(), Some("lidarr"));
    // Trouble alone keeps the list.
    tx.send_replace(Snapshot {
        rows: None,
        trouble: Some("couldn't refresh containers: x".into()),
    });
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(names(&p).len(), 5, "{:?}", names(&p));
    // The selection on a service stays there.
    press(&mut p, KeyCode::Home);
    tx.send_replace(Snapshot {
        rows: Some(Vec::new()),
        trouble: None,
    });
    p.wake();
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    assert_eq!(names(&p), ["docker", "saltbox_managed_web"]);
    // A list where an unreachable row was replaces it.
    let (tx, rx) = watch::channel(Snapshot::default());
    let mut p = picker(&[]).with_unreachable("down").with_updates(rx);
    tx.send_replace(Snapshot {
        rows: Some(containers()),
        trouble: None,
    });
    p.wake();
    assert_eq!(selected(&p).as_deref(), Some("0123456789ab"));
}

/// A choice made before a reload isn't taken after it (its index would
/// name another source).
#[tokio::test]
async fn a_reload_drops_a_choice_not_taken() {
    let (tx, rx) = watch::channel(Snapshot::default());
    let mut p = both().with_updates(rx);
    press(&mut p, KeyCode::Enter);
    tx.send_replace(Snapshot {
        rows: Some(containers()),
        trouble: None,
    });
    p.wake();
    assert_eq!(p.take_choice(), None);
}

#[test]
fn snapshot_services_and_containers_120x40() {
    let mut p = both();
    assert_snapshot!(drawn(&mut p, 120, 40).backend());
}

#[test]
fn snapshot_containers_80x24() {
    let mut p = both().with_filter(Filter::Containers);
    press(&mut p, KeyCode::Down);
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_docker_unreachable_80x24() {
    let mut p = picker(&services()).with_unreachable(
        "couldn't get Docker's version at unix:///var/run/docker.sock: connection refused",
    );
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}
