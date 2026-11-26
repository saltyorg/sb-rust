//! The runner on a real terminal and piped, through the `demo` binary
//! (term-ui spec, success criteria 1, 3 and 5). Terminal runs go through
//! util-linux `script`, as sb's colour tests do.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The demo binary, which cargo builds for these tests.
fn demo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_demo"))
}

fn on_a_terminal(args: &str) -> Output {
    on_a_terminal_with(args, "xterm-256color")
}

fn on_a_terminal_with(args: &str, term: &str) -> Output {
    on_a_terminal_env(args, term, None)
}

/// The demo with `args` on a 100x30 pseudo-terminal; `NO_COLOR` set to
/// `no_color` when given, removed otherwise; none of the developer's
/// `CLICOLOR` and `CLICOLOR_FORCE` (`CLICOLOR=0` takes colour away on a
/// terminal).
fn on_a_terminal_env(args: &str, term: &str, no_color: Option<&str>) -> Output {
    let program = demo().display().to_string();
    let mut cmd = Command::new("script");
    cmd.args([
        "-qec",
        &format!("stty cols 100 rows 30; exec '{program}' {args}"),
        "/dev/null",
    ])
    .env("TERM", term)
    .env_remove("CLICOLOR")
    .env_remove("CLICOLOR_FORCE");
    match no_color {
        Some(value) => cmd.env("NO_COLOR", value),
        None => cmd.env_remove("NO_COLOR"),
    };
    cmd.output().expect("run script")
}

/// Every way a terminal can be told to erase part of the screen.
const ERASES: [&str; 5] = ["\x1b[2K", ROW_END, "\x1b[0K", "\x1b[1K", "\x1b[J"];

/// The erase from the cursor to the row's end that ends a live row.
const ROW_END: &str = "\x1b[K";

/// Plain output on a terminal: start and end lines, no cursor movement.
fn assert_plain_on_a_terminal(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("- Reading accounts.yml"), "{text:?}");
    assert!(text.contains("✓ accounts.yml read "), "{text:?}");
    let moves = cursor_up(&text) || ERASES.iter().any(|e| text.contains(e));
    assert!(!moves, "{text:?}");
}

/// Whether `text` moves the cursor up (`ESC [ n A`, any `n`).
fn cursor_up(text: &str) -> bool {
    text.split("\x1b[").skip(1).any(|rest| {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        rest[digits..].starts_with('A')
    })
}

/// `TERM=dumb` on a terminal: plain, as indicatif hides its target there.
#[test]
fn a_dumb_terminal_gets_plain_output() {
    assert_plain_on_a_terminal(&on_a_terminal_with("", "dumb"));
}

/// Live on a terminal: no line is erased while running, only in the final
/// clear after the last tree line (the measured no-blink technique). A
/// row's end is erased after its text instead of padded with spaces
/// (`Rows`, so a terminal that rewraps on a resize keeps the rows
/// apart): that erases only what the text didn't draw over, never a row
/// before it is drawn, so it is the one erase allowed.
#[test]
fn nothing_is_erased_while_running() {
    let out = on_a_terminal("");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    let last_line = text
        .rfind("credentials valid")
        .expect("the last step's line");
    let erases_before: usize = ERASES
        .iter()
        .filter(|&&e| e != ROW_END)
        .map(|e| text[..last_line].matches(e).count())
        .sum();
    assert_eq!(erases_before, 0);
    // A row's end is erased only after its text: never where a row
    // starts, before it is drawn.
    assert!(text[..last_line].contains(ROW_END), "{text:?}");
    assert!(
        !text[..last_line].contains(&format!("\r{ROW_END}")),
        "{text:?}"
    );
    // Live, no plain-mode end line is written beside the tree (live
    // lines start with their branch).
    assert!(!text.contains("\n✓ accounts.yml read"), "{text:?}");
    assert!(
        text[last_line..].contains("\x1b[2K"),
        "the live area is cleared at the end"
    );
}

/// Piped: plain start and end lines, no escape codes (`auto`).
#[test]
fn piped_output_is_plain() {
    let out = Command::new(demo())
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(
        text.starts_with("sb demo\n- Reading accounts.yml\n"),
        "{text:?}"
    );
    // Concurrent steps' lines carry their label (they interleave).
    assert!(
        text.contains("\nChecking Cloudflare token: checking item 1 of 6\n"),
        "{text:?}"
    );
    assert!(
        text.contains("\nChecking Docker Hub login: checking item 1 of 4\n"),
        "{text:?}"
    );
    // A sub-step inside a sibling keeps the sibling's label in front.
    assert!(
        text.contains("\nChecking Docker Hub login: - Logging in\n"),
        "{text:?}"
    );
    assert!(
        text.contains("\nChecking Docker Hub login: ✓ logged in   "),
        "{text:?}"
    );
    // Without `--sensitive` the demo has no secrets to mask.
    assert!(!text.contains("[REDACTED]"), "{text:?}");
    // Notices and output lines come through too (criterion 5).
    assert!(text.contains("\nchecking item 1 of 4\n"), "{text:?}");
    assert!(text.contains("\n› 3 accounts\n"), "{text:?}");
    assert!(
        text.contains("\n! unknown key: downloads.extra\n"),
        "{text:?}"
    );
    // Times are whole seconds; how many depends on the machine's load.
    assert!(text.contains("\n✓ accounts.yml read "), "{text:?}");
    let last = text.lines().last().unwrap_or_default();
    assert!(
        last.starts_with("✓ credentials valid ") && last.ends_with('s'),
        "{text:?}"
    );
}

