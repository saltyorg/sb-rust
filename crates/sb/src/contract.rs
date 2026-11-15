//! The command-tree contract (spec success criterion 9): every sb command
//! and flag exists in sb-go, with the same name, short flag, help text,
//! visibility and switch-or-value shape, but the texts `REWORDED` declares
//! (a short help that wrapped at 120 columns). Full equality is the `ops`
//! module's done criterion; until then sb has a subset.
//!
//! sb-go's tree is `tests/fixtures/sb-go-command-tree.tsv` (its header
//! says how it was made). clap's tree is walked as built by derive, before
//! clap adds `help` and `-h/--help` (cobra adds its help flag at run time
//! too, so neither side lists it).
//!
//! Not compared yet: aliases, positional arguments (sb-go checks their
//! count through cobra's `Args`), default values and the order commands
//! are listed in (cobra sorts them, clap keeps declaration order). The
//! `ops` module's full-equality criterion covers them.

use std::collections::BTreeSet;

use clap::CommandFactory;

use crate::Cli;

const SB_GO_TREE: &str = include_str!("../tests/fixtures/sb-go-command-tree.tsv");

/// What sb has that sb-go doesn't, on purpose: `--color`, the one new
/// global flag, and `sb docker ps --json` (cli-core spec, command tree;
/// docker spec decision 2).
const EXTENSIONS: &[&str] = &["flag\tsb\tglobal\tcolor", "flag\tsb docker ps\tlocal\tjson"];

/// sb-go's lines sb words differently on purpose, with sb's line for each:
/// a short help that wrapped in `sb --help` at fang's 120 columns
/// (the maintainer, 2026-11-14). Each is a difference in the spec that
/// owns the command's help (python-env difference 15).
const REWORDED: &[(&str, &str)] = &[(
    "command\tsb reinstall-python\tfalse\tReinstall the Python version used by Saltbox and related Ansible virtual environment using uv",
    "command\tsb reinstall-python\tfalse\tReinstall Saltbox's Python and its Ansible virtual environment using uv",
)];

/// sb's commands and flags in the fixture's line format.
fn sb_tree() -> Vec<String> {
    tree(&Cli::command())
}

/// `root`'s commands and flags in the fixture's line format.
fn tree(root: &clap::Command) -> Vec<String> {
    fn walk(cmd: &clap::Command, parent: Option<&str>, lines: &mut Vec<String>) {
        let path = match parent {
            Some(parent) => format!("{parent} {}", cmd.get_name()),
            None => cmd.get_name().to_string(),
        };
        let about = cmd.get_about().map_or_default(ToString::to_string);
        lines.push(format!("command\t{path}\t{}\t{about}", cmd.is_hide_set()));
        for arg in cmd.get_arguments() {
            if arg.is_positional() {
                continue;
            }
            // Every sb-go flag has a long name, so a short-only flag can
            // never match: it shows up as missing rather than unchecked.
            let long = arg.get_long().unwrap_or_default();
            let scope = if arg.is_global_set() {
                "global"
            } else {
                "local"
            };
            let short = arg.get_short().map_or_default(String::from);
            let shape = if arg.get_action().takes_values() {
                "value"
            } else {
                "switch"
            };
            let help = arg.get_help().map_or_default(ToString::to_string);
            lines.push(format!(
                "flag\t{path}\t{scope}\t{long}\t{short}\t{shape}\t{}\t{help}",
                arg.is_hide_set()
            ));
        }
        for sub in cmd.get_subcommands() {
            walk(sub, Some(&path), lines);
        }
    }
    let mut lines = Vec::new();
    walk(root, None, &mut lines);
    lines
}

/// The lines of `tree` that sb-go doesn't have, extensions aside; a
/// reworded line counts as sb-go's when sb-go has the line it replaces.
fn not_in_sb_go(tree: Vec<String>) -> Vec<String> {
    let mut sb_go: BTreeSet<&str> = SB_GO_TREE
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    for (theirs, ours) in REWORDED {
        if sb_go.contains(theirs) {
            sb_go.insert(ours);
        }
    }
    tree.into_iter()
        .filter(|line| !is_extension(line) && !sb_go.contains(line.as_str()))
        .collect()
}

fn is_extension(line: &str) -> bool {
    EXTENSIONS
        .iter()
        .any(|ext| line.starts_with(&format!("{ext}\t")))
}

