//! The picker (tui spec, Testing Strategy): the Services section's rows
//! sorted by name with every status's words, the selection skipping the
//! headings and stopping at both ends, Enter choosing, `q` and Esc
//! quitting with no choice, the selected row one solid bar; the
//! Containers section with sb-docker's picker status, its filter, the
//! Docker-unreachable row and its live reloads; Needs attention, the kind
//! and name filters, the mouse and the services' refresh; and
//! `TestBackend` snapshots at 120×40 and 80×24.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
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
        Some(Row::Source(_, i)) => Some(picker.entries[i].name.clone()),
        _ => None,
    }
}

#[test]
fn services_are_sorted_by_name_under_their_heading_with_their_status() {
    let mut p = picker(&services());
    let rows = screen(&drawn(&mut p, 80, 18));
    assert_eq!(rows[0], " ◆ Logs   8 services · 3 need attention");
    assert_eq!(rows[1], "");
    assert!(rows[2].starts_with("  Needs attention (3) ───"), "{rows:?}");
    assert!(rows[6].starts_with("  Services (8) ───"), "{rows:?}");
    assert!(rows[6].ends_with('─'));
    assert_eq!(
        rows[7..15],
        [
            "  docker                   ✓ running",
            "  saltbox_managed_backup   ✗ failed 2h 5m ago",
            "  saltbox_managed_cleanup  ✓ scheduled · next run in 3h",
            "  saltbox_managed_mount    ✓ finished",
            "  saltbox_managed_old      ! disabled · not cleaned up",
            "  saltbox_managed_retry    ↻ restarting",
            "  saltbox_managed_stopped  ○ stopped",
            "  saltbox_managed_web      ✓ running · 2d 3h",
        ]
    );
    assert_eq!(
        rows[17],
        "Enter open · / filter · Tab kind · ↑↓ move                       ? keys · q quit"
    );
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
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
    assert_eq!(press(&mut p, KeyCode::Up), Flow::Redraw);
    assert_eq!(
        selected(&p).as_deref(),
        Some("saltbox_managed_backup"),
        "the heading"
    );
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_old"));
    press(&mut p, KeyCode::Char('j'));
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(selected(&p).as_deref(), Some("docker"), "past a heading");
    press(&mut p, KeyCode::Char('k'));
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_retry"));
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    press(&mut p, KeyCode::Down);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    press(&mut p, KeyCode::Home);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
}

