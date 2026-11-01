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
        "\n   ERROR \n\n  unrecognized subcommand 'nope'\n\n",
    ));
}

#[test]
fn a_parse_error_keeps_claps_tips_and_context() {
    sb().arg("verison").assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  unrecognized subcommand 'verison'\n\n  hint: did you mean 'version'?\n\n",
    ));
    sb().args(["--color=bad", "version"]).assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  invalid value 'bad' for '--color <WHEN>'\n\n  [possible values: auto, always, never]\n\n",
    ));
}

/// An error on a subcommand is one block too, with no pointer to help (as
/// sb-go).
#[test]
fn a_parse_error_on_a_subcommand_is_one_block() {
    sb().args(["version", "--bogus"])
        .assert()
        .code(2)
        .stderr(predicate::eq(
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n",
        ));
}

/// clap's suggestions for a flag and for a value read as "did you mean";
/// a value suggestion follows the possible values.
#[test]
fn flag_and_value_suggestions_read_as_did_you_mean() {
    sb().args(["--colr=always", "version"]).assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  unexpected argument '--colr' found\n\n  hint: did you mean '--color'?\n\n",
    ));
    sb().args(["--color=alw", "version"]).assert().code(2).stderr(predicate::eq(
        "\n   ERROR \n\n  invalid value 'alw' for '--color <WHEN>'\n\n  [possible values: auto, always, never]\n  hint: did you mean 'always'?\n\n",
    ));
}

/// With colour forced, the whole block is pinned: the badge, the typed
/// value in the problem colour, the orange label and the green fix.
#[test]
fn a_coloured_parse_error_is_pinned() {
    let out = sb()
        .args(["--color=always", "verison"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let expected = "\n  \u{1b}[1m\u{1b}[38;2;11;26;36m\u{1b}[48;2;255;110;122m ERROR \u{1b}[0m\n\n  unrecognized subcommand '\u{1b}[38;2;255;110;122mverison\u{1b}[0m'\n\n  \u{1b}[38;2;245;181;91mhint:\u{1b}[0m did you mean '\u{1b}[38;2;111;211;154mversion\u{1b}[0m'?\n\n";
    assert_eq!(String::from_utf8(out).expect("utf-8"), expected);
}
