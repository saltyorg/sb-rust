//! The fact editor's page (tui spec, editors, Testing Strategy, "Fact
//! screen"): the headings and settings, folding, search with and without
//! matches, every value shown as it is, Copy's OSC 52 payload, the run
//! log without a value, and `TestBackend` snapshots at 110 and 80 columns (M54); the
//! changes, the panel, the save and every dialog, on real facts
//! directories through sb-config's session (M55).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use sb_cli_core::theme::{Palette, Profile};
use std::time::Duration;

use sb_config::facts::{Fact, Instance};

use super::*;

/// A known secret: shown on screen as it is (tui spec, editors, Values),
/// never in a log.
const SECRET: &str = "not-a-real-password";

fn fact(key: &str, value: &str) -> Fact {
    Fact {
        key: key.into(),
        value: value.into(),
    }
}

fn role(name: &str, instances: Vec<(&str, Vec<Fact>)>) -> App {
    App {
        name: name.into(),
        instances: instances
            .into_iter()
            .map(|(name, facts)| Instance {
                name: name.into(),
                facts,
            })
            .collect(),
    }
}

/// proto-facts' synthetic set, as sb-config's catalog lists it: roles and
/// copies sorted, keys sorted.
fn roles() -> Vec<App> {
    vec![
        role(
            "homepage",
            vec![(
                "homepage",
                vec![
                    fact("motd", "first line\nsecond line"),
                    fact("theme", "dark"),
                    fact("title", "家庭メディア 🎬 Server"),
                ],
            )],
        ),
        role(
            "komodo",
            vec![(
                "komodo",
                vec![
                    fact("database_password", SECRET),
                    fact("jwt_secret", "not-a-real-jwt-secret"),
                ],
            )],
        ),
        role(
            "plex",
            vec![
                (
                    "plex",
                    vec![
                        fact("claim_token", "claim-FAKE0000"),
                        fact("transcode_path", "/mnt/local/transcodes"),
                    ],
                ),
                (
                    "plex2",
                    vec![
                        fact("claim_token", "claim-FAKE1111"),
                        fact("transcode_path", "/mnt/local/transcodes2"),
                    ],
                ),
            ],
        ),
    ]
}

/// Where a test's Copy goes.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("sink").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Sink {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("sink").clone()).expect("utf-8")
    }
}

/// `roles` as Saltbox's files in `dir`: a section per copy, `key = value`
/// lines, continuation lines after a tab (the writer's form).
fn write_roles(dir: &std::path::Path, roles: &[App]) {
    for role in roles {
        let mut text = String::new();
        for instance in &role.instances {
            writeln!(text, "[{}]", instance.name).expect("a string");
            for fact in &instance.facts {
                writeln!(text, "{} = {}", fact.key, fact.value.replace('\n', "\n\t"))
                    .expect("a string");
            }
            text.push('\n');
        }
        std::fs::write(dir.join(format!("{}.ini", role.name)), text).expect("fact file");
    }
}

/// A page over `roles` written to a facts directory and opened by
/// sb-config's session, in `theme`, its Copy into `out`. The directory
/// goes at once: the session has read it, and these tests save nothing.
fn page_in(roles: &[App], theme: Theme, out: Box<dyn Write + Send>) -> Page {
    let dir = tempfile::tempdir().expect("temp dir");
    write_roles(dir.path(), roles);
    Page::new(Session::open(dir.path()).expect("session"), theme, out)
}

fn page_with(roles: &[App], sink: &Sink) -> Page {
    page_in(
        roles,
        Theme::resolve(&Palette::default(), Profile::Ascii),
        Box::new(sink.clone()),
    )
}

fn page() -> Page {
    page_with(&roles(), &Sink::default())
}

fn press(page: &mut Page, code: KeyCode) -> Flow {
    page.input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn ctrl(page: &mut Page, c: char) -> Flow {
    page.input(Input::Key(KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    )))
}

fn type_text(page: &mut Page, text: &str) {
    for c in text.chars() {
        press(page, KeyCode::Char(c));
    }
}

fn drawn(page: &mut Page, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal.draw(|f| page.draw(f)).expect("drawn");
    terminal
}

/// The screen's rows, trailing spaces trimmed; a wide character's
/// second cell (drawn as a space) is skipped.
fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                row.push_str(symbol);
                x += u16::try_from(Span::raw(symbol).width().max(1)).expect("a width");
            }
            row.trim_end().to_owned()
        })
        .collect()
}

fn shows(page: &mut Page, width: u16, height: u16, text: &str) -> bool {
    screen(&drawn(page, width, height))
        .iter()
        .any(|row| row.contains(text))
}

/// What `f` logged on this thread: a global subscriber writes each
/// thread's records to that thread's buffer, so tests running at once
/// keep theirs apart. Why not `subscriber::with_default` for each test:
/// with tests on several threads, a callsite another thread hit with no
/// subscriber set stayed off for this one (seen here, deterministic with
/// the whole suite), and `callsite::rebuild_interest_cache` didn't help.
fn logged(f: impl FnOnce()) -> String {
    thread_local! {
        static RECORDS: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    struct ThisThread;
    impl Write for ThisThread {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            RECORDS.with(|records| records.borrow_mut().extend_from_slice(buf));
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    static GLOBAL: std::sync::Once = std::sync::Once::new();
    GLOBAL.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| ThisThread)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the tests' subscriber");
    });
    RECORDS.with(|records| records.borrow_mut().clear());
    f();
    RECORDS.with(|records| String::from_utf8_lossy(&records.borrow()).into_owned())
}

/// Down `n` times.
fn down(page: &mut Page, n: usize) {
    for _ in 0..n {
        press(page, KeyCode::Down);
    }
}

#[test]
fn each_app_is_a_heading_with_its_settings_and_values() {
    let mut p = page();
    let rows = screen(&drawn(&mut p, 80, 24));
    assert_eq!(
        rows[0],
        "╭ Saltbox settings ────────────────────────────────────────────────────────────╮"
    );
    assert_eq!(
        rows[1],
        "│ Search   press / to search                                                   │"
    );
    assert_eq!(
        rows[3..18],
        [
            "› ▾ Homepage",
            "    motd               first line …",
            "    theme              dark",
            "    title              家庭メディア 🎬 Server",
            "  ▾ Komodo",
            "    database_password  not-a-real-password",
            "    jwt_secret         not-a-real-jwt-secret",
            "  ▾ Plex",
            "    plex",
            "      claim_token      claim-FAKE0000",
            "      transcode_path   /mnt/local/transcodes",
            "    plex2",
            "      claim_token      claim-FAKE1111",
            "      transcode_path   /mnt/local/transcodes2",
            "",
        ]
    );
    assert_eq!(
        rows[23],
        "  Edit (e)   Add (a)   Delete (d)   Save (s)   Keys (?)   Quit (q)"
    );
}

/// A copy heading shows for one copy not named after its app, too.
#[test]
fn a_lone_copy_not_named_after_its_app_has_a_heading() {
    let mut p = page_with(
        &[role(
            "sonarr",
            vec![("sonarr4k", vec![fact("port", "8990")])],
        )],
        &Sink::default(),
    );
    let rows = screen(&drawn(&mut p, 60, 14));
    assert_eq!(
        rows[3..6],
        ["› ▾ Sonarr", "    sonarr4k", "      port  8990"]
    );
}

/// Control characters in a name, key or value are escaped, never sent to
/// the terminal.
#[test]
fn control_characters_are_escaped() {
    let mut p = page_with(
        &[role(
            "app\x1b]0;x\x07",
            vec![("app", vec![fact("k", "v\x1b[31mred\x1b]0;x\x07")])],
        )],
        &Sink::default(),
    );
    let terminal = drawn(&mut p, 60, 10);
    let buffer = terminal.backend().buffer();
    assert!(
        buffer
            .content()
            .iter()
            .all(|cell| !cell.symbol().contains('\x1b')),
        "{:?}",
        screen(&terminal)
    );
}

#[test]
fn a_page_too_long_for_the_screen_opens_folded() {
    let mut p = page();
    let rows = screen(&drawn(&mut p, 80, 12));
    assert_eq!(
        rows[3..6],
        [
            "› ▸ Homepage  3 settings",
            "  ▸ Komodo  2 settings",
            "  ▸ Plex  4 settings"
        ]
    );
    // Right opens the selected app; Left folds it again from any of its
    // rows, selecting its heading.
    press(&mut p, KeyCode::Right);
    assert!(shows(&mut p, 80, 12, "theme"));
    down(&mut p, 2);
    press(&mut p, KeyCode::Left);
    assert_eq!(p.selected(), Some(Row::App(0)));
    assert!(!shows(&mut p, 80, 12, "theme"));
    // Enter on a heading opens and folds it.
    press(&mut p, KeyCode::Enter);
    assert!(shows(&mut p, 80, 12, "theme"));
    press(&mut p, KeyCode::Enter);
    assert!(!shows(&mut p, 80, 12, "theme"));
    // The choice is made once: a taller screen keeps the folds.
    assert!(!shows(&mut p, 80, 40, "theme"));
}

/// One fact is "1 setting".
#[test]
fn a_folded_app_counts_its_settings() {
    let mut p = page_with(
        &[role(
            "sure",
            vec![("sure", vec![fact("secret_key", SECRET)])],
        )],
        &Sink::default(),
    );
    press(&mut p, KeyCode::Enter);
    assert!(shows(&mut p, 60, 10, "▸ Sure  1 setting"));
}