#[test]
fn page_keys_move_by_the_lists_height() {
    let mut p = picker(&services());
    // A list of 3 rows: the title, a blank row and the footer take 3.
    drawn(&mut p, 60, 6);
    press(&mut p, KeyCode::PageDown);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    press(&mut p, KeyCode::PageDown);
    press(&mut p, KeyCode::PageDown);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_stopped"));
    press(&mut p, KeyCode::PageUp);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_mount"));
    // The window follows the selection.
    let rows = screen(&drawn(&mut p, 60, 6));
    assert!(
        rows.iter()
            .any(|r| r.starts_with("› saltbox_managed_mount")),
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
        Some(Choice::Service("saltbox_managed_old".into()))
    );
    assert_eq!(p.take_choice(), None, "taken");
    // The selection stays for the picker's next run.
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_old"));
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
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
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

/// `p` with `Tab` pressed twice: the containers only, the first row
/// selected (Home), as a user reaches them.
fn containers_only(mut p: Picker) -> Picker {
    press(&mut p, KeyCode::Tab);
    press(&mut p, KeyCode::Tab);
    assert_eq!(p.filter, Filter::Containers);
    press(&mut p, KeyCode::Home);
    p
}

fn names(p: &Picker) -> Vec<String> {
    p.rows
        .iter()
        .filter_map(|row| match row {
            Row::Source(_, i) => Some(p.entries[*i].name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn containers_follow_the_services_with_their_picker_status() {
    let mut p = both();
    let rows = screen(&drawn(&mut p, 80, 18));
    assert_eq!(
        rows[0],
        " ◆ Logs   2 services · 6 containers · 3 need attention"
    );
    assert!(rows[6].starts_with("  Services (2) ───"), "{rows:?}");
    assert_eq!(rows[7], "  docker               ✓ running");
    assert!(
        rows[3].starts_with("› 0123456789ab"),
        "the first row: {rows:?}"
    );
    assert_eq!(rows[8], "  saltbox_managed_web  ✓ running · 2d 3h");
    assert!(rows[9].starts_with("  Containers (6) ───"), "{rows:?}");
    assert_eq!(
        rows[10..16],
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
    let mut p = containers_only(both());
    let rows = screen(&drawn(&mut p, 80, 16));
    assert_eq!(rows[0], " ◆ Logs   6 containers · 3 need attention");
    assert!(rows[2].starts_with("  Needs attention (3) ───"), "{rows:?}");
    assert!(rows[6].starts_with("  Containers (6) ───"), "{rows:?}");
    assert_eq!(selected(&p).as_deref(), Some("0123456789ab"));
    assert_eq!(
        names(&p).len(),
        9,
        "3 under Needs attention, 6 under their kind"
    );
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
    // At 80 columns the trouble comes first, whole; the hints that fit
    // after it, the tail (tui spec, "Hints" mock-up).
    let screen_rows = screen(&drawn(&mut p, 80, 12));
    assert_eq!(
        screen_rows[11],
        "Docker events stopped; retrying   Enter open · / filter          ? keys · q quit",
        "{screen_rows:?}"
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
    assert_eq!(names(&p).len(), 6, "{:?}", names(&p));
    // The selection on a service stays there.
    press(&mut p, KeyCode::Home);
    press(&mut p, KeyCode::Down);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
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
    let mut p = containers_only(both());
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

// --- the unified picker (M70) ---

fn key_with(picker: &mut Picker, code: KeyCode, modifiers: KeyModifiers) -> Flow {
    picker.input(Input::Key(KeyEvent::new(code, modifiers)))
}

fn type_text(picker: &mut Picker, text: &str) {
    for c in text.chars() {
        press(picker, KeyCode::Char(c));
    }
}

fn click(picker: &mut Picker, kind: MouseEventKind, column: u16, row: u16) -> Flow {
    picker.input(Input::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

/// Needs attention heads the list, sorted by name across the kinds, each
/// row ending with its kind's word; its sources are under their kind too.
#[test]
fn needs_attention_heads_the_list_with_each_sources_kind() {
    let mut p = picker(&services()).with_containers(&containers(), now());
    let rows = screen(&drawn(&mut p, 100, 30));
    assert!(rows[2].starts_with("  Needs attention (6) ───"), "{rows:?}");
    assert_eq!(
        rows[3..9],
        [
            "› 0123456789ab             ✗ created (failed to start)       container",
            "  radarr                   ↻ running · restarted 3× · up 4m  container",
            "  saltbox_managed_backup   ✗ failed 2h 5m ago                service",
            "  saltbox_managed_old      ! disabled · not cleaned up       service",
            "  saltbox_managed_retry    ↻ restarting                      service",
            "  sonarr                   ✗ exited (code 1) · 2h 5m ago     container",
        ]
    );
    assert!(rows[9].starts_with("  Services (8) ───"), "{rows:?}");
    assert!(rows[18].starts_with("  Containers (6) ───"), "{rows:?}");
    assert!(rows[24].starts_with("  sonarr "), "{rows:?}");
    // The picker as built starts on its first row.
    assert_eq!(selected(&p).as_deref(), Some("0123456789ab"));
}

/// No cap: every source with attention is listed.
#[test]
fn needs_attention_has_no_cap() {
    let failed: Vec<Service> = (0..40)
        .map(|i| service(&format!("saltbox_managed_f{i:02}"), "failed", "failed"))
        .collect();
    let p = picker(&failed);
    let attention = p
        .rows
        .iter()
        .filter(|row| matches!(row, Row::Source(Section::Attention, _)))
        .count();
    assert_eq!(attention, 40);
}

/// The 10-minute rule: a container restarted 9 minutes ago stays under
/// Needs attention, one restarted 11 minutes ago leaves it, and a
/// recovered source leaves at once (tui spec, success criterion 3).
#[test]
fn a_restart_keeps_attention_for_ten_minutes_and_a_recovery_leaves_at_once() {
    let mut rows = containers();
    rows.retain(|r| r.name == "radarr" || r.name == "authelia");
    for row in &mut rows {
        row.restart_count = 1;
        row.started_at = Some(if row.name == "radarr" {
            ago(9 * 60)
        } else {
            ago(11 * 60)
        });
    }
    let p = picker(&[]).with_containers(&rows, now());
    let attention: Vec<String> = p
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Source(Section::Attention, i) => Some(p.entries[*i].name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(attention, ["radarr"]);
    // Two minutes on, radarr's restart is 11 minutes old: it leaves.
    let mut p = p;
    p.restate(now() + jiff::SignedDuration::from_secs(120));
    assert!(
        !p.rows
            .iter()
            .any(|row| matches!(row, Row::Source(Section::Attention, _))),
        "{:?}",
        p.rows
    );
    assert_eq!(
        selected(&p).as_deref(),
        Some("radarr"),
        "kept, under its kind"
    );
}

/// `Tab` cycles all, services, containers and back; the title, the
/// sections and the footer's hint follow, the selection staying on its
/// source while that is shown.
#[test]
fn tab_cycles_the_kind_filter() {
    let mut p = both();
    press(&mut p, KeyCode::End);
    press(&mut p, KeyCode::Up);
    assert_eq!(selected(&p).as_deref(), Some("radarr"));
    assert_eq!(press(&mut p, KeyCode::Tab), Flow::Redraw);
    let rows = screen(&drawn(&mut p, 80, 16));
    assert_eq!(rows[0], " ◆ Logs   2 services · all healthy");
    assert!(rows[2].starts_with("  Services (2) ───"), "{rows:?}");

    assert_eq!(
        selected(&p).as_deref(),
        Some("saltbox_managed_web"),
        "its place"
    );
    press(&mut p, KeyCode::Tab);
    let rows = screen(&drawn(&mut p, 80, 16));
    assert_eq!(rows[0], " ◆ Logs   6 containers · 3 need attention");

    press(&mut p, KeyCode::Down);
    let at = selected(&p);
    press(&mut p, KeyCode::Tab);
    assert_eq!(p.filter, Filter::All);
    assert_eq!(selected(&p), at, "still shown: kept");
    assert_eq!(names(&p).len(), 11);
}

/// `/` opens the filter line; the list narrows as it is typed, ignoring
/// case; Enter keeps the filter, which the title then shows, and the
/// footer its Esc; Esc clears it, and a second Esc quits.
#[test]
fn the_name_filter_narrows_as_it_is_typed() {
    let mut p = both();
    assert_eq!(press(&mut p, KeyCode::Char('/')), Flow::Redraw);
    type_text(&mut p, "RR");
    // Upper case: exact.
    assert_eq!(names(&p), Vec::<String>::new());
    let rows = screen(&drawn(&mut p, 80, 12));
    assert_eq!(rows[2], "Nothing matches \"RR\". Esc clears the filter.");
    assert!(rows[11].starts_with(" Filter: RR"), "{rows:?}");
    assert!(rows[11].ends_with("Enter keeps · Esc clears"), "{rows:?}");
    assert_eq!(
        rows[0],
        " ◆ Logs   2 services · 6 containers · 3 need attention"
    );
    press(&mut p, KeyCode::Backspace);
    press(&mut p, KeyCode::Backspace);
    type_text(&mut p, "rr");
    assert_eq!(
        names(&p),
        ["radarr", "sonarr", "lidarr", "radarr", "sonarr"]
    );
    let rows = screen(&drawn(&mut p, 80, 12));
    assert!(rows[2].starts_with("  Needs attention (2) ───"), "{rows:?}");
    assert!(rows[5].starts_with("  Containers (3) ───"), "{rows:?}");
    assert_eq!(selected(&p).as_deref(), Some("radarr"));
    // The arrows still move while typing; `j` is text.
    press(&mut p, KeyCode::Down);
    assert_eq!(selected(&p).as_deref(), Some("sonarr"));
    assert_eq!(
        press(&mut p, KeyCode::Enter),
        Flow::Redraw,
        "kept, not opened"
    );
    assert_eq!(p.take_choice(), None);
    let rows = screen(&drawn(&mut p, 100, 12));
    assert!(
        rows[0].ends_with("need attention   filter \"rr\""),
        "{rows:?}"
    );
    assert_eq!(
        rows[11],
        "Enter open · Esc clear · / filter · Tab kind · ↑↓ move                               ? keys · q quit"
    );
    // Filtering again starts from the filter in effect.
    press(&mut p, KeyCode::Char('/'));
    press(&mut p, KeyCode::Backspace);
    type_text(&mut p, "ad");
    assert_eq!(p.query, "rad");
    assert_eq!(names(&p), ["radarr", "radarr"]);
    press(&mut p, KeyCode::Enter);
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Redraw, "clears");
    assert_eq!(names(&p).len(), 11);
    assert_eq!(selected(&p).as_deref(), Some("radarr"), "kept");
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Quit);
}

/// In the line: `j` and `k` are text, Esc clears the filter and closes
/// the line, Tab cycles the kinds, Ctrl-M ends the line as Enter does,
/// keys the line ignores change nothing.
#[test]
fn the_filter_lines_own_keys() {
    let mut p = both();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "j");
    assert_eq!(names(&p), Vec::<String>::new());
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Redraw);
    assert!(p.editing.is_none());
    assert_eq!(names(&p).len(), 11);
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "ock");
    press(&mut p, KeyCode::Tab);
    assert_eq!(p.filter, Filter::Services);
    assert_eq!(names(&p), ["docker"]);
    assert_eq!(
        key_with(&mut p, KeyCode::Char('m'), KeyModifiers::CONTROL),
        Flow::Redraw
    );
    assert!(p.editing.is_none());
    assert_eq!(p.query, "ock");
    press(&mut p, KeyCode::Char('/'));
    assert_eq!(press(&mut p, KeyCode::F(5)), Flow::Redraw);
    assert_eq!(p.query, "ock");
    // Ctrl and Alt keys outside the line still change nothing.
    press(&mut p, KeyCode::Enter);
    assert_eq!(
        key_with(&mut p, KeyCode::Char('/'), KeyModifiers::ALT),
        Flow::Unchanged
    );
    assert!(p.editing.is_none());
}

/// The kind filters' empty lists say so.
#[test]
fn an_empty_kind_says_so() {
    let mut p = picker(&[]).with_containers(&containers(), now());
    press(&mut p, KeyCode::Tab);
    let rows = screen(&drawn(&mut p, 80, 6));
    assert_eq!(rows[2], "No matching systemd services found.");
    let mut p = picker(&services()[..1]).with_containers(&[], now());
    press(&mut p, KeyCode::Tab);
    press(&mut p, KeyCode::Tab);
    let rows = screen(&drawn(&mut p, 80, 6));
    assert_eq!(rows[2], "No containers found.");
    let mut p = picker(&services()[..1]);
    p.query = "zz".into();
    p.lay_out();
    p.query.clear();
    let rows = screen(&drawn(&mut p, 80, 6));
    assert_eq!(
        rows[2],
        "No matching systemd services or Docker containers found."
    );
}

/// The wheel moves the selection; a click selects a source's row, a
/// click on the selected row opens it; headings and rows past the list
/// ignore clicks; the rows count from the list's offset.
#[test]
fn the_mouse_moves_selects_and_opens() {
    let mut p = picker(&services());
    drawn(&mut p, 80, 18);
    assert_eq!(
        click(&mut p, MouseEventKind::ScrollDown, 5, 5),
        Flow::Redraw
    );
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_old"));
    click(&mut p, MouseEventKind::ScrollUp, 5, 5);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_backup"));
    // Row 7 of the screen is docker's (the list starts on row 2).
    let down = MouseEventKind::Down(MouseButton::Left);
    assert_eq!(click(&mut p, down, 10, 7), Flow::Redraw);
    assert_eq!(selected(&p).as_deref(), Some("docker"));
    assert_eq!(click(&mut p, down, 10, 6), Flow::Unchanged, "a heading");
    assert_eq!(
        click(&mut p, down, 10, 16),
        Flow::Unchanged,
        "past the rows"
    );
    assert_eq!(click(&mut p, down, 10, 0), Flow::Unchanged, "the title");
    assert_eq!(click(&mut p, MouseEventKind::Moved, 10, 7), Flow::Unchanged);
    assert_eq!(click(&mut p, down, 10, 7), Flow::Quit);
    assert_eq!(p.take_choice(), Some(Choice::Service("docker".into())));
    // Scrolled: a list of 3 rows showing the end.
    let mut p = picker(&services());
    drawn(&mut p, 60, 6);
    press(&mut p, KeyCode::End);
    drawn(&mut p, 60, 6);
    assert_eq!(click(&mut p, down, 10, 2), Flow::Redraw);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_retry"));
}

/// A click that opens while the filter line is open closes the line,
/// keeping the filter.
#[test]
fn a_click_that_opens_closes_the_filter_line() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "web");
    drawn(&mut p, 80, 18);
    let down = MouseEventKind::Down(MouseButton::Left);
    assert_eq!(click(&mut p, down, 10, 3), Flow::Quit);
    assert!(p.editing.is_none());
    assert_eq!(p.query, "web");
}

/// A stand-in service list for the refresh: each read takes the next of
/// `results`.
fn lister(
    results: Vec<Result<Vec<Service>, String>>,
) -> (ListServices, Arc<std::sync::Mutex<usize>>) {
    let reads = Arc::new(std::sync::Mutex::new(0));
    let count = Arc::clone(&reads);
    let results = Arc::new(std::sync::Mutex::new(results.into_iter()));
    let list: ListServices = Box::new(move || {
        *count.lock().expect("count") += 1;
        let next = results.lock().expect("results").next();
        Box::pin(async move { next.unwrap_or_else(|| Err("no more".into())) })
    });
    (list, reads)
}

/// Every 5 s the picker's tick reads the services again; a new list
/// replaces the section keeping the selection, a failed read keeps the
/// last list and says so in the footer until a read succeeds.
#[tokio::test(start_paused = true)]
async fn the_services_are_read_again_every_five_seconds() {
    let mut web = services()[0].clone();
    web.active = "failed".into();
    web.sub = "failed".into();
    let (list, reads) = lister(vec![
        Ok(vec![services()[1].clone(), web]),
        Err("systemctl timed out".into()),
        Ok(services()[..1].to_vec()),
    ]);
    let waker = Arc::new(Notify::new());
    let mut p = picker(&services()[..2]).with_refresh(list, Arc::clone(&waker));
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    assert_eq!(p.tick(), Flow::Unchanged, "not yet");
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(p.tick(), Flow::Redraw);
    assert_eq!(p.tick(), Flow::Unchanged, "one read at a time");
    waker.notified().await;
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(*reads.lock().expect("count"), 1);
    // web failed: it heads the list now, and the selection stays on it
    // under Services.
    assert_eq!(
        names(&p),
        ["saltbox_managed_web", "docker", "saltbox_managed_web"]
    );
    assert_eq!(p.state.selected(), Some(4));
    assert_eq!(p.wake(), Flow::Unchanged, "nothing new");
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    waker.notified().await;
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(names(&p).len(), 3, "the last list kept");
    let rows = screen(&drawn(&mut p, 80, 10));
    assert!(
        rows[9].starts_with("couldn't refresh services: systemctl timed out   Enter open"),
        "{rows:?}"
    );
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    waker.notified().await;
    p.wake();
    assert_eq!(names(&p), ["saltbox_managed_web"]);
    let rows = screen(&drawn(&mut p, 80, 10));
    assert!(rows[9].starts_with("Enter open"), "{rows:?}");
}

/// The tick works the statuses out again without a refresh (a picker
/// built without one): times move on. The rows were listed as at
/// 2026-11-13, so the tick's now (the clock's) moves every time.
#[tokio::test(start_paused = true)]
async fn the_tick_restates_the_times() {
    let mut p = picker(&[]).with_containers(&containers(), now());
    let before = screen(&drawn(&mut p, 80, 18));
    assert!(before.iter().any(|r| r.contains("· up 2h")), "{before:?}");
    assert_eq!(p.tick(), Flow::Unchanged, "not yet");
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(p.tick(), Flow::Redraw);
    let after = screen(&drawn(&mut p, 80, 18));
    assert!(!after.iter().any(|r| r.contains("· up 2h")), "{after:?}");
}

/// A read whose task ended without a result is dropped, and the next
/// tick starts another.
#[tokio::test(start_paused = true)]
async fn a_read_that_ended_without_a_result_is_tried_again() {
    let list: ListServices = Box::new(|| Box::pin(async { panic!("a panicking read") }));
    let waker = Arc::new(Notify::new());
    let mut p = picker(&services()[..1]).with_refresh(list, Arc::clone(&waker));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    tokio::task::yield_now().await;
    assert_eq!(p.wake(), Flow::Unchanged);
    assert!(p.refresh.as_ref().is_some_and(|r| r.pending.is_none()));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    assert!(
        p.refresh.as_ref().is_some_and(|r| r.pending.is_some()),
        "read again"
    );
}

/// Under the containers filter (`Tab` twice) the services aren't shown,
/// so they aren't read again until `Tab` shows them.
#[tokio::test(start_paused = true)]
async fn hidden_services_are_not_read_again() {
    let (list, reads) = lister(vec![Ok(Vec::new()), Ok(Vec::new())]);
    let mut p = containers_only(picker(&[]).with_refresh(list, Arc::new(Notify::new())));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    assert_eq!(*reads.lock().expect("count"), 0);
    press(&mut p, KeyCode::Tab);
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    assert_eq!(*reads.lock().expect("count"), 1);
}

/// A name filter hides a section's notice row too, so a filter that
/// matches nothing says so.
#[test]
fn a_filter_hides_the_notice_rows() {
    let mut p = picker(&[]).with_unreachable("down");
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "x");
    let rows = screen(&drawn(&mut p, 80, 6));
    assert_eq!(rows[2], "Nothing matches \"x\". Esc clears the filter.");
}

#[test]
fn snapshot_needs_attention_120x40() {
    let mut p = picker(&services()).with_containers(&containers(), now());
    assert_snapshot!(drawn(&mut p, 120, 40).backend());
}

#[test]
fn snapshot_filter_line_80x24() {
    let mut p = picker(&services()).with_containers(&containers(), now());
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "ar");
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_filtered_services_80x24() {
    let mut p = picker(&services()).with_containers(&containers(), now());
    press(&mut p, KeyCode::Tab);
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "managed_");
    press(&mut p, KeyCode::Enter);
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

/// A service and a container of one name, both needing attention: the
/// service first, in Needs attention as in the sections.
#[test]
fn a_service_and_a_container_of_one_name_list_the_service_first() {
    let mut rows = containers();
    rows.retain(|r| r.name == "sonarr");
    let p = picker(&[service("sonarr", "failed", "failed")]).with_containers(&rows, now());
    let kinds: Vec<Kind> = p
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Source(Section::Attention, i) => Some(p.entries[*i].kind),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, [Kind::Service, Kind::Container]);
}

/// The picker's Debug says whether a read is under way, never the
/// lister.
#[tokio::test(start_paused = true)]
async fn the_refresh_shows_in_debug_as_its_read_under_way() {
    let (list, _) = lister(vec![Ok(Vec::new())]);
    let mut p = picker(&[]).with_refresh(list, Arc::new(Notify::new()));
    assert!(format!("{p:?}").contains("Refresh { pending: false, .. }"));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    assert!(format!("{p:?}").contains("Refresh { pending: true, .. }"));
}

/// Dropping the picker stops a read under way (its task is aborted, so
/// a hung systemctl doesn't outlive the screen).
#[tokio::test(start_paused = true)]
async fn dropping_the_picker_stops_a_read_under_way() {
    struct Dropped(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&dropped);
    let list: ListServices = Box::new(move || {
        let guard = Dropped(Arc::clone(&flag));
        Box::pin(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
            Ok(Vec::new())
        })
    });
    let mut p = picker(&[]).with_refresh(list, Arc::new(Notify::new()));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    tokio::task::yield_now().await;
    assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
    drop(p);
    tokio::task::yield_now().await;
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}

/// A paste into the filter line is one input, its newlines spaces, a
/// last newline dropped: it narrows the list and leaves the line open.
/// Outside the line, a paste does nothing (its letters aren't keys).
#[test]
fn a_paste_goes_into_the_filter_line_whole() {
    let mut p = both();
    assert_eq!(p.input(Input::Paste("q".into())), Flow::Unchanged);
    press(&mut p, KeyCode::Char('/'));
    assert_eq!(p.input(Input::Paste("rad\n".into())), Flow::Redraw);
    assert_eq!(p.query, "rad");
    assert!(p.editing.is_some());
    assert_eq!(names(&p), ["radarr", "radarr"]);
}

/// Every service gone while the last one is selected, with Docker
/// unreachable: the row at its place is the Containers section's notice,
/// and nothing above it is a source, so nothing is selected and Enter
/// chooses nothing.
#[tokio::test(start_paused = true)]
async fn nothing_is_selected_when_no_source_is_left() {
    let (list, _) = lister(vec![Ok(Vec::new())]);
    let waker = Arc::new(Notify::new());
    let mut p = picker(&services()[..1])
        .with_unreachable("down")
        .with_refresh(list, Arc::clone(&waker));
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p).as_deref(), Some("saltbox_managed_web"));
    tokio::time::advance(Duration::from_secs(5)).await;
    p.tick();
    waker.notified().await;
    assert_eq!(p.wake(), Flow::Redraw);
    assert_eq!(names(&p), Vec::<String>::new());
    // What is left: the Containers section's notice, unindented as no row
    // is selected; the empty Services section is left out.
    let rows = screen(&drawn(&mut p, 80, 8));
    assert_eq!(rows[0], " ◆ Logs   0 services · all healthy");
    assert!(rows[2].starts_with("Containers ───"), "{rows:?}");
    assert_eq!(rows[3], "Docker unreachable: down");
    assert!(!rows.iter().any(|row| row.contains("Services")), "{rows:?}");
    assert_eq!(p.state.selected(), None);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Unchanged);
    assert_eq!(p.take_choice(), None);
}

