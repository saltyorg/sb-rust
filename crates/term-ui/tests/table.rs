//! Tables end to end, through the `demo` binary's `--table` (term-ui spec,
//! "Tables"; success criterion 7). Terminal runs go through util-linux
//! `script`, for the reason in crates/sb/tests/colour.rs.

use std::path::PathBuf;
use std::process::{Command, Output};

fn demo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_demo"))
}

fn piped(args: &[&str]) -> Output {
    Command::new(demo())
        .args(args)
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .output()
        .expect("run demo")
}

fn on_a_terminal(cols: u16, args: &str) -> String {
    let program = demo().display().to_string();
    let out = Command::new("script")
        .args([
            "-qec",
            &format!("stty cols {cols} rows 30; exec '{program}' {args}"),
            "/dev/null",
        ])
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .output()
        .expect("run script");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Piped: the table at its natural width, no escape codes.
#[test]
fn piped_tables_are_plain_and_natural_width() {
    let out = piped(&["--table"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(
        text.contains("│ plex      │ running    │        32400 │ 0.0.0.0:32400->32400/tcp │"),
        "{text}"
    );
}

/// `--color=always` piped: the theme's colours, the border included.
#[test]
fn always_colours_a_piped_table() {
    let text = String::from_utf8_lossy(&piped(&["--table", "--colour"]).stdout).into_owned();
    // The border role's colour (truecolor, as the demo's theme).
    assert!(text.contains("\x1b[38;2;"), "{text:?}");
    assert!(text.contains("\x1b[1mContainer"), "{text:?}");
    assert!(text.contains("running"), "{text:?}");
}

/// `--color=never` on a terminal: no SGR codes, bold included; `auto` on
/// the same terminal has them.
#[test]
fn never_means_no_codes_on_a_terminal() {
    let text = on_a_terminal(120, "--table --no-colour");
    assert!(!text.contains("\x1b["), "{text:?}");
    // The same run with `auto` has them (so the check above means something).
    let auto = on_a_terminal(120, "--table");
    assert!(auto.contains("\x1b[1mContainer"), "{auto:?}");
}

/// On a 50-column terminal the table fits it.
#[test]
fn a_table_fits_a_narrow_terminal() {
    let text = on_a_terminal(50, "--table");
    let plain = console::strip_ansi_codes(&text);
    assert!(
        plain.lines().all(|l| console::measure_text_width(l) <= 50),
        "{plain}"
    );
    assert!(plain.contains("╭"), "{plain}");
}

/// Cells as untrusted text (term-ui spec, "Escape handling"; san-H1): the
/// sequences a container label can carry, and what each cell shows.
mod sanitized {
    use sb_cli_core::theme::{Palette, Profile, Role, Theme};
    use sb_term_ui::{Cell, Table};

    /// (raw, shown): OSC 52 and a title, a screen-clearing CSI, 8-bit CSI,
    /// OSC, NEL and ST (C1), DEL, a tab, a CR.
    const CELLS: [(&str, &str); 6] = [
        ("a\x1b]52;c;ZGF0YQ==\x07b", "ab"),
        ("\x1b]0;owned\x07t\x1b[2J\x1b[Hu", "tu"),
        ("c\u{9b}31md\u{9d}0;title\x07e\u{85}", "cde"),
        ("f\u{9c}g\u{7f}h", "fgh"),
        ("x\ty", "x       y"),
        ("old\rnew", "new"),
    ];

    /// A heading holding a title change, and the heading it shows.
    const HEADING: (&str, &str) = ("Na\x1b]0;t\x07me", "Name");

    fn table(heading: &str, cells: [&str; 6]) -> Table {
        let mut table = Table::new([heading, "Status"]);
        for text in cells {
            table.row([Cell::new(text), Cell::new(text).role(Role::Ok)]);
        }
        table
    }

    fn themes() -> [Theme; 2] {
        [
            Theme::resolve(&Palette::default(), Profile::TrueColor),
            Theme::resolve(&Palette::default(), Profile::Ascii),
        ]
    }

    /// OSC, CSI, C1, DEL, a tab and a CR, in a cell or a heading: none
    /// reaches the table, which draws as the same table of the cleaned text
    /// would, colour on or off.
    #[test]
    fn a_cell_renders_without_escapes_or_controls() {
        let raw = || table(HEADING.0, CELLS.map(|(raw, _)| raw));
        let clean = || table(HEADING.1, CELLS.map(|(_, shown)| shown));
        for theme in themes() {
            let text = raw().render(&theme, false, None);
            assert!(
                !text.chars().any(|c| c.is_control() && c != '\n'),
                "{text:?}"
            );
            for colour in [false, true] {
                assert_eq!(
                    raw().render(&theme, colour, None),
                    clean().render(&theme, colour, None),
                    "colour {colour}"
                );
            }
        }
    }

    /// The table's own colour is kept: the border, the bold headings and a
    /// cell's role; a cell's own SGR is not.
    #[test]
    fn the_tables_own_colour_is_kept() {
        let [theme, _] = themes();
        let mut hostile = Table::new(["Name"]);
        hostile.row([Cell::new("\x1b[31mred\x1b[0m").role(Role::Ok)]);
        let text = hostile.render(&theme, true, None);
        assert!(text.contains("\x1b[1mName"), "{text:?}");
        assert!(text.contains("\x1b[38;2;"), "{text:?}");
        assert!(
            text.contains(&sb_term_ui::style::paint(&theme, Role::Ok, true, "red")),
            "{text:?}"
        );
        assert!(!text.contains("\x1b[31m"), "{text:?}");
    }

    /// `\n` still separates a cell's lines (the bindings, one per line).
    #[test]
    fn a_newline_still_separates_a_cells_lines() {
        let [_, plain] = themes();
        let mut lines = Table::new(["Port Bindings"]);
        lines.row(["one\ntwo"]);
        let text = lines.render(&plain, false, None);
        assert!(
            text.contains("│ one           │\n│ two           │"),
            "{text}"
        );
    }
}
