//! The command-tree contract (spec success criterion 9): every sb command
//! and flag exists in sb-go, with the same name, short flag, help text,
//! visibility and switch-or-value shape. Full equality is the `ops`
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

/// What sb has that sb-go doesn't, on purpose: `--color` is the one new
/// global flag (spec, command tree).
const EXTENSIONS: &[&str] = &["flag\tsb\tglobal\tcolor"];

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
        let about = cmd.get_about().map(ToString::to_string).unwrap_or_default();
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
            let short = arg.get_short().map(String::from).unwrap_or_default();
            let shape = if arg.get_action().takes_values() {
                "value"
            } else {
                "switch"
            };
            let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
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

/// The lines of `tree` that sb-go doesn't have, extensions aside.
fn not_in_sb_go(tree: Vec<String>) -> Vec<String> {
    let sb_go: BTreeSet<&str> = SB_GO_TREE
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
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

/// The walk sees what it should (so an empty walk can't pass): the root,
/// `version`, its `--json`, and `--color` as an extension.
#[test]
fn the_walk_covers_the_tree() {
    let tree = sb_tree();
    assert!(tree.contains(&"command\tsb\tfalse\tSaltbox CLI".to_string()));
    assert!(tree.iter().any(|l| l.starts_with("command\tsb version\t")));
    assert!(
        tree.iter()
            .any(|l| l.starts_with("flag\tsb version\tlocal\tjson\t"))
    );
    assert_eq!(tree.iter().filter(|l| is_extension(l)).count(), 1);
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