/// The key map's keys (tui spec, "Keys: one map for every screen"): `g`
/// and `G` the first and last source, `b` and Space a page, Shift-Tab the
/// kind backwards.
#[test]
fn g_and_capital_g_the_ends_b_and_space_a_page_and_shift_tab_back() {
    let mut p = picker(&services());
    drawn(&mut p, 80, 8);
    press(&mut p, KeyCode::Char('G'));
    let last = selected(&p);
    press(&mut p, KeyCode::Home);
    let first = selected(&p);
    press(&mut p, KeyCode::End);
    assert_eq!(selected(&p), last);
    press(&mut p, KeyCode::Char('g'));
    assert_eq!(selected(&p), first);
    press(&mut p, KeyCode::Char(' '));
    let paged = selected(&p);
    assert_ne!(paged, first);
    press(&mut p, KeyCode::PageUp);
    let up = selected(&p);
    press(&mut p, KeyCode::Char(' '));
    press(&mut p, KeyCode::Char('b'));
    assert_eq!(selected(&p), up);
    assert_eq!(p.filter, Filter::All);
    let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(p.input(Input::Key(back)), Flow::Redraw);
    assert_eq!(p.filter, Filter::Containers);
    p.input(Input::Key(back));
    assert_eq!(p.filter, Filter::Services);
    press(&mut p, KeyCode::Tab);
    assert_eq!(p.filter, Filter::Containers);
}

