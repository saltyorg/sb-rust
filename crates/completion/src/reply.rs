//! The reply to a completion request (spec, decision 11): clap's engine's
//! candidates (`clap_complete::engine::complete`), without the hidden
//! commands and flags, written as clap's adapter for the shell writes
//! them (`clap_complete` 4.6.11, `env/shells.rs`, each adapter's
//! `write_complete`; `unstable-dynamic`, ADR-001).
//!
//! Why not clap's adapters as they are: the engine keeps a hidden
//! candidate when no shown one matches the typed prefix
//! (`engine/complete.rs:217-218`, clap issue #3951: "keep a completion
//! hidden unless there are no visible choices"), so `sb self<Tab>` offered
//! `self-update`; `clap_complete` has no option or hook to drop them, and
//! `EnvCompleter` is the extension point `CompleteEnv::shells` takes
//! (<https://docs.rs/clap_complete/4.6.11/clap_complete/env/trait.EnvCompleter.html>).

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use clap_complete::engine::CompletionCandidate;

use crate::{Shell, describe};

/// Write the reply to the request `args` (the words after `--`, `sb`
/// first) from `shell`'s stub to `buf`: the index and separator from the
/// environment the stub sets, as clap's adapters read them.
///
/// # Errors
///
/// The engine's (a request it can't place) and `buf`'s.
pub(crate) fn write(
    shell: Shell,
    cmd: &mut clap::Command,
    args: Vec<OsString>,
    current_dir: Option<&Path>,
    buf: &mut dyn Write,
) -> std::io::Result<()> {
    let var = |name: &str| std::env::var(name).ok();
    let (args, index) = request(shell, args, var("_CLAP_COMPLETE_INDEX"));
    let candidates = clap_complete::engine::complete(cmd, args.clone(), index, current_dir)?;
    let shown = shown(cmd, &args, index, candidates);
    let stub = Stub {
        ifs: var("_CLAP_IFS"),
        comp_type: var("_CLAP_COMPLETE_COMP_TYPE"),
        columns: var(describe::COLUMNS_VAR),
    };
    respond(shell, &shown, &stub, buf)
}

/// What the stub passed besides the words: the separator, and bash's
/// `COMP_TYPE` and width (only bash's stub passes those two).
#[derive(Debug, Default)]
struct Stub {
    ifs: Option<String>,
    comp_type: Option<String>,
    columns: Option<String>,
}

/// `candidates` written for `shell`: bash's descriptions on a listing
/// call with the width known (spec, "Descriptions in bash"; the module
/// `describe`), else clap's format ([`format`]).
fn respond(
    shell: Shell,
    candidates: &[CompletionCandidate],
    stub: &Stub,
    buf: &mut dyn Write,
) -> std::io::Result<()> {
    let ifs = stub.ifs.as_deref();
    if shell == Shell::Bash && describe::listing(stub.comp_type.as_deref()) {
        let columns = stub
            .columns
            .as_deref()
            .and_then(|columns| columns.parse().ok());
        if let Some(entries) = columns.and_then(|columns| describe::described(candidates, columns))
        {
            return write!(buf, "{}", entries.join(ifs.unwrap_or("\n")));
        }
    }
    format(shell, candidates, ifs, buf)
}

/// The words and the current word's index as clap's adapter for `shell`
/// takes them: bash's and zsh's stubs pass the index (`index`, 0 when
/// unset or not a number), and zsh's passes no word for an empty one at
/// the end, so one is added; fish's current word is the last.
fn request(shell: Shell, mut args: Vec<OsString>, index: Option<String>) -> (Vec<OsString>, usize) {
    let index = match shell {
        Shell::Fish => args.len().saturating_sub(1),
        Shell::Bash | Shell::Zsh => index.and_then(|i| i.parse().ok()).unwrap_or_default(),
    };
    if shell == Shell::Zsh && args.len() == index {
        args.push(OsString::new());
    }
    (args, index)
}

