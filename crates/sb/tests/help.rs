//! sb's help in sb-go's format (cli-core spec: Help and parse errors), at
//! every level and through `sb help`.

mod common;

use assert_cmd::prelude::*;
use common::sb;
use predicates::prelude::*;

#[test]
fn root_help_follows_sb_gos_format() {
    sb().arg("--help").assert().success().stdout(
        // fang's blank line first.
        predicate::str::starts_with("\n  Saltbox CLI\n\n  USAGE\n\n    sb [command] [--flags]\n")
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
        .stdout(predicate::str::starts_with("\n  Saltbox CLI\n"))
        .stderr(predicate::str::is_empty());
}

/// `help` (from `args`) starts with one blank line, as fang v2.0.1's does
/// (the maintainer, 2026-11-14), and ends with its last row: no other
/// blank line is added.
fn assert_one_leading_blank_line(args: &[&str], help: &str) {
    let mut lines = help.lines();
    assert_eq!(lines.next(), Some(""), "{args:?}: {help}");
    assert!(
        lines
            .next()
            .is_some_and(|l| l.starts_with("  ") && !l.trim().is_empty()),
        "{args:?}: {help}"
    );
    assert!(
        help.ends_with('\n') && !help.ends_with("\n\n"),
        "{args:?}: {help}"
    );
}

/// Bare `sb` and `sb help` lead with the blank line too (each command's
/// `-h` and `--help` are checked by the walk below).
#[test]
fn help_starts_with_one_blank_line() {
    for args in [
        &[][..],
        &["--help"],
        &["-h"],
        &["help"],
        &["help", "install"],
        &["install", "--help"],
        &["install", "-h"],
        &["docker", "--help"],
        &["docker", "logs", "--help"],
        &["reinstall-python", "--help"],
    ] {
        let out = sb().args(args).assert().success();
        let help = String::from_utf8(out.get_output().stdout.clone()).expect("utf-8");
        assert_one_leading_blank_line(args, &help);
    }
}

/// `path`'s short help (`-h`) at 120 columns, piped and plain.
fn short_help_at_120(path: &[&str]) -> String {
    let out = sb()
        .args(path)
        .args(["-h", "--color=never"])
        .env("COLUMNS", "120")
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).expect("utf-8")
}

/// The rows of `help`'s COMMANDS (if any) and FLAGS sections.
fn rows(help: &str) -> (Vec<&str>, Vec<&str>) {
    let section = |name: &str| {
        help.split_once(&format!("  {name}\n\n"))
            .map(|(_, rest)| rest.split_once("\n\n").map_or(rest, |(rows, _)| rows))
            .map_or_default(|rows| rows.lines().collect())
    };
    (section("COMMANDS"), section("FLAGS"))
}

/// At 120 columns, fang's widest, no command's or flag's short help wraps
/// (the maintainer, 2026-11-14): in `sb -h` and every command's `-h`, each
/// row is one line. A row starts four spaces in; a wrapped one goes on in
/// a line further in. Each of those helps, and its `--help`, leads with
/// one blank line.
#[test]
fn no_short_help_wraps_at_120_columns() {
    let mut paths: Vec<Vec<String>> = vec![Vec::new()];
    let mut seen: Vec<String> = Vec::new();
    while let Some(path) = paths.pop() {
        let args: Vec<&str> = path.iter().map(String::as_str).collect();
        let help = short_help_at_120(&args);
        assert_one_leading_blank_line(&args, &help);
        let long = sb().args(&args).arg("--help").assert().success();
        let long = String::from_utf8(long.get_output().stdout.clone()).expect("utf-8");
        assert_one_leading_blank_line(&args, &long);
        let (commands, flags) = rows(&help);
        assert!(!flags.is_empty(), "{args:?}: {help}");
        for row in commands.iter().chain(&flags) {
            assert!(
                row.starts_with("    ") && !row.starts_with("     "),
                "{args:?}: a wrapped row:\n{help}"
            );
        }
        for row in commands {
            let name = row.split_whitespace().next().expect("a name");
            if name != "help" {
                paths.push(path.iter().cloned().chain([name.to_owned()]).collect());
            }
        }
        seen.push(args.join(" "));
    }
    // The walk reaches the nested commands, not only the root's.
    for nested in ["docker logs", "docker ps", "edit", "reinstall-python"] {
        assert!(seen.iter().any(|p| p == nested), "{nested}: {seen:?}");
    }
    assert!(
        seen.iter().filter(|p| p.starts_with("edit ")).count() > 1,
        "{seen:?}"
    );
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
        at_80.contains(&format!("\n                       -e @vars.yml         a YAML/JSON file, relative to the\n{}repository the playbook runs in\n", " ".repeat(44))),
        "{at_80}"
    );
    let at_120 = install_help(Some("120"));
    assert!(widest(&at_120) <= 120, "{at_120}");
    assert!(widest(&at_120) > 80, "{at_120}");
    assert!(
        at_120.contains("\n                       -e @vars.yml         a YAML/JSON file, relative to the repository the playbook runs in\n"),
        "{at_120}"
    );
    assert_eq!(install_help(Some("200")), at_120);
    assert_eq!(install_help(None), at_120, "no terminal, no COLUMNS");
    // Too narrow for the help column: the text keeps 20 columns.
    let narrow = install_help(Some("20"));
    assert!(
        narrow.contains(
            "\n                     Each value reaches\n                     Ansible whole, as\n"
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
        text.contains(&format!(
            "relative to the repository\n{}the playbook runs in\n",
            " ".repeat(44)
        )),
        "{text}"
    );
}

/// `-e`'s examples (the maintainer, 2026-11-15), each line as written,
/// under the help column and its own two-space indent.
const EXAMPLES: [&str; 8] = [
    "Each value reaches Ansible whole, as one --extra-vars:",
    "  -e my_var=value",
    "  -e 'my_var=value other_var=value'",
    "  -e '{\"my_list\": [1, 2]}'",
    "  -e @vars.yml         a YAML/JSON file, relative to",
    "  -e @/root/vars.yml   or an absolute path",
    "A key=value value is passed as text;",
    "Repeat -e for more; later values win.",
];

/// `sb install --help` shows `-e`'s examples in order at 80, 120 and 200
/// columns, each example's command whole on its line; `-h` shows the
/// hint that `--help` shows more, and no example.
#[test]
fn install_help_shows_the_extra_vars_examples() {
    let column = " ".repeat(21);
    for columns in ["80", "120", "200"] {
        let help = install_help(Some(columns));
        let lines: Vec<&str> = help.lines().collect();
        let mut at = 0;
        for example in EXAMPLES {
            let found = lines[at..]
                .iter()
                .position(|l| l.starts_with(&format!("{column}{example}")))
                .unwrap_or_else(|| panic!("COLUMNS={columns}: {example:?} in order\n{help}"));
            at += found + 1;
        }
        assert!(
            help.contains("    -h --help        Help for install (--help shows more details)\n"),
            "COLUMNS={columns}: {help}"
        );
    }
    let short = sb()
        .args(["install", "-h", "--color=never"])
        .assert()
        .success();
    let short = String::from_utf8(short.get_output().stdout.clone()).expect("utf-8");
    assert!(
        short.contains("    -h --help        Help for install (--help shows more details)\n"),
        "{short}"
    );
    assert!(!short.contains("my_var"), "{short}");
    assert!(!short.contains("Each value"), "{short}");
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