/// `?` (or F1) opens the keys overlay; any other key closes it and does
/// nothing else, so `q` doesn't quit; in the filter line `?` is typed and
/// F1 still opens it; a paste under it does nothing.
#[test]
fn question_mark_opens_the_keys_and_any_key_closes_them() {
    let mut p = picker(&services());
    assert_eq!(press(&mut p, KeyCode::Char('?')), Flow::Redraw);
    let all = screen(&drawn(&mut p, 80, 24)).join("\n");
    assert!(all.contains("Keys: log picker"), "{all}");
    assert_eq!(press(&mut p, KeyCode::Char('q')), Flow::Redraw);
    assert!(!screen(&drawn(&mut p, 80, 24)).join("\n").contains("Keys:"));
    assert_eq!(p.take_choice(), None);
    press(&mut p, KeyCode::Char('/'));
    press(&mut p, KeyCode::Char('?'));
    assert_eq!(p.query, "?");
    press(&mut p, KeyCode::F(1));
    assert!(
        screen(&drawn(&mut p, 80, 24))
            .join("\n")
            .contains("Keys: log picker")
    );
    assert_eq!(p.input(Input::Paste("plex".into())), Flow::Unchanged);
    assert_eq!(p.query, "?");
}

#[test]
fn snapshot_keys_80x24() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Char('?'));
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

