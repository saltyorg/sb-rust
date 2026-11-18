//! The edit picker (tui spec, editors, Testing Strategy): the rows and
//! the prompt, keys and mouse through the event handler (no terminal),
//! and `TestBackend` snapshots at 80 and 40 columns.

use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sb_cli_core::theme::{Palette, Profile};

use super::*;

/// sb-go's six files, in its order (cmd/edit.go:175-205).
const FILES: [Item; 6] = [
    Item {
        title: "Accounts",
        shown: "accounts.yml",
    },
    Item {
        title: "Settings",
        shown: "settings.yml",
    },
    Item {
        title: "Advanced Settings",
        shown: "adv_settings.yml",
    },
    Item {
        title: "Backup Settings",
        shown: "backup_config.yml",
    },
    Item {
        title: "Hetzner VLAN Settings",
        shown: "hetzner_vlan.yml",
    },
    Item {
        title: "Inventory Settings",
        shown: "localhost.yml",
    },
];

fn picker() -> EditPicker {
    EditPicker::new(&FILES, Theme::resolve(&Palette::default(), Profile::Ascii))
}

fn press(picker: &mut EditPicker, code: KeyCode) -> Flow {
    picker.input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn drawn(picker: &mut EditPicker, width: u16, height: u16) -> Terminal<TestBackend> {
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

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn left(column: u16, row: u16) -> MouseEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

#[test]
fn each_file_is_a_row_under_the_prompt_with_its_name_lined_up() {
    let mut p = picker();
    let rows = screen(&drawn(&mut p, 80, 24));
    assert_eq!(
        rows[0],
        " ◆ Edit   Select a Saltbox configuration file to edit:"
    );
    assert_eq!(rows[1], "");
    assert_eq!(
        rows[2..8],
        [
            "› Accounts               accounts.yml",
            "  Settings               settings.yml",
            "  Advanced Settings      adv_settings.yml",
            "  Backup Settings        backup_config.yml",
            "  Hetzner VLAN Settings  hetzner_vlan.yml",
            "  Inventory Settings     localhost.yml",
        ]
    );
    assert_eq!(rows[23], "↑↓ move · Enter open · / filter · q quit");
}

/// Too narrow for a title and its name on one row: each name goes under
/// its title, and the prompt wraps, so nothing is cut.
#[test]
fn a_narrow_terminal_puts_each_name_under_its_title() {
    let mut p = picker();
    let rows = screen(&drawn(&mut p, 40, 20));
    assert_eq!(rows[0], " ◆ Edit   Select a Saltbox configuration");
    assert_eq!(rows[1], "file to edit:");
    assert_eq!(rows[2], "");
    assert_eq!(
        rows[3..7],
        [
            "› Accounts",
            "    accounts.yml",
            "  Settings",
            "    settings.yml"
        ]
    );
    assert_eq!(rows[11], "  Hetzner VLAN Settings");
    assert_eq!(rows[12], "    hetzner_vlan.yml");
    // The widest row (the widest title, then backup_config.yml) needs 42
    // columns: one fewer keeps two rows per file, 42 one.
    let rows = screen(&drawn(&mut p, 41, 20));
    assert_eq!(rows[11], "  Hetzner VLAN Settings");
    let rows = screen(&drawn(&mut p, 42, 20));
    assert_eq!(rows[6], "  Backup Settings        backup_config.yml");
}

/// The selected row is one solid bar across the list's width.
#[test]
fn the_selected_row_is_one_solid_bar() {
    let mut p = EditPicker::new(
        &FILES,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    );
    press(&mut p, KeyCode::Down);
    let terminal = drawn(&mut p, 60, 12);
    let buffer = terminal.backend().buffer();
    let want = buffer[(0, 3)].bg;
    assert_ne!(want, ratatui::style::Color::Reset);
    for x in 0..60 {
        assert_eq!(buffer[(x, 3)].bg, want, "column {x}");
    }
    assert_ne!(buffer[(0, 2)].bg, want);
}

#[test]
fn keys_move_the_selection_and_stop_at_either_end() {
    let mut p = picker();
    assert_eq!(p.state.selected(), Some(0));
    assert_eq!(press(&mut p, KeyCode::Up), Flow::Redraw);
    assert_eq!(p.state.selected(), Some(0));
    press(&mut p, KeyCode::Down);
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(p.state.selected(), Some(2));
    press(&mut p, KeyCode::Char('k'));
    assert_eq!(p.state.selected(), Some(1));
    for _ in 0..10 {
        press(&mut p, KeyCode::Down);
    }
    assert_eq!(p.state.selected(), Some(5));
    // Other keys, and keys with Ctrl or Alt, do nothing.
    assert_eq!(press(&mut p, KeyCode::Char('x')), Flow::Unchanged);
    let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    assert_eq!(p.input(Input::Key(ctrl_j)), Flow::Unchanged);
    let alt_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
    assert_eq!(p.input(Input::Key(alt_q)), Flow::Unchanged);
    assert_eq!(p.state.selected(), Some(5));
    assert_eq!(p.chosen, None);
}

/// The page and end keys (tui spec, "Keys: one map for every screen"):
/// `PgDn` and Space a page down, `PgUp` and `b` up, Home and `g` the first
/// file, End and `G` the last.
#[test]
fn the_page_and_end_keys_move_the_selection() {
    let mut p = picker();
    // Four rows of list at 80×7: the title, a blank row, the list, the
    // footer.
    drawn(&mut p, 80, 7);
    for (code, at) in [
        (KeyCode::PageDown, 4),
        (KeyCode::Char(' '), 5),
        (KeyCode::PageUp, 1),
        (KeyCode::Char('b'), 0),
        (KeyCode::End, 5),
        (KeyCode::Home, 0),
        (KeyCode::Char('G'), 5),
        (KeyCode::Char('g'), 0),
    ] {
        assert_eq!(press(&mut p, code), Flow::Redraw, "{code:?}");
        assert_eq!(p.state.selected(), Some(at), "{code:?}");
    }
}

/// `/` filters the files by title or name, smart case, as the log
/// picker's filter (sb-go's edit picker had `/`): Enter keeps the filter
/// and opens the file it leaves selected, Esc clears it, and `q` typed in
/// the line is the line's.
#[test]
fn slash_filters_the_files_and_enter_opens_the_one_shown() {
    let mut p = picker();
    press(&mut p, KeyCode::Char('/'));
    for c in "vlanq".chars() {
        assert_eq!(press(&mut p, KeyCode::Char(c)), Flow::Redraw);
    }
    press(&mut p, KeyCode::Backspace);
    let rows = screen(&drawn(&mut p, 80, 24));
    assert_eq!(
        rows[2], "› Hetzner VLAN Settings  hetzner_vlan.yml",
        "{rows:?}"
    );
    assert_eq!(rows[3], "", "{rows:?}");
    assert!(rows[23].starts_with(" Filter: vlan"), "{rows:?}");
    // Enter keeps the filter; a second Enter opens the file.
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Redraw);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Quit);
    assert_eq!(p.chosen, Some(4));
}

