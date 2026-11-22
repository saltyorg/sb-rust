//! The fact completers for `sb fact get` and `sb fact list` (completion
//! spec, "Facts for `sb fact get` and `sb fact list`"; decision 15):
//! apps, then instances, then keys, read from the facts store through
//! sb-config's read-only `facts::read::Store`, as the calling user,
//! without sudo and without a lock.
//!
//! Names only: the file's values are parsed with it and dropped with the
//! process. A name with whitespace or a control character is left out,
//! as the tag completer leaves such tags out, and any error (no
//! directory, a symlink, a file that can't be read or parsed) gives no
//! candidates. Nothing is written or logged. Why no names cache: one a
//! root run wrote would go stale between Saltbox runs, since Ansible
//! writes the store and sb doesn't.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use sb_config::facts::{self, read::Store};

/// The command whose subcommands the completers serve.
pub const FACT: &str = "fact";
/// The positionals' ids (`sb fact get`'s and `sb fact list`'s).
pub const APP: &str = "app";
pub const INSTANCE: &str = "instance";
pub const KEY: &str = "key";

/// The group every candidate is in, for the reason the tag completer
/// gives its own (`tags::candidate`): sb's reply never shows it.
const GROUP: &str = "facts";

/// Which names a completer offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Want<'a> {
    Apps,
    Instances { app: &'a str },
    Keys { app: &'a str, instance: &'a str },
}

/// The completer for the app positional: the store's `*.ini` names,
/// for clap's `Arg::add` through the derive's `add`
/// (<https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.ArgValueCompleter.html>;
/// `unstable-dynamic`, pinned exactly: ADR-001).
#[must_use]
pub fn apps() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &OsStr| complete(current, Want::Apps))
}

/// The completer for the instance positional: the instances of the app
/// given in the earlier words, parsed with `command`'s tree as the tag
/// completer parses them.
#[must_use]
pub fn instances(command: fn() -> clap::Command) -> ArgValueCompleter {
    ArgValueCompleter::new(move |current: &OsStr| {
        let Some((app, _)) = request(command) else {
            return Vec::new();
        };
        complete(current, Want::Instances { app: &app })
    })
}

/// The completer for the key positional: the keys of the app's instance
/// given in the earlier words.
#[must_use]
pub fn keys(command: fn() -> clap::Command) -> ArgValueCompleter {
    ArgValueCompleter::new(move |current: &OsStr| {
        let Some((app, Some(instance))) = request(command) else {
            return Vec::new();
        };
        complete(
            current,
            Want::Keys {
                app: &app,
                instance: &instance,
            },
        )
    })
}

/// The app and instance given in this request's earlier words.
fn request(command: fn() -> clap::Command) -> Option<(String, Option<String>)> {
    earlier(command, &crate::tags::request_words()?)
}

/// The app and instance `words` (sb and its arguments) give `sb fact
/// get` or `sb fact list`, by clap's own parse (`tags::parsed`), so a
/// global flag's value (`--color never`) is never taken for one.
fn earlier(command: fn() -> clap::Command, words: &[OsString]) -> Option<(String, Option<String>)> {
    let matches = crate::tags::parsed(command, words)?;
    let (_, sub) = matches.subcommand_matches(FACT)?.subcommand()?;
    let get = |id: &str| sub.try_get_one::<String>(id).ok().flatten().cloned();
    Some((get(APP)?, get(INSTANCE)))
}

/// The candidates for `current` from the store under the Saltbox root
/// (`SB_SALTBOX_DIR`, as every command reads it).
fn complete(current: &OsStr, want: Want<'_>) -> Vec<CompletionCandidate> {
    let dir = facts::directory(std::env::var_os("SB_SALTBOX_DIR"));
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    names(&dir, want)
        .map_or_default(|names| offered(names, current))
        .into_iter()
        .map(|name| CompletionCandidate::new(name).tag(Some(GROUP.into())))
        .collect()
}

/// The names `want` asks for in the facts directory `dir`; `None` on any
/// error or a name that isn't there.
fn names(dir: &Path, want: Want<'_>) -> Option<Vec<String>> {
    let store = Store::open(dir).ok()?;
    let (app, instance) = match want {
        Want::Apps => return store.apps().ok(),
        Want::Instances { app } => (app, None),
        Want::Keys { app, instance } => (app, Some(instance)),
    };
    let role = store.role(app).ok()??;
    match instance {
        None => Some(role.instances.into_iter().map(|i| i.name).collect()),
        Some(instance) => role
            .instances
            .into_iter()
            .find(|i| i.name == instance)
            .map(|i| i.facts.into_iter().map(|fact| fact.key).collect()),
    }
}