#[test]
fn the_keys_move_the_selection_and_stop_at_either_end() {
    let mut p = page();
    drawn(&mut p, 80, 24);
    press(&mut p, KeyCode::Up);
    assert_eq!(p.selected(), Some(Row::App(0)));
    press(&mut p, KeyCode::Char('j'));
    assert_eq!(p.selected(), Some(Row::Setting((0, 0, 0))));
    press(&mut p, KeyCode::Char('k'));
    assert_eq!(p.selected(), Some(Row::App(0)));
    press(&mut p, KeyCode::End);
    assert_eq!(p.selected(), Some(Row::Setting((2, 1, 1))));
    press(&mut p, KeyCode::Down);
    assert_eq!(p.selected(), Some(Row::Setting((2, 1, 1))));
    press(&mut p, KeyCode::Home);
    assert_eq!(p.selected(), Some(Row::App(0)));
    // A page is the list's height (19 rows at 80×24): past the end, the
    // last row.
    press(&mut p, KeyCode::PageDown);
    assert_eq!(p.selected(), Some(Row::Setting((2, 1, 1))));
    press(&mut p, KeyCode::PageUp);
    assert_eq!(p.selected(), Some(Row::App(0)));
    // Ctrl and Alt with a key do nothing.
    assert_eq!(ctrl(&mut p, 'j'), Flow::Unchanged);
    let alt_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
    assert_eq!(p.input(Input::Key(alt_q)), Flow::Unchanged);
    assert_eq!(press(&mut p, KeyCode::Char('x')), Flow::Unchanged);
    assert_eq!(p.selected(), Some(Row::App(0)));
}

#[test]
fn q_and_esc_quit() {
    for code in [KeyCode::Char('q'), KeyCode::Esc] {
        let mut p = page();
        assert_eq!(press(&mut p, code), Flow::Quit, "{code:?}");
    }
}

/// Esc goes back one step: a search in effect is cleared first, as sb-go's
/// editor did, and only then does Esc leave (tui spec, "Keys", rule 5).
#[test]
fn esc_clears_a_search_in_effect_before_it_quits() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "komodo");
    press(&mut p, KeyCode::Enter);
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Redraw);
    assert_eq!(p.query, "");
    assert_eq!(p.search.lines().concat(), "");
    assert!(shows(&mut p, 80, 24, "Homepage"));
    assert_eq!(press(&mut p, KeyCode::Esc), Flow::Quit);
}

/// `s` and Ctrl+S save (sb-go's keys, the log viewer's `s`); `v` does
/// nothing now (tui spec, "Keys").
#[test]
fn s_and_ctrl_s_save_and_v_does_nothing() {
    let mut p = page();
    down(&mut p, 2);
    press(&mut p, KeyCode::Char('d'));
    assert_eq!(press(&mut p, KeyCode::Char('v')), Flow::Unchanged);
    assert!(matches!(p.popup, Popup::None));
    press(&mut p, KeyCode::Char('s'));
    assert!(matches!(p.popup, Popup::ConfirmSave));
    press(&mut p, KeyCode::Esc);
    ctrl(&mut p, 's');
    assert!(matches!(p.popup, Popup::ConfirmSave));
}

/// `g` and `G` go to the first and last row, as Home and End; `b` and
/// Space page up and down, as `PgUp` and `PgDn`; `h` and `l` fold and open,
/// as ← and → (tui spec, "Keys").
#[test]
fn the_alias_keys_move_page_and_fold() {
    let mut p = page();
    drawn(&mut p, 80, 24);
    press(&mut p, KeyCode::Char('G'));
    assert_eq!(p.selected(), Some(Row::Setting((2, 1, 1))));
    press(&mut p, KeyCode::Char('g'));
    assert_eq!(p.selected(), Some(Row::App(0)));
    press(&mut p, KeyCode::Char(' '));
    assert_eq!(p.selected(), Some(Row::Setting((2, 1, 1))));
    press(&mut p, KeyCode::Char('b'));
    assert_eq!(p.selected(), Some(Row::App(0)));
    press(&mut p, KeyCode::Char('h'));
    assert!(shows(&mut p, 80, 24, "▸ Homepage"));
    press(&mut p, KeyCode::Char('l'));
    assert!(shows(&mut p, 80, 24, "▾ Homepage"));
}

/// Searching narrows the page to the matching apps, copies and keys, in
/// any case, opens folded apps, and never matches a value.
#[test]
fn search_narrows_the_page_and_opens_matching_apps() {
    let mut p = page();
    drawn(&mut p, 80, 16);
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "TRANS");
    let rows = screen(&drawn(&mut p, 80, 16));
    assert_eq!(
        rows[1],
        "│ Search  TRANS                                                                │"
    );
    assert_eq!(
        rows[3..9],
        [
            "› ▾ Plex",
            "    plex",
            "      transcode_path   /mnt/local/transcodes",
            "    plex2",
            "      transcode_path   /mnt/local/transcodes2",
            "",
        ]
    );
    // A copy's name matches all its settings.
    press(&mut p, KeyCode::Esc);
    type_text(&mut p, "plex2");
    let rows = screen(&drawn(&mut p, 80, 16));
    assert_eq!(rows[4], "    plex2", "{rows:?}");
    assert_eq!(rows[6], "      transcode_path   /mnt/local/transcodes2");
    // Values aren't searched.
    press(&mut p, KeyCode::Esc);
    type_text(&mut p, "dark");
    assert!(shows(
        &mut p,
        80,
        12,
        "No settings match \"dark\". Press Esc in the search box to clear it."
    ));
}

/// Esc clears the search, then leaves it; Enter keeps it and gives the
/// keys back to the page.
#[test]
fn esc_clears_then_leaves_the_search() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "plex");
    press(&mut p, KeyCode::Esc);
    assert!(p.searching);
    assert_eq!(p.query, "");
    press(&mut p, KeyCode::Esc);
    assert!(!p.searching);
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "komodo");
    press(&mut p, KeyCode::Enter);
    assert!(!p.searching);
    assert_eq!(p.query, "komodo");
    // `q` is the page's again, not the line's.
    assert_eq!(press(&mut p, KeyCode::Char('q')), Flow::Quit);
}

/// Down and Tab also leave the search; Ctrl-M adds no line.
#[test]
fn down_and_tab_leave_the_search_and_ctrl_m_adds_no_line() {
    for code in [KeyCode::Down, KeyCode::Tab] {
        let mut p = page();
        press(&mut p, KeyCode::Char('/'));
        type_text(&mut p, "x");
        ctrl(&mut p, 'm');
        assert_eq!(p.search.lines().len(), 1);
        press(&mut p, code);
        assert!(!p.searching, "{code:?}");
        assert_eq!(p.query, "x");
    }
}

/// Backspace and Ctrl-H delete in the search (the proto-facts lesson:
/// the line is ratatui-textarea's).
#[test]
fn backspace_and_ctrl_h_delete_in_the_search() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "plexx");
    press(&mut p, KeyCode::Backspace);
    assert_eq!(p.query, "plex");
    ctrl(&mut p, 'h');
    assert_eq!(p.query, "ple");
}

/// The selection goes back to the first row when the query changes.
#[test]
fn a_new_query_selects_the_first_row() {
    let mut p = page();
    drawn(&mut p, 80, 24);
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "jwt");
    assert_eq!(p.selected(), Some(Row::App(1)));
    // Nothing matches: no row is selected.
    type_text(&mut p, "zzz");
    assert_eq!(p.selected(), None);
    press(&mut p, KeyCode::Enter);
    press(&mut p, KeyCode::Down);
    assert_eq!(p.selected(), None);
}

/// Every value shows as it is, a secret's too (the maintainer,
/// 2026-11-18: values are shown, not hidden): no dots anywhere,
/// and `s` reveals nothing because nothing is hidden.
#[test]
fn every_value_shows_as_it_is() {
    let mut p = page();
    let rows = screen(&drawn(&mut p, 80, 24)).join("\n");
    for value in [SECRET, "not-a-real-jwt-secret", "claim-FAKE0000"] {
        assert!(rows.contains(value), "{value}: {rows}");
    }
    assert!(!rows.contains('•'), "{rows}");
    assert!(!rows.contains("Show"), "{rows}");
}

/// Copy acts on settings only; on a heading it says so.
#[test]
fn copy_on_a_heading_says_to_pick_a_setting() {
    let sink = Sink::default();
    let mut p = page_with(&roles(), &sink);
    press(&mut p, KeyCode::Char('c'));
    assert!(shows(&mut p, 80, 24, "Pick a setting to copy"));
    assert_eq!(sink.text(), "");
}

/// Copy sends exactly the value through OSC 52 and says it can't confirm
/// it worked.
#[test]
fn copy_sends_the_value_through_osc_52() {
    let sink = Sink::default();
    let mut p = page_with(&roles(), &sink);
    drawn(&mut p, 80, 24);
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('c'));
    // base64 of "not-a-real-password".
    assert_eq!(sink.text(), "\x1b]52;c;bm90LWEtcmVhbC1wYXNzd29yZA==\x1b\\");
    // Whole on an 80-column screen.
    let rows = screen(&drawn(&mut p, 80, 24));
    assert_eq!(
        rows[22],
        " Copied database_password (if your terminal allows it)"
    );
}

/// A copy that can't be written says why.
#[test]
fn a_copy_that_fails_says_so() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no terminal"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut p = page_in(&roles(), Theme::plain(), Box::new(Broken));
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('c'));
    assert_eq!(p.status, "Couldn't copy database_password: no terminal");
}

/// The run log gets what was copied, by name, never a value, though the
/// screen shows it.
#[test]
fn the_log_names_what_was_copied_without_a_value() {
    let text = logged(|| {
        let mut p = page();
        drawn(&mut p, 80, 24);
        down(&mut p, 5);
        press(&mut p, KeyCode::Char('c'));
        press(&mut p, KeyCode::Char('/'));
        type_text(&mut p, "pass");
        drawn(&mut p, 80, 24);
    });
    assert!(text.contains("fact copied"), "{text}");
    assert!(
        text.contains("role=komodo copy=komodo key=database_password"),
        "{text}"
    );
    assert!(!text.contains(SECRET), "{text}");
}

/// At 100 columns and more the changes panel sits on the right.
#[test]
fn a_wide_screen_shows_the_changes_panel_on_the_right() {
    let mut p = page();
    let rows = screen(&drawn(&mut p, 110, 30));
    assert!(
        rows[3][..rows[3].len() - "╮".len()].ends_with('─'),
        "{}",
        rows[3]
    );
    let panel = rows[3].find("╭ Unsaved changes (0) ").expect("the panel");
    // 36 columns from the right edge.
    assert_eq!(Span::raw(&rows[3][panel..]).width(), 36, "{}", rows[3]);
    assert!(rows[4].contains("│ Nothing changed yet"), "{}", rows[4]);
}

