//! The shared hints (tui spec, "Keys: one map for every screen", Hints
//! and The `?` overlay): the footer's fit at every width, the tail kept;
//! the overlay's opening, closing, scrolling and layout; and rule 1 over
//! every screen's table.

use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

const TAIL: [Hint; 2] = [("?", "keys"), ("q", "quit")];

const HINTS: [Hint; 4] = [
    ("Esc", "back"),
    ("/", "search"),
    ("s", "save"),
    ("p", "print"),
];

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn plain() -> Theme {
    Theme::plain()
}

#[test]
fn the_footer_right_aligns_the_tail_and_drops_hints_whole_from_the_end() {
    let line = footer(&plain(), &HINTS, &TAIL, 60);
    assert_eq!(
        text(&line),
        "Esc back · / search · s save · p print       ? keys · q quit"
    );
    let line = footer(&plain(), &HINTS, &TAIL, 50);
    assert_eq!(
        text(&line),
        "Esc back · / search · s save       ? keys · q quit"
    );
    // Three spaces at least before the tail: 46 columns hold three
    // hints, 45 two.
    let line = footer(&plain(), &HINTS, &TAIL, 46);
    assert_eq!(
        text(&line),
        "Esc back · / search · s save   ? keys · q quit"
    );
    let line = footer(&plain(), &HINTS, &TAIL, 45);
    assert_eq!(text(&line), "Esc back · / search           ? keys · q quit");
    assert_eq!(
        text(&footer(&plain(), &HINTS, &TAIL, 15)),
        "? keys · q quit"
    );
    // Narrower than the tail: `? keys` alone.
    assert_eq!(text(&footer(&plain(), &HINTS, &TAIL, 10)), "    ? keys");
}

/// At every width the row is exactly that wide, each hint whole or
/// absent, and the tail there from 15 columns.
#[test]
fn the_footer_fits_every_width_without_cutting_a_hint() {
    for width in 10..=240_u16 {
        let line = footer(&plain(), &HINTS, &TAIL, width);
        assert_eq!(line.width(), usize::from(width), "{width}");
        let shown = text(&line);
        let whole: Vec<String> = HINTS.iter().map(|(k, w)| format!("{k} {w}")).collect();
        if width >= 15 {
            let left = shown.strip_suffix("? keys · q quit").expect("the tail");
            for piece in left.trim_end().split(" · ").filter(|p| !p.is_empty()) {
                assert!(whole.iter().any(|w| w == piece), "{width}: {shown:?}");
            }
        } else {
            assert_eq!(shown.trim_start(), "? keys", "{width}");
        }
    }
}

/// The picker's footer with trouble (tui spec, "Hints" mock-ups): the
/// lead first and kept, three spaces, the hints that still fit, the tail;
/// the lead cut with `…` only when it alone is too wide.
#[test]
fn a_lead_comes_first_and_is_kept_whole_while_it_fits() {
    const PICKER: [Hint; 4] = [
        ("Enter", "open"),
        ("/", "filter"),
        ("Tab", "kind"),
        ("↑↓", "move"),
    ];
    let trouble = "Docker events stopped; retrying";
    let led = |width| {
        text(&footer_led(
            &plain(),
            Some((trouble, Style::default())),
            &PICKER,
            &TAIL,
            width,
        ))
    };
    assert_eq!(
        led(120),
        "Docker events stopped; retrying   Enter open · / filter · Tab kind · ↑↓ move                             ? keys · q quit"
    );
    assert_eq!(
        led(80),
        "Docker events stopped; retrying   Enter open · / filter          ? keys · q quit"
    );
    assert_eq!(
        led(50),
        "Docker events stopped; retrying    ? keys · q quit"
    );
    // Alone too wide: cut to the room the tail leaves.
    assert_eq!(led(40), "Docker events stopped…   ? keys · q quit");
    for width in 10..=240_u16 {
        let line = footer_led(
            &plain(),
            Some((trouble, Style::default())),
            &PICKER,
            &TAIL,
            width,
        );
        assert_eq!(line.width(), usize::from(width), "{width}");
        // The whole tail from 15 columns; the lead (or its `…`) first once
        // there is room beside it, from 19.
        let shown = text(&line);
        if width >= 15 {
            assert!(shown.ends_with("? keys · q quit"), "{width}: {shown:?}");
        }
        if width >= 19 {
            // The lead cut to what fits: its first letter on, or `…`.
            assert!(
                shown.starts_with('D') || shown.starts_with('…'),
                "{width}: {shown:?}"
            );
        } else {
            assert!(!shown.contains('…'), "{width}: {shown:?}");
        }
    }
    // No lead: the plain footer.
    assert_eq!(
        footer_led(&plain(), None, &PICKER, &TAIL, 80),
        footer(&plain(), &PICKER, &TAIL, 80)
    );
}

