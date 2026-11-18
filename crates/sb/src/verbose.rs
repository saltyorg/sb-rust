//! sb-go's `-v/--verbose` on the commands where sb has no verbose mode:
//! accepted and hidden, changing nothing, with a one-line notice on stderr
//! pointing at the run log (the maintainer, 2026-11-18). `install` keeps
//! its own counted `-v` for Ansible, and `setup`'s stays hidden and
//! silent (the served installer passes it); neither is here.
//!
//! Why clap's builder (`Command::mut_subcommand`, as `force_update_arg`),
//! not a derived field on each command: one table holds sb-go's 14
//! commands, checked against sb-go's command tree, without reshaping 14
//! variants (several flatten another crate's arguments). The cost: a
//! parse that skips [`with_flags`] refuses `-v`; `Cli`'s doc says so, and
//! the four parses (the real one, the colour pre-pass, the completion
//! tree and the contract test) all go through it.

/// sb-go's command, its flag's scope (cobra's persistent flags are
/// `global`) and its help text, from tests/fixtures/sb-go-command-tree.tsv:
/// kept as sb-go has them, so the contract test finds each flag.
const COMMANDS: &[(&[&str], bool, &str)] = &[
    (&["announcements"], true, ENABLE),
    (&["docker", "restart"], false, ENABLE),
    (&["docker", "start"], false, ENABLE),
    (&["docker", "stop"], false, ENABLE),
    (&["list"], false, LEVEL),
    (&["motd"], false, LEVEL),
    (&["reinstall-facts"], true, ENABLE),
    (&["reinstall-python"], true, ENABLE),
    (&["reinstall-venv"], true, ENABLE),
    (&["restore"], false, ENABLE),
    (&["self-update"], false, "Enable verbose debug output"),
    (&["update"], true, ENABLE),
    (&["update-venv"], true, ENABLE),
    (&["validate-config"], true, ENABLE),
];

const ENABLE: &str = "Enable verbose output";
const LEVEL: &str = "Increase verbosity level (can be used multiple times, e.g. -vvv)";

/// The flag's id.
const ID: &str = "verbose";

/// `cmd` with the hidden `-v/--verbose` on each of [`COMMANDS`]: counted,
/// so `-vv` parses as sb-go's took it, and read by [`given`] only.
pub(crate) fn with_flags(cmd: clap::Command) -> clap::Command {
    COMMANDS.iter().fold(cmd, |cmd, (path, global, help)| {
        add(cmd, path, *global, help)
    })
}

fn add(cmd: clap::Command, path: &[&str], global: bool, help: &'static str) -> clap::Command {
    match path {
        [] => cmd.arg(
            clap::Arg::new(ID)
                .short('v')
                .long("verbose")
                .action(clap::ArgAction::Count)
                .global(global)
                .hide(true)
                .help(help),
        ),
        [first, rest @ ..] => cmd.mut_subcommand(*first, |sub| add(sub, rest, global, help)),
    }
}

/// Whether the parsed command is one of [`COMMANDS`] and was given `-v`
/// or `--verbose`, once or more (not `install`'s own, nor `setup`'s).
pub(crate) fn given(matches: &clap::ArgMatches) -> bool {
    let mut matches = matches;
    let mut path = Vec::new();
    while let Some((name, sub)) = matches.subcommand() {
        path.push(name);
        matches = sub;
    }
    COMMANDS
        .iter()
        .any(|(command, _, _)| *command == path.as_slice())
        && matches
            .try_get_one::<u8>(ID)
            .ok()
            .flatten()
            .is_some_and(|count| *count > 0)
}

