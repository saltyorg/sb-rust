//! sb's help in sb-go's format (cli-core spec: Help and parse errors), at
//! every level and through `sb help`.

mod common;

use assert_cmd::prelude::*;
use common::sb;
use predicates::prelude::*;

#[test]
fn root_help_follows_sb_gos_format() {
    sb().arg("--help").assert().success().stdout(
        // clap trims the text's leading blank line.
        predicate::str::starts_with("  Saltbox CLI\n\n  USAGE\n\n    sb [command] [--flags]\n")
            .and(predicate::str::contains("\n  COMMANDS\n\n"))
            .and(predicate::str::contains("    version [--flags]"))
            .and(predicate::str::contains("    -h --help"))
            .and(predicate::str::contains("Help for sb"))
            .and(predicate::str::contains(
                "Controls when to use color (auto)",
            )),
    );
}

#[test]
fn subcommand_help_and_the_help_subcommand_agree() {
    let direct = sb().args(["version", "--help"]).assert().success();
    let direct = String::from_utf8(direct.get_output().stdout.clone()).expect("utf-8");
    assert!(direct.contains("    sb version [--flags]\n"), "{direct}");
    assert!(
        direct.contains("Print machine-readable version information"),
        "{direct}"
    );
    sb().args(["help", "version"])
        .assert()
        .success()
        .stdout(predicate::eq(direct));
}

/// Piped (colour off), help has no escape codes and no shaded padding rows.
#[test]
fn piped_help_is_plain() {
    sb().arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\x1b").not())
        .stdout(predicate::str::is_match(r"(?m)^ +$").expect("regex").not());
}

/// Bare `sb` shows the help on stdout and succeeds, as sb-go does (clap
/// would print it on stderr and exit 2).
#[test]
fn bare_sb_prints_help_like_sb_go() {
    sb().assert()
        .success()
        .stdout(predicate::str::starts_with("  Saltbox CLI\n"))
        .stderr(predicate::str::is_empty());
}

/// sb's help is drawn already laid out: clap doesn't wrap it at the
/// terminal's width (its `wrap_help` feature is off), however narrow.
#[test]
fn help_is_not_wrapped_to_the_terminals_width() {
    let wide = sb().arg("--help").assert().success();
    let narrow = sb().arg("--help").env("COLUMNS", "20").assert().success();
    assert_eq!(narrow.get_output().stdout, wide.get_output().stdout);
    assert!(
        String::from_utf8_lossy(&wide.get_output().stdout)
            .lines()
            .any(|line| line.len() > 40)
    );
}
