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