#[test]
fn every_sb_command_and_flag_matches_sb_go() {
    let missing = not_in_sb_go(sb_tree());
    assert!(
        missing.is_empty(),
        "not as in sb-go:\n{}",
        missing.join("\n")
    );
}

/// Every rewording replaces a line sb-go has, and sb shows its own line,
/// not sb-go's: a stale or mistyped entry fails here.
#[test]
fn every_rewording_replaces_a_line_of_sb_gos() {
    let tree = sb_tree();
    for (theirs, ours) in REWORDED {
        assert!(SB_GO_TREE.lines().any(|line| line == *theirs), "{theirs}");
        assert!(tree.iter().any(|line| line == ours), "{ours}");
        assert!(!tree.iter().any(|line| line == theirs), "{theirs}");
    }
}

/// The walk sees what it should (so an empty walk can't pass): the root,
/// `version`, its `--json`, and every extension.
#[test]
fn the_walk_covers_the_tree() {
    let tree = sb_tree();
    assert!(tree.contains(&"command\tsb\tfalse\tSaltbox CLI".to_string()));
    assert!(tree.iter().any(|l| l.starts_with("command\tsb version\t")));
    assert!(
        tree.iter()
            .any(|l| l.starts_with("flag\tsb version\tlocal\tjson\t"))
    );
    assert_eq!(
        tree.iter().filter(|l| is_extension(l)).count(),
        EXTENSIONS.len()
    );
}

/// `sb motd` has every flag of sb-go's (cmd/motd.go:68-100) but `-v`,
/// dropped with a parse error (motd decision 9).
#[test]
fn motd_has_every_sb_go_flag_but_verbose() {
    let tree = sb_tree();
    let sb_go: Vec<&str> = SB_GO_TREE
        .lines()
        .filter(|line| {
            line.starts_with("flag\tsb motd\t") || line.starts_with("command\tsb motd\t")
        })
        .collect();
    assert_eq!(sb_go.len(), 34, "sb-go's motd command and its 33 flags");
    let missing: Vec<&str> = sb_go
        .into_iter()
        .filter(|line| !tree.iter().any(|ours| ours == line))
        .collect();
    assert_eq!(
        missing,
        [
            "flag\tsb motd\tlocal\tverbose\tv\tswitch\tfalse\tIncrease verbosity level (can be used multiple times, e.g. -vvv)"
        ]
    );
}

/// A flag with only a short name, or one sb-go lacks, is reported, not
/// skipped; positional arguments are left out.
#[test]
fn flags_sb_go_lacks_are_reported() {
    let cmd = clap::Command::new("sb")
        .about("Saltbox CLI")
        .arg(
            clap::Arg::new("v")
                .short('v')
                .action(clap::ArgAction::SetTrue),
        )
        .arg(clap::Arg::new("target"));
    assert_eq!(
        not_in_sb_go(tree(&cmd)),
        vec!["flag\tsb\tlocal\t\tv\tswitch\tfalse\t".to_string()]
    );
}

/// python-env's commands are sb-go's, `update-venv` hidden, but for `-v`
/// (python-env decision 9; success criterion 11).
#[test]
fn python_env_commands_have_every_sb_go_flag_but_verbose() {
    let tree = sb_tree();
    for (name, hidden, about) in [
        (
            "update-venv",
            true,
            "Update the Ansible virtual environment and saltbox.fact",
        ),
        (
            "reinstall-venv",
            false,
            "Reinstall the Ansible virtual environment",
        ),
        (
            "reinstall-python",
            false,
            "Reinstall Saltbox's Python and its Ansible virtual environment using uv",
        ),
    ] {
        let missing: Vec<&str> = SB_GO_TREE
            .lines()
            .filter(|line| line.contains(&format!("\tsb {name}\t")))
            .filter(|line| !REWORDED.iter().any(|(theirs, _)| line == theirs))
            .filter(|line| !tree.iter().any(|ours| ours == line))
            .collect();
        assert_eq!(
            missing,
            [format!(
                "flag\tsb {name}\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output"
            )],
            "{name}"
        );
        assert!(
            tree.contains(&format!("command\tsb {name}\t{hidden}\t{about}")),
            "{name}"
        );
    }
}