/// Below 100 columns the changes panel is a band at the bottom, above the
/// status line.
#[test]
fn a_narrow_screen_shows_the_changes_panel_at_the_bottom() {
    let mut p = page();
    let rows = screen(&drawn(&mut p, 99, 30));
    assert!(rows[25].starts_with("╭ Unsaved changes (0) ─"), "{rows:?}");
    assert!(rows[26].starts_with("│ Nothing changed yet"), "{rows:?}");
    assert!(rows[27].starts_with("╰─"), "{rows:?}");
    assert_eq!(
        rows[29],
        "  Edit (e)   Add (a)   Copy (c)   Delete (d)   Save changes (s)   Keys (?)   Quit (q)"
    );
}

/// The buttons that can't act on the selected row are drawn faint; the
/// others aren't.
#[test]
fn buttons_that_cannot_act_are_faint() {
    let mut p = EditStyle::page();
    let faint = colours(p.style(Role::Faint).patch(p.bg(Role::ButtonDisabledBg)));
    let on = colours(p.style(Role::Text).patch(p.bg(Role::ButtonBg)));
    // On an app's heading (at 100 columns, every button shown): Edit
    // (column 2) and Copy (23) faint; Add (13, the app has one copy) and
    // Delete (34) not.
    let terminal = drawn(&mut p, 100, 24);
    let buffer = terminal.backend().buffer();
    assert_eq!(cell(buffer, 2, 23), faint);
    assert_eq!(cell(buffer, 13, 23), on);
    assert_eq!(cell(buffer, 23, 23), faint);
    assert_eq!(cell(buffer, 34, 23), on);
    // On a setting: Edit and Copy can act.
    down(&mut p, 5);
    let terminal = drawn(&mut p, 100, 24);
    let buffer = terminal.backend().buffer();
    assert_eq!(cell(buffer, 2, 23), on);
    assert_eq!(cell(buffer, 23, 23), on);
}

/// The search box's border lights up while it has focus.
#[test]
fn the_search_border_shows_its_focus() {
    let mut p = EditStyle::page();
    let border = |p: &mut Page| cell(drawn(p, 80, 24).backend().buffer(), 0, 0).0;
    assert_eq!(border(&mut p), colours(p.style(Role::Border)).0);
    press(&mut p, KeyCode::Char('/'));
    assert_eq!(border(&mut p), colours(p.style(Role::Accent)).0);
}

/// A style's foreground and background, unset as `Reset` (as a drawn
/// cell has them).
fn colours(style: Style) -> (Color, Color) {
    (
        style.fg.unwrap_or(Color::Reset),
        style.bg.unwrap_or(Color::Reset),
    )
}

/// The cell at `x`, `y`: its foreground and background.
fn cell(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> (Color, Color) {
    let cell = &buffer[(x, y)];
    (cell.fg, cell.bg)
}

/// Pages drawn in colour, for the style tests.
struct EditStyle;

impl EditStyle {
    fn page() -> Page {
        page_in(
            &roles(),
            Theme::resolve(&Palette::default(), Profile::TrueColor),
            Box::new(Sink::default()),
        )
    }
}

/// While a search shows every match open, the fold keys do nothing: no
/// fold changes where it can't be seen.
#[test]
fn the_fold_keys_do_nothing_while_searching() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "plex");
    press(&mut p, KeyCode::Enter);
    for code in [KeyCode::Enter, KeyCode::Left, KeyCode::Right] {
        press(&mut p, code);
        assert!(p.folded.is_empty(), "{code:?}");
    }
}

/// A copy with no settings still has its heading, and an app or copy whose
/// name matches the search shows, settings or not.
#[test]
fn empty_copies_show_and_match_by_name() {
    let mut p = page_with(
        &[role(
            "sonarr",
            vec![
                ("sonarr", vec![fact("port", "8989")]),
                ("sonarr4k", Vec::new()),
            ],
        )],
        &Sink::default(),
    );
    let rows = screen(&drawn(&mut p, 60, 12));
    assert_eq!(
        rows[3..7],
        [
            "› ▾ Sonarr",
            "    sonarr",
            "      port  8989",
            "    sonarr4k"
        ]
    );
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "4k");
    let rows = screen(&drawn(&mut p, 60, 12));
    assert_eq!(rows[3..6], ["› ▾ Sonarr", "    sonarr4k", ""]);
    // An app with no copy at all shows by its name.
    let mut p = page_with(&[role("lidarr", Vec::new())], &Sink::default());
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "lid");
    assert!(shows(&mut p, 60, 12, "▾ Lidarr"));
}

/// A key longer than the cap pushes its own value along; the others keep
/// the column the cap gives.
#[test]
fn keys_past_the_cap_push_only_their_own_value() {
    let long = "a_name_well_past_the_cap_of_the_column";
    let mut p = page_with(
        &[role(
            "app",
            vec![("app", vec![fact(long, "one"), fact("short", "two")])],
        )],
        &Sink::default(),
    );
    let rows = screen(&drawn(&mut p, 100, 12));
    // Values start after the indent (2) and the cap (28), then two
    // spaces: 25 spaces after `short`, past the mark's two columns.
    assert!(
        rows[5].starts_with(&format!("    short{}two ", " ".repeat(25))),
        "{}",
        rows[5]
    );
    assert!(
        rows[4].starts_with(&format!("    {long}  one ")),
        "{}",
        rows[4]
    );
}

/// The status line's message goes when the selection moves.
#[test]
fn the_status_line_clears_when_the_selection_moves() {
    let mut p = page();
    press(&mut p, KeyCode::Char('c'));
    assert_eq!(p.status, "Pick a setting to copy");
    press(&mut p, KeyCode::Down);
    assert_eq!(p.status, "");
}

/// A copy that couldn't be sent is logged as such, without the value.
#[test]
fn a_failed_copy_is_logged_as_not_copied() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("no terminal"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let text = logged(|| {
        let mut p = page_in(&roles(), Theme::plain(), Box::new(Broken));
        down(&mut p, 5);
        press(&mut p, KeyCode::Char('c'));
    });
    assert!(text.contains("fact not copied"), "{text}");
    assert!(!text.contains("fact copied"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
}

/// The page's `Debug` holds no value.
#[test]
fn the_page_s_debug_holds_no_value() {
    let mut p = page();
    down(&mut p, 5);
    let debug = format!("{p:?}");
    assert!(debug.contains("database_password"), "{debug}");
    assert!(!debug.contains(SECRET), "{debug}");
}

#[test]
fn an_empty_catalog_says_so() {
    let mut p = page_with(&[], &Sink::default());
    assert!(shows(&mut p, 80, 24, "No settings found"));
    press(&mut p, KeyCode::Down);
    assert_eq!(p.selected(), None);
}

/// A resize redraws; other events don't.
#[test]
fn a_resize_redraws() {
    let mut p = page();
    assert_eq!(p.input(Input::Resize(80, 24)), Flow::Redraw);
    assert_eq!(p.input(Input::FocusGained), Flow::Unchanged);
}

#[test]
fn snapshot_page_110x30() {
    let mut p = page();
    down(&mut p, 2);
    assert_snapshot!(drawn(&mut p, 110, 30).backend());
}

#[test]
fn snapshot_page_80x24() {
    let mut p = page();
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_folded_80x12() {
    let mut p = page();
    assert_snapshot!(drawn(&mut p, 80, 12).backend());
}

#[test]
fn snapshot_search_80x24() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "token");
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_search_without_matches_80x24() {
    let mut p = page();
    press(&mut p, KeyCode::Char('/'));
    type_text(&mut p, "sonarr");
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

// --- changes, the changes panel, saving and the dialogs (M55) ---

/// A test's facts directory, kept for the save: the files written from
/// `roles`, the page over them.
struct Fixture {
    dir: tempfile::TempDir,
    page: Page,
}

impl Fixture {
    fn new(roles: &[App]) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        write_roles(dir.path(), roles);
        let session = Session::open(dir.path()).expect("session");
        let page = Page::new(
            session,
            Theme::resolve(&Palette::default(), Profile::Ascii),
            Box::new(Sink::default()),
        );
        Self { dir, page }
    }

    /// `role`'s file as it is on disk now.
    fn file(&self, role: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(format!("{role}.ini"))).ok()
    }

    /// The save's events taken until it is over, the dialogs answered by
    /// `answer` (a drift's), up to 10 s.
    fn settle(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.page.saving.is_some() {
            assert!(std::time::Instant::now() < deadline, "the save didn't end");
            self.page.wake();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait until the save asks or says something other than "Saving".
    fn wait_for_dialog(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            self.page.wake();
            if !matches!(self.page.popup, Popup::Saving) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "no dialog came");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Type `text` into the dialog's box.
fn type_into(page: &mut Page, text: &str) {
    for c in text.chars() {
        press(page, KeyCode::Char(c));
    }
}

fn alt_enter(page: &mut Page) -> Flow {
    page.input(Input::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)))
}

/// Select the row whose screen text holds `text` (at 100×40).
fn select(page: &mut Page, text: &str) {
    let rows = screen(&drawn(page, 100, 40));
    let at = rows[3..]
        .iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text}: {rows:?}"));
    page.list.select(Some(at));
}

fn setting_target(role: &str, copy: &str, key: &str) -> Target {
    Target::Setting(role.into(), copy.into(), key.into())
}

/// Edit opens on the value; Enter stages it, the row and the panel say
/// so, and the file is untouched until the save.
#[test]
fn an_edit_is_staged_and_listed_with_its_undo() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    assert!(matches!(f.page.popup, Popup::Edit { .. }));
    assert!(shows(&mut f.page, 100, 40, "Edit theme"));
    // The box opens on the value, the cursor at its end.
    press(&mut f.page, KeyCode::Backspace);
    press(&mut f.page, KeyCode::Backspace);
    type_into(&mut f.page, "rk-blue");
    press(&mut f.page, KeyCode::Enter);
    assert!(matches!(f.page.popup, Popup::None));
    assert_eq!(
        f.page
            .pending
            .get(&setting_target("homepage", "homepage", "theme")),
        Some(&Pending::Set("dark-blue".into()))
    );
    let rows = screen(&drawn(&mut f.page, 110, 30));
    assert!(
        rows.iter()
            .any(|r| r.contains("theme") && r.contains("dark-blue  changed")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("Unsaved changes (1)")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("Edit   homepage · theme") && r.contains(" undo ")),
        "{rows:?}"
    );
    assert_eq!(f.page.status, "homepage · theme changed (not saved yet)");
    assert!(f.file("homepage").expect("file").contains("theme = dark\n"));
}