/// `candidates` without those for a hidden command or argument of the
/// command being completed. A hidden candidate of another kind stays, as
/// the engine keeps it: a shown command's hidden alias (`accounts.yml` for
/// `edit accounts`), a dot file. Then, on an empty word, none at all when
/// only flags are left (spec, difference 6, the maintainer's rule): every
/// shell would insert their common `--`, and sb-go offers flags only
/// after a `-`. The rest in alphabetical order.
fn shown(
    cmd: &clap::Command,
    args: &[OsString],
    index: usize,
    candidates: Vec<CompletionCandidate>,
) -> Vec<CompletionCandidate> {
    let completed = completed_command(cmd, args, index);
    let mut shown: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| !(candidate.is_hide_set() && for_hidden(completed, candidate)))
        .collect();
    let empty_word = args.get(index).is_none_or(|word| word.is_empty());
    let flag = |candidate: &CompletionCandidate| {
        candidate.get_value().as_encoded_bytes().starts_with(b"-")
    };
    if empty_word && shown.iter().all(flag) {
        return Vec::new();
    }
    // Alphabetical, by the text (spec, "Every list is in alphabetical
    // order"): the engine orders by heading and declaration, and clap's
    // stubs keep the reply's order (bash's `-o nosort`, zsh's `_describe
    // -V`, fish's `--keep-order`), so the order is set here, for every
    // shell at once, and no stub changes.
    shown.sort_by(|a, b| a.get_value().cmp(b.get_value()));
    shown
}

/// The command the word at `index` belongs to, found as the engine finds
/// it: each earlier word that names a subcommand of the command so far
/// (hidden ones included) moves to it, the binary's name aside
/// (`engine/complete.rs`, `complete`'s loop).
fn completed_command<'a>(
    cmd: &'a clap::Command,
    args: &[OsString],
    index: usize,
) -> &'a clap::Command {
    let binary = usize::from(!cmd.is_no_binary_name_set());
    args.iter()
        .take(index)
        .skip(binary)
        .fold(cmd, |current, word| {
            word.to_str()
                .and_then(|word| current.find_subcommand(word))
                .unwrap_or(current)
        })
}

/// Whether `candidate` is for a hidden subcommand or argument of
/// `command`: the engine names its candidates' items by id,
/// `command::<name>` and `arg::<id>` (`engine/complete.rs`,
/// `populate_command_candidate` and `populate_arg_candidate`; the tests
/// check that format on sb's tree).
fn for_hidden(command: &clap::Command, candidate: &CompletionCandidate) -> bool {
    let Some(id) = candidate.get_id() else {
        return false;
    };
    if let Some(name) = id.strip_prefix("command::") {
        command
            .get_subcommands()
            .any(|sub| sub.get_name() == name && sub.is_hide_set())
    } else if let Some(arg) = id.strip_prefix("arg::") {
        command
            .get_arguments()
            .any(|a| a.get_id() == arg && a.is_hide_set())
    } else {
        false
    }
}

