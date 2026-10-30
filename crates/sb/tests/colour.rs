//! `--color` on every output path (spec success criterion 7; Task 7):
//! `never` means no escape codes, help included, even on a terminal;
//! `always` colours piped output; `auto` follows NO_COLOR and the terminal.
//! Terminal runs go through util-linux `script` (a test dependency), which
//! gives sb a pty and merges its output streams. Why not portable-pty: a
//! reader thread and a dev-dependency for a few lines of test; the plan's
//! PTY tests for the terminal UI (rexpect) come with term-ui.

mod common;

use std::process::Output;

fn has_escape(bytes: &[u8]) -> bool {
    bytes.contains(&0x1b)
}

/// sb with `args` on a pseudo-terminal (via `script`), `env` added.
fn sb_on_a_terminal(args: &str, env: &[(&str, &str)]) -> Output {
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("script");
    cmd.args(["-qec", &format!("exec '{program}' {args}"), "/dev/null"])
        .env("SB_LOG_DIR", common::test_log_dir())
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run script")
}

#[test]
fn help_is_coloured_on_a_terminal_by_default() {
    let out = sb_on_a_terminal("--help", &[]);
    assert!(out.status.success());
    assert!(
        has_escape(&out.stdout),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn never_means_no_escapes_in_help_even_on_a_terminal() {
    for args in [
        "--color=never --help",
        "--help --color never",
        "version --color never --help",
        "--color=never help",
        "--color never help version",
    ] {
        let out = sb_on_a_terminal(args, &[]);
        assert!(out.status.success(), "{args}");
        assert!(
            !has_escape(&out.stdout),
            "{args}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// clap's `help` subcommand takes no flags after it, so `sb help
/// --color=never` is clap's usage error; that error follows `never` too.
#[test]
fn a_misplaced_flag_after_help_gives_a_plain_error() {
    let out = sb_on_a_terminal("help --color=never", &[]);
    assert_eq!(out.status.code(), Some(2));
    // script merges the pty's output streams into stdout.
    assert!(
        !has_escape(&out.stdout),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The control for the next test: parse errors are coloured by default on
/// a terminal, so their absence under `never` means something.
#[test]
fn parse_errors_are_coloured_on_a_terminal_by_default() {
    for args in ["version --bogus", "bogus"] {
        let out = sb_on_a_terminal(args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args}");
        assert!(
            has_escape(&out.stdout),
            "{args}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Parse errors follow `never` too, wherever the flag is: clap prints them
/// before sb runs, through the same global choice.
#[test]
fn never_means_no_escapes_in_parse_errors() {
    for args in [
        "--color=never version --bogus",
        "version --bogus --color=never",
        "bogus --color never",
    ] {
        let out = sb_on_a_terminal(args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args}");
        assert!(
            !has_escape(&out.stdout),
            "{args}: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn no_color_turns_help_colour_off_on_a_terminal() {
    let out = sb_on_a_terminal("--help", &[("NO_COLOR", "1")]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage"));
    assert!(!has_escape(&out.stdout));
}

#[test]
fn always_colours_piped_help_even_with_no_color() {
    for env in [None, Some(("NO_COLOR", "1"))] {
        let mut cmd = common::sb();
        cmd.args(["--color=always", "--help"]);
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run sb");
        assert!(has_escape(&out.stdout), "{env:?}");
    }
}

#[test]
fn piped_help_is_plain_by_default() {
    let out = common::sb()
        .arg("--help")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run sb");
    assert!(!has_escape(&out.stdout));
}
