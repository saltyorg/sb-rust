//! The runner on a real terminal and piped, through the `demo` binary
//! (term-ui spec, success criteria 3 and 5). Terminal runs go through
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
    let program = demo().display().to_string();
    Command::new("script")
        .args([
            "-qec",
            &format!("stty cols 100 rows 30; exec '{program}' {args}"),
            "/dev/null",
        ])
        .env("TERM", term)
        .env_remove("NO_COLOR")
        .output()
        .expect("run script")
}

/// Every way a terminal can be told to erase part of the screen.
const ERASES: [&str; 5] = ["\x1b[2K", "\x1b[K", "\x1b[0K", "\x1b[1K", "\x1b[J"];

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
/// clear after the last tree line (the measured no-blink technique).
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
        .map(|e| text[..last_line].matches(e).count())
        .sum();
    assert_eq!(erases_before, 0);
    // Live, no plain-mode end line is written beside the tree.
    assert!(!text.contains("✓ accounts.yml read 0s"), "{text:?}");
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
    // Without `--sensitive` the demo has no sensitive lines.
    assert!(!text.contains("(sensitive, withheld)"), "{text:?}");
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
        text.contains("\nChecking Docker Hub login: ✓ logged in "),
        "{text:?}"
    );
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

/// Sensitive text (decision 8): piped, both lines show as the placeholder;
/// on a terminal, only the one the command chose to show appears.
#[test]
fn sensitive_text_is_withheld_off_a_terminal() {
    let piped = Command::new(demo())
        .arg("--sensitive")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run demo");
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    let text = String::from_utf8_lossy(&piped.stderr);
    assert!(!text.contains("demo-secret"), "{text:?}");
    assert!(text.contains("\n(sensitive, withheld)\n"), "{text:?}");
    assert!(text.contains("\n› (sensitive, withheld)\n"), "{text:?}");
    let terminal = on_a_terminal("--sensitive");
    assert_eq!(terminal.status.code(), Some(0), "{terminal:?}");
    let text = String::from_utf8_lossy(&terminal.stdout);
    assert!(!text.contains("demo-secret-withheld"), "{text:?}");
    assert!(text.contains("demo-secret-shown"), "{text:?}");
}
