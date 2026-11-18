//! The command-tree contract (spec success criterion 9): every sb command
//! and flag exists in sb-go, with the same name, short flag, help text,
//! visibility and switch-or-value shape, but the texts `REWORDED` declares
//! (a short help that wrapped at 120 columns) and the `EXTENSIONS`.
//!
//! sb-go's tree is `tests/fixtures/sb-go-command-tree.tsv` (its header
//! says how it was made). clap's tree is walked as built by derive, before
//! clap adds `help` and `-h/--help` (cobra adds its help flag at run time
//! too, so neither side lists it).
//!
//! Both ways (2026-11-18): every sb-go command and flag is sb's too, or
//! listed in `REMOVED` with its reason; sb-go's aliases (none), the count
//! of positional arguments each runnable command takes (cobra's `Args`
//! check of 0 to 3 arguments, against clap's parse of the same), and each
//! flag's default are compared, a difference listed with its reason. Not
//! compared: the order commands are listed in (cobra sorts them, clap
//! keeps declaration order).

use std::collections::BTreeSet;

use clap::CommandFactory;

use crate::Cli;

const SB_GO_TREE: &str = include_str!("../tests/fixtures/sb-go-command-tree.tsv");

/// What sb has that sb-go doesn't, on purpose: `--color`, the one new
/// global flag, `sb docker ps --json` (cli-core spec, command tree;
/// docker spec decision 2), `sb list --json` (ansible decision 20),
/// `sb validate-config --json` (config decision 9), `sb edit --no-edit-again` (tui spec,
/// editors, difference 22), `sb setup`'s `--keep-branch` and
/// `--reset-branch`, update's flags (ops decision 29, difference 6),
/// `sb update --migrations` (ops decision 19, difference 23), and
/// `sb restore --user` and `--password-file` (ops spec, decision 8; the
/// cli-core spec's extensions), `sb announcements`' hidden `preview`
/// and `check` with their flags (ops decisions 34 and 35, differences 60
/// and 61), `sb self-update --channel` (release spec, "Release
/// channels", difference 12), and `sb edit motd` and `sb edit providers`
/// (tui spec, Difference 38).
const EXTENSIONS: &[&str] = &[
    "flag\tsb\tglobal\tcolor",
    "flag\tsb docker ps\tlocal\tjson",
    "flag\tsb list\tlocal\tjson",
    "flag\tsb validate-config\tlocal\tjson",
    "flag\tsb edit\tglobal\tno-edit-again",
    "flag\tsb setup\tglobal\tkeep-branch",
    "flag\tsb setup\tglobal\treset-branch",
    "flag\tsb update\tglobal\tmigrations",
    "flag\tsb restore\tlocal\tuser",
    "flag\tsb restore\tlocal\tpassword-file",
    "command\tsb announcements preview",
    "flag\tsb announcements preview\tlocal\tsince",
    "flag\tsb announcements preview\tlocal\trepo",
    "flag\tsb announcements preview\tlocal\trun-migrations",
    "command\tsb announcements check",
    "flag\tsb announcements check\tlocal\trepo",
    "flag\tsb self-update\tlocal\tchannel",
    "command\tsb edit motd",
    "command\tsb edit providers",
];