/// Enter on a setting edits it too; Esc leaves the box, staging nothing;
/// an edit back to the file's value drops the change.
#[test]
fn esc_cancels_and_the_file_s_value_drops_the_change() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Enter);
    type_into(&mut f.page, "x");
    press(&mut f.page, KeyCode::Esc);
    assert!(f.page.pending.is_empty());
    assert_eq!(f.page.status, "Edit cancelled");
    press(&mut f.page, KeyCode::Char('e'));
    type_into(&mut f.page, "x");
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(f.page.pending.len(), 1);
    press(&mut f.page, KeyCode::Char('e'));
    press(&mut f.page, KeyCode::Backspace);
    press(&mut f.page, KeyCode::Enter);
    assert!(f.page.pending.is_empty());
    assert_eq!(f.page.status, "No change");
}

/// Alt+Enter adds a line; the value is staged with it, shown with " …".
#[test]
fn alt_enter_adds_a_line_to_the_value() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    alt_enter(&mut f.page);
    type_into(&mut f.page, "second");
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(
        f.page
            .pending
            .get(&setting_target("homepage", "homepage", "theme")),
        Some(&Pending::Set("dark\nsecond".into()))
    );
    assert!(shows(&mut f.page, 100, 40, "dark …  changed"));
}

/// A value the file can't hold is refused before it is staged, with
/// sb-config's reason; the box stays open.
#[test]
fn a_value_the_file_cannot_hold_is_refused() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    type_into(&mut f.page, " ");
    press(&mut f.page, KeyCode::Enter);
    assert!(f.page.pending.is_empty());
    assert!(matches!(f.page.popup, Popup::Edit { .. }));
    assert!(
        f.page
            .status
            .starts_with("Can't stage that change: homepage: "),
        "{}",
        f.page.status
    );
}

/// A secret's edit box shows its text as it is; Ctrl+S hides nothing.
#[test]
fn a_secret_s_edit_box_shows_its_value() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "database_password");
    press(&mut f.page, KeyCode::Char('e'));
    let rows = screen(&drawn(&mut f.page, 100, 40)).join("\n");
    assert!(rows.contains(SECRET), "{rows}");
    assert!(!rows.contains('•'), "{rows}");
    assert!(!rows.contains("Ctrl+S"), "{rows}");
    // Enter stages; "save" is the file's (tui spec, "Keys", Dialogs).
    assert!(
        rows.contains("Enter keep · Alt+Enter new line · Esc cancel"),
        "{rows}"
    );
    ctrl(&mut f.page, 's');
    assert!(shows(&mut f.page, 100, 40, SECRET));
}

/// Add asks a name, then a value, and stages the new setting under the
/// selected copy: shown as new, listed as an addition.
#[test]
fn add_stages_a_new_setting_under_the_selected_copy() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('a'));
    assert!(shows(&mut f.page, 100, 40, "Add a setting to plex · plex2"));
    type_into(&mut f.page, "port");
    press(&mut f.page, KeyCode::Enter);
    assert!(matches!(f.page.popup, Popup::AddValue { .. }));
    assert!(shows(&mut f.page, 100, 40, "Value for plex · plex2 · port"));
    type_into(&mut f.page, "32400");
    press(&mut f.page, KeyCode::Enter);
    assert!(matches!(f.page.popup, Popup::None));
    assert_eq!(f.page.status, "plex · plex2 · port added (not saved yet)");
    let rows = screen(&drawn(&mut f.page, 110, 30));
    assert!(
        rows.iter()
            .any(|r| r.contains("port") && r.contains("32400  new")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("Add    plex · plex2 · por")),
        "{rows:?}"
    );
    // Deleting the new setting drops the addition.
    select(&mut f.page, "32400");
    press(&mut f.page, KeyCode::Char('d'));
    assert!(f.page.pending.is_empty());
}

/// Add refuses an empty name, one already there, and one Saltbox's reader
/// would misread, before asking for a value; a name with brackets inside
/// is fine (decision 40).
#[test]
fn add_refuses_names_before_the_value() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "transcode_path   /mnt/local/transcodes2");
    press(&mut f.page, KeyCode::Char('a'));
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(f.page.status, "Type the new setting's name");
    type_into(&mut f.page, "claim_token");
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(f.page.status, "claim_token is already there");
    for bad in ["#x", "a=b", " x"] {
        if let Popup::AddKey { line, .. } = &mut f.page.popup {
            line.select_all();
            line.cut();
        }
        type_into(&mut f.page, bad);
        press(&mut f.page, KeyCode::Enter);
        assert!(
            f.page.status.starts_with("Can't add that name: plex: "),
            "{bad}: {}",
            f.page.status
        );
        assert!(matches!(f.page.popup, Popup::AddKey { .. }), "{bad}");
    }
    if let Popup::AddKey { line, .. } = &mut f.page.popup {
        line.select_all();
        line.cut();
    }
    type_into(&mut f.page, "headers[Authorization]");
    press(&mut f.page, KeyCode::Enter);
    assert!(
        matches!(f.page.popup, Popup::AddValue { .. }),
        "{}",
        f.page.status
    );
    press(&mut f.page, KeyCode::Esc);
    assert!(f.page.pending.is_empty());
}

/// Add needs a copy: an app's heading with several copies says so; one
/// with a single copy adds to it.
#[test]
fn add_on_an_app_heading_needs_a_single_copy() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "▾ Plex");
    press(&mut f.page, KeyCode::Char('a'));
    assert_eq!(f.page.status, "Pick a copy to add a setting to");
    assert!(matches!(f.page.popup, Popup::None));
    select(&mut f.page, "▾ Komodo");
    press(&mut f.page, KeyCode::Char('a'));
    assert!(matches!(f.page.popup, Popup::AddKey { .. }));
}

/// Delete works on a setting, a copy and an app, with the note that
/// Saltbox makes a new value; Delete again keeps it. An app's deletion
/// replaces the changes inside it.
#[test]
fn delete_at_three_levels_and_keep() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "transcode_path   /mnt/local/transcodes2");
    press(&mut f.page, KeyCode::Char('d'));
    assert_eq!(
        f.page.status,
        "plex · plex2 · transcode_path: deleted on save; Saltbox recreates it next run"
    );
    // Whole on an 80-column screen.
    assert!(
        screen(&drawn(&mut f.page, 80, 30)).contains(
            &" plex · plex2 · transcode_path: deleted on save; Saltbox recreates it next run"
                .to_owned()
        )
    );
    assert!(shows(&mut f.page, 100, 40, "will be deleted"));
    assert!(shows(&mut f.page, 100, 40, "Keep (d)"));
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('d'));
    // The copy's deletion replaced its setting's.
    assert_eq!(
        f.page.pending.keys().collect::<Vec<_>>(),
        [&Target::Copy("plex".into(), "plex2".into())]
    );
    // Inside a deleted copy nothing else can be staged.
    select(&mut f.page, "plex2  will be deleted");
    press(&mut f.page, KeyCode::Down);
    press(&mut f.page, KeyCode::Char('d'));
    assert_eq!(f.page.status, "Its app or copy is already to be deleted");
    press(&mut f.page, KeyCode::Char('e'));
    assert_eq!(f.page.status, "Keep it first: it is to be deleted");
    select(&mut f.page, "▾ Plex");
    press(&mut f.page, KeyCode::Char('d'));
    assert_eq!(
        f.page.pending.keys().collect::<Vec<_>>(),
        [&Target::App("plex".into())]
    );
    // Keep.
    press(&mut f.page, KeyCode::Char('d'));
    assert!(f.page.pending.is_empty());
    assert_eq!(f.page.status, "Undid the change to plex");
}

/// A row staged to be deleted is struck through.
#[test]
fn a_deleted_row_is_struck_through() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "▾ Komodo");
    press(&mut f.page, KeyCode::Char('d'));
    let terminal = drawn(&mut f.page, 100, 40);
    let buffer = terminal.backend().buffer();
    let row = screen(&terminal)
        .iter()
        .position(|r| r.contains("jwt_secret"))
        .expect("the row");
    let row = u16::try_from(row).expect("a row");
    assert!(buffer[(4, row)].modifier.contains(Modifier::CROSSED_OUT));
}

/// `u` undoes the selected row's change.
#[test]
fn u_undoes_the_selected_row_s_change() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('u'));
    assert!(f.page.pending.is_empty());
    assert_eq!(f.page.status, "Undid the change to homepage · theme");
    press(&mut f.page, KeyCode::Char('u'));
    assert_eq!(f.page.status, "Nothing to undo here");
}

/// Below 100 columns the band grows with the changes, up to four rows.
#[test]
fn the_bottom_band_grows_to_four_changes() {
    let mut f = Fixture::new(&roles());
    for key in [
        "motd",
        "theme",
        "title",
        "jwt_secret",
        "transcode_path   /mnt/local/transcodes2",
    ] {
        select(&mut f.page, key);
        press(&mut f.page, KeyCode::Char('d'));
    }
    assert_eq!(f.page.pending.len(), 5);
    let rows = screen(&drawn(&mut f.page, 80, 30));
    let top = rows
        .iter()
        .position(|r| r.starts_with("╭ Unsaved changes (5)"))
        .expect("the band");
    assert!(rows[top + 5].starts_with("╰"), "{rows:?}");
    assert!(rows[top + 1].contains("Delete homepage · motd"), "{rows:?}");
    // The changes past its rows: the last says how many.
    assert!(rows[top + 4].contains("+2 more"), "{rows:?}");
    assert!(!rows.iter().any(|r| r.contains("Delete plex")), "{rows:?}");
}