const GROUPS: &[Group] = &[
    Group {
        title: "Move",
        keys: &[
            Key {
                keys: &["↑", "↓", "k", "j"],
                what: "a line",
                meaning: Meaning::Move,
            },
            Key {
                keys: &["PgUp", "PgDn", "b", "Space"],
                what: "a page",
                meaning: Meaning::Page,
            },
        ],
    },
    Group {
        title: "Find",
        keys: &[Key {
            keys: &["/"],
            what: "search",
            meaning: Meaning::Find,
        }],
    },
    Group {
        title: "Leave",
        keys: &[Key {
            keys: &["q"],
            what: "quit",
            meaning: Meaning::Leave,
        }],
    },
];

const TEST_KEYS: Keys = Keys {
    screen: "test screen",
    groups: GROUPS,
    mouse: "the wheel scrolls",
};

fn plain_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn drawn(overlay: &mut Overlay, width: u16, height: u16) -> Vec<String> {
    drawn_keys(overlay, &TEST_KEYS, width, height)
}

/// [`drawn`] for `keys`.
fn drawn_keys(overlay: &mut Overlay, keys: &Keys, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| overlay.draw(frame, keys, &plain()))
        .expect("drawn");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// `?` opens it unless a text box takes the key; F1 always; Ctrl or Alt
/// with `?` doesn't.
#[test]
fn question_mark_and_f1_open_it() {
    assert!(Overlay::opens(&plain_key(KeyCode::Char('?')), false));
    assert!(!Overlay::opens(&plain_key(KeyCode::Char('?')), true));
    assert!(Overlay::opens(&plain_key(KeyCode::F(1)), true));
    let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert!(Overlay::opens(&shifted, false));
    let ctrl = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::CONTROL);
    assert!(!Overlay::opens(&ctrl, false));
    assert!(!Overlay::opens(&plain_key(KeyCode::Char('h')), false));
}

/// Any key that doesn't scroll closes it, `q` too, and nothing else
/// happens (the screen never sees the key); the scroll keys keep it open
/// only while it scrolls: drawn whole, they close it too ("any key
/// closes").
#[test]
fn any_other_key_closes_it() {
    let mut overlay = Overlay::default();
    assert!(!overlay.is_open());
    overlay.open();
    // Too tall for 30×8: it scrolls.
    drawn(&mut overlay, 30, 8);
    for code in [
        KeyCode::Down,
        KeyCode::Char('j'),
        KeyCode::PageDown,
        KeyCode::End,
    ] {
        assert_eq!(overlay.key(plain_key(code)), Flow::Redraw);
        assert!(overlay.is_open(), "{code:?}");
    }
    assert_eq!(overlay.key(plain_key(KeyCode::Char('q'))), Flow::Redraw);
    assert!(!overlay.is_open());
    overlay.open();
    drawn(&mut overlay, 30, 8);
    let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
    overlay.key(ctrl_j);
    assert!(!overlay.is_open(), "Ctrl-J is no scroll key");
    // Whole on 80×24: ↓ and Space close it as any key does.
    for code in [KeyCode::Down, KeyCode::Char(' ')] {
        overlay.open();
        drawn(&mut overlay, 80, 24);
        overlay.key(plain_key(code));
        assert!(!overlay.is_open(), "{code:?}");
    }
}

/// It lists every group and key of the screen, in a titled box, with the
/// mouse's line; it fits, so any key closes it.
#[test]
fn it_lists_the_screen_s_keys() {
    let mut overlay = Overlay::default();
    assert_eq!(drawn(&mut overlay, 80, 24).concat(), "", "closed: nothing");
    overlay.open();
    let rows = drawn(&mut overlay, 80, 24);
    let all = rows.join("\n");
    assert!(all.contains("╭ Keys: test screen "), "{all}");
    assert!(all.contains("│ Move"), "{all}");
    assert!(all.contains("│  ↑ ↓ k j            a line"), "{all}");
    assert!(all.contains("│  PgUp PgDn b Space  a page"), "{all}");
    assert!(all.contains("│ Mouse: the wheel scrolls"), "{all}");
    assert!(all.contains("╰ any key closes "), "{all}");
}