/// Piped, a run whose transient steps all succeed prints nothing, nor its
/// title, though a step had output and a sub-step (decision 12).
#[test]
fn piped_a_run_of_transient_steps_that_succeeds_prints_nothing() {
    let out = Command::new(demo())
        .arg("--transient")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

/// Piped, a run with transient steps holds every line until it ends; when
/// a step fails, all of them are printed, in order, the steps that
/// succeeded included, as a run without transient steps prints them
/// (decision 12).
#[test]
fn piped_a_failed_transient_run_prints_every_line_at_its_end() {
    let out = Command::new(demo())
        .arg("--transient-fail")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.starts_with("sb demo\n- Listing tags\nTASK TAGS: [always]\n- Reading\n✓ read   "),
        "{text:?}"
    );
    assert!(text.contains("\n✓ tags listed   "), "{text:?}");
    assert!(
        text.contains("\n- Checking tags\n✗ Checking tags   "),
        "{text:?}"
    );
    assert!(text.contains("\n  listing refused\n"), "{text:?}");
}

/// Piped, a run that succeeds with transient steps among others prints
/// the others' lines, in order and after the title, and nothing of the
/// transient steps or what ran under them (decision 12).
#[test]
fn piped_a_mixed_run_that_succeeds_prints_only_the_other_steps() {
    let out = Command::new(demo())
        .arg("--transient-mixed")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "{text:?}");
    assert_eq!(lines[..3], ["sb demo", "- Preparing", "preparing output"]);
    assert!(lines[3].starts_with("✓ prepared   "), "{text:?}");
    assert_eq!(lines[4], "- Writing", "{text:?}");
    assert!(lines[5].starts_with("✓ written   "), "{text:?}");
}

/// The SGR sequences in `text` (`ESC [ params m`), resets included.
fn sgr_codes(text: &str) -> Vec<&str> {
    text.split("\x1b[")
        .skip(1)
        .filter_map(|rest| {
            let params = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ';')
                .count();
            rest[params..].starts_with('m').then(|| &rest[..params])
        })
        .collect()
}

/// `NO_COLOR` or `--color=never` on a terminal: no SGR codes at all, not
/// even resets, but the live tree's cursor movement remains (success
/// criterion 1). The same terminal with colour on has codes, so the
/// counting finds them.
#[test]
fn colour_off_keeps_the_cursor_movement_only() {
    let auto = on_a_terminal("");
    assert_eq!(auto.status.code(), Some(0), "{auto:?}");
    let text = String::from_utf8_lossy(&auto.stdout);
    assert!(!sgr_codes(&text).is_empty(), "{text:?}");
    let no_color = on_a_terminal_env("", "xterm-256color", Some("1"));
    let never = on_a_terminal("--no-colour");
    for out in [no_color, never] {
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(sgr_codes(&text), Vec::<&str>::new(), "{text:?}");
        assert!(cursor_up(&text), "no cursor movement: {text:?}");
    }
}

/// `NO_COLOR` or `--color=never` on a terminal: a child's own colour codes
/// go too (the runner passes `keeps_codes` to `DisplayText`), while the
/// same run with colour on keeps them.
#[test]
fn colour_off_strips_a_childs_own_colours() {
    let auto = on_a_terminal("--child-colour");
    assert_eq!(auto.status.code(), Some(0), "{auto:?}");
    let text = String::from_utf8_lossy(&auto.stdout);
    assert!(text.contains("\x1b[31mchild red"), "{text:?}");
    let no_color = on_a_terminal_env("--child-colour", "xterm-256color", Some("1"));
    let never = on_a_terminal("--no-colour --child-colour");
    for out in [no_color, never] {
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("child red"), "{text:?}");
        assert_eq!(sgr_codes(&text), Vec::<&str>::new(), "{text:?}");
    }
}

/// A child's own colour codes in a step's output, plain with
/// `--color=always`: kept under a colour profile, stripped under `ascii`
/// (maintainer, 2026-11-05: `ascii` means no SGR code in anything term-ui
/// draws).
#[test]
fn ascii_strips_a_childs_own_colours() {
    for (profile, kept) in [(None, true), (Some("--ascii"), false)] {
        let out = Command::new(demo())
            .args(["--colour", "--child-colour"])
            .args(profile)
            .output()
            .expect("run demo");
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(
            text.contains("child red") && text.contains("green note"),
            "{text:?}"
        );
        assert_eq!(
            text.contains("\x1b[31mchild red"),
            kept,
            "{profile:?}: {text:?}"
        );
        if !kept {
            assert!(!text.contains('\x1b'), "{text:?}");
        }
    }
}

/// `--color=always` piped: the runner's plain lines in the theme's colours.
#[test]
fn always_colours_piped_runner_output() {
    let out = Command::new(demo())
        .arg("--colour")
        .output()
        .expect("run demo");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stderr);
    // The runner's own end line, its glyph in a truecolor code (the demo's
    // profile) right before it.
    let line = text
        .lines()
        .find(|l| l.contains("accounts.yml read"))
        .unwrap_or_default();
    assert!(line.starts_with("\x1b[38;2;"), "{text:?}");
    assert!(line.contains("✓"), "{text:?}");
}