/// Save asks first; Enter saves: the file holds the change, the panel
/// empties, the outcome says so.
#[test]
fn save_writes_the_changes_and_says_so() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    type_into(&mut f.page, "er");
    press(&mut f.page, KeyCode::Enter);
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    assert!(matches!(f.page.popup, Popup::ConfirmSave));
    assert!(shows(
        &mut f.page,
        100,
        40,
        "Save 2 changes to the Saltbox settings?"
    ));
    assert!(shows(
        &mut f.page,
        100,
        40,
        "Deleted values: Saltbox creates a new value"
    ));
    // n cancels.
    press(&mut f.page, KeyCode::Char('n'));
    assert!(matches!(f.page.popup, Popup::None));
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Char('y'));
    f.settle();
    assert!(f.page.pending.is_empty());
    assert!(
        f.file("homepage")
            .expect("file")
            .contains("theme = darker\n")
    );
    assert_eq!(
        f.file("plex").expect("file"),
        "[plex]\nclaim_token = claim-FAKE0000\ntranscode_path = /mnt/local/transcodes\n\n"
    );
    assert!(shows(&mut f.page, 100, 40, " Saved "));
    assert!(shows(&mut f.page, 100, 40, "homepage: saved"));
    assert!(shows(&mut f.page, 100, 40, "plex: saved"));
    // The page shows the file as saved.
    press(&mut f.page, KeyCode::Enter);
    assert!(matches!(f.page.popup, Popup::None));
    assert!(shows(&mut f.page, 100, 40, "darker"));
    assert!(!shows(&mut f.page, 100, 40, "plex2"));
    assert!(shows(&mut f.page, 100, 40, "Nothing changed yet"));
}

/// Deleting an app unlinks its file.
#[test]
fn saving_an_app_s_deletion_removes_its_file() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "▾ Komodo");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.settle();
    assert_eq!(f.file("komodo"), None);
    press(&mut f.page, KeyCode::Enter);
    assert!(!shows(&mut f.page, 100, 40, "Komodo"));
}

/// Nothing staged: Save says so and asks nothing.
#[test]
fn save_with_nothing_staged_says_so() {
    let mut f = Fixture::new(&roles());
    press(&mut f.page, KeyCode::Char('s'));
    assert!(matches!(f.page.popup, Popup::None));
    assert_eq!(f.page.status, "Nothing to save");
}

/// Quit with staged changes asks Save, Discard or Cancel.
#[test]
fn quit_with_staged_changes_asks() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    assert_eq!(press(&mut f.page, KeyCode::Char('q')), Flow::Redraw);
    assert!(matches!(f.page.popup, Popup::ConfirmQuit));
    assert!(shows(
        &mut f.page,
        100,
        40,
        "You have 1 unsaved change. Save them before you quit?"
    ));
    assert!(shows(&mut f.page, 100, 40, "Discard (d)"));
    press(&mut f.page, KeyCode::Esc);
    assert!(matches!(f.page.popup, Popup::None));
    press(&mut f.page, KeyCode::Char('q'));
    assert_eq!(press(&mut f.page, KeyCode::Char('d')), Flow::Quit);
    assert!(f.file("homepage").expect("file").contains("theme = dark"));
}

/// Quit's Save saves, then quits once the outcome is read, when
/// everything was saved.
#[test]
fn quit_s_save_saves_then_quits() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('q'));
    press(&mut f.page, KeyCode::Char('y'));
    f.settle();
    assert!(!f.file("homepage").expect("file").contains("theme"));
    assert_eq!(press(&mut f.page, KeyCode::Enter), Flow::Quit);
}

/// A role changed on disk since the open: the drift dialog shows before
/// and after, values as they are; Reload takes the disk version and drops
/// its change.
#[test]
fn a_drift_reloads_on_r() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    std::fs::write(
        f.dir.path().join("komodo.ini"),
        format!("[komodo]\ndatabase_password = {SECRET}\njwt_secret = other\nport = 1\n"),
    )
    .expect("write");
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    assert!(matches!(f.page.popup, Popup::Drift { .. }));
    let rows = screen(&drawn(&mut f.page, 100, 40)).join("\n");
    assert!(rows.contains("Changed on disk"), "{rows}");
    assert!(rows.contains("Before"), "{rows}");
    assert!(rows.contains("On disk"), "{rows}");
    assert!(rows.contains("port = 1"), "{rows}");
    assert!(rows.contains(SECRET), "{rows}");
    assert!(!rows.contains('•'), "{rows}");
    assert!(rows.contains("Reload (r)"), "{rows}");
    press(&mut f.page, KeyCode::Char('r'));
    f.settle();
    assert!(f.page.pending.is_empty());
    assert!(shows(
        &mut f.page,
        100,
        40,
        "komodo: changed on disk; reloaded, its changes dropped"
    ));
    press(&mut f.page, KeyCode::Enter);
    assert!(shows(&mut f.page, 100, 40, "port"));
}

/// Cancel at the drift keeps the change staged and writes nothing.
#[test]
fn a_drift_cancel_keeps_the_change() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    std::fs::remove_file(f.dir.path().join("komodo.ini")).expect("remove");
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    assert!(shows(&mut f.page, 100, 40, "(the file is gone)"));
    press(&mut f.page, KeyCode::Down);
    // `c` copies on the page: it doesn't cancel here (tui spec, "Keys").
    assert_eq!(press(&mut f.page, KeyCode::Char('c')), Flow::Unchanged);
    assert!(shows(&mut f.page, 100, 40, "Cancel (Esc)"));
    press(&mut f.page, KeyCode::Esc);
    f.settle();
    assert_eq!(f.page.pending.len(), 1);
    assert!(shows(&mut f.page, 100, 40, " Not everything was saved "));
    assert!(shows(
        &mut f.page,
        100,
        40,
        "komodo: changed on disk; nothing written, its changes kept"
    ));
}

