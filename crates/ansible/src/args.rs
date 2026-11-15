//! `sb install`'s arguments (ansible spec, `sb install <tags>...`): the
//! clap definitions, with sb-go's names, short letters and help texts, the
//! tag parsing and the routing to the three repositories.

use std::fmt;

use crate::{Repo, Repos, RunArgs};

/// `-e`'s long help: sb-go's short help, then how each value is passed
/// (spec, `sb install`; decision 3) and its forms by example (the
/// maintainer, 2026-11-15). The forms are Ansible's
/// (<https://docs.ansible.com/ansible/latest/playbook_guide/playbooks_variables.html#defining-variables-at-runtime>):
/// key=value pairs, whose values are strings ("use the JSON format if you
/// need to pass non-string values", said for users as text, a number or a
/// list; the maintainer, 2026-11-15, kept booleans out, since Ansible
/// reads truthy strings), a JSON or YAML string, and `@` before a JSON or
/// YAML file. Two points that page doesn't state are from
/// ansible-core's source (devel, 2026-11-15): a relative `@` path is
/// joined to the loader's base directory, which is the working directory
/// when extra vars load (`DataLoader` starts at `os.path.abspath('.')`,
/// `path_dwim`; ansible-playbook sets no base directory first), and sb
/// runs ansible-playbook in the repository (`run.rs`, `cwd(repo.dir())`);
/// each value is combined over the ones before it, so later ones win
/// (`utils/vars.py`, `load_extra_vars`, `combine_vars`). The example
/// lines are indented and their notes set after a gap of two or more
/// spaces, so the help's wrap keeps each command whole and hangs a note
/// under itself; an example's command holds no such gap.
const EXTRA_VARS_LONG: &str = "Extra variables to pass to Ansible

Each value reaches Ansible whole, as one --extra-vars:
  -e my_var=value
  -e 'my_var=value other_var=value'
  -e '{\"my_list\": [1, 2]}'
  -e @vars.yml         a YAML/JSON file, relative to the repository the playbook runs in
  -e @/root/vars.yml   or an absolute path
A key=value value is passed as text; use the JSON form to pass a number or a list.
Repeat -e for more; later values win.";

/// `sb install`'s arguments. Its `Debug` shows counts only: the tags and
/// `-e` values are shown and logged once, in the playbook's command
/// (decision 17), not in every record that carries them.
#[derive(clap::Args, Clone, Default, PartialEq, Eq)]
pub struct InstallArgs {
    /// The tags to run: words, comma lists, or both.
    #[arg(required = true, num_args = 1.., value_name = "tags")]
    pub tags: Vec<String>,
    /// Extra variables to pass to Ansible
    // Each value whole, never split on commas (no `value_delimiter`), and
    // one starting with `-` taken as a value
    // (<https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.allow_hyphen_values>;
    // decision 2).
    #[arg(
        short = 'e',
        long,
        action = clap::ArgAction::Append,
        allow_hyphen_values = true,
        long_help = EXTRA_VARS_LONG
    )]
    pub extra_vars: Vec<String>,
    /// Tags to skip during Ansible playbook execution
    // Repeatable and comma-separated: sb-go's `StringSlice`.
    #[arg(short = 's', long, value_delimiter = ',')]
    pub skip_tags: Vec<String>,
    /// Increase verbosity level (can be used multiple times, e.g. -vvv)
    // Counted, passed to Ansible as one `-v...` element of the same count
    // (<https://docs.rs/clap/4.6.7/clap/enum.ArgAction.html#variant.Count>;
    // decision 1).
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Skip cache validation and always perform tag checks
    #[arg(long)]
    pub no_cache: bool,
    /// Force disk space failure (debug)
    #[arg(long, hide = true)]
    pub force_disk_full: bool,
}

impl fmt::Debug for InstallArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallArgs")
            .field("tags", &self.tags.len())
            .field("extra_vars", &self.extra_vars.len())
            .field("skip_tags", &self.skip_tags.len())
            .field("verbose", &self.verbose)
            .field("no_cache", &self.no_cache)
            .field("force_disk_full", &self.force_disk_full)
            .finish()
    }
}

impl InstallArgs {
    /// What reaches every playbook besides its tags.
    #[must_use]
    pub fn run_args(&self) -> RunArgs {
        RunArgs {
            extra_vars: self.extra_vars.clone(),
            skip_tags: self.skip_tags.clone(),
            verbose: self.verbose,
        }
    }
}