/// The footer at the spec's widths: the hints that fit, whole, and the
/// tail; at 50 `Tab kind` stays and `↑↓ move` goes.
#[test]
fn the_footer_fits_120_80_and_50_columns() {
    let mut p = picker(&services());
    for (width, row) in [
        (
            120,
            "Enter open · / filter · Tab kind · ↑↓ move                                                               ? keys · q quit",
        ),
        (
            80,
            "Enter open · / filter · Tab kind · ↑↓ move                       ? keys · q quit",
        ),
        (50, "Enter open · / filter · Tab kind   ? keys · q quit"),
    ] {
        let rows = screen(&drawn(&mut p, width, 10));
        assert_eq!(rows[9], row, "{width}");
    }
}

/// Every key the overlay lists does something here, and keys it doesn't
/// list do nothing (tui spec, "Keys", Testing).
#[test]
fn every_listed_key_acts_and_others_do_nothing() {
    for (name, event) in crate::hints::events(&KEYS, |_| true) {
        let mut p = picker(&services());
        drawn(&mut p, 80, 8);
        press(&mut p, KeyCode::Down);
        assert_ne!(p.input(Input::Key(event)), Flow::Unchanged, "{name}");
    }
    for c in ['x', 'z', 'v', 'r', 's', 'u', 'd', 'f', 'h', 'l'] {
        let mut p = picker(&services());
        assert_eq!(press(&mut p, KeyCode::Char(c)), Flow::Unchanged, "{c}");
    }
    for code in [KeyCode::Left, KeyCode::Right] {
        assert_eq!(press(&mut picker(&services()), code), Flow::Unchanged);
    }
}