/// The notice for a run given `-v`: where the detail is, this run's place
/// in the run log (`RunLog::pointer`: the file and the run's id, as the
/// error block names it) when there is a log.
pub(crate) fn notice(pointer: Option<&str>) -> String {
    match pointer {
        Some(pointer) => format!("-v isn't needed: sb logs full detail to {pointer}"),
        None => "-v isn't needed: sb has no verbose output".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use crate::Cli;

    const SB_GO_TREE: &str = include_str!("../tests/fixtures/sb-go-command-tree.tsv");

    /// The commands are sb-go's that have `-v` but `install` and `setup`,
    /// each with sb-go's scope and help.
    #[test]
    fn the_commands_are_sb_gos_but_install_and_setup() {
        let mut theirs: Vec<String> = SB_GO_TREE
            .lines()
            .filter(|line| line.split('\t').nth(3) == Some("verbose"))
            .filter(|line| {
                !line.starts_with("flag\tsb install\t") && !line.starts_with("flag\tsb setup\t")
            })
            .map(ToString::to_string)
            .collect();
        theirs.sort();
        let mut ours: Vec<String> = COMMANDS
            .iter()
            .map(|(path, global, help)| {
                let scope = if *global { "global" } else { "local" };
                format!(
                    "flag\tsb {}\t{scope}\tverbose\tv\tswitch\tfalse\t{help}",
                    path.join(" ")
                )
            })
            .collect();
        ours.sort();
        assert_eq!(ours, theirs);
    }

    /// The leaf command's path, and what it gets from the parse but the
    /// flag: each argument's id, where its value came from, and its raw
    /// values (not their positions, which the flag shifts).
    fn leaf_without_flag(args: &[&str]) -> (String, Vec<String>, bool) {
        let matches = with_flags(Cli::command())
            .try_get_matches_from(args)
            .unwrap_or_else(|err| panic!("{args:?}: {err}"));
        let given = given(&matches);
        let mut leaf = &matches;
        let mut name = String::new();
        while let Some((sub, sub_matches)) = leaf.subcommand() {
            name.push_str(sub);
            name.push(' ');
            leaf = sub_matches;
        }
        let got = leaf
            .ids()
            .filter(|id| id.as_str() != ID)
            .map(|id| {
                let raw: Vec<_> = leaf.get_raw(id.as_str()).into_iter().flatten().collect();
                format!("{id} {:?} {raw:?}", leaf.value_source(id.as_str()))
            })
            .collect();
        (name, got, given)
    }

    /// Each command takes `-v`, `-vv` and `--verbose`, before or after its
    /// other arguments, and gets what it gets without them; only
    /// [`given`] sees the flag.
    #[test]
    fn the_flag_changes_nothing_the_command_gets() {
        for (path, _, _) in COMMANDS {
            let mut plain = vec!["sb"];
            plain.extend_from_slice(path);
            let without = leaf_without_flag(&plain);
            assert!(!without.2, "{path:?}");
            for flag in ["-v", "-vv", "--verbose", "-v -v"] {
                let mut args = plain.clone();
                args.extend(flag.split(' '));
                let with = leaf_without_flag(&args);
                assert_eq!(
                    (&with.0, &with.1),
                    (&without.0, &without.1),
                    "{path:?} {flag}"
                );
                assert!(with.2, "{path:?} {flag}");
            }
        }
        let with = leaf_without_flag(&["sb", "docker", "stop", "-v", "--ignore", "plex"]);
        assert_eq!(with.0, "docker stop ");
        assert!(
            with.1.iter().any(|arg| arg.contains("plex")),
            "{:?}",
            with.1
        );
        assert!(with.2);
    }

    /// `install` keeps its own counted `-v`, `setup` its hidden one, and a
    /// command sb-go gave none still refuses it.
    #[test]
    fn other_commands_are_left_alone() {
        let cmd = || with_flags(Cli::command());
        let install = cmd()
            .try_get_matches_from(["sb", "install", "plex", "-vv"])
            .expect("install");
        assert!(!given(&install), "install's -v is Ansible's");
        let setup = cmd()
            .try_get_matches_from(["sb", "setup", "-v"])
            .expect("setup");
        assert!(!given(&setup), "setup's -v is silent");
        for args in [
            &["sb", "version", "-v"][..],
            &["sb", "diag", "-v"],
            &["sb", "-v", "update"],
        ] {
            assert!(cmd().try_get_matches_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_notice_names_the_log() {
        assert_eq!(
            notice(Some("/var/log/sb/sb.log (run 1a2b)")),
            "-v isn't needed: sb logs full detail to /var/log/sb/sb.log (run 1a2b)"
        );
        assert_eq!(notice(None), "-v isn't needed: sb has no verbose output");
    }
}