/// Saltbox holds the role's lock: the waiting dialog counts, and its
/// Cancel stops the save with the change kept.
#[test]
fn a_held_lock_shows_the_waiting_dialog_and_cancels() {
    let mut f = Fixture::new(&roles());
    let lock = std::fs::File::create(f.dir.path().join("homepage.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    assert!(matches!(f.page.popup, Popup::Waiting { .. }));
    assert!(shows(&mut f.page, 100, 40, "Waiting for Saltbox"));
    assert!(shows(
        &mut f.page,
        100,
        40,
        "Saltbox is changing homepage's settings."
    ));
    assert!(shows(&mut f.page, 100, 40, "Cancel (Esc)"));
    press(&mut f.page, KeyCode::Char('c'));
    assert!(matches!(f.page.popup, Popup::Waiting { .. }));
    press(&mut f.page, KeyCode::Esc);
    f.settle();
    assert_eq!(f.page.pending.len(), 1);
    assert!(shows(
        &mut f.page,
        100,
        40,
        "homepage: not saved (the save stopped first); its changes kept"
    ));
    drop(lock);
}

/// While a save runs, the page's keys do nothing.
#[test]
fn the_page_s_keys_wait_while_saving() {
    let mut f = Fixture::new(&roles());
    let lock = std::fs::File::create(f.dir.path().join("homepage.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    assert_eq!(press(&mut f.page, KeyCode::Char('q')), Flow::Redraw);
    assert!(f.page.saving.is_some());
    f.page.saving.as_ref().expect("saving").stop();
    f.settle();
    drop(lock);
}

/// A stop (Ctrl-C, SIGTERM) during the save leaves the rest unattempted,
/// and `finish` hands the session back.
#[test]
fn a_stopped_save_finishes_with_the_session() {
    let mut f = Fixture::new(&roles());
    let lock = std::fs::File::create(f.dir.path().join("homepage.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    let running = f.page.saving.take().expect("saving");
    running.stop();
    let (_, outcomes) = running.finish().expect("the session back");
    assert!(matches!(outcomes["homepage"], RoleOutcome::Unattempted));
    drop(lock);
}

/// The run log names each staged and undone change and its kind, never
/// a value.
#[test]
fn the_log_names_each_change_without_its_value() {
    let text = logged(|| {
        let mut f = Fixture::new(&roles());
        select(&mut f.page, "database_password");
        press(&mut f.page, KeyCode::Char('e'));
        type_into(&mut f.page, "-new");
        press(&mut f.page, KeyCode::Enter);
        select(&mut f.page, "    plex2");
        press(&mut f.page, KeyCode::Char('a'));
        type_into(&mut f.page, "api_key");
        press(&mut f.page, KeyCode::Enter);
        type_into(&mut f.page, "added-secret-value");
        press(&mut f.page, KeyCode::Enter);
        select(&mut f.page, "api_key");
        press(&mut f.page, KeyCode::Char('u'));
        press(&mut f.page, KeyCode::Char('s'));
        press(&mut f.page, KeyCode::Enter);
        f.settle();
    });
    assert!(text.contains("change staged"), "{text}");
    assert!(text.contains("change=\"set setting\""), "{text}");
    assert!(text.contains("change=\"add setting\""), "{text}");
    assert!(text.contains("change undone"), "{text}");
    assert!(text.contains("save started"), "{text}");
    for value in [SECRET, "added-secret-value"] {
        assert!(!text.contains(value), "{value}: {text}");
    }
}

/// "Save changes" shortens to "Save" when the buttons wouldn't fit.
#[test]
fn the_save_button_shortens_on_a_narrow_screen() {
    let mut f = Fixture::new(&roles());
    assert!(shows(&mut f.page, 100, 40, "Save changes (s)"));
    let rows = screen(&drawn(&mut f.page, 70, 40));
    assert_eq!(
        rows[39],
        "  Edit (e)   Add (a)   Delete (d)   Save (s)   Keys (?)   Quit (q)"
    );
}

#[test]
fn snapshot_changes_panel_110x30() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    type_into(&mut f.page, "er");
    press(&mut f.page, KeyCode::Enter);
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('d'));
    assert_snapshot!(drawn(&mut f.page, 110, 30).backend());
}

#[test]
fn snapshot_changes_band_80x24() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    select(&mut f.page, "▾ Komodo");
    press(&mut f.page, KeyCode::Char('d'));
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_edit_80x24() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_add_80x24() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('a'));
    type_into(&mut f.page, "port");
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_save_80x24() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_quit_80x24() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('q'));
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_outcome_80x24() {
    let mut f = Fixture::new(&roles());
    f.page.popup = Popup::Outcome {
        lines: dialog::outcome_lines(&BTreeMap::from([
            ("homepage".to_owned(), RoleOutcome::Applied),
            (
                "komodo".to_owned(),
                RoleOutcome::Drifted { reloaded: false },
            ),
            ("plex".to_owned(), RoleOutcome::Unattempted),
        ])),
        all_saved: false,
    };
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_waiting_80x24() {
    let mut f = Fixture::new(&roles());
    f.page.popup = Popup::Waiting {
        role: "homepage".into(),
        waited: Duration::from_secs(3),
    };
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_dialog_drift_80x24() {
    let mut f = Fixture::new(&roles());
    let before = f.page.roles[1].clone();
    let mut after = before.clone();
    after.instances[0].facts.push(Fact {
        key: "port".into(),
        value: "1".into(),
    });
    let (reply, _answer) = std::sync::mpsc::channel();
    f.page.popup = Popup::Drift {
        role: "komodo".into(),
        drift: Box::new(sb_config::facts::session::Drift {
            before,
            after: Some(after),
        }),
        reply,
        scroll: tui_scrollview::ScrollViewState::default(),
    };
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

/// Quit's Save, then Reload at a drift: the reloaded app's changes are
/// gone, not saved, so the editor stays open to make them again.
#[test]
fn quit_s_save_with_a_reload_stays_open() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    std::fs::write(f.dir.path().join("komodo.ini"), "[komodo]\nport = 1\n").expect("write");
    press(&mut f.page, KeyCode::Char('q'));
    press(&mut f.page, KeyCode::Char('y'));
    f.wait_for_dialog();
    press(&mut f.page, KeyCode::Char('r'));
    f.settle();
    assert!(shows(&mut f.page, 100, 40, " Not everything was saved "));
    assert_eq!(press(&mut f.page, KeyCode::Enter), Flow::Redraw);
    assert!(matches!(f.page.popup, Popup::None));
}

/// Quit's Save that leaves changes staged (a cancelled wait): the editor
/// stays open with them.
#[test]
fn quit_s_save_that_saves_nothing_stays_open_with_the_changes() {
    let mut f = Fixture::new(&roles());
    let lock = std::fs::File::create(f.dir.path().join("homepage.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('q'));
    press(&mut f.page, KeyCode::Char('y'));
    f.wait_for_dialog();
    press(&mut f.page, KeyCode::Esc);
    f.settle();
    assert_eq!(press(&mut f.page, KeyCode::Enter), Flow::Redraw);
    assert_eq!(f.page.pending.len(), 1);
    drop(lock);
}

/// The screen going (Ctrl-C, SIGTERM) with a drift question open: the
/// question goes with the dialog, the save answers it Cancel, writes
/// nothing for the role and hands the session back.
#[test]
fn teardown_with_a_drift_open_writes_nothing() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    let disk = "[komodo]\nport = 1\n";
    std::fs::write(f.dir.path().join("komodo.ini"), disk).expect("write");
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    assert!(matches!(f.page.popup, Popup::Drift { .. }));
    // `browse`'s teardown.
    let running = f.page.saving.take().expect("saving");
    f.page.popup = Popup::None;
    running.stop();
    let (_, outcomes) = running.finish().expect("the session back");
    assert!(matches!(
        outcomes["komodo"],
        RoleOutcome::Drifted { reloaded: false }
    ));
    assert_eq!(f.file("komodo").as_deref(), Some(disk));
}

/// A save whose thread ended without its outcome: the dialog says so,
/// and a later save says why it can't run.
#[test]
fn a_lost_save_says_so() {
    let mut f = Fixture::new(&roles());
    f.page.quit_after_save = true;
    f.page.session = None;
    f.page.save_lost();
    assert!(!f.page.quit_after_save);
    assert!(shows(
        &mut f.page,
        100,
        40,
        "The save stopped unexpectedly."
    ));
    press(&mut f.page, KeyCode::Enter);
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    // No session: nothing checks, the change is staged; the save can't run.
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Char('y'));
    assert!(
        f.page
            .status
            .starts_with("Can't save: the save before stopped unexpectedly")
    );
}

/// A new secret's value box shows what is typed, as the edit box does.
#[test]
fn a_new_secret_s_value_box_shows_what_is_typed() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('a'));
    type_into(&mut f.page, "api_key");
    press(&mut f.page, KeyCode::Enter);
    type_into(&mut f.page, "typed-secret");
    assert!(shows(&mut f.page, 100, 40, "typed-secret"));
    assert!(!shows(&mut f.page, 100, 40, "Ctrl+S"));
}

/// Inside a deleted app, Delete can't act (its button is faint); on the
/// app, Keep can.
#[test]
fn delete_is_faint_inside_a_deleted_app() {
    let mut f = Fixture {
        page: EditStyle::page(),
        dir: tempfile::tempdir().expect("temp dir"),
    };
    let faint = colours(
        f.page
            .style(Role::Faint)
            .patch(f.page.bg(Role::ButtonDisabledBg)),
    );
    let on = colours(f.page.style(Role::Text).patch(f.page.bg(Role::ButtonBg)));
    select(&mut f.page, "▾ Komodo");
    press(&mut f.page, KeyCode::Char('d'));
    // Delete (or Keep), after Edit, Add and Copy.
    let at = |p: &mut Page| cell(drawn(p, 100, 40).backend().buffer(), 34, 39);
    assert_eq!(at(&mut f.page), on);
    press(&mut f.page, KeyCode::Down);
    assert_eq!(at(&mut f.page), faint);
}

/// A message set by the key stays even when that key moves the
/// selection: undoing an addition removes its row.
#[test]
fn undo_s_message_stays_when_its_row_goes() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('a'));
    type_into(&mut f.page, "port");
    press(&mut f.page, KeyCode::Enter);
    type_into(&mut f.page, "1");
    press(&mut f.page, KeyCode::Enter);
    select(&mut f.page, "port");
    press(&mut f.page, KeyCode::Char('u'));
    assert_eq!(f.page.status, "Undid the change to plex · plex2 · port");
}

/// Many apps in the outcome: as many as fit, the rest counted, the
/// button still there.
#[test]
fn a_long_outcome_counts_what_does_not_fit() {
    let mut f = Fixture::new(&roles());
    let outcomes: BTreeMap<String, RoleOutcome> = (0..30)
        .map(|n| (format!("app{n:02}"), RoleOutcome::Applied))
        .collect();
    f.page.popup = Popup::Outcome {
        lines: dialog::outcome_lines(&outcomes),
        all_saved: true,
    };
    let rows = screen(&drawn(&mut f.page, 80, 24)).join("\n");
    assert!(rows.contains("… and 13 more"), "{rows}");
    assert!(rows.contains("OK (Enter)"), "{rows}");
}

/// The drift dialog on a screen too small for its text draws what fits.
#[test]
fn the_drift_dialog_fits_a_tiny_screen() {
    let mut f = Fixture::new(&roles());
    let before = f.page.roles[1].clone();
    let (reply, _answer) = std::sync::mpsc::channel();
    f.page.popup = Popup::Drift {
        role: "komodo".into(),
        drift: Box::new(sb_config::facts::session::Drift {
            before,
            after: None,
        }),
        reply,
        scroll: tui_scrollview::ScrollViewState::default(),
    };
    for (width, height) in [(20, 4), (3, 3), (80, 5)] {
        drawn(&mut f.page, width, height);
    }
}

/// The outcome's lines for a failed and an unsynced app name the cause.
#[test]
fn failed_and_unsynced_outcomes_name_the_cause() {
    use sb_config::facts::apply::SaveError;
    let lines = dialog::outcome_lines(&BTreeMap::from([
        ("a".to_owned(), RoleOutcome::Failed(SaveError::LockTimeout)),
        ("b".to_owned(), RoleOutcome::Unsynced(SaveError::Changed)),
    ]));
    assert_eq!(
        lines[0],
        (
            "a: failed: timed out waiting for Saltbox's lock".to_owned(),
            Role::Problem
        )
    );
    assert!(
        lines[1]
            .0
            .starts_with("b: saved, but the folder couldn't be synced"),
        "{:?}",
        lines[1]
    );
    assert_eq!(lines[1].1, Role::Warn);
}

// --- the mouse and the right-click menu (M56) ---

fn mouse_at(kind: crossterm::event::MouseEventKind, at: ratatui::layout::Position) -> Input {
    Input::Mouse(crossterm::event::MouseEvent {
        kind,
        column: at.x,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    })
}

fn left_click(page: &mut Page, at: ratatui::layout::Position) -> Flow {
    page.input(mouse_at(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        at,
    ))
}

fn right_click(page: &mut Page, at: ratatui::layout::Position) -> Flow {
    page.input(mouse_at(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right),
        at,
    ))
}

/// Where `text` is on the page drawn at 100×40 (its first cell), so a
/// click lands where the screen shows it.
fn find(page: &mut Page, text: &str) -> ratatui::layout::Position {
    find_at(page, 100, 40, text)
}