#[test]
fn snapshot_keys_50x19() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Char('?'));
    assert_snapshot!(drawn(&mut p, 50, 19).backend());
}

/// Shift-Tab works in the filter line too, as Tab does: the kind
/// backwards, the line still open.
#[test]
fn shift_tab_in_the_filter_line_goes_back_a_kind() {
    let mut p = picker(&services());
    press(&mut p, KeyCode::Char('/'));
    let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(p.input(Input::Key(back)), Flow::Redraw);
    assert_eq!(p.filter, Filter::Containers);
    assert!(p.editing.is_some());
    assert_eq!(p.query, "");
}

/// Under the keys overlay the screen takes nothing: the wheel scrolls
/// the overlay (or does nothing when it fits), never the selection, and
/// a key closes it without moving.
#[test]
fn under_the_keys_the_wheel_and_keys_leave_the_list_alone() {
    let mut p = picker(&services());
    drawn(&mut p, 80, 24);
    let before = selected(&p);
    press(&mut p, KeyCode::Char('?'));
    let wheel = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 1,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    p.input(Input::Mouse(wheel));
    assert_eq!(selected(&p), before);
    assert!(screen(&drawn(&mut p, 80, 24)).join("\n").contains("Keys:"));
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(selected(&p), before);
    assert!(!screen(&drawn(&mut p, 80, 24)).join("\n").contains("Keys:"));
}