/// Esc goes back one step: it clears a filter in effect, then leaves.
#[test]
fn esc_clears_the_filter_then_leaves() {
    let mut p = picker();
    press(&mut p, KeyCode::Char('/'));
    press(&mut p, KeyCode::Char('Y'));
    press(&mut p, KeyCode::Enter);
    // Smart case: an upper-case letter matches exactly, so nothing.
    assert!(
        !screen(&drawn(&mut p, 80, 24))
            .iter()
            .any(|r| r.contains("yml"))
    );
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Unchanged);
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Redraw);
    assert_eq!(
        screen(&drawn(&mut p, 80, 24))[2],
        "› Accounts               accounts.yml"
    );
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Quit);
    assert_eq!(p.chosen, None);
}

/// A click on a filtered list opens the file shown there, and a paste goes
/// into the filter line whole.
#[test]
fn a_click_on_a_filtered_list_is_the_file_shown() {
    let mut p = picker();
    press(&mut p, KeyCode::Char('/'));
    assert_eq!(p.input(Input::Paste("backup\n".into())), Flow::Redraw);
    press(&mut p, KeyCode::Enter);
    drawn(&mut p, 80, 24);
    let now = Instant::now();
    p.click(left(4, 2), now);
    assert_eq!(p.click(left(4, 2), now), Flow::Quit);
    assert_eq!(p.chosen, Some(3));
}

#[test]
fn enter_chooses_the_selected_file() {
    let mut p = picker();
    press(&mut p, KeyCode::Down);
    press(&mut p, KeyCode::Down);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Quit);
    assert_eq!(p.chosen, Some(2));
}

#[test]
fn q_and_esc_leave_with_nothing_chosen() {
    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        let mut p = picker();
        press(&mut p, KeyCode::Down);
        assert_eq!(press(&mut p, code), Flow::Quit, "{code:?}");
        assert_eq!(p.chosen, None, "{code:?}");
    }
}

/// No files: Enter chooses nothing, the arrows do nothing.
#[test]
fn an_empty_list_chooses_nothing() {
    let mut p = EditPicker::new(&[], Theme::plain());
    assert_eq!(p.state.selected(), None);
    press(&mut p, KeyCode::Down);
    assert_eq!(p.state.selected(), None);
    assert_eq!(press(&mut p, KeyCode::Enter), Flow::Unchanged);
    assert_eq!(p.chosen, None);
}

#[test]
fn the_wheel_moves_the_selection() {
    let mut p = picker();
    drawn(&mut p, 80, 24);
    assert_eq!(
        p.input(Input::Mouse(mouse(MouseEventKind::ScrollDown, 5, 5))),
        Flow::Redraw
    );
    p.input(Input::Mouse(mouse(MouseEventKind::ScrollDown, 5, 5)));
    assert_eq!(p.state.selected(), Some(2));
    p.input(Input::Mouse(mouse(MouseEventKind::ScrollUp, 5, 5)));
    assert_eq!(p.state.selected(), Some(1));
    // Other mouse events do nothing.
    assert_eq!(
        p.input(Input::Mouse(mouse(MouseEventKind::Moved, 5, 5))),
        Flow::Unchanged
    );
    assert_eq!(
        p.input(Input::Mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            5,
            4
        ))),
        Flow::Unchanged
    );
    assert_eq!(p.state.selected(), Some(1));
}