/// sb-go's lines sb words differently on purpose, with sb's line for each:
/// a short help that wrapped in `sb --help` at fang's 120 columns (the
/// maintainer, 2026-11-14), a command sb shows that sb-go hid (the
/// maintainer, 2026-11-15), setup's `-v`, accepted but hidden and
/// ignored, the other commands' `-v`, accepted, hidden and noticed (the
/// maintainer, 2026-11-18), and `sb bench`'s help, as bench is rewritten
/// in Rust (the ops spec lists `command\tsb bench` as an extension; a
/// rewording is the
/// stricter check, as only the help text differs). Each is a difference
/// in the spec that owns the command (python-env difference 15, ops
/// differences 31, 1 and 15).
const REWORDED: &[(&str, &str)] = &[
    (
        "command\tsb reinstall-python\tfalse\tReinstall the Python version used by Saltbox and related Ansible virtual environment using uv",
        "command\tsb reinstall-python\tfalse\tReinstall Saltbox's Python and its Ansible virtual environment using uv",
    ),
    (
        "command\tsb announcements\ttrue\tDisplay announcements from announcement files",
        "command\tsb announcements\tfalse\tDisplay announcements from announcement files",
    ),
    (
        "flag\tsb setup\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb setup\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "command\tsb bench\tfalse\tRuns bench.sh benchmark",
        "command\tsb bench\tfalse\tBenchmarks this server's system, disk and network",
    ),
    // sb-go's `-v` where sb has no verbose mode: accepted, hidden, a
    // notice (the maintainer, 2026-11-18; crate::verbose).
    (
        "flag\tsb announcements\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb announcements\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb docker restart\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb docker restart\tlocal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb docker start\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb docker start\tlocal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb docker stop\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb docker stop\tlocal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb list\tlocal\tverbose\tv\tswitch\tfalse\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
        "flag\tsb list\tlocal\tverbose\tv\tswitch\ttrue\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
    ),
    (
        "flag\tsb motd\tlocal\tverbose\tv\tswitch\tfalse\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
        "flag\tsb motd\tlocal\tverbose\tv\tswitch\ttrue\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
    ),
    (
        "flag\tsb reinstall-facts\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb reinstall-facts\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb reinstall-python\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb reinstall-python\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb reinstall-venv\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb reinstall-venv\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb restore\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb restore\tlocal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb self-update\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose debug output",
        "flag\tsb self-update\tlocal\tverbose\tv\tswitch\ttrue\tEnable verbose debug output",
    ),
    (
        "flag\tsb update\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb update\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb update-venv\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb update-venv\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
    (
        "flag\tsb validate-config\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "flag\tsb validate-config\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output",
    ),
];

/// sb's commands and flags in the fixture's line format.
fn sb_tree() -> Vec<String> {
    tree(&crate::verbose::with_flags(Cli::command()))
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

/// `sb motd` has every flag of sb-go's (cmd/motd.go:68-100), `-v`
/// hidden: accepted with a notice, as sb has no verbose mode (motd
/// decision 9, the maintainer, 2026-11-18).
#[test]
fn motd_has_every_sb_go_flag_verbose_hidden() {
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
    assert!(tree.iter().any(|ours| ours
        == "flag\tsb motd\tlocal\tverbose\tv\tswitch\ttrue\tIncrease verbosity level (can be used multiple times, e.g. -vvv)"));
}

/// `sb announcements` has none of sb-go's flags (cmd/announcements.go)
/// but a hidden `-v`, and none of its own: each of sb-go's other flags is
/// missing on purpose.
#[test]
fn announcements_has_no_flag_of_its_own() {
    let tree = sb_tree();
    let missing: Vec<&str> = SB_GO_TREE
        .lines()
        .filter(|line| line.starts_with("flag\tsb announcements\t"))
        .filter(|line| !tree.iter().any(|ours| ours == line))
        .collect();
    assert_eq!(
        missing,
        [
            // "New" is `sb update`'s alone, which compares the files
            // before and after its pull in-process; nothing ever called
            // sb-go's hidden command with them (the maintainer,
            // 2026-11-17; ops difference 45).
            "flag\tsb announcements\tglobal\tafter\ta\tvalue\tfalse\tPath to the after announcements.yml file",
            "flag\tsb announcements\tglobal\tbefore\tb\tvalue\tfalse\tPath to the before announcements.yml file",
            // Both repositories are always shown (the maintainer,
            // 2026-11-17; ops difference 45).
            "flag\tsb announcements\tglobal\trepo\tr\tvalue\tfalse\tRepository name (Saltbox or Sandbox)",
            // sb has no verbose mode (ops decision 18): `-v` is accepted
            // hidden, with a notice (the maintainer, 2026-11-18).
            "flag\tsb announcements\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        ]
    );
    let ours: Vec<&String> = tree
        .iter()
        .filter(|line| line.starts_with("flag\tsb announcements\t"))
        .collect();
    assert_eq!(
        ours,
        ["flag\tsb announcements\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output"],
        "no flag of its own"
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

/// python-env's commands are sb-go's, `update-venv` hidden, `-v` hidden
/// too (python-env decision 9 and its 2026-11-18 amendment; success
/// criterion 11).
#[test]
fn python_env_commands_have_every_sb_go_flag() {
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
        (
            "reinstall-facts",
            false,
            "Reinstall the Rust saltbox.fact file",
        ),
    ] {
        let missing: Vec<&str> = SB_GO_TREE
            .lines()
            .filter(|line| line.starts_with("command\t") || line.starts_with("flag\t"))
            .filter(|line| line.contains(&format!("\tsb {name}\t")))
            .filter(|line| !REWORDED.iter().any(|(theirs, _)| line == theirs))
            .filter(|line| !tree.iter().any(|ours| ours == line))
            .collect();
        // `-v` is reworded hidden (the maintainer, 2026-11-18), so none
        // is missing.
        assert_eq!(missing, Vec::<&str>::new(), "{name}");
        assert!(
            tree.contains(&format!(
                "flag\tsb {name}\tglobal\tverbose\tv\tswitch\ttrue\tEnable verbose output"
            )),
            "{name}"
        );
        assert!(
            tree.contains(&format!("command\tsb {name}\t{hidden}\t{about}")),
            "{name}"
        );
    }
}

/// sb-go's commands and flags sb doesn't have, on purpose, each with its
/// reason (the spec that owns the command).
const REMOVED: &[(&str, &str)] = &[
    (
        "flag\tsb announcements\tglobal\tafter\ta\tvalue\tfalse\tPath to the after announcements.yml file",
        "\"new\" is sb update's alone (ops difference 45)",
    ),
    (
        "flag\tsb announcements\tglobal\tbefore\tb\tvalue\tfalse\tPath to the before announcements.yml file",
        "\"new\" is sb update's alone (ops difference 45)",
    ),
    (
        "flag\tsb announcements\tglobal\trepo\tr\tvalue\tfalse\tRepository name (Saltbox or Sandbox)",
        "both repositories are always shown (ops difference 45)",
    ),
    (
        "flag\tsb announcements\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode; the run log has the detail (ops decision 18)",
    ),
    (
        "flag\tsb docker restart\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (docker decision 6)",
    ),
    (
        "flag\tsb docker start\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (docker decision 6)",
    ),
    (
        "flag\tsb docker stop\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (docker decision 6)",
    ),
    (
        "flag\tsb list\tlocal\tverbose\tv\tswitch\tfalse\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
        "no verbose mode (ansible decision 14)",
    ),
    (
        "flag\tsb motd\tlocal\tverbose\tv\tswitch\tfalse\tIncrease verbosity level (can be used multiple times, e.g. -vvv)",
        "no verbose mode (motd decision 9)",
    ),
    (
        "flag\tsb reinstall-facts\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (python-env decision 9)",
    ),
    (
        "flag\tsb reinstall-python\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (python-env decision 9)",
    ),
    (
        "flag\tsb reinstall-venv\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (python-env decision 9)",
    ),
    (
        "flag\tsb restore\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (ops decision 3)",
    ),
    (
        "flag\tsb self-update\tlocal\tverbose\tv\tswitch\tfalse\tEnable verbose debug output",
        "no verbose mode (release decision 13)",
    ),
    (
        "flag\tsb update\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (ops decision 18)",
    ),
    (
        "flag\tsb update-venv\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode (python-env decision 9)",
    ),
    (
        "flag\tsb validate-config\tglobal\tverbose\tv\tswitch\tfalse\tEnable verbose output",
        "no verbose mode; the run log has the detail (config decision 1)",
    ),
];

/// sb-go's lines of a kind (`command`, `flag`, `alias`, `args`,
/// `default`), split at their tabs.
fn sb_go(kind: &str) -> Vec<Vec<&'static str>> {
    SB_GO_TREE
        .lines()
        .filter(|line| line.split('\t').next() == Some(kind))
        .map(|line| line.split('\t').collect())
        .collect()
}

/// sb-go's command and flag lines sb doesn't have, rewordings and
/// `REMOVED` aside.
fn not_in_sb(tree: &[String]) -> Vec<&'static str> {
    SB_GO_TREE
        .lines()
        .filter(|line| line.starts_with("command\t") || line.starts_with("flag\t"))
        .filter(|line| !tree.iter().any(|ours| ours == line))
        .filter(|line| !REWORDED.iter().any(|(theirs, _)| theirs == line))
        .filter(|line| !REMOVED.iter().any(|(gone, _)| gone == line))
        .collect()
}

/// The other way round: every sb-go command and flag is sb's, reworded,
/// or removed on purpose with its reason; each `REMOVED` entry is one sb
/// really lacks (a stale entry fails).
#[test]
fn every_sb_go_command_and_flag_is_sbs_or_removed() {
    let tree = sb_tree();
    let missing = not_in_sb(&tree);
    assert!(
        missing.is_empty(),
        "sb-go's, not in sb and not listed as removed:\n{}",
        missing.join("\n")
    );
    for (gone, reason) in REMOVED {
        assert!(
            SB_GO_TREE.lines().any(|line| line == *gone),
            "not sb-go's: {gone}"
        );
        assert!(!tree.iter().any(|line| line == gone), "sb has it: {gone}");
        assert!(!reason.is_empty(), "{gone}");
    }
}

/// `root`'s aliases in the fixture's line format.
fn aliases(root: &clap::Command) -> Vec<String> {
    fn walk(cmd: &clap::Command, parent: Option<&str>, lines: &mut Vec<String>) {
        let path = match parent {
            Some(parent) => format!("{parent} {}", cmd.get_name()),
            None => cmd.get_name().to_string(),
        };
        for alias in cmd.get_all_aliases() {
            lines.push(format!("alias\t{path}\t{alias}"));
        }
        // Spellings sb-go lacks too: pflag has no flag aliases.
        for short in cmd.get_all_short_flag_aliases() {
            lines.push(format!("alias\t{path}\t-{short}"));
        }
        for long in cmd.get_all_long_flag_aliases() {
            lines.push(format!("alias\t{path}\t--{long}"));
        }
        for arg in cmd.get_arguments() {
            for alias in arg.get_all_aliases().unwrap_or_default() {
                lines.push(format!("alias\t{path}\t--{alias}"));
            }
            for short in arg.get_all_short_aliases().unwrap_or_default() {
                lines.push(format!("alias\t{path}\t-{short}"));
            }
        }
        for sub in cmd.get_subcommands() {
            walk(sub, Some(&path), lines);
        }
    }
    let mut lines = Vec::new();
    walk(root, None, &mut lines);
    lines
}

/// sb-go has no aliases, and neither has sb.
#[test]
fn aliases_are_sb_gos() {
    let theirs: Vec<String> = sb_go("alias").iter().map(|f| f.join("\t")).collect();
    assert_eq!(aliases(&crate::Cli::command()), theirs);
}

/// What clap makes of `path` (sb-go's command path) followed by `words`:
/// `ok` when it parses, `help` when it shows help instead (a group
/// alone), else `error`.
fn sb_takes(path: &str, words: &str) -> &'static str {
    let mut argv: Vec<&str> = path.split(' ').collect();
    argv.extend(words.split(' ').filter(|w| !w.is_empty()));
    match crate::Cli::command().try_get_matches_from(argv) {
        Ok(_) => "ok",
        Err(err) => match err.kind() {
            clap::error::ErrorKind::DisplayHelp
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => "help",
            _ => "error",
        },
    }
}

/// The commands that show their help alone, as sb-go's `RunE` shows it
/// (cmd/docker.go): sb's `help` there is sb-go's `ok`.
const HELP_ALONE: &[&str] = &["sb docker"];

/// Where sb takes positional arguments sb-go's `Args` check takes, but
/// refuses them, on purpose: (path, counts, reason).
const ARGS_DIFFER: &[(&str, &[usize], &str)] = &[(
    "sb docker",
    &[1, 2, 3],
    "sb-go's Args takes them and its RunE refuses them, `unknown command`, exit 1; sb's parse error, exit 2 (docker spec, Differences, `sb docker <unknown>`)",
)];

/// Every runnable sb-go command takes as many positional arguments in sb
/// (0 to 3, cobra's check of the fixture's words against clap's parse of
/// them), but the differences listed; each listed one is a fixture line
/// and a real difference. The root takes none.
#[test]
fn positional_arguments_are_sb_gos() {
    let lines = sb_go("args");
    assert_eq!(
        lines.len(),
        40 * 4,
        "every runnable sb-go command, 0 to 3 arguments"
    );
    let mut differ = Vec::new();
    for line in &lines {
        let (path, count, words, theirs) = (
            line[1],
            line[2].parse::<usize>().expect("a count"),
            line[3],
            line[4],
        );
        let ours = match sb_takes(path, words) {
            "help" if HELP_ALONE.contains(&path) => "ok",
            other => other,
        };
        let listed = ARGS_DIFFER
            .iter()
            .find(|(p, counts, _)| *p == path && counts.contains(&count));
        match listed {
            Some((_, _, reason)) => assert_ne!(
                ours, theirs,
                "listed but the same: {path} {count}: {reason}"
            ),
            None if ours != theirs => {
                differ.push(format!(
                    "{path} {count} ({words}): sb-go {theirs}, sb {ours}"
                ));
            }
            None => {}
        }
    }
    assert!(
        differ.is_empty(),
        "positional arguments not as in sb-go:\n{}",
        differ.join("\n")
    );
    for (path, counts, _) in ARGS_DIFFER {
        for count in *counts {
            assert!(
                lines
                    .iter()
                    .any(|l| l[1] == *path && l[2] == count.to_string()),
                "not a fixture line: {path} {count}"
            );
        }
    }
    // sb-go's root isn't runnable: alone its help, an argument unknown.
    assert_eq!(sb_takes("sb", "arg1"), "error");
}

/// Each flag's default as clap has it, by path and long name; an unset
/// default is empty.
fn sb_defaults(root: &clap::Command) -> Vec<(String, String, String)> {
    fn walk(cmd: &clap::Command, parent: Option<&str>, out: &mut Vec<(String, String, String)>) {
        let path = match parent {
            Some(parent) => format!("{parent} {}", cmd.get_name()),
            None => cmd.get_name().to_string(),
        };
        for arg in cmd.get_arguments().filter(|a| !a.is_positional()) {
            let values: Vec<String> = arg
                .get_default_values()
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect();
            out.push((
                path.clone(),
                arg.get_long().unwrap_or_default().to_string(),
                values.join(","),
            ));
        }
        for sub in cmd.get_subcommands() {
            walk(sub, Some(&path), out);
        }
    }
    let mut out = Vec::new();
    walk(root, None, &mut out);
    out
}

/// A default as compared: cobra's zero values (`false`, `0`, `[]`, an
/// empty string) are no default, as clap's unset one is.
fn meaningful(default: &str) -> &str {
    match default {
        "false" | "0" | "[]" => "",
        other => other,
    }
}

/// Every flag sb shares with sb-go has sb-go's default (motd's font,
/// title and banner type, setup's branch; the switches and lists none).
#[test]
fn defaults_are_sb_gos() {
    let ours = sb_defaults(&crate::Cli::command());
    // A flag sb lacks is one `REMOVED` lists, by its path, scope and name.
    let removed = |path: &str, scope: &str, name: &str| {
        REMOVED
            .iter()
            .any(|(line, _)| line.starts_with(&format!("flag\t{path}\t{scope}\t{name}\t")))
    };
    for line in sb_go("default") {
        let (path, scope, name, theirs) = (line[1], line[2], line[3], line[4]);
        if removed(path, scope, name) {
            continue;
        }
        let (_, _, default) = ours
            .iter()
            .find(|(p, n, _)| p == path && n == name)
            .unwrap_or_else(|| panic!("sb has no {path} --{name}"));
        assert_eq!(meaningful(default), meaningful(theirs), "{path} --{name}");
    }
    let named = |path: &str, name: &str| {
        ours.iter()
            .find(|(p, n, _)| p == path && n == name)
            .map(|(_, _, d)| d.as_str())
    };
    assert_eq!(named("sb setup", "branch"), Some("master"));
    assert_eq!(named("sb motd", "font"), Some("ivrit"));
}