/// No tag left after the parsing (sb-go's text).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("no tags provided")]
pub struct NoTags;

/// The tags as sb-go reads them (cmd/install.go; sb-go 0058dbf): every
/// argument joined with `,`, split on `,`, each trimmed, empties dropped,
/// duplicates kept, so `plex sonarr`, `plex,sonarr` and `"plex, sonarr"`
/// are the same. Why not clap's `value_delimiter`: it splits, but doesn't
/// trim or drop empties.
///
/// # Errors
///
/// [`NoTags`] when none is left.
pub fn parse_tags(args: &[String]) -> Result<Vec<String>, NoTags> {
    let tags: Vec<String> = args
        .iter()
        .flat_map(|arg| arg.split(','))
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect();
    if tags.is_empty() {
        Err(NoTags)
    } else {
        Ok(tags)
    }
}

/// The tags of each repository, prefixes stripped, in the order given.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Routed {
    /// Tags with no prefix.
    pub saltbox: Vec<String>,
    /// `sandbox-` tags, the prefix stripped.
    pub sandbox: Vec<String>,
    /// `mod-` tags, the prefix stripped.
    pub saltbox_mod: Vec<String>,
}

impl fmt::Debug for Routed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Routed")
            .field("saltbox", &self.saltbox.len())
            .field("sandbox", &self.sandbox.len())
            .field("saltbox_mod", &self.saltbox_mod.len())
            .finish()
    }
}

impl Routed {
    /// Each tag to its repository (sb-go cmd/install.go:136-144): `mod-`
    /// to `saltbox_mod`, checked first, else `sandbox-` to Sandbox, else
    /// Saltbox, the prefix stripped.
    #[must_use]
    pub fn route(tags: &[String]) -> Self {
        let mut routed = Self::default();
        for tag in tags {
            if let Some(rest) = tag.strip_prefix("mod-") {
                routed.saltbox_mod.push(rest.to_string());
            } else if let Some(rest) = tag.strip_prefix("sandbox-") {
                routed.sandbox.push(rest.to_string());
            } else {
                routed.saltbox.push(tag.clone());
            }
        }
        routed
    }

    /// The runs, in sb-go's order (cmd/install.go:192-210): Saltbox, then
    /// `saltbox_mod`, then Sandbox, each only when it has tags.
    #[must_use]
    pub fn runs<'a>(&'a self, repos: &'a Repos) -> Vec<(&'a Repo, &'a [String])> {
        [
            (&repos.saltbox, &self.saltbox),
            (&repos.saltbox_mod, &self.saltbox_mod),
            (&repos.sandbox, &self.sandbox),
        ]
        .into_iter()
        .filter(|(_, tags)| !tags.is_empty())
        .map(|(repo, tags)| (repo, tags.as_slice()))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;

    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// `sb install`'s arguments as clap parses them.
    #[derive(clap::Parser)]
    struct Install {
        #[command(flatten)]
        args: InstallArgs,
    }

    fn parse(args: &[&str]) -> InstallArgs {
        Install::try_parse_from(std::iter::once("install").chain(args.iter().copied()))
            .expect("parses")
            .args
    }

    /// sb-go 0058dbf: words, comma lists and both read the same; spaces
    /// trimmed, empties dropped, duplicates kept (sb-go's `TestTagParsing`
    /// and `TestEmptyTagHandling`).
    #[test]
    fn tags_are_joined_split_and_trimmed() {
        let parsed = |args: &[&str]| parse_tags(&strings(args));
        assert_eq!(
            parsed(&["plex", "sonarr"]),
            Ok(strings(&["plex", "sonarr"]))
        );
        assert_eq!(parsed(&["plex,sonarr"]), Ok(strings(&["plex", "sonarr"])));
        assert_eq!(parsed(&["plex, sonarr"]), Ok(strings(&["plex", "sonarr"])));
        assert_eq!(
            parsed(&["  plex  ", "", "   ", " sandbox-tautulli ", ",,"]),
            Ok(strings(&["plex", "sandbox-tautulli"]))
        );
        assert_eq!(
            parsed(&["plex", "plex", "sonarr"]),
            Ok(strings(&["plex", "plex", "sonarr"]))
        );
        assert_eq!(parsed(&["", "  ", "\t", " , "]), Err(NoTags));
        assert_eq!(NoTags.to_string(), "no tags provided");
    }