/// `candidates` as clap's adapter for `shell` writes them
/// (`env/shells.rs`): bash, the values joined by `ifs` (newline when the
/// stub sets none); zsh, each value with `\` and `:` escaped and its
/// help's first line after a `:`, joined the same way; fish, a line each,
/// the help's first line after a tab.
fn format(
    shell: Shell,
    candidates: &[CompletionCandidate],
    ifs: Option<&str>,
    buf: &mut dyn Write,
) -> std::io::Result<()> {
    let separator = ifs.unwrap_or("\n");
    for (i, candidate) in candidates.iter().enumerate() {
        let value = candidate.get_value().to_string_lossy();
        // The help's first line, which bash's reply doesn't carry.
        let help = || {
            candidate.get_help().map(|help| {
                help.to_string()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
        };
        match shell {
            Shell::Bash | Shell::Zsh if i != 0 => write!(buf, "{separator}")?,
            _ => {}
        }
        match shell {
            Shell::Bash => write!(buf, "{value}")?,
            Shell::Zsh => {
                write!(buf, "{}", value.replace('\\', "\\\\").replace(':', "\\:"))?;
                if let Some(help) = help() {
                    write!(buf, ":{}", help.replace('\\', "\\\\"))?;
                }
            }
            Shell::Fish => {
                write!(buf, "{value}")?;
                if let Some(help) = help() {
                    write!(buf, "\t{help}")?;
                }
                writeln!(buf)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::env::EnvCompleter as _;

    /// A tree with what sb's has: a shown and a hidden command, a hidden
    /// flag and a hidden global `-v`, a shown command's hidden alias, a
    /// hidden command's shown flag, and a value with a `:`.
    fn tree() -> clap::Command {
        clap::Command::new("t")
            .arg(
                clap::Arg::new("verbose")
                    .short('v')
                    .long("verbose")
                    .global(true)
                    .hide(true)
                    .action(clap::ArgAction::Count),
            )
            .subcommand(
                clap::Command::new("update")
                    .about("Update it")
                    .arg(
                        clap::Arg::new("keep")
                            .long("keep")
                            .action(clap::ArgAction::SetTrue)
                            .help("Keep: it\\all"),
                    )
                    .arg(
                        clap::Arg::new("skip_git")
                            .long("skip-git")
                            .hide(true)
                            .action(clap::ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                clap::Command::new("self-update").hide(true).arg(
                    clap::Arg::new("yes")
                        .long("yes")
                        .action(clap::ArgAction::SetTrue),
                ),
            )
            .subcommand(
                clap::Command::new("accounts")
                    .alias("accounts.yml")
                    .about("Accounts"),
            )
            .subcommand(
                clap::Command::new("mode").arg(
                    clap::Arg::new("mode")
                        .long("mode")
                        .value_parser(["a:b", "c"]),
                ),
            )
    }

    /// The values sb's filter leaves for `words` (after `t`), the current
    /// word last.
    fn values(words: &[&str]) -> Vec<String> {
        let mut cmd = tree();
        cmd.build();
        let args: Vec<OsString> = std::iter::once("t")
            .chain(words.iter().copied())
            .map(OsString::from)
            .collect();
        let index = args.len() - 1;
        let candidates =
            clap_complete::engine::complete(&mut cmd, args.clone(), index, None).expect("complete");
        shown(&cmd, &args, index, candidates)
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn hidden_commands_and_flags_are_dropped_on_a_prefix() {
        assert_eq!(values(&["self"]), Vec::<String>::new());
        assert_eq!(values(&["self-update"]), Vec::<String>::new());
        assert_eq!(values(&["update", "--skip"]), Vec::<String>::new());
        assert_eq!(values(&["update", "--verb"]), Vec::<String>::new());
        // `-h` and `--help` are one item: the engine keeps the first.
        assert_eq!(values(&["update", "-"]), ["--keep", "-h"]);
        assert_eq!(values(&["u"]), ["update"]);
    }

    /// On an empty word, a list of flags only is no list (spec,
    /// difference 6): the shells would insert `--`. A typed `-`, and a
    /// list with anything else, keep the flags.
    #[test]
    fn an_empty_word_with_only_flags_gets_nothing() {
        assert_eq!(values(&["update", ""]), Vec::<String>::new());
        assert_eq!(values(&["self-update", ""]), Vec::<String>::new());
        assert_eq!(values(&["update", "--"]), ["--help", "--keep"]);
        assert_eq!(values(&["mode", "--mode", ""]), ["a:b", "c"]);
        let top = values(&[""]);
        assert!(top.contains(&"update".to_owned()), "{top:?}");
        assert!(top.contains(&"--help".to_owned()), "{top:?}");
    }

    /// Alphabetical, by the candidate's text (spec, "Every list is in
    /// alphabetical order"); the engine's order is by heading and
    /// declaration.
    #[test]
    fn every_list_is_alphabetical() {
        assert_eq!(
            values(&[""]),
            ["--help", "accounts", "help", "mode", "update"]
        );
        assert_eq!(values(&["mode", "--mode", ""]), ["a:b", "c"]);
    }

    #[test]
    fn what_is_not_a_hidden_item_stays() {
        // A hidden command's own shown flags, once it is named.
        assert_eq!(values(&["self-update", "--"]), ["--help", "--yes"]);
        // A shown command's hidden alias.
        assert_eq!(values(&["accounts."]), ["accounts.yml"]);
        // Shown items, and values.
        assert_eq!(values(&["up"]), ["update"]);
        assert_eq!(values(&["mode", "--mode", ""]), ["a:b", "c"]);
    }

    /// The engine's walk to the command being completed: subcommands by
    /// name, the binary's own name skipped unless the tree has none.
    #[test]
    fn the_completed_command_is_the_engines() {
        let mut cmd = tree();
        cmd.build();
        let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        let words = args(&["t", "update", "--keep", ""]);
        assert_eq!(completed_command(&cmd, &words, 3).get_name(), "update");
        assert_eq!(completed_command(&cmd, &words, 1).get_name(), "t");
        // A word at or after the index doesn't count.
        let words = args(&["t", "update"]);
        assert_eq!(completed_command(&cmd, &words, 1).get_name(), "t");
        let mut bare = tree().no_binary_name(true);
        bare.build();
        let words = args(&["update", ""]);
        assert_eq!(completed_command(&bare, &words, 1).get_name(), "update");
        // A later word is looked up in the command so far: `update` has no
        // `self-update`.
        let words = args(&["t", "update", "self-update", ""]);
        assert_eq!(completed_command(&cmd, &words, 3).get_name(), "update");
    }

    /// A hidden candidate the engine names some other way stays.
    #[test]
    fn only_command_and_arg_ids_are_items() {
        let mut cmd = tree();
        cmd.build();
        let hidden = |id: &str| {
            CompletionCandidate::new("x")
                .id(Some(id.to_owned()))
                .hide(true)
        };
        assert!(for_hidden(&cmd, &hidden("command::self-update")));
        assert!(!for_hidden(&cmd, &hidden("command::update")));
        assert!(for_hidden(&cmd, &hidden("arg::verbose")));
        assert!(!for_hidden(&cmd, &hidden("arg::nothing")));
        assert!(!for_hidden(&cmd, &hidden("self-update")));
        assert!(!for_hidden(
            &cmd,
            &CompletionCandidate::new(".file").hide(true)
        ));
        let words: Vec<OsString> = vec!["t".into(), ".f".into()];
        let kept = shown(
            &cmd,
            &words,
            1,
            vec![CompletionCandidate::new(".file").hide(true)],
        );
        assert_eq!(kept.len(), 1);
    }

    /// The reply is clap's adapter's, entry for entry and byte for byte,
    /// where nothing hidden is offered (the order aside): the same request
    /// through clap's `write_complete` and sb's. The tree has no binary
    /// name, so the default index (0, no `_CLAP_COMPLETE_INDEX` in a
    /// test's environment) is the first word.
    #[test]
    fn the_reply_is_claps_format() {
        let requests: &[&[&str]] = &[
            &[""],
            &["u"],
            &["update", "-"],
            &["mode", "--mode", ""],
            &["acc"],
        ];
        for shell in Shell::ALL {
            for words in requests {
                let args: Vec<OsString> = words.iter().map(OsString::from).collect();
                let mut theirs = Vec::new();
                let mut cmd = tree().no_binary_name(true);
                cmd.build();
                shell
                    .engine()
                    .write_complete(&mut cmd, args.clone(), None, &mut theirs)
                    .expect("clap's");
                let mut ours = Vec::new();
                let mut cmd = tree().no_binary_name(true);
                cmd.build();
                shell
                    .write_complete(&mut cmd, args, None, &mut ours)
                    .expect("sb's");
                // The same entries, each written as clap writes it; sb's
                // in alphabetical order.
                let entries = |reply: &[u8]| {
                    let mut entries: Vec<String> = String::from_utf8_lossy(reply)
                        .split('\n')
                        .map(ToOwned::to_owned)
                        .collect();
                    entries.sort_unstable();
                    entries
                };
                assert_eq!(entries(&ours), entries(&theirs), "{shell:?} {words:?}");
            }
        }
    }

    /// Only bash describes: zsh and fish carry the help in their own
    /// format, whatever the variables say; bash describes only on a
    /// listing call with a width.
    #[test]
    fn only_bash_describes_and_only_when_listing() {
        let candidates = [
            CompletionCandidate::new("start").help(Some("Start it".into())),
            CompletionCandidate::new("stop").help(Some("Stop it".into())),
        ];
        let text = |shell, comp_type: &str, columns: Option<&str>| {
            let stub = Stub {
                ifs: Some("|".to_owned()),
                comp_type: Some(comp_type.to_owned()),
                columns: columns.map(ToOwned::to_owned),
            };
            let mut buf = Vec::new();
            respond(shell, &candidates, &stub, &mut buf).expect("memory");
            String::from_utf8(buf).expect("UTF-8")
        };
        assert_eq!(
            text(Shell::Bash, "63", Some("80")),
            "start  (Start it)|stop   (Stop it)"
        );
        assert_eq!(text(Shell::Bash, "9", Some("80")), "start|stop");
        assert_eq!(text(Shell::Bash, "63", None), "start|stop");
        assert_eq!(text(Shell::Bash, "63", Some("wide")), "start|stop");
        assert_eq!(
            text(Shell::Zsh, "63", Some("80")),
            "start:Start it|stop:Stop it"
        );
        assert_eq!(
            text(Shell::Fish, "63", Some("80")),
            "start\tStart it\nstop\tStop it\n"
        );
        let mut buf = Vec::new();
        respond(Shell::Bash, &candidates, &Stub::default(), &mut buf).expect("memory");
        assert_eq!(buf, b"start\nstop");
    }

    /// The separator and the escaping, written out.
    #[test]
    fn each_shells_format() {
        let candidates = [
            CompletionCandidate::new("a:b\\c").help(Some("first\\ line\nsecond".into())),
            CompletionCandidate::new("plain"),
        ];
        let text = |shell, ifs| {
            let mut buf = Vec::new();
            format(shell, &candidates, ifs, &mut buf).expect("memory");
            String::from_utf8(buf).expect("UTF-8")
        };
        assert_eq!(text(Shell::Bash, Some("\u{b}")), "a:b\\c\u{b}plain");
        assert_eq!(text(Shell::Bash, None), "a:b\\c\nplain");
        assert_eq!(text(Shell::Zsh, None), "a\\:b\\\\c:first\\\\ line\nplain");
        assert_eq!(
            text(Shell::Fish, Some("\u{b}")),
            "a:b\\c\tfirst\\ line\nplain\n"
        );
    }

    #[test]
    fn requests_are_read_as_claps_adapters_read_them() {
        let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        let words = args(&["sb", "up"]);
        assert_eq!(
            request(Shell::Fish, words.clone(), Some("0".into())),
            (words.clone(), 1)
        );
        assert_eq!(
            request(Shell::Bash, words.clone(), Some("1".into())),
            (words.clone(), 1)
        );
        assert_eq!(
            request(Shell::Bash, words.clone(), Some("x".into())),
            (words.clone(), 0)
        );
        assert_eq!(
            request(Shell::Bash, words.clone(), None),
            (words.clone(), 0)
        );
        assert_eq!(
            request(Shell::Bash, words.clone(), Some("2".into())),
            (words.clone(), 2)
        );
        assert_eq!(
            request(Shell::Zsh, words.clone(), Some("2".into())),
            (args(&["sb", "up", ""]), 2)
        );
        assert_eq!(
            request(Shell::Zsh, words.clone(), Some("1".into())),
            (words, 1)
        );
        assert_eq!(request(Shell::Fish, Vec::new(), None), (Vec::new(), 0));
    }
}