/// Too tall for the screen and too narrow for two columns: one column,
/// scrolled, saying so; the wheel scrolls; a click outside closes.
#[test]
fn a_short_screen_scrolls_it() {
    let mut overlay = Overlay::default();
    overlay.open();
    let rows = drawn(&mut overlay, 30, 8);
    let all = rows.join("\n");
    assert!(all.contains("↑↓ scroll"), "{all}");
    assert!(all.contains("Move"), "{all}");
    overlay.key(plain_key(KeyCode::End));
    let all = drawn(&mut overlay, 30, 8).join("\n");
    assert!(all.contains("Mouse"), "{all}");
    assert!(!all.contains(" Move"), "{all}");
    let wheel = |kind| MouseEvent {
        kind,
        column: 10,
        row: 3,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(overlay.mouse(wheel(MouseEventKind::ScrollUp)), Flow::Redraw);
    assert_eq!(
        overlay.mouse(wheel(MouseEventKind::Down(MouseButton::Left))),
        Flow::Unchanged,
        "a click inside"
    );
    assert!(overlay.is_open());
    let outside = MouseEvent {
        column: 0,
        row: 0,
        ..wheel(MouseEventKind::Down(MouseButton::Left))
    };
    assert_eq!(overlay.mouse(outside), Flow::Redraw);
    assert!(!overlay.is_open());
}

/// Too tall for one column but wide enough for two: the groups side by
/// side, the mouse's line under them.
#[test]
fn a_wide_short_screen_puts_the_groups_in_two_columns() {
    let mut overlay = Overlay::default();
    overlay.open();
    let rows = drawn(&mut overlay, 80, 11);
    let all = rows.join("\n");
    assert!(all.contains("any key closes"), "{all}");
    let moves = rows.iter().find(|r| r.contains("Move")).expect("Move");
    assert!(moves.contains("Leave") || moves.contains("Find"), "{all}");
    assert!(all.contains("Mouse: the wheel scrolls"), "{all}");
}

/// Every screen's key table, for rule 1.
fn every_screen() -> [&'static Keys; 8] {
    [
        &crate::picker::KEYS,
        &crate::viewer::KEYS,
        &crate::edit_picker::KEYS,
        &crate::fact::KEYS,
        &crate::announcements::KEYS,
        &crate::restore::FORM_KEYS,
        &crate::restore::RUN_KEYS,
        &crate::restore::REVIEW_KEYS,
    ]
}

/// Rule 1 (tui spec, "Keys: one map for every screen"): a key means the
/// same on every screen it works on, ← → and `h` `l` (rule 7's
/// direction) included as their one class.
#[test]
fn each_key_has_one_meaning_on_every_screen() {
    let mut meanings: std::collections::BTreeMap<&str, (Meaning, &str)> =
        std::collections::BTreeMap::new();
    for screen in every_screen() {
        for key in screen.groups.iter().flat_map(|g| g.keys) {
            for name in key.keys {
                let (meaning, first) =
                    *meanings.entry(name).or_insert((key.meaning, screen.screen));
                assert_eq!(
                    meaning, key.meaning,
                    "{name}: {meaning:?} on {first}, {:?} on {}",
                    key.meaning, screen.screen
                );
            }
        }
    }
    // No key saves on one screen and does anything else on another, and
    // none reveals anything: nothing is hidden (decision 2).
    assert_eq!(meanings.get("s").map(|m| m.0), Some(Meaning::Save));
    assert_eq!(meanings.get("v"), None);
    assert_eq!(meanings.get("r"), None);
}

/// Every screen lists its help key and how to leave.
#[test]
fn every_screen_lists_help_and_a_way_out() {
    for screen in every_screen() {
        let all: Vec<Meaning> = screen
            .groups
            .iter()
            .flat_map(|g| g.keys)
            .map(|k| k.meaning)
            .collect();
        assert!(all.contains(&Meaning::Help), "{}", screen.screen);
        assert!(all.contains(&Meaning::Stop), "{}", screen.screen);
    }
}

/// One column wraps each description at words to the box, its
/// continuation under the description column, never under the keys;
/// the mouse's line hangs under its text the same way.
#[test]
fn a_long_description_hangs_under_its_column() {
    const LONG: &[Group] = &[Group {
        title: "Show",
        keys: &[
            Key {
                keys: &["Tab", "Shift-Tab"],
                what: "all, services, containers, and every other kind",
                meaning: Meaning::Part,
            },
            Key {
                keys: &["w"],
                what: "wrap",
                meaning: Meaning::Wrap,
            },
        ],
    }];
    let lines: Vec<String> = column(LONG, &plain(), Some(40)).iter().map(text).collect();
    // `  Tab Shift-Tab  ` is 17 columns: the descriptions start there.
    assert_eq!(
        lines,
        [
            " Show",
            "  Tab Shift-Tab  all, services,",
            "                 containers, and every",
            "                 other kind",
            "  w              wrap",
        ]
    );
    assert!(lines.iter().all(|l| Span::raw(l.as_str()).width() <= 40));
    // Unwrapped without a width, and when the room left is under 8
    // columns: at 24 the room is 7, at 25 it is 8 and the text wraps.
    assert_eq!(column(LONG, &plain(), None).len(), 3);
    assert_eq!(column(LONG, &plain(), Some(20)).len(), 3);
    assert_eq!(column(LONG, &plain(), Some(24)).len(), 3);
    assert!(column(LONG, &plain(), Some(25)).len() > 3);
    let mouse: Vec<String> = mouse_lines(
        "the wheel scrolls; Shift+wheel goes sideways",
        &plain(),
        Some(30),
    )
    .iter()
    .map(text)
    .collect();
    assert_eq!(
        mouse,
        [
            " Mouse: the wheel scrolls;",
            "        Shift+wheel goes",
            "        sideways",
        ]
    );
}

/// Wide characters wrap by their display width: no line is wider than the
/// text's room, each continuation under the description column.
#[test]
fn wide_characters_hang_by_their_width() {
    const WIDE: &[Group] = &[Group {
        title: "Show",
        keys: &[Key {
            keys: &["Tab", "Shift-Tab"],
            what: "日本語の説明 です 長い説明文ですね",
            meaning: Meaning::Part,
        }],
    }];
    let lines: Vec<String> = column(WIDE, &plain(), Some(30)).iter().map(text).collect();
    assert!(lines.len() > 3, "{lines:?}");
    for (i, line) in lines.iter().enumerate() {
        assert!(Span::raw(line.as_str()).width() <= 30, "{lines:?}");
        if i > 1 {
            assert!(line.starts_with(&" ".repeat(17)), "{lines:?}");
            assert!(!line[17..].starts_with(' '), "{lines:?}");
        }
    }
}

/// Keys whose first description is exactly the room a 40-column screen's
/// box leaves it (`PgUp PgDn b Space` puts the descriptions at column 21;
/// the box's inside is 36, so 15 columns), and enough rows to scroll at 8
/// rows but not at 24.
const HANGING: Keys = Keys {
    screen: "test screen",
    groups: &[
        Group {
            title: "Move",
            keys: &[
                Key {
                    keys: &["PgUp", "PgDn", "b", "Space"],
                    what: "abcdefgh abcdef",
                    meaning: Meaning::Page,
                },
                Key {
                    keys: &["↑", "↓"],
                    what: "a line",
                    meaning: Meaning::Move,
                },
            ],
        },
        Group {
            title: "Leave",
            keys: &[
                Key {
                    keys: &["q"],
                    what: "quit",
                    meaning: Meaning::Leave,
                },
                Key {
                    keys: &["?"],
                    what: "these keys",
                    meaning: Meaning::Help,
                },
            ],
        },
    ],
    mouse: "the wheel scrolls; Shift+wheel goes sideways",
};

/// Scrolled, the column is wrapped one narrower for its scrollbar, so
/// the description exactly the box's room wraps there, under its column,
/// never from the box's edge; fitting, it stays on its row; the mouse's
/// continuation hangs under its text.
#[test]
fn a_scrolled_column_wraps_one_narrower_under_its_column() {
    let hung = format!("│{}abcdef", " ".repeat(21));
    let mut overlay = Overlay::default();
    overlay.open();
    let rows = drawn_keys(&mut overlay, &HANGING, 40, 8);
    let all = rows.join("\n");
    assert!(all.contains("↑↓ scroll"), "{all}");
    assert!(all.contains("PgUp PgDn b Space  abcdefgh"), "{all}");
    assert!(rows.iter().any(|row| row.contains(&hung)), "{all}");
    assert!(!all.contains("│abcdef"), "{all}");
    overlay.key(plain_key(KeyCode::End));
    let all = drawn_keys(&mut overlay, &HANGING, 40, 8).join("\n");
    assert!(
        all.contains(&format!("│{}Shift+wheel goes sideways", " ".repeat(8))),
        "{all}"
    );
    // Fitting, nothing scrolls, and the description keeps its one row.
    let mut overlay = Overlay::default();
    overlay.open();
    let all = drawn_keys(&mut overlay, &HANGING, 40, 24).join("\n");
    assert!(all.contains("any key closes"), "{all}");
    assert!(all.contains("PgUp PgDn b Space  abcdefgh abcdef"), "{all}");
}