    /// `mod-` first, then `sandbox-`, else Saltbox; the prefix stripped
    /// once; the order kept (sb-go's `TestTagParsing`).
    #[test]
    fn tags_route_by_prefix() {
        let routed = Routed::route(&strings(&[
            "plex",
            "sandbox-tautulli",
            "mod-jellyfin",
            "mod-sandbox-x",
            "sandbox-mod-y",
            "sonarr",
        ]));
        assert_eq!(routed.saltbox, strings(&["plex", "sonarr"]));
        assert_eq!(routed.sandbox, strings(&["tautulli", "mod-y"]));
        assert_eq!(routed.saltbox_mod, strings(&["jellyfin", "sandbox-x"]));
        assert_eq!(
            format!("{routed:?}"),
            "Routed { saltbox: 2, sandbox: 2, saltbox_mod: 2 }"
        );
    }

    /// Saltbox, then mod, then Sandbox; a repository with no tags doesn't
    /// run.
    #[test]
    fn runs_go_saltbox_mod_then_sandbox() {
        let repos = Repos::new(Path::new("/s"), Path::new("/a"));
        let routed = Routed::route(&strings(&["sandbox-b", "mod-c", "a"]));
        let runs: Vec<_> = routed
            .runs(&repos)
            .into_iter()
            .map(|(repo, tags)| (repo.kind(), tags.to_vec()))
            .collect();
        assert_eq!(
            runs,
            [
                (crate::Kind::Saltbox, strings(&["a"])),
                (crate::Kind::Mod, strings(&["c"])),
                (crate::Kind::Sandbox, strings(&["b"])),
            ]
        );
        let only_sandbox = Routed::route(&strings(&["sandbox-b"]));
        let runs = only_sandbox.runs(&repos);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0.kind(), crate::Kind::Sandbox);
    }

    /// The flags as sb-go's: `-e` repeatable and whole, a leading `-`
    /// included; `-s` repeatable and comma-split; `-v` counted.
    #[test]
    fn the_flags_parse_as_sb_gos() {
        let args = parse(&[
            "plex",
            "-e",
            r#"{"a": 1, "b": [2, 3]}"#,
            "-e",
            "-x=1",
            "--extra-vars",
            "--no-cache",
            "-s",
            "a,b",
            "--skip-tags",
            "c",
            "-vv",
            "--verbose",
            "sonarr",
        ]);
        assert_eq!(args.tags, strings(&["plex", "sonarr"]));
        assert_eq!(
            args.extra_vars,
            strings(&[r#"{"a": 1, "b": [2, 3]}"#, "-x=1", "--no-cache"])
        );
        assert!(!args.no_cache, "`-e --no-cache` is a value");
        assert_eq!(args.skip_tags, strings(&["a", "b", "c"]));
        assert_eq!(args.verbose, 3);
        let run = args.run_args();
        assert_eq!(run.extra_vars, args.extra_vars);
        assert_eq!(run.skip_tags, args.skip_tags);
        assert_eq!(run.verbose, 3);
        assert_eq!(
            format!("{args:?}"),
            "InstallArgs { tags: 2, extra_vars: 3, skip_tags: 3, verbose: 3, \
             no_cache: false, force_disk_full: false }"
        );

        let flags = parse(&["plex", "--no-cache", "--force-disk-full"]);
        assert!(flags.no_cache && flags.force_disk_full);
        assert_eq!(flags.verbose, 0);
        let bare = Install::try_parse_from(["install"]).err().expect("no tags");
        assert_eq!(bare.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    /// `-e`'s help is sb-go's; its long help adds how values are passed
    /// (spec, `sb install`).
    #[test]
    fn extra_vars_help_is_sb_gos_and_its_long_help_adds_the_rule() {
        use clap::CommandFactory;
        let cmd = Install::command();
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_id() == "extra_vars")
            .expect("-e");
        assert_eq!(
            arg.get_help().map(ToString::to_string).as_deref(),
            Some("Extra variables to pass to Ansible")
        );
        let long = arg.get_long_help().map_or_default(ToString::to_string);
        assert!(
            long.ends_with("Repeat -e for more; later values win."),
            "{long}"
        );
        assert!(long.contains("\n  -e @vars.yml         a YAML/JSON file, relative to the repository the playbook runs in\n"), "{long}");
        let hidden = cmd
            .get_arguments()
            .find(|arg| arg.get_id() == "force_disk_full")
            .expect("--force-disk-full");
        assert!(hidden.is_hide_set());
    }
}
