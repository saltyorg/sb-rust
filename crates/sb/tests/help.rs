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

/// `sb install --help` piped, plain, with `COLUMNS` set to `columns`.
fn install_help(columns: Option<&str>) -> String {
    let mut cmd = sb();
    cmd.args(["install", "--help", "--color=never"]);
    if let Some(columns) = columns {
        cmd.env("COLUMNS", columns);
    }
    let out = cmd.assert().success();
    String::from_utf8(out.get_output().stdout.clone()).expect("utf-8")
}

fn widest(text: &str) -> usize {
    text.lines().map(|l| l.chars().count()).max().unwrap_or(0)
}

/// Help wraps at the width `COLUMNS` gives, capped at 120 as fang's is
/// (cli-core spec, Help): every line fits, and `-e`'s long help goes on
/// under its help column, not at column 0 (so clap's own wrap, its
/// `wrap_help` feature, stays off). Piped with no `COLUMNS`, the width is
/// 120.
#[test]
fn help_wraps_at_the_width_capped_at_120() {
    let at_80 = install_help(Some("80"));
    assert!(widest(&at_80) <= 80, "{at_80}");
    assert!(
        at_80.contains("\n                     Each value reaches Ansible whole: key=value, YAML or JSON,\n                     or @file (a path relative to the repository the playbook\n                     runs in).\n"),
        "{at_80}"
    );
    let at_120 = install_help(Some("120"));
    assert!(widest(&at_120) <= 120, "{at_120}");
    assert!(widest(&at_120) > 80, "{at_120}");
    assert!(
        at_120.contains("\n                     Each value reaches Ansible whole: key=value, YAML or JSON, or @file (a path relative to the\n                     repository the playbook runs in).\n"),
        "{at_120}"
    );
    assert_eq!(install_help(Some("200")), at_120);
    assert_eq!(install_help(None), at_120, "no terminal, no COLUMNS");
    // Too narrow for the help column: the text keeps 20 columns.
    let narrow = install_help(Some("20"));
    assert!(
        narrow.contains(
            "\n                     Each value reaches\n                     Ansible whole:\n"
        ),
        "{narrow}"
    );
}

/// On a terminal its own width wins over `COLUMNS`, as clap reads them;
/// `script` gives sb a terminal, sized with `stty`.
#[test]
fn a_terminals_width_wins_over_columns() {
    let program = sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("script");
    common::apply_sb_env(&mut cmd);
    cmd.args([
        "-qec",
        &format!("stty cols 90; exec '{program}' install --help --color=never"),
        "/dev/null",
    ])
    .env("COLUMNS", "200");
    let out = cmd.output().expect("run script");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).replace('\r', "");
    assert!(widest(&text) <= 90, "{text}");
    assert!(widest(&text) > 80, "{text}");
    assert!(
        text.contains("JSON, or @file\n                     (a path relative"),
        "{text}"
    );
}

/// The flag list has no blank line (maintainer, 2026-11-14): `-e`'s long
/// help holds a paragraph break, which its row leaves out, at every width.
#[test]
fn the_flag_list_has_no_blank_line() {
    for columns in ["80", "120", "200"] {
        let help = install_help(Some(columns));
        // FLAGS is the help's last section: everything after its heading.
        let flags = help
            .split_once("  FLAGS\n\n")
            .map(|(_, flags)| flags)
            .expect("a FLAGS section");
        assert!(
            flags.lines().all(|line| !line.trim().is_empty()),
            "COLUMNS={columns}: {help}"
        );
        assert!(
            flags.contains(
                "Extra variables to pass to Ansible\n                     Each value reaches"
            ),
            "COLUMNS={columns}: {help}"
        );
    }
}
