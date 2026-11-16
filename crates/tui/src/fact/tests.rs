//! The fact editor's page (tui spec, editors, Testing Strategy, "Fact
//! screen"): the headings and settings, folding, search with and without
//! matches, masking and Show, Copy's OSC 52 payload, the run log without
//! a value, and `TestBackend` snapshots at 110 and 80 columns.

use std::sync::{Arc, Mutex};

use insta::assert_snapshot;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use sb_cli_core::theme::{Palette, Profile};
use sb_config::facts::{Fact, Instance};

use super::*;

/// A known secret: no buffer holds it until Show, no log ever.
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

fn page_with(roles: Vec<App>, sink: &Sink) -> Page {
    Page::new(
        roles,
        Theme::resolve(&Palette::default(), Profile::Ascii),
        Box::new(sink.clone()),
    )
}

fn page() -> Page {
    page_with(roles(), &Sink::default())
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
            "    database_password  ••••••••",
            "    jwt_secret         ••••••••",
            "  ▾ Plex",
            "    plex",
            "      claim_token      ••••••••",
            "      transcode_path   /mnt/local/transcodes",
            "    plex2",
            "      claim_token      ••••••••",
            "      transcode_path   /mnt/local/transcodes2",
            "",
        ]
    );
    assert_eq!(rows[23], "  Copy (c)   Show (s)   Quit (q)");
}