/// Where `text` is on the page drawn at `width`×`height`.
fn find_at(page: &mut Page, width: u16, height: u16, text: &str) -> ratatui::layout::Position {
    let rows = screen(&drawn(page, width, height));
    for (y, row) in rows.iter().enumerate() {
        if let Some(byte) = row.find(text) {
            let x = Span::raw(&row[..byte]).width();
            return ratatui::layout::Position::new(
                u16::try_from(x).expect("a column"),
                u16::try_from(y).expect("a row"),
            );
        }
    }
    panic!("{text}: {rows:?}");
}

/// A click selects the row under it; a second click on it is Enter.
#[test]
fn a_click_selects_and_a_second_click_activates() {
    let mut f = Fixture::new(&roles());
    let at = find(&mut f.page, "theme");
    assert_eq!(left_click(&mut f.page, at), Flow::Redraw);
    assert_eq!(f.page.selected(), Some(Row::Setting((0, 0, 1))));
    assert!(matches!(f.page.popup, Popup::None));
    left_click(&mut f.page, at);
    assert!(matches!(f.page.popup, Popup::Edit { .. }));
    press(&mut f.page, KeyCode::Esc);
    // On an app's heading the second click folds it.
    let at = find(&mut f.page, "▾ Komodo");
    left_click(&mut f.page, at);
    left_click(&mut f.page, at);
    assert!(f.page.folded.contains("komodo"));
    // A click below the last row leaves the selection as it was.
    let below = ratatui::layout::Position::new(5, 30);
    left_click(&mut f.page, below);
    assert_eq!(f.page.selected(), Some(Row::App(1)));
}

/// The wheel over the list moves the selection; elsewhere it doesn't.
#[test]
fn the_wheel_moves_the_selection_over_the_list() {
    let mut f = Fixture::new(&roles());
    let at = find(&mut f.page, "theme");
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::ScrollDown, at));
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::ScrollDown, at));
    assert_eq!(f.page.list.selected(), Some(2));
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::ScrollUp, at));
    assert_eq!(f.page.list.selected(), Some(1));
    let search = find(&mut f.page, "Search");
    assert_eq!(
        f.page.input(mouse_at(
            crossterm::event::MouseEventKind::ScrollDown,
            search
        )),
        Flow::Unchanged
    );
    assert_eq!(f.page.list.selected(), Some(1));
}

/// A click on the search box gives it the keys; a click on the list takes
/// them back.
#[test]
fn a_click_on_the_search_box_focuses_it() {
    let mut f = Fixture::new(&roles());
    let at = find(&mut f.page, "Search");
    left_click(&mut f.page, at);
    assert!(f.page.searching);
    type_text(&mut f.page, "plex");
    assert_eq!(f.page.query, "plex");
    let row = find(&mut f.page, "▾ Plex");
    left_click(&mut f.page, row);
    assert!(!f.page.searching);
}

/// A click on a bottom button does what its key does; a faint one
/// doesn't act.
#[test]
fn a_click_on_a_button_acts() {
    let mut f = Fixture::new(&roles());
    // On an app's heading Copy can't act.
    let copy = find(&mut f.page, "Copy (c)");
    left_click(&mut f.page, copy);
    assert_eq!(f.page.status, "");
    let delete = find(&mut f.page, "Delete (d)");
    left_click(&mut f.page, delete);
    assert_eq!(f.page.pending.len(), 1);
    let save = find(&mut f.page, "Save changes (s)");
    left_click(&mut f.page, save);
    assert!(matches!(f.page.popup, Popup::ConfirmSave));
    // The dialog's Cancel, by its button.
    let cancel = find(&mut f.page, "Cancel (n)");
    left_click(&mut f.page, cancel);
    assert!(matches!(f.page.popup, Popup::None));
}

/// A click on a change's undo in the panel undoes it.
#[test]
fn a_click_on_undo_undoes_its_change() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    // The panel lists homepage's change first.
    let undo = find(&mut f.page, " undo ");
    left_click(
        &mut f.page,
        ratatui::layout::Position::new(undo.x + 1, undo.y),
    );
    assert_eq!(
        f.page.pending.keys().collect::<Vec<_>>(),
        [&setting_target("komodo", "komodo", "jwt_secret")]
    );
}

/// Quit's dialog by its buttons: Discard quits.
#[test]
fn the_quit_dialog_s_discard_button_quits() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('q'));
    let discard = find(&mut f.page, "Discard (d)");
    assert_eq!(left_click(&mut f.page, discard), Flow::Quit);
}

/// The drift dialog's Reload, by its button.
#[test]
fn the_drift_dialog_s_reload_button_reloads() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "jwt_secret");
    press(&mut f.page, KeyCode::Char('d'));
    std::fs::write(f.dir.path().join("komodo.ini"), "[komodo]\nport = 1\n").expect("write");
    press(&mut f.page, KeyCode::Char('s'));
    press(&mut f.page, KeyCode::Enter);
    f.wait_for_dialog();
    let reload = find(&mut f.page, "Reload (r)");
    left_click(&mut f.page, reload);
    f.settle();
    assert!(shows(
        &mut f.page,
        100,
        40,
        "komodo: changed on disk; reloaded"
    ));
    // Under a dialog, the page takes no clicks.
    let row = find(&mut f.page, "▾ Homepage");
    assert_eq!(left_click(&mut f.page, row), Flow::Unchanged);
}

/// A right click opens the row's menu at the pointer; a hover lights an
/// item, a click on it acts; a click elsewhere closes the menu.
#[test]
fn a_right_click_opens_the_row_s_menu() {
    let mut f = Fixture::new(&roles());
    let at = find(&mut f.page, "database_password");
    right_click(&mut f.page, at);
    assert_eq!(f.page.selected(), Some(Row::Setting((1, 0, 0))));
    let rows = screen(&drawn(&mut f.page, 100, 40));
    let menu: Vec<&String> = rows.iter().filter(|r| r.contains('│')).collect();
    // Just under the clicked row, whose name stays in sight (the
    // maintainer, 2026-11-17).
    let y = usize::from(at.y);
    assert!(rows[y].contains("database_password"), "{rows:?}");
    assert!(rows[y + 1].contains('╭'), "{rows:?}");
    assert!(rows[y + 2].contains("Copy"), "{rows:?}");
    assert!(
        menu.iter().any(|r| r.contains("Copy") && r.contains('c')),
        "{rows:?}"
    );
    // Copy, Edit, Delete: no Undo change without a change.
    assert_eq!(
        f.page
            .menu_items(Row::Setting((1, 0, 0)))
            .iter()
            .map(|(l, k, _)| (l.as_str(), *k))
            .collect::<Vec<_>>(),
        [("Copy", 'c'), ("Edit", 'e'), ("Delete", 'd')]
    );
    // A hover over Delete lights it; a click on it deletes.
    let delete = find(&mut f.page, "Delete d");
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::Moved, delete));
    assert_eq!(f.page.menu.expect("open").lit, 2);
    left_click(&mut f.page, delete);
    assert!(f.page.menu.is_none());
    assert_eq!(
        f.page
            .pending
            .get(&setting_target("komodo", "komodo", "database_password")),
        Some(&Pending::Delete)
    );
    // Another right click, then a click outside: the menu closes, nothing
    // happens.
    right_click(&mut f.page, at);
    assert!(f.page.menu.is_some());
    left_click(&mut f.page, ratatui::layout::Position::new(0, 39));
    assert!(f.page.menu.is_none());
    assert_eq!(f.page.pending.len(), 1);
    // A right click below the rows opens nothing.
    right_click(&mut f.page, ratatui::layout::Position::new(5, 30));
    assert!(f.page.menu.is_none());
}

/// `m` opens the menu on the selected row: Up and Down light an item,
/// Enter does it; an item's key works with the menu open; Esc closes it.
#[test]
fn m_opens_the_menu_from_the_keyboard() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('m'));
    // From the keyboard too, the menu opens just under the row.
    let rows = screen(&drawn(&mut f.page, 100, 40));
    let row = rows
        .iter()
        .position(|r| r.contains("theme"))
        .expect("the row");
    // The menu's top edge (its corner can fall on a wide character's
    // second cell in the row below).
    assert!(rows[row + 1].contains('╮'), "{rows:?}");
    assert_eq!(
        f.page
            .menu_items(Row::Setting((0, 0, 1)))
            .iter()
            .map(|(l, _, _)| l.as_str())
            .collect::<Vec<_>>(),
        ["Copy", "Edit", "Delete"]
    );
    press(&mut f.page, KeyCode::Down);
    press(&mut f.page, KeyCode::Enter);
    assert!(f.page.menu.is_none());
    assert!(matches!(f.page.popup, Popup::Edit { .. }));
    type_into(&mut f.page, "er");
    press(&mut f.page, KeyCode::Enter);
    // After an edit, Undo change is offered; its key works in the menu.
    press(&mut f.page, KeyCode::Menu);
    assert!(
        f.page
            .menu_items(Row::Setting((0, 0, 1)))
            .iter()
            .any(|(l, k, _)| l == "Undo change" && *k == 'u')
    );
    press(&mut f.page, KeyCode::Char('u'));
    assert!(f.page.menu.is_none());
    assert!(f.page.pending.is_empty());
    press(&mut f.page, KeyCode::Char('m'));
    press(&mut f.page, KeyCode::Up);
    assert_eq!(f.page.menu.expect("open").lit, 0);
    // A key that is no item's does nothing.
    assert_eq!(press(&mut f.page, KeyCode::Char('x')), Flow::Unchanged);
    press(&mut f.page, KeyCode::Esc);
    assert!(f.page.menu.is_none());
}

/// A copy's and an app's menu: Delete, or Keep once deleted.
#[test]
fn copy_and_app_menus_delete_or_keep() {
    let mut f = Fixture::new(&roles());
    let names = |p: &Page, row| -> Vec<String> {
        p.menu_items(row).into_iter().map(|(l, _, _)| l).collect()
    };
    assert_eq!(names(&f.page, Row::Copy(2, 1)), ["Delete copy"]);
    assert_eq!(names(&f.page, Row::App(2)), ["Delete app"]);
    select(&mut f.page, "▾ Plex");
    press(&mut f.page, KeyCode::Char('d'));
    assert_eq!(names(&f.page, Row::App(2)), ["Keep app"]);
    // Inside the deleted app, a setting has no Edit.
    assert!(!names(&f.page, Row::Setting((2, 0, 1))).contains(&"Edit".to_owned()));
}

