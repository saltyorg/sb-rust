//! Parse errors stay clap's (spec: "Errors"; plan Task 2).

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn an_unknown_command_keeps_clap_message_and_code() {
    Command::cargo_bin("sb")
        .expect("sb binary")
        .arg("no-such-command")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "unrecognized subcommand 'no-such-command'",
        ));
}

/// A failing write (a full disk) ends in one error block on stderr and
/// exit 1.
#[test]
fn a_failed_write_shows_the_error_block() {
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"))
        .arg("version")
        .stdout(full)
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).expect("utf-8"),
        "✗ No space left on device (os error 28)\n"
    );
}

/// The reader went away: no message, exit 141 like a SIGPIPE'd process.
/// The read end is closed before sb starts, so the write always fails
/// (https://doc.rust-lang.org/std/io/fn.pipe.html).
#[test]
fn a_closed_pipe_is_quiet() {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"))
        .arg("version")
        .stdout(writer)
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(141));
    assert_eq!(String::from_utf8(out.stderr).expect("utf-8"), "");
}