/// The names offered for `current`: those that start with it, without
/// whitespace or a control character, sorted, each once. A current word
/// with a control character gets none.
fn offered(mut names: Vec<String>, current: &str) -> Vec<String> {
    if current.chars().any(char::is_control) {
        return Vec::new();
    }
    names.retain(|name| {
        !name.is_empty()
            && name.starts_with(current)
            && !name.chars().any(|c| c.is_whitespace() || c.is_control())
    });
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A facts directory: plex with two instances, one of them holding a
    /// key with a space and one with a control character, and a file
    /// named with a space.
    fn store() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::write(
            root.join("plex.ini"),
            "[plex]\ntoken = not-a-real-token\nclient_identifier = c\nwith space = x\n\n[plex-4k]\ntoken = t\n",
        )
        .expect("write");
        std::fs::write(
            root.join("teslamate.ini"),
            "[teslamate]\npostgres_password = p\n",
        )
        .expect("write");
        std::fs::write(root.join("bad name.ini"), "[x]\nk = v\n").expect("write");
        (dir, root)
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn apps_instances_and_keys_come_from_the_store() {
        let (_dir, root) = store();
        assert_eq!(
            names(&root, Want::Apps).map(|n| offered(n, "")),
            Some(strings(&["plex", "teslamate"]))
        );
        assert_eq!(
            names(&root, Want::Instances { app: "plex" }),
            Some(strings(&["plex", "plex-4k"]))
        );
        assert_eq!(
            names(
                &root,
                Want::Keys {
                    app: "plex",
                    instance: "plex"
                }
            )
            .map(|n| offered(n, "")),
            Some(strings(&["client_identifier", "token"]))
        );
    }

    #[test]
    fn no_value_is_ever_a_candidate() {
        let (_dir, root) = store();
        for want in [
            Want::Apps,
            Want::Instances { app: "plex" },
            Want::Keys {
                app: "plex",
                instance: "plex",
            },
        ] {
            let got = names(&root, want).expect("names");
            assert!(
                !got.iter().any(|n| n.contains("not-a-real-token")),
                "{got:?}"
            );
        }
    }

    #[test]
    fn a_miss_or_an_error_gives_nothing() {
        let (_dir, root) = store();
        assert_eq!(names(&root, Want::Instances { app: "plexx" }), None);
        assert_eq!(
            names(
                &root,
                Want::Keys {
                    app: "plex",
                    instance: "nope"
                }
            ),
            None
        );
        assert_eq!(names(&root.join("missing"), Want::Apps), None);
        std::fs::write(root.join("broken.ini"), "[broken]\nport = 1\rx\n").expect("write");
        assert_eq!(names(&root, Want::Instances { app: "broken" }), None);
        let link = root.join("link");
        std::os::unix::fs::symlink(&root, &link).expect("symlink");
        assert_eq!(names(&link, Want::Apps), None);
    }

    #[test]
    fn offered_filters_by_prefix_and_drops_odd_names() {
        let names = strings(&[
            "token",
            "tab\tname",
            "sp ace",
            "t\u{1b}x",
            "",
            "token",
            "team",
        ]);
        assert_eq!(offered(names.clone(), "t"), strings(&["team", "token"]));
        assert_eq!(offered(names.clone(), "to"), strings(&["token"]));
        assert_eq!(offered(names, "t\n"), Vec::<String>::new());
    }

    /// A tree with sb's global `--color` and `fact`'s two subcommands.
    fn tree() -> clap::Command {
        let positional = |id: &'static str| clap::Arg::new(id).required(false);
        clap::Command::new("sb")
            .arg(clap::Arg::new("color").long("color").global(true))
            .subcommand(
                clap::Command::new(FACT)
                    .subcommand(
                        clap::Command::new("get")
                            .arg(positional(APP))
                            .arg(positional(INSTANCE))
                            .arg(positional(KEY))
                            .arg(clap::Arg::new("json").long("json").num_args(0)),
                    )
                    .subcommand(
                        clap::Command::new("list")
                            .arg(positional(APP))
                            .arg(positional(INSTANCE)),
                    ),
            )
            .subcommand(clap::Command::new("install").arg(positional("tags")))
    }

    fn words(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_earlier_words_give_the_app_and_instance() {
        let some = |app: &str, instance: Option<&str>| {
            Some((app.to_owned(), instance.map(ToOwned::to_owned)))
        };
        assert_eq!(
            earlier(tree, &words(&["sb", "fact", "get", "plex"])),
            some("plex", None)
        );
        assert_eq!(
            earlier(
                tree,
                &words(&[
                    "sb", "--color", "never", "fact", "get", "--json", "'plex'", "plex-4k"
                ])
            ),
            some("plex", Some("plex-4k"))
        );
        assert_eq!(
            earlier(tree, &words(&["sb", "fact", "list", "plex"])),
            some("plex", None)
        );
        assert_eq!(earlier(tree, &words(&["sb", "fact", "get"])), None);
        assert_eq!(earlier(tree, &words(&["sb", "install", "plex"])), None);
        assert_eq!(earlier(tree, &words(&["sb", "fact"])), None);
    }
}