/// A copy heading shows for one copy not named after its app, too.
#[test]
fn a_lone_copy_not_named_after_its_app_has_a_heading() {
    let mut p = page_with(
        vec![role(
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
        vec![role(
            "app\x1b]0;x\x07",
            vec![("app\x1b]0;x\x07", vec![fact("k\x1b[2J", "v\x1b[31mred")])],
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
        vec![role(
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

/// No known secret in any buffer drawn until Show, which reveals the
/// selected value only, until the selection moves.
#[test]
fn secrets_are_masked_until_show_and_only_while_selected() {
    let mut p = page();
    let contains = |p: &mut Page| {
        drawn(p, 80, 24)
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>()
            .contains(SECRET)
    };
    assert!(!contains(&mut p));
    // Komodo's database_password: Show reveals it, Hide masks it again.
    down(&mut p, 5);
    assert_eq!(p.selected(), Some(Row::Setting((1, 0, 0))));
    press(&mut p, KeyCode::Char('s'));
    assert!(contains(&mut p));
    assert!(shows(&mut p, 80, 24, "Hide (s)"));
    // The other secret stays masked.
    assert!(!shows(&mut p, 80, 24, "not-a-real-jwt-secret"));
    press(&mut p, KeyCode::Char('s'));
    assert!(!contains(&mut p));
    // Shown, then the selection moves: masked again.
    press(&mut p, KeyCode::Char('s'));
    press(&mut p, KeyCode::Down);
    assert!(!contains(&mut p));
    press(&mut p, KeyCode::Up);
    assert!(!contains(&mut p));
}

/// Show and Copy act on settings only; on a heading they say so.
#[test]
fn show_and_copy_on_a_heading_say_to_pick_a_setting() {
    let sink = Sink::default();
    let mut p = page_with(roles(), &sink);
    press(&mut p, KeyCode::Char('s'));
    assert!(shows(&mut p, 80, 24, "Pick a setting to show"));
    press(&mut p, KeyCode::Char('c'));
    assert!(shows(&mut p, 80, 24, "Pick a setting to copy"));
    assert_eq!(sink.text(), "");
}

/// Copy sends exactly the value through OSC 52, masked or not, and says it
/// can't confirm it worked.
#[test]
fn copy_sends_the_value_through_osc_52() {
    let sink = Sink::default();
    let mut p = page_with(roles(), &sink);
    drawn(&mut p, 80, 24);
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('c'));
    // base64 of "not-a-real-password".
    assert_eq!(sink.text(), "\x1b]52;c;bm90LWEtcmVhbC1wYXNzd29yZA==\x1b\\");
    // Whole on an 80-column screen.
    let rows = screen(&drawn(&mut p, 80, 24));
    assert_eq!(
        rows[22],
        " Copied database_password (if your terminal allows it; otherwise use Show)"
    );
    // Still masked.
    assert!(!rows.iter().any(|row| row.contains(SECRET)));
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
    let mut p = Page::new(roles(), Theme::plain(), Box::new(Broken));
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('c'));
    assert_eq!(p.status, "Couldn't copy database_password: no terminal");
}

/// The run log gets what was shown and copied, by name, never a value.
#[test]
fn the_log_names_what_was_shown_and_copied_without_a_value() {
    let log = Sink::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut p = page();
        drawn(&mut p, 80, 24);
        down(&mut p, 5);
        press(&mut p, KeyCode::Char('s'));
        drawn(&mut p, 80, 24);
        press(&mut p, KeyCode::Char('c'));
        press(&mut p, KeyCode::Char('/'));
        type_text(&mut p, "pass");
        drawn(&mut p, 80, 24);
    });
    let text = log.text();
    assert!(text.contains("fact shown"), "{text}");
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
    assert_eq!(rows[29], "  Copy (c)   Show (s)   Quit (q)");
}

/// Show on a value that isn't masked has nothing to do: it says so, logs
/// nothing, and its button is faint.
#[test]
fn show_on_a_value_that_is_not_hidden_says_so() {
    let mut p = page();
    down(&mut p, 2);
    assert_eq!(p.selected(), Some(Row::Setting((0, 0, 1))));
    press(&mut p, KeyCode::Char('s'));
    assert_eq!(p.shown, None);
    assert!(shows(&mut p, 80, 24, "This value isn't hidden"));
    assert!(!shows(&mut p, 80, 24, "Hide (s)"));
}

/// The buttons that can't act on the selected row are drawn faint; the
/// others aren't.
#[test]
fn buttons_that_cannot_act_are_faint() {
    let mut p = EditStyle::page();
    let faint = colours(p.style(Role::Faint).patch(p.bg(Role::ButtonDisabledBg)));
    let on = colours(p.style(Role::Text).patch(p.bg(Role::ButtonBg)));
    // On a heading: Copy (column 2) and Show (column 13) faint, Quit (24)
    // not.
    let terminal = drawn(&mut p, 80, 24);
    let buffer = terminal.backend().buffer();
    assert_eq!(cell(buffer, 2, 23), faint);
    assert_eq!(cell(buffer, 13, 23), faint);
    assert_eq!(cell(buffer, 24, 23), on);
    // On a secret: all three can act.
    down(&mut p, 5);
    let terminal = drawn(&mut p, 80, 24);
    let buffer = terminal.backend().buffer();
    assert_eq!(cell(buffer, 2, 23), on);
    assert_eq!(cell(buffer, 13, 23), on);
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
        Page::new(
            roles(),
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
        vec![role(
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
    let mut p = page_with(vec![role("lidarr", Vec::new())], &Sink::default());
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
        vec![role(
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
    let log = Sink::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut p = Page::new(roles(), Theme::plain(), Box::new(Broken));
        down(&mut p, 5);
        press(&mut p, KeyCode::Char('c'));
    });
    let text = log.text();
    assert!(text.contains("fact not copied"), "{text}");
    assert!(!text.contains("fact copied"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
}

/// The page's `Debug` holds no value.
#[test]
fn the_page_s_debug_holds_no_value() {
    let mut p = page();
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('s'));
    let debug = format!("{p:?}");
    assert!(debug.contains("database_password"), "{debug}");
    assert!(!debug.contains(SECRET), "{debug}");
}

#[test]
fn an_empty_catalog_says_so() {
    let mut p = page_with(Vec::new(), &Sink::default());
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

#[test]
fn snapshot_shown_80x24() {
    let mut p = page();
    down(&mut p, 5);
    press(&mut p, KeyCode::Char('s'));
    assert_snapshot!(drawn(&mut p, 80, 24).backend());
}
