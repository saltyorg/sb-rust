//! The announcement viewer (tui spec, "The announcement viewer", Testing
//! Strategy), on ratatui's `TestBackend`: proto-markdown's tests, each
//! mutation-checked there, and the colour profiles, the sanitizing and
//! the row cap.

use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use sb_cli_core::theme::{Palette, Profile};

use super::*;

fn entry(date: &str, title: &str, message: &str, new: bool) -> Entry {
    Entry {
        repo: "Saltbox".into(),
        date: date.into(),
        title: title.into(),
        message: message.into(),
        new,
    }
}

fn colour() -> Theme {
    Theme::resolve(&Palette::default(), Profile::TrueColor)
}

fn viewer(entries: Vec<Entry>) -> Viewer {
    Viewer::new(entries, colour())
}

/// Sixty paragraphs: taller than any test screen.
fn long() -> String {
    (1..=60)
        .map(|n| ["Paragraph ", &n.to_string(), ".\n\n"].concat())
        .collect()
}

fn drawn(v: &mut Viewer, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
    terminal.draw(|f| v.draw(f)).expect("draw");
    terminal
}

fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

fn key(v: &mut Viewer, code: KeyCode) -> Flow {
    v.input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn mouse(v: &mut Viewer, kind: MouseEventKind, column: u16, row: u16) -> Flow {
    v.input(Input::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn click(v: &mut Viewer, column: u16, row: u16) -> Flow {
    mouse(v, MouseEventKind::Down(MouseButton::Left), column, row)
}

/// Three entries: an older one, a new one, an older one, out of order.
fn mixed() -> Vec<Entry> {
    vec![
        entry("2026-01-01", "Second Entry", "Plain paragraph.", false),
        entry(
            "2026-01-15",
            "Example Feature Update",
            "## Feature\n\nNew.",
            true,
        ),
        entry("2026-01-08", "Formatting Reference", "Older.", false),
    ]
}

#[test]
fn the_list_is_newest_first_and_opens_on_the_newest_new_one() {
    let v = viewer(vec![
        entry("2026-01-01", "Old", "", false),
        entry("2026-01-02", "Newer new", "", true),
        entry("2026-01-03", "Newest, seen", "", false),
        entry("2026-01-02", "Same day", "", true),
    ]);
    let titles: Vec<&str> = v.entries.iter().map(|e| e.title.as_str()).collect();
    // Newest first; on equal dates the order given.
    assert_eq!(titles, ["Newest, seen", "Newer new", "Same day", "Old"]);
    assert_eq!(v.list.selected(), Some(1));
    // No new one: the newest.
    let v = viewer(vec![
        entry("2026-01-01", "A", "", false),
        entry("2026-01-02", "B", "", false),
    ]);
    assert_eq!(v.list.selected(), Some(0));
    assert_eq!(viewer(Vec::new()).list.selected(), None);
}

#[test]
fn a_short_announcement_does_not_scroll() {
    let mut v = viewer(vec![entry("2026-01-01", "Short", "One line.", true)]);
    drawn(&mut v, 100, 30);
    key(&mut v, KeyCode::PageDown);
    key(&mut v, KeyCode::Char('G'));
    mouse(&mut v, MouseEventKind::ScrollDown, 80, 10);
    let terminal = drawn(&mut v, 100, 30);
    assert_eq!(v.article.offset().y, 0);
    let rows = screen(&terminal);
    assert!(rows[1].contains("2026-01-01 · Short"), "{rows:?}");
    assert!(rows[2].contains("One line."), "{rows:?}");
    // No scrollbar: nothing in the pane's last column.
    assert!(!rows.iter().any(|r| r.contains('█')), "{rows:#?}");
}

#[test]
fn a_long_announcement_scrolls_only_to_its_last_line() {
    let mut v = viewer(vec![entry("2026-01-01", "Long", &long(), true)]);
    drawn(&mut v, 100, 30);
    for _ in 0..50 {
        key(&mut v, KeyCode::PageDown);
    }
    let terminal = drawn(&mut v, 100, 30);
    assert!(v.article.offset().y > 0);
    let rows = screen(&terminal);
    // The last line sits on the last row inside the border, above the
    // footer.
    assert!(rows[27].contains("Paragraph 60."), "{:?}", &rows[25..29]);
    key(&mut v, KeyCode::Home);
    drawn(&mut v, 100, 30);
    assert_eq!(v.article.offset().y, 0);
    key(&mut v, KeyCode::End);
    drawn(&mut v, 100, 30);
    let bottom = v.article.offset().y;
    assert!(bottom > 0);
    key(&mut v, KeyCode::PageUp);
    drawn(&mut v, 100, 30);
    assert!(v.article.offset().y < bottom);
    for (code, moved) in [
        (KeyCode::Char('g'), 0),
        (KeyCode::Char(' '), 1),
        (KeyCode::Char('b'), 0),
    ] {
        key(&mut v, code);
        drawn(&mut v, 100, 30);
        assert_eq!(v.article.offset().y > 0, moved == 1, "{code:?}");
    }
}

#[test]
fn the_wheel_scrolls_the_announcement_three_rows() {
    let mut v = viewer(vec![entry("2026-01-01", "Long", &long(), true)]);
    drawn(&mut v, 100, 30);
    assert_eq!(
        mouse(&mut v, MouseEventKind::ScrollDown, 80, 10),
        Flow::Redraw
    );
    drawn(&mut v, 100, 30);
    assert_eq!(v.article.offset().y, 3);
    mouse(&mut v, MouseEventKind::ScrollUp, 80, 10);
    drawn(&mut v, 100, 30);
    assert_eq!(v.article.offset().y, 0);
}

#[test]
fn switching_announcement_starts_at_its_top() {
    let mut v = viewer(vec![
        entry("2026-02-01", "First", &long(), true),
        entry("2026-01-01", "Second", &long(), true),
    ]);
    drawn(&mut v, 100, 30);
    key(&mut v, KeyCode::PageDown);
    drawn(&mut v, 100, 30);
    assert!(v.article.offset().y > 0);
    assert_eq!(key(&mut v, KeyCode::Down), Flow::Redraw);
    let rows = screen(&drawn(&mut v, 100, 30));
    assert_eq!(v.article.offset().y, 0);
    assert_eq!(v.list.selected(), Some(1));
    // The pane shows the entry now selected.
    assert!(rows[1].contains("2026-01-01 · Second"), "{rows:#?}");
    key(&mut v, KeyCode::Char('k'));
    assert_eq!(v.list.selected(), Some(0));
    key(&mut v, KeyCode::Char('j'));
    assert_eq!(v.list.selected(), Some(1));
    key(&mut v, KeyCode::Up);
    assert_eq!(v.list.selected(), Some(0));
}

/// ← → and `h` `l` choose the previous and next entry, as ↑ ↓ do: sb-go's
/// keys for its previous and next announcement (tui spec, "Keys: one map
/// for every screen", rule 7).
#[test]
fn left_right_h_and_l_choose_the_entry() {
    let mut v = viewer(vec![
        entry("2026-02-01", "First", "one", true),
        entry("2026-01-01", "Second", "two", true),
    ]);
    drawn(&mut v, 100, 30);
    for (code, at) in [
        (KeyCode::Right, 1),
        (KeyCode::Left, 0),
        (KeyCode::Char('l'), 1),
        (KeyCode::Char('h'), 0),
    ] {
        assert_eq!(key(&mut v, code), Flow::Redraw, "{code:?}");
        assert_eq!(v.list.selected(), Some(at), "{code:?}");
    }
}

#[test]
fn moving_past_the_last_announcement_keeps_the_scroll_position() {
    let mut v = viewer(vec![
        entry("2026-01-02", "One", &long(), true),
        entry("2026-01-01", "Two", &long(), true),
    ]);
    drawn(&mut v, 100, 30);
    key(&mut v, KeyCode::Down);
    drawn(&mut v, 100, 30);
    key(&mut v, KeyCode::PageDown);
    drawn(&mut v, 100, 30);
    let scrolled = v.article.offset().y;
    assert!(scrolled > 0);
    // Nothing below: the selection stays, and so does the scroll.
    assert_eq!(key(&mut v, KeyCode::Down), Flow::Unchanged);
    drawn(&mut v, 100, 30);
    assert_eq!(v.article.offset().y, scrolled);
    assert_eq!(v.list.selected(), Some(1));
}

#[test]
fn q_and_esc_close_other_keys_change_nothing() {
    let mut v = viewer(mixed());
    assert_eq!(key(&mut v, KeyCode::Char('q')), Flow::Quit);
    assert_eq!(key(&mut v, KeyCode::Esc), Flow::Quit);
    assert_eq!(key(&mut v, KeyCode::Char('x')), Flow::Unchanged);
    assert_eq!(v.input(Input::FocusGained), Flow::Unchanged);
}

#[test]
fn the_list_marks_new_announcements_and_tints_older_ones() {
    let mut v = viewer(mixed());
    let terminal = drawn(&mut v, 100, 24);
    let rows = screen(&terminal);
    let buffer = terminal.backend().buffer();
    let at = |text: &str| rows.iter().position(|r| r.contains(text)).expect(text);
    let (new_row, old_row) = (at("Example Feature Update"), at("Second Entry"));
    assert!(rows[new_row].contains('●'));
    assert!(!rows[old_row].contains('●'));
    assert!(rows[0].contains("Announcements · 1 new"), "{}", rows[0]);
    // The old row's title is faint (it isn't selected); the new one's
    // title is Text.
    let column = |row: usize, text: &str| {
        let byte = rows[row].find(text).expect(text);
        u16::try_from(rows[row][..byte].chars().count()).expect("column")
    };
    let y = |row: usize| u16::try_from(row).expect("row");
    let theme = colour();
    let fg = |role| Style::from(theme.style(role)).fg.expect("a colour");
    assert_eq!(
        buffer[(column(old_row, "Second"), y(old_row))].fg,
        fg(Role::Faint)
    );
    // The footer's legend.
    assert!(rows[23].contains("● new"), "{}", rows[23]);
    // It opens on the new one, with its badge, in Saltbox's pane.
    assert!(v.entries[v.list.selected().expect("selected")].new);
    assert!(rows[1].contains(" NEW "), "{}", rows[1]);
    assert!(rows[0].contains(" Saltbox "), "{}", rows[0]);
}

/// With none marked (`sb announcements`), every row is at full strength:
/// faint means older than a marked one, and there is none.
#[test]
fn with_none_marked_no_row_is_faint() {
    let mut v = viewer(vec![
        entry("2026-01-02", "First", "", false),
        entry("2026-01-01", "Second", "", false),
    ]);
    let terminal = drawn(&mut v, 100, 24);
    let rows = screen(&terminal);
    let buffer = terminal.backend().buffer();
    let row = rows.iter().position(|r| r.contains("Second")).expect("row");
    let byte = rows[row].find("Second").expect("title");
    let x = u16::try_from(rows[row][..byte].chars().count()).expect("column");
    let y = u16::try_from(row).expect("row");
    let theme = colour();
    let fg = |role| Style::from(theme.style(role)).fg.expect("a colour");
    assert_eq!(
        buffer[(x, y)].fg,
        fg(Role::Text),
        "an unmarked row's title is the text colour"
    );
    assert!(!rows[row].contains('●'), "{}", rows[row]);
}

#[test]
fn no_new_one_has_no_count_badge_or_legend() {
    let mut v = viewer(vec![entry("2026-01-01", "Seen", "Body.", false)]);
    let rows = screen(&drawn(&mut v, 100, 24));
    assert!(rows[0].contains("Announcements"), "{}", rows[0]);
    assert!(!rows[0].contains("new"), "{}", rows[0]);
    assert!(!rows[1].contains("NEW"), "{}", rows[1]);
    assert!(!rows[23].contains("● new"), "{}", rows[23]);
}

#[test]
fn clicking_a_list_row_selects_it() {
    let mut v = viewer(mixed());
    drawn(&mut v, 100, 24);
    let (x, y) = (v.list_area.x + 2, v.list_area.y);
    // Rows: Example (new, selected), Formatting, Second.
    assert_eq!(click(&mut v, x, y + 3), Flow::Redraw);
    assert_eq!(v.list.selected(), Some(2));
    // The selected row again: no change.
    assert_eq!(click(&mut v, x, y + 3), Flow::Unchanged);
    // Below the last row, inside the block: nothing.
    assert_eq!(click(&mut v, x, y + 5), Flow::Unchanged);
    assert_eq!(v.list.selected(), Some(2));
}

#[test]
fn the_wheel_over_the_list_moves_the_selection() {
    let mut v = viewer(mixed());
    drawn(&mut v, 100, 24);
    let (x, y) = (v.list_area.x + 2, v.list_area.y + 1);
    mouse(&mut v, MouseEventKind::ScrollDown, x, y);
    assert_eq!(v.list.selected(), Some(1));
    mouse(&mut v, MouseEventKind::ScrollUp, x, y);
    assert_eq!(v.list.selected(), Some(0));
    assert_eq!(v.article.offset().y, 0);
    assert_eq!(mouse(&mut v, MouseEventKind::Moved, x, y), Flow::Unchanged);
}

#[test]
fn clicks_on_the_list_border_select_nothing() {
    // Narrow layout with more announcements than rows, so a click on the
    // bottom border would land on a hidden entry if borders counted.
    let entries = (0..10)
        .map(|i| {
            entry(
                &format!("2026-01-{:02}", 28 - i),
                &format!("Entry {i}"),
                "",
                true,
            )
        })
        .collect();
    let mut v = viewer(entries);
    drawn(&mut v, 60, 20);
    let before = v.list.selected();
    let area = v.list_area;
    for (column, row) in [
        (area.x + 3, area.bottom() - 1),
        (area.x + 3, area.y),
        (area.x, area.y + 2),
        (area.right() - 1, area.y + 2),
    ] {
        assert_eq!(click(&mut v, column, row), Flow::Unchanged);
    }
    assert_eq!(v.list.selected(), before);
}

#[test]
fn the_list_is_wide_enough_for_the_longest_title() {
    let title = "Example migration notice";
    let mut v = viewer(vec![
        entry("2026-01-02", "Short", "Body.", false),
        entry("2026-01-01", title, "Body.", false),
    ]);
    let rows = screen(&drawn(&mut v, 120, 20));
    // The whole title, then the list's right border.
    assert!(rows[2].contains(&format!("{title}│")), "{:?}", rows[2]);
}

#[test]
fn the_list_is_at_least_24_and_at_most_two_fifths_wide() {
    // An empty date: the row, `   Saltbox  A`, is narrower than the
    // minimum with its border.
    let mut v = viewer(vec![entry("", "A", "Body.", false)]);
    drawn(&mut v, 100, 20);
    assert_eq!(v.list_area.width, 24);
    let mut v = viewer(vec![entry(
        "2026-01-01",
        &"long ".repeat(30),
        "Body.",
        false,
    )]);
    drawn(&mut v, 100, 20);
    assert_eq!(v.list_area.width, 40);
    drawn(&mut v, 70, 20);
    assert_eq!(v.list_area.width, 28);
    assert_eq!(v.list_area.height, 19);
}

#[test]
fn the_list_measures_wide_characters_by_display_width() {
    let mut v = viewer(vec![entry("2026-01-01", "家庭メディア 🎬", "Body.", false)]);
    let rows = screen(&drawn(&mut v, 100, 20));
    // A wide character takes two cells; the second reads as a space here.
    let row = rows[1].replace(' ', "");
    assert!(row.contains("家庭メディア🎬│"), "{row:?}");
}

#[test]
fn below_70_columns_the_list_goes_above() {
    let mut v = viewer(mixed());
    let terminal = drawn(&mut v, 69, 24);
    // Three rows and the border, under a third of the body.
    assert_eq!(v.list_area, Rect::new(0, 0, 69, 5));
    let rows = screen(&terminal);
    assert!(rows[5].contains(" Saltbox "), "{rows:#?}");
    assert!(
        rows[6].contains("2026-01-15 · Example Feature Update"),
        "{rows:#?}"
    );
    // Many entries: a third of the body at most.
    let entries = (0..20)
        .map(|i| entry(&format!("2026-01-{:02}", 28 - i), "x", "", false))
        .collect();
    let mut v = viewer(entries);
    drawn(&mut v, 60, 31);
    assert_eq!(v.list_area.height, 10);
    // From 70 columns, side by side.
    let mut v = viewer(mixed());
    drawn(&mut v, 70, 24);
    assert_eq!(v.list_area.height, 23);
}

#[test]
fn the_scrollbar_uses_the_faint_role() {
    let mut v = viewer(vec![entry("2026-01-01", "Long", &long(), true)]);
    let terminal = drawn(&mut v, 100, 30);
    let buffer = terminal.backend().buffer();
    // The last column inside the pane's border.
    let x = 100 - 2;
    let cells: Vec<_> = (1..28).map(|y| &buffer[(x, y)]).collect();
    assert!(cells.iter().any(|c| c.symbol() == "█"), "no thumb at {x}");
    let theme = colour();
    let faint = Style::from(theme.style(Role::Faint)).fg;
    assert!(cells.iter().all(|c| Some(c.fg) == faint));
}

#[test]
fn no_announcement_says_so() {
    let mut v = viewer(Vec::new());
    let rows = screen(&drawn(&mut v, 100, 20));
    assert!(
        rows[1].contains("No announcements apply to this system."),
        "{rows:#?}"
    );
    assert_eq!(key(&mut v, KeyCode::Down), Flow::Unchanged);
    assert_eq!(click(&mut v, 3, 1), Flow::Unchanged);
}

#[test]
fn the_pane_holds_at_most_2000_rows() {
    let many: String = (1..=2500)
        .map(|n| ["Line ", &n.to_string(), ".\n\n"].concat())
        .collect();
    let mut v = viewer(vec![entry("2026-01-01", "Huge", &many, true)]);
    drawn(&mut v, 100, 30);
    key(&mut v, KeyCode::End);
    let rows = screen(&drawn(&mut v, 100, 30));
    // 2000 rows: the heading, then 1999 of the message's rows (a line and
    // a blank one each).
    assert_eq!(v.article.size().map(|s| s.height), Some(2000));
    assert!(rows[27].contains("Line 1000."), "{:?}", &rows[24..29]);
    assert!(!rows.iter().any(|r| r.contains("Line 1001.")));
}

#[test]
fn nothing_of_the_files_escapes_or_controls_reaches_a_cell() {
    let mut v = viewer(vec![Entry {
        repo: "Salt\x1b[2Jbox".into(),
        date: "2026-01-01\x07".into(),
        title: "Title\x1b]0;x\x07".into(),
        message: "a &#27;[2J b \x1b[31mred\x1b[0m &#x9b;H c".into(),
        new: true,
    }]);
    let terminal = drawn(&mut v, 100, 20);
    for cell in terminal.backend().buffer().content() {
        assert!(
            !cell.symbol().chars().any(char::is_control),
            "{:?}",
            cell.symbol()
        );
    }
    let rows = screen(&terminal);
    assert!(rows.iter().any(|r| r.contains("red")), "{rows:#?}");
}

/// Every colour fits the profile: RGB only under truecolor (the palette
/// and syntect's highlighting alike), no RGB under ansi256, none at all
/// with colour off, where the markers show instead.
#[test]
fn every_colour_fits_the_profile() {
    let message = "## Heading\n\nSome `code`.\n\n```yaml\nsettings:\n  enabled: true\n```\n";
    for profile in [
        Profile::TrueColor,
        Profile::Ansi256,
        Profile::Ansi,
        Profile::Ascii,
    ] {
        let theme = if profile == Profile::Ascii {
            Theme::plain()
        } else {
            Theme::resolve(&Palette::default(), profile)
        };
        let mut v = Viewer::new(vec![entry("2026-01-01", "T", message, true)], theme);
        for (width, height) in [(100, 30), (60, 30)] {
            let terminal = drawn(&mut v, width, height);
            for cell in terminal.backend().buffer().content() {
                for colour in [cell.fg, cell.bg] {
                    let fits = match profile {
                        Profile::TrueColor => matches!(colour, Color::Rgb(..) | Color::Reset),
                        Profile::Ansi256 => !matches!(colour, Color::Rgb(..)),
                        Profile::Ansi => !matches!(colour, Color::Rgb(..) | Color::Indexed(_)),
                        Profile::Ascii => colour == Color::Reset,
                    };
                    assert!(fits, "{profile:?} at {width}: {colour:?}");
                }
            }
            let rows = screen(&terminal);
            assert_eq!(
                rows.iter().any(|r| r.contains("## Heading")),
                profile == Profile::Ascii,
                "{profile:?}: {rows:#?}"
            );
        }
    }
}

/// With colour off the selected row is reverse video; in colour, the
/// Selected roles' bar.
#[test]
fn the_selected_row_is_a_bar() {
    let mut v = Viewer::new(mixed(), Theme::plain());
    let terminal = drawn(&mut v, 100, 24);
    let buffer = terminal.backend().buffer();
    assert!(buffer[(3, 1)].modifier.contains(Modifier::REVERSED));
    assert!(!buffer[(3, 2)].modifier.contains(Modifier::REVERSED));
    let mut v = viewer(mixed());
    let terminal = drawn(&mut v, 100, 24);
    let buffer = terminal.backend().buffer();
    let theme = colour();
    assert_eq!(
        Some(buffer[(3, 1)].bg),
        theme.color(Role::SelectedBg).map(Into::into)
    );
    assert!(!buffer[(3, 1)].modifier.contains(Modifier::REVERSED));
}

/// A table is laid out to the pane: drawn again at another width, the
/// message is rendered for it.
#[test]
fn the_message_is_laid_out_for_the_panes_width() {
    let message = "| Setting | Meaning |\n|---|---|\n| enabled | a long cell that has to wrap in a narrow pane |\n";
    let mut v = viewer(vec![entry("2026-01-01", "Table", message, true)]);
    drawn(&mut v, 120, 20);
    let wide = v.rendered.first().map(|(width, _)| *width);
    drawn(&mut v, 60, 20);
    let narrow = v.rendered.first().map(|(width, _)| *width);
    assert!(narrow < wide, "{wide:?} then {narrow:?}");
}

/// With the list scrolled, a click selects the row under it: the
/// state's offset plus the row inside the block.
#[test]
fn a_click_on_a_scrolled_list_selects_the_row_under_it() {
    let entries = (0..40)
        .map(|i| {
            entry(
                &format!("2026-{:02}-01", 12 - i / 4),
                &format!("Entry {i}"),
                "",
                false,
            )
        })
        .collect();
    let mut v = viewer(entries);
    drawn(&mut v, 80, 24);
    for _ in 0..30 {
        key(&mut v, KeyCode::Down);
    }
    let rows = screen(&drawn(&mut v, 80, 24));
    let offset = v.list.offset();
    assert!(offset > 0, "the list scrolled");
    let (x, y) = (v.list_rows.x + 1, v.list_rows.y + 1);
    assert_eq!(click(&mut v, x, y), Flow::Redraw);
    assert_eq!(v.list.selected(), Some(offset + 1));
    let shown = &v.entries[offset + 1].title;
    assert!(rows[usize::from(y)].contains(shown.as_str()), "{rows:#?}");
}

#[test]
fn the_footer_is_the_key_hints() {
    let mut v = viewer(mixed());
    let rows = screen(&drawn(&mut v, 100, 24));
    assert!(
        rows[23].starts_with("↑↓ announcement · PgUp/PgDn scroll · q close   ● new"),
        "{}",
        rows[23]
    );
}

/// The pane's inner width at a 100-column screen, from the last draw.
fn inner_width(v: &Viewer) -> usize {
    usize::from(100 - v.list_area.width - 2)
}

/// A line as wide as the pane fills one row when the text fits: it is
/// laid out at the pane's whole width, with no scrollbar.
#[test]
fn text_that_fits_takes_the_whole_width() {
    let mut v = viewer(vec![entry("2026-01-01", "T", "x", true)]);
    drawn(&mut v, 100, 30);
    let wide = "x".repeat(inner_width(&v));
    let mut v = viewer(vec![entry("2026-01-01", "T", &wide, true)]);
    let rows = screen(&drawn(&mut v, 100, 30));
    assert!(rows[2].contains(&format!("│{wide}│")), "{rows:#?}");
    assert!(!rows[3].contains('x'), "{rows:#?}");
}

/// Text exactly as tall as the pane fits: no scrollbar, and its lines
/// keep the whole width.
#[test]
fn text_exactly_as_tall_as_the_pane_fits() {
    let mut v = viewer(vec![entry("2026-01-01", "T", "x", true)]);
    drawn(&mut v, 100, 29);
    let wide = "x".repeat(inner_width(&v));
    // The heading, then 13 paragraphs with a blank row between each: 26
    // rows, the pane's inner height at 29 rows.
    let mut message = wide.clone();
    for n in 1..13 {
        message.push_str(&["\n\nP", &n.to_string()].concat());
    }
    let mut v = viewer(vec![entry("2026-01-01", "T", &message, true)]);
    let rows = screen(&drawn(&mut v, 100, 29));
    assert!(rows[26].contains("P12"), "{rows:#?}");
    assert!(rows[2].contains(&format!("│{wide}│")), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains('█')), "{rows:#?}");
}

/// Text taller than the pane is laid out one column narrower, beside its
/// scrollbar: a line as wide as the pane wraps its last character.
#[test]
fn text_that_scrolls_is_laid_out_beside_the_scrollbar() {
    let mut v = viewer(vec![entry("2026-01-01", "T", "x", true)]);
    drawn(&mut v, 100, 30);
    let wide = "y".repeat(inner_width(&v));
    let message = format!("{wide}\n\n{}", long());
    let mut v = viewer(vec![entry("2026-01-01", "T", &message, true)]);
    let rows = screen(&drawn(&mut v, 100, 30));
    assert!(
        rows[2].contains(&"y".repeat(inner_width(&v) - 1)),
        "{rows:#?}"
    );
    assert!(rows[3].contains("│y "), "{rows:#?}");
    // Laid out at both widths once; a second frame renders nothing new.
    let widths: Vec<u16> = v.rendered.iter().map(|(w, _)| *w).collect();
    assert_eq!(widths.len(), 2, "{widths:?}");
    drawn(&mut v, 100, 30);
    assert_eq!(
        v.rendered.iter().map(|(w, _)| *w).collect::<Vec<_>>(),
        widths
    );
}

/// In colour, the message keeps the sheet's roles: inline code is drawn
/// in the Code role.
#[test]
fn the_message_keeps_its_colours() {
    let mut v = viewer(vec![entry("2026-01-01", "T", "Some `snippet` here.", true)]);
    let terminal = drawn(&mut v, 100, 20);
    let rows = screen(&terminal);
    let byte = rows[2].find("snippet").expect("the code");
    let x = u16::try_from(rows[2][..byte].chars().count()).expect("column");
    let theme = colour();
    assert_eq!(
        Some(terminal.backend().buffer()[(x, 2)].fg),
        Style::from(theme.style(Role::Code)).fg
    );
}

/// The list's title fits: with many new entries and short titles the
/// list is as wide as `Announcements · N new`.
#[test]
fn the_list_is_wide_enough_for_its_title() {
    // Empty dates: each row, `●   Saltbox  x`, is narrower than the title.
    let entries = (0..40).map(|_| entry("", "x", "", true)).collect();
    let mut v = viewer(entries);
    let rows = screen(&drawn(&mut v, 100, 24));
    assert!(
        rows[0].contains("╭ Announcements · 40 new ╮"),
        "{}",
        rows[0]
    );
}

/// Entries of both repositories, as `sb announcements` gives them.
fn both_repositories(new: bool) -> Vec<Entry> {
    let sandbox = |date: &str, title: &str, message: &str| Entry {
        repo: "Sandbox".into(),
        ..entry(date, title, message, new)
    };
    vec![
        entry(
            "2026-03-02",
            "Settings moved",
            "## Settings moved\n\nRun the migration.",
            new,
        ),
        sandbox("2026-04-01", "Sandbox news", "From the sandbox."),
        entry("2026-01-10", "Old news", "Already seen.", false),
    ]
}

/// Every row names its repository, between the date and the title, in
/// the Muted role, marked or not (the maintainer, 2026-11-17: both
/// repositories are always shown, so each says whose it is); the pane's
/// title names the selected entry's.
#[test]
fn each_row_names_its_repository() {
    for new in [false, true] {
        let mut v = viewer(both_repositories(new));
        let terminal = drawn(&mut v, 100, 24);
        let rows = screen(&terminal);
        let buffer = terminal.backend().buffer();
        for (date, repo, title) in [
            ("2026-04-01", "Sandbox", "Sandbox news"),
            ("2026-03-02", "Saltbox", "Settings moved"),
            ("2026-01-10", "Saltbox", "Old news"),
        ] {
            let row = rows
                .iter()
                .position(|r| r.contains(&format!("{date}  {repo}  {title}")))
                .unwrap_or_else(|| {
                    panic!("{date} {repo} {title} (new {new}):\n{}", rows.join("\n"))
                });
            let byte = rows[row].find(&format!("  {repo}  ")).expect("repo") + 2;
            let x = u16::try_from(rows[row][..byte].chars().count()).expect("column");
            let y = u16::try_from(row).expect("row");
            let selected = v.list.selected() == Some(row - 1);
            if !selected {
                // Muted, but Faint on an older row among marked ones.
                let older = new && title == "Old news";
                let theme = colour();
                let role = if older { Role::Faint } else { Role::Muted };
                let want = Style::from(theme.style(role)).fg.expect("a colour");
                assert_eq!(buffer[(x, y)].fg, want, "{repo} at ({x},{y}), new {new}");
            }
        }
        // The pane opens on the newest, Sandbox's, and names it.
        assert!(rows[0].contains("╮╭ Sandbox "), "{}", rows[0]);
    }
}

/// `sb announcements`' viewer (none marked) with both repositories.
#[test]
fn snapshot_both_repositories_unmarked_100x24() {
    let mut v = viewer(both_repositories(false));
    let terminal = drawn(&mut v, 100, 24);
    insta::assert_snapshot!(terminal.backend());
}

/// `sb update`'s viewer (two marked, one older) with both repositories:
/// the same labelling.
#[test]
fn snapshot_both_repositories_marked_100x24() {
    let mut v = viewer(both_repositories(true));
    let terminal = drawn(&mut v, 100, 24);
    insta::assert_snapshot!(terminal.backend());
}