/// A click selects the row under it; a second click on it soon after
/// opens it. Clicks off the rows, or a slow second click, open nothing.
#[test]
fn a_click_selects_and_a_double_click_opens() {
    let mut p = picker();
    drawn(&mut p, 80, 24);
    // Row 6 is the fifth file (the list starts at row 2).
    assert_eq!(p.input(Input::Mouse(left(30, 6))), Flow::Redraw);
    assert_eq!(p.state.selected(), Some(4));
    assert_eq!(p.chosen, None);
    assert_eq!(p.input(Input::Mouse(left(3, 6))), Flow::Quit);
    assert_eq!(p.chosen, Some(4));

    let mut p = picker();
    drawn(&mut p, 80, 24);
    let start = Instant::now();
    assert_eq!(p.click(left(3, 3), start), Flow::Redraw);
    // Too late: a click again, which selects.
    assert_eq!(
        p.click(left(3, 3), start + DOUBLE_CLICK + Duration::from_millis(1)),
        Flow::Redraw
    );
    assert_eq!(p.chosen, None);
    // Within the time, from that click: opens. Exactly at the limit counts.
    let second = start + DOUBLE_CLICK + Duration::from_millis(1);
    assert_eq!(p.click(left(3, 3), second + DOUBLE_CLICK), Flow::Quit);
    assert_eq!(p.chosen, Some(1));
}

/// Two clicks on different rows are two selections; a click off the rows
/// (the title, the blank rows under the list, the footer) does nothing and
/// breaks a double click.
#[test]
fn clicks_on_other_rows_or_off_the_list_do_not_open() {
    let mut p = picker();
    drawn(&mut p, 80, 24);
    let now = Instant::now();
    p.click(left(3, 2), now);
    assert_eq!(p.click(left(3, 3), now), Flow::Redraw);
    assert_eq!(p.state.selected(), Some(1));
    assert_eq!(p.chosen, None);
    for row in [0, 1, 8, 22, 23] {
        assert_eq!(p.click(left(3, row), now), Flow::Unchanged, "row {row}");
    }
    assert_eq!(p.state.selected(), Some(1));
    // The click before the off-list one doesn't pair with the next.
    p.click(left(3, 3), now);
    p.click(left(3, 0), now);
    assert_eq!(p.click(left(3, 3), now), Flow::Redraw);
    assert_eq!(p.chosen, None);
}

/// A key that moves the selection between the two clicks doesn't change
/// what the double click opens: the row clicked.
#[test]
fn a_double_click_opens_the_row_clicked_whatever_moved_between() {
    let mut p = picker();
    drawn(&mut p, 80, 24);
    let now = Instant::now();
    p.click(left(3, 4), now);
    press(&mut p, KeyCode::Down);
    assert_eq!(p.state.selected(), Some(3));
    assert_eq!(p.click(left(3, 4), now), Flow::Quit);
    assert_eq!(p.state.selected(), Some(2));
    assert_eq!(p.chosen, Some(2));
}

/// On a narrow terminal each file has two rows; a click on either is that
/// file.
#[test]
fn on_a_narrow_terminal_both_rows_of_a_file_are_its() {
    let mut p = picker();
    drawn(&mut p, 40, 20);
    // The list starts at row 3 (the prompt wraps): rows 5 and 6 are the
    // second file.
    let now = Instant::now();
    p.click(left(3, 5), now);
    assert_eq!(p.state.selected(), Some(1));
    assert_eq!(p.click(left(3, 6), now), Flow::Quit);
    assert_eq!(p.chosen, Some(1));
}

/// A list taller than the screen scrolls; a click maps through the
/// state's offset.
#[test]
fn a_click_on_a_scrolled_list_is_the_row_shown() {
    let mut p = picker();
    for _ in 0..5 {
        press(&mut p, KeyCode::Down);
    }
    // 40×12: the prompt takes two rows, then a blank, the list's 8 rows
    // (four files of two rows) and the footer.
    let rows = screen(&drawn(&mut p, 40, 12));
    assert_eq!(rows[3], "  Advanced Settings", "{rows:?}");
    assert_eq!(p.state.offset(), 2);
    let now = Instant::now();
    p.click(left(3, 3), now);
    assert_eq!(p.state.selected(), Some(2));
}

/// A resize redraws; focus changes and pastes don't.
#[test]
fn a_resize_redraws() {
    let mut p = picker();
    assert_eq!(p.input(Input::Resize(40, 20)), Flow::Redraw);
    assert_eq!(p.input(Input::FocusGained), Flow::Unchanged);
}

#[test]
fn snapshot_80x24() {
    let mut p = picker();
    press(&mut p, KeyCode::Down);
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_40x20() {
    let mut p = picker();
    assert_snapshot!(drawn(&mut p, 40, 20).backend());
}