#[test]
fn snapshot_menu_80x24() {
    let mut f = Fixture::new(&roles());
    let at = find_at(&mut f.page, 80, 24, "database_password");
    right_click(&mut f.page, at);
    assert_snapshot!(drawn(&mut f.page, 80, 24).backend());
}

#[test]
fn snapshot_menu_from_the_keyboard_110x30() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('m'));
    assert_snapshot!(drawn(&mut f.page, 110, 30).backend());
}

/// The remaining dialog buttons, by click: the save question's Save, the
/// waiting dialog's Cancel, the outcome's OK.
#[test]
fn the_save_waiting_and_outcome_buttons_click() {
    let mut f = Fixture::new(&roles());
    let lock = std::fs::File::create(f.dir.path().join("homepage.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('d'));
    press(&mut f.page, KeyCode::Char('s'));
    let save = find(&mut f.page, "Save (y)");
    left_click(&mut f.page, save);
    assert!(f.page.saving.is_some());
    f.wait_for_dialog();
    let cancel = find(&mut f.page, "Cancel (Esc)");
    left_click(&mut f.page, cancel);
    f.settle();
    assert!(matches!(f.page.popup, Popup::Outcome { .. }));
    let ok = find(&mut f.page, "OK (Enter)");
    left_click(&mut f.page, ok);
    assert!(matches!(f.page.popup, Popup::None));
    drop(lock);
}

/// The wheel scrolls the drift dialog's text, as its arrow keys do.
#[test]
fn the_wheel_scrolls_the_drift_dialog() {
    let mut f = Fixture::new(&roles());
    let before = f.page.roles[2].clone();
    let (reply, _answer) = std::sync::mpsc::channel();
    f.page.popup = Popup::Drift {
        role: "plex".into(),
        drift: Box::new(sb_config::facts::session::Drift {
            before: before.clone(),
            after: Some(before),
        }),
        reply,
        scroll: tui_scrollview::ScrollViewState::default(),
    };
    drawn(&mut f.page, 80, 14);
    let at = ratatui::layout::Position::new(20, 6);
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::ScrollDown, at));
    let Popup::Drift { scroll, .. } = &f.page.popup else {
        panic!("the drift dialog");
    };
    assert_eq!(scroll.offset().y, 1);
    f.page
        .input(mouse_at(crossterm::event::MouseEventKind::ScrollUp, at));
    let Popup::Drift { scroll, .. } = &f.page.popup else {
        panic!("the drift dialog");
    };
    assert_eq!(scroll.offset().y, 0);
}

/// A click on a button while the search box has focus gives the page its
/// keys back, as Enter in the box does.
#[test]
fn a_button_click_ends_the_search_s_focus() {
    let mut f = Fixture::new(&roles());
    press(&mut f.page, KeyCode::Char('/'));
    let quit = find(&mut f.page, "Quit (q)");
    assert_eq!(left_click(&mut f.page, quit), Flow::Quit);
    assert!(!f.page.searching);
}

/// Pointer moves with no menu open do nothing.
#[test]
fn pointer_moves_without_a_menu_do_nothing() {
    let mut f = Fixture::new(&roles());
    let at = find(&mut f.page, "theme");
    assert_eq!(
        f.page
            .input(mouse_at(crossterm::event::MouseEventKind::Moved, at)),
        Flow::Unchanged
    );
}

// --- pastes (bracketed paste, asked for after M55) ---

fn paste(page: &mut Page, text: &str) -> Flow {
    page.input(Input::Paste(text.into()))
}

/// A paste into the edit box is one input, its lines kept: a newline in
/// it doesn't stage the value, and what follows it isn't keys. Enter
/// stages it.
#[test]
fn a_paste_into_the_edit_box_stages_nothing_until_enter() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "theme");
    press(&mut f.page, KeyCode::Char('e'));
    paste(&mut f.page, "-blue\nq");
    assert!(matches!(f.page.popup, Popup::Edit { .. }));
    assert!(f.page.pending.is_empty());
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(
        f.page
            .pending
            .get(&setting_target("homepage", "homepage", "theme")),
        Some(&Pending::Set("dark-blue\nq".into()))
    );
}

/// A pasted token with a carriage return and a last newline, as a
/// terminal may send it, goes into a value box as its one line.
#[test]
fn a_pasted_token_s_line_ending_stays_out_of_the_value() {
    let mut f = Fixture::new(&roles());
    select(&mut f.page, "    plex2");
    press(&mut f.page, KeyCode::Char('a'));
    paste(&mut f.page, "api_key\r\n");
    assert!(matches!(f.page.popup, Popup::AddKey { .. }));
    press(&mut f.page, KeyCode::Enter);
    paste(&mut f.page, "tok-123\r\n");
    press(&mut f.page, KeyCode::Enter);
    assert_eq!(
        f.page
            .pending
            .get(&setting_target("plex", "plex2", "api_key")),
        Some(&Pending::Set("tok-123".into()))
    );
}

/// A paste into the search box narrows the page as typing does; on the
/// page itself, a paste does nothing (its letters aren't keys).
#[test]
fn a_paste_goes_into_the_search_box_and_never_acts_on_the_page() {
    let mut f = Fixture::new(&roles());
    assert_eq!(paste(&mut f.page, "dq"), Flow::Unchanged);
    assert!(f.page.pending.is_empty());
    press(&mut f.page, KeyCode::Char('/'));
    paste(&mut f.page, "plex\n");
    assert_eq!(f.page.query, "plex");
    assert!(f.page.searching);
}

/// With no room under the row (the list's last line), the menu opens just
/// over it instead, so the row's name still shows.
#[test]
fn with_no_room_below_the_menu_opens_over_the_row() {
    let mut f = Fixture::new(&roles());
    // 100×19: the list's 15 rows fill it; the last one is plex2's
    // transcode_path, on line 17, with the status line and the buttons
    // under it.
    let at = find_at(&mut f.page, 100, 19, "/mnt/local/transcodes2");
    right_click(&mut f.page, at);
    let terminal = drawn(&mut f.page, 100, 19);
    let rows = screen(&terminal);
    let y = usize::from(at.y);
    assert!(rows[y].contains("/mnt/local/transcodes2"), "{rows:?}");
    assert!(rows[y - 1].contains('╯'), "{rows:?}");
    // The click and hover targets are the rectangle drawn.
    assert_eq!(f.page.hits.menu.bottom(), at.y);
    // From the keyboard on the same row, the same.
    press(&mut f.page, KeyCode::Esc);
    press(&mut f.page, KeyCode::Char('m'));
    let rows = screen(&drawn(&mut f.page, 100, 19));
    assert!(rows[y].contains("/mnt/local/transcodes2"), "{rows:?}");
    assert_eq!(f.page.hits.menu.bottom(), at.y);
}

/// The buttons fit the row by dropping the least used first (Copy, Add,
/// Delete, Edit), "Save changes" shortening to "Save" before any goes;
/// Save, Keys and Quit always stay (tui spec, "Keys", Hints).
#[test]
fn the_buttons_fit_the_width_and_keep_save_keys_and_quit() {
    let mut p = page();
    let last = |p: &mut Page, width| screen(&drawn(p, width, 24))[23].clone();
    assert_eq!(
        last(&mut p, 120),
        "  Edit (e)   Add (a)   Copy (c)   Delete (d)   Save changes (s)   Keys (?)   Quit (q)"
    );
    // 80 columns leave 78 for the row: Copy goes first.
    assert_eq!(
        last(&mut p, 80),
        "  Edit (e)   Add (a)   Delete (d)   Save (s)   Keys (?)   Quit (q)"
    );
    assert_eq!(
        last(&mut p, 60),
        "  Edit (e)   Delete (d)   Save (s)   Keys (?)   Quit (q)"
    );
    assert_eq!(
        last(&mut p, 50),
        "  Edit (e)   Save (s)   Keys (?)   Quit (q)"
    );
    assert_eq!(last(&mut p, 40), "  Save (s)   Keys (?)   Quit (q)");
}

/// `?` (or F1, or a click on Keys) opens the keys overlay; any other key
/// closes it and does nothing else, so `q` doesn't quit; in the search box
/// `?` is typed and F1 still opens it.
#[test]
fn question_mark_opens_the_keys() {
    let mut p = page();
    assert_eq!(press(&mut p, KeyCode::Char('?')), Flow::Redraw);
    let all = screen(&drawn(&mut p, 100, 40)).join("\n");
    assert!(all.contains("Keys: fact editor"), "{all}");
    assert!(all.contains("s Ctrl+S"), "{all}");
    assert_eq!(press(&mut p, KeyCode::Char('q')), Flow::Redraw);
    assert!(
        !screen(&drawn(&mut p, 100, 40))
            .join("\n")
            .contains("Keys: fact")
    );
    press(&mut p, KeyCode::Char('/'));
    press(&mut p, KeyCode::Char('?'));
    assert_eq!(p.query, "?");
    press(&mut p, KeyCode::F(1));
    assert!(
        screen(&drawn(&mut p, 100, 40))
            .join("\n")
            .contains("Keys: fact")
    );
    press(&mut p, KeyCode::Esc);
    press(&mut p, KeyCode::Esc);
    press(&mut p, KeyCode::Esc);
    let at = find(&mut p, "Keys (?)");
    left_click(&mut p, at);
    assert!(
        screen(&drawn(&mut p, 100, 40))
            .join("\n")
            .contains("Keys: fact")
    );
}

#[test]
fn snapshot_keys_80x24() {
    let mut p = page();
    press(&mut p, KeyCode::Char('?'));
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}

#[test]
fn snapshot_keys_50x19() {
    let mut p = page();
    press(&mut p, KeyCode::Char('?'));
    assert_snapshot!(drawn(&mut p, 50, 19).backend());
}
