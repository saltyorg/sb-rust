//! `--color` on every output path (spec success criterion 7; Task 7):
//! `never` means no escape codes, help included, even on a terminal;
//! `always` colours piped output; `auto` follows `NO_COLOR` and the terminal.
//! Terminal runs go through util-linux `script` (a test dependency), which
//! gives sb a pty and merges its output streams. Why not portable-pty: a
//! reader thread and a dev-dependency for a few lines of test. Every PTY
//! test uses `script`, rexpect included in that decision (cli-core spec,
//! testing).

mod common;

use std::process::Output;

fn has_escape(bytes: &[u8]) -> bool {
    bytes.contains(&0x1b)
}

/// sb with `args` on a pseudo-terminal (via `script`), `env` added.
fn sb_on_a_terminal(args: &str, env: &[(&str, &str)]) -> Output {
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("script");
    common::apply_sb_env(&mut cmd);
    cmd.args(["-qec", &format!("exec '{program}' {args}"), "/dev/null"])
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

/// Parse errors follow `never` too, wherever the flag is: they're printed
/// before the run starts, through the same global choice.
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
    // Help was printed: a flag both help formats list.
    assert!(String::from_utf8_lossy(&out.stdout).contains("--color"));
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

/// sb's own error block (here: a write to a full disk) in each
/// `SB_COLOR_PROFILE`, coloured with `--color=always` though piped.
fn error_block_with_profile(profile: Option<&str>) -> String {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let mut cmd = common::sb();
    cmd.args(["--color=always", "version"])
        .stdout(full)
        .env_remove("SB_COLOR_PROFILE");
    if let Some(profile) = profile {
        cmd.env("SB_COLOR_PROFILE", profile);
    }
    let out = cmd.output().expect("run sb");
    String::from_utf8(out.stderr).expect("utf-8")
}

/// The mark's colour: truecolor by default, 256 colours, 16, or none
/// (ADR-002), the message itself unchanged.
#[test]
fn the_error_block_follows_the_colour_profile() {
    let mark = |stderr: &str| stderr.split('✗').next().unwrap_or_default().to_string();
    assert!(mark(&error_block_with_profile(None)).ends_with("\x1b[38;2;255;110;122m"));
    assert!(mark(&error_block_with_profile(Some("ansi256"))).contains("\x1b[38;5;"));
    assert!(mark(&error_block_with_profile(Some("ansi"))).ends_with("\x1b[91m"));
    let ascii = error_block_with_profile(Some("ascii"));
    assert!(!has_escape(ascii.as_bytes()), "{ascii:?}");
    assert!(
        ascii.starts_with("✗ No space left on device (os error 28)"),
        "{ascii:?}"
    );
}

/// An unknown profile warns and uses truecolor, as sb-go.
#[test]
fn an_unknown_profile_warns_and_uses_truecolor() {
    let stderr = error_block_with_profile(Some("16bit"));
    assert!(
        stderr.contains("unknown SB_COLOR_PROFILE \"16bit\""),
        "{stderr:?}"
    );
    assert!(stderr.contains("\x1b[38;2;255;110;122m✗"), "{stderr:?}");
}

/// `--color=never` wins over any profile.
#[test]
fn never_wins_over_the_profile() {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = common::sb()
        .args(["--color=never", "version"])
        .stdout(full)
        .env("SB_COLOR_PROFILE", "truecolor")
        .output()
        .expect("run sb");
    assert!(!has_escape(&out.stderr));
}

/// `ascii` means no colour anywhere, parse errors and help included (drawn
/// from the theme, which `ascii` leaves plain).
#[test]
fn ascii_reaches_clap_output_too() {
    for args in [
        &["--color=always", "--bogus"][..],
        &["--color=always", "--help"][..],
    ] {
        let out = common::sb()
            .args(args)
            .env("SB_COLOR_PROFILE", "ascii")
            .output()
            .expect("run sb");
        assert!(
            !has_escape(&out.stdout) && !has_escape(&out.stderr),
            "{args:?}: {out:?}"
        );
        assert!(!out.stdout.is_empty() || !out.stderr.is_empty(), "{args:?}");
    }
}

/// Under `ascii`, help is the plain layout even with `--color=always`: no
/// shaded usage block's padding rows (they only come with colour).
#[test]
fn ascii_help_is_the_plain_layout() {
    let help = |args: &[&str], env: Option<&str>| {
        let mut cmd = common::sb();
        cmd.args(args).env_remove("NO_COLOR");
        match env {
            Some(profile) => cmd.env("SB_COLOR_PROFILE", profile),
            None => cmd.env_remove("SB_COLOR_PROFILE"),
        };
        cmd.output().expect("run sb").stdout
    };
    let plain = help(&["--color=never", "--help"], None);
    assert_eq!(
        String::from_utf8_lossy(&help(&["--color=always", "--help"], Some("ascii"))),
        String::from_utf8_lossy(&plain)
    );
}
