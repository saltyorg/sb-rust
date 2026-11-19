//! The blank line at the end of a run (cli-core spec, "The blank line at
//! the end"; the maintainer, 2026-11-19): on a terminal, a run ends with
//! one blank line before the shell's prompt, never two; on a pipe, every
//! byte is as before. The terminal is util-linux's `script`, whose output
//! is the PTY's (a newline reads `\r\n` there).

mod common;
mod root_gate;

use std::process::{Command, Output};

/// `sb <args>` on a PTY of its own: what the terminal got.
fn on_a_terminal(args: &str, env: &[(&str, &std::path::Path)]) -> Vec<u8> {
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = Command::new("script");
    common::apply_sb_env(&mut cmd);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd
        .args([
            "-qec",
            &format!("stty cols 100 rows 30; exec '{program}' {args}"),
            "/dev/null",
        ])
        .env("TERM", "xterm-256color")
        .env("NO_COLOR", "1")
        .output()
        .expect("run script");
    output.stdout
}

/// `sb <args>` with stdout and stderr pipes.
fn piped(args: &[&str]) -> Output {
    common::sb()
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sb")
}

/// The terminal's output ends with exactly one blank line: its last line
/// ended, then one empty line, not two.
fn ends_with_one_blank_line(text: &[u8]) {
    let shown = String::from_utf8_lossy(text);
    assert!(text.ends_with(b"\r\n\r\n"), "{shown:?}");
    assert!(!text.ends_with(b"\r\n\r\n\r\n"), "{shown:?}");
}

/// `sb version`: one blank line after its lines on a terminal; piped, its
/// stdout ends as before, with its line, and stderr stays empty.
#[test]
fn version_ends_with_a_blank_line_only_on_a_terminal() {
    let shown = on_a_terminal("version", &[]);
    ends_with_one_blank_line(&shown);
    let out = piped(&["version"]);
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.ends_with(b"\n"), "{out:?}");
    assert!(!out.stdout.ends_with(b"\n\n"), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
}

/// Help is output like any other: one blank line after it on a terminal,
/// none added piped.
#[test]
fn help_ends_with_a_blank_line_only_on_a_terminal() {
    ends_with_one_blank_line(&on_a_terminal("--help", &[]));
    ends_with_one_blank_line(&on_a_terminal("", &[]));
    for args in [&["--help"][..], &[]] {
        let out = piped(args);
        assert!(out.status.success(), "{out:?}");
        assert!(!out.stdout.ends_with(b"\n\n"), "{out:?}");
        assert!(out.stderr.is_empty(), "{out:?}");
    }
}

/// A parse error's block ends in a blank line already: nothing is added
/// on a terminal, and piped it is as before (one blank line, exit 2).
#[test]
fn a_parse_error_keeps_its_one_blank_line() {
    ends_with_one_blank_line(&on_a_terminal("bogus", &[]));
    let out = piped(&["bogus"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stderr.ends_with(b"\n\n"), "{out:?}");
    assert!(!out.stderr.ends_with(b"\n\n\n"), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// A failing command (validate-config over a Saltbox root with no
/// files): its tree and error, then one blank line, on a terminal; piped,
/// stderr ends with its last line.
#[test]
fn a_failing_command_ends_with_a_blank_line_on_a_terminal() {
    if !root_gate::requested() {
        return;
    }
    let root = tempfile::tempdir().expect("root");
    let env = [("SB_SALTBOX_DIR", root.path())];
    let shown = on_a_terminal("validate-config", &env);
    assert!(String::from_utf8_lossy(&shown).contains('✗'), "{shown:?}");
    ends_with_one_blank_line(&shown);
    let out = common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", root.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stderr.ends_with(b"\n"), "{out:?}");
    assert!(!out.stderr.ends_with(b"\n\n"), "{out:?}");
}
