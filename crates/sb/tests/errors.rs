//! Parse errors keep clap's message and exit code 2, drawn in sb's error
//! style (cli-core spec: Help and parse errors).

mod common;

use assert_cmd::prelude::*;
use common::sb;
use predicates::prelude::*;

#[test]
fn an_unknown_command_keeps_clap_message_and_code() {
    sb().arg("no-such-command")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'no-such-command'",
        ));
}

/// A failing write (a full disk) ends in one error block on stderr and
/// exit 1. A root run's block also ends with the log pointer, checked in
/// run_log.rs.
#[test]
fn a_failed_write_shows_the_error_block() {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = sb().arg("version").stdout(full).output().expect("run sb");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(
        stderr.lines().next(),
        Some("✗ No space left on device (os error 28)")
    );
    assert!(
        stderr.lines().skip(1).all(|l| l.starts_with("  details: ")),
        "{stderr}"
    );
}

/// The reader went away: no message, exit 141 like a SIGPIPE'd process.
/// The read end is closed before sb starts, so the write always fails
/// (https://doc.rust-lang.org/std/io/fn.pipe.html).
#[test]
fn a_closed_pipe_is_quiet() {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let out = sb().arg("version").stdout(writer).output().expect("run sb");
    assert_eq!(out.status.code(), Some(141));
    assert_eq!(String::from_utf8(out.stderr).expect("utf-8"), "");
}

/// Parse errors are drawn in sb-go's layout (cli-core spec: Help and parse
/// errors): an ERROR badge, clap's message, its tips and context lines,
/// then where to look; no usage paragraph; exit 2.
#[test]
fn a_parse_error_is_an_sb_error_block() {
    sb().arg("nope").assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  unrecognized subcommand 'nope'\n\n  help: try 'sb --help'\n\n",
    ));
}

#[test]
fn a_parse_error_keeps_claps_tips_and_context() {
    sb().arg("verison").assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  unrecognized subcommand 'verison'\n\n  tip: a similar subcommand exists: 'version'\n  help: try 'sb --help'\n\n",
    ));
    sb().args(["--color=bad", "version"]).assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  invalid value 'bad' for '--color <WHEN>'\n\n  [possible values: auto, always, never]\n  help: try 'sb --help'\n\n",
    ));
}

/// The help pointer names the command that failed.
#[test]
fn a_parse_error_points_at_the_failing_commands_help() {
    sb().args(["version", "--bogus"])
        .assert()
        .code(2)
        .stderr(predicate::eq(
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n  help: try 'sb version --help'\n\n",
        ));
}
