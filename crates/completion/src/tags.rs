//! The tag completer for `sb install` (completion spec, "Tags for `sb
//! install`"): sb-go's rules (`cmd/tag_completion.go`), over the tag cache
//! `ansible` writes. Pressing Tab reads the cache and the inventory, and
//! nothing else: no playbook runs and nothing is written.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::Path;

use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use sb_ansible::{Repo, TagCache};
use sb_cli_core::config;

/// The command whose positional the completer serves.
pub const INSTALL: &str = "install";
/// That positional's id (`sb_ansible::InstallArgs::tags`).
pub const TAGS: &str = "tags";
/// The prefix that routes a tag to Sandbox (`sb install`'s routing).
const SANDBOX: &str = "sandbox-";

/// The completer for `install`'s tags, for clap's
/// `Arg::add` (<https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.ArgValueCompleter.html>;
/// `unstable-dynamic`, pinned exactly: ADR-001). `command` builds the
/// tree it completes, without the completer: the earlier words are parsed
/// with it.
#[must_use]
pub fn completer(command: fn() -> clap::Command) -> ArgValueCompleter {
    ArgValueCompleter::new(move |current: &OsStr| {
        // A word that isn't UTF-8 can't start any tag.
        let Some(current) = current.to_str() else {
            return Vec::new();
        };
        let index = std::env::var("_CLAP_COMPLETE_INDEX")
            .ok()
            .and_then(|index| index.parse().ok());
        let words = earlier_words(std::env::args_os(), index);
        if split_off_value(&words) {
            return Vec::new();
        }
        let earlier = earlier_tags(command, &words);
        let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
        let appdata = config::appdata_path(&config::inventory_path(&root));
        candidates(&cached(&root, &appdata), &earlier, current)
            .into_iter()
            .map(candidate)
            .collect()
    })
}

/// The group every tag candidate is in. sb's reply never shows a group
/// (`reply`, which sorts the candidates by value).
const GROUP: &str = "tags";

/// `tag` as a candidate in [`GROUP`]: clap's engine gives a candidate
/// without a group the argument's display text, formatted anew for each
/// one (`clap_complete` 4.6.11 `engine/complete.rs`, `complete_arg_value`;
/// <https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.CompletionCandidate.html#method.tag>),
/// about 3,000 instructions a tag.
fn candidate(tag: String) -> CompletionCandidate {
    CompletionCandidate::new(tag).tag(Some(GROUP.into()))
}

/// The words of the command line before the one being completed: the
/// arguments after the first `--` in sb's own (clap's stubs call
/// `sb -- <words>`, as `CompleteEnv` reads them), up to `index`, the
/// current word's position (`_CLAP_COMPLETE_INDEX`, which the bash and
/// zsh stubs set). Fish's stub sets none and passes the words up to the
/// cursor, the current one last (`clap_complete` 4.6.11, `env/shells.rs`).
/// `UNVERIFIED` against clap's docs (`ValueCompleter` gets only the
/// current word); checked in each shell by tests/shells.sh.
fn earlier_words(args: impl IntoIterator<Item = OsString>, index: Option<usize>) -> Vec<OsString> {
    let mut words: Vec<OsString> = args
        .into_iter()
        .skip_while(|arg| arg != "--")
        .skip(1)
        .collect();
    let index = index.unwrap_or(words.len().saturating_sub(1));
    words.truncate(index);
    words
}

/// Whether the word being completed is a flag's value that bash split
/// off at its `=`: the word before it is `=` (bash breaks words at `=`,
/// `COMP_WORDBREAKS`, and clap's bash stub passes `COMP_WORDS` as they
/// are), which clap's engine would take for a tag. No tag is `=`.
/// Found by tests/shells.sh (`--extra-vars=so`, sb-go's case).
fn split_off_value(words: &[OsString]) -> bool {
    words.last().is_some_and(|word| word == "=")
}

/// The tags given in `words` (sb and its arguments), as the real parse
/// reads them: clap's own parse of `command`, errors ignored
/// (<https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.ignore_errors>),
/// so a flag's value (`-e sonarr`) is never taken for a tag, as sb-go's
/// cobra parse doesn't. Each word is unquoted first, as the shell will
/// pass it to sb: bash's and zsh's stubs pass the words as typed
/// (`"plex"`), fish's already unquoted (tests/shells.sh). Nothing when
/// the words aren't `install`'s.
fn earlier_tags(command: fn() -> clap::Command, words: &[OsString]) -> Vec<String> {
    let words = rejoined(words);
    let words = words.iter().map(|word| unquoted(word));
    let Ok(matches) = command().ignore_errors(true).try_get_matches_from(words) else {
        return Vec::new();
    };
    matches
        .subcommand_matches(INSTALL)
        .and_then(|install| install.try_get_many::<String>(TAGS).ok().flatten())
        .map_or_default(|tags| tags.cloned().collect())
}

/// `words` with each `=` joined to the words round it again: bash splits
/// `--skip-tags=plex` into `--skip-tags`, `=`, `plex` (`COMP_WORDBREAKS`),
/// and clap's bash stub passes them so; joined, as bash-completion's
/// `_get_comp_words_by_ref -n =` joins them, the parse reads `plex` as the
/// flag's value, not as a tag. zsh and fish never pass a lone `=`; one
/// with no word before it stays.
fn rejoined(words: &[OsString]) -> Vec<OsString> {
    let mut joined: Vec<OsString> = Vec::with_capacity(words.len());
    let mut join_next = false;
    for word in words {
        let is_equals = word == "=";
        match joined.last_mut() {
            Some(last) if is_equals || join_next => {
                last.push(word);
                join_next = is_equals;
            }
            _ => {
                joined.push(word.clone());
                join_next = false;
            }
        }
    }
    joined
}

/// `word` as the shell will pass it: shlex's split, POSIX shell quoting
/// (<https://docs.rs/shlex/2.0.1/shlex/fn.split.html>), when that is
/// one word; else `word` as typed (an open quote, or a fish word holding
/// a quote character).
fn unquoted(word: &OsStr) -> OsString {
    let split = word.to_str().and_then(shlex::split);
    match split.as_deref() {
        Some([one]) => OsString::from(one),
        _ => word.to_os_string(),
    }
}

/// The tags completion offers (sb-go's `getCompletionTags`): Saltbox's,
/// then `sandbox-` and each of Sandbox's, from the cache at `root`;
/// Sandbox's entry is keyed by `appdata`'s `sandbox`. Mod tags aren't
/// offered. A missing, unreadable or corrupt cache offers none.
#[must_use]
pub(crate) fn cached(root: &Path, appdata: &Path) -> Vec<String> {
    let cache = TagCache::load(root);
    let saltbox = cache.cached_tags(&Repo::saltbox(root));
    let sandbox = cache.cached_tags(&Repo::sandbox(appdata));
    let saltbox = saltbox
        .iter()
        .filter_map(|tag| completion_tag(tag))
        .map(ToString::to_string);
    let sandbox = sandbox
        .iter()
        .filter_map(|tag| completion_tag(tag))
        .map(|tag| format!("{SANDBOX}{tag}"));
    saltbox.chain(sandbox).collect()
}

/// A cached tag as completion offers it (sb-go's `completionTag`): trimmed,
/// and none when it holds a comma or a control character, or is blank.
fn completion_tag(raw: &str) -> Option<&str> {
    if raw.contains(',') || raw.chars().any(char::is_control) {
        return None;
    }
    Some(raw.trim()).filter(|tag| !tag.is_empty())
}

/// The candidates for `current` (sb-go's `completeInstallTags`): the part
/// after its last comma is completed, the part before it kept, with the
/// whitespace that starts the completed part; tags given in `earlier`
/// words or earlier in `current` are left out; sorted, each once. A word
/// with a control character, or starting with a quote (fish passes an
/// unparsed opening one), gets none.
#[must_use]
pub(crate) fn candidates(tags: &[String], earlier: &[String], current: &str) -> Vec<String> {
    if current.chars().any(char::is_control) || current.starts_with(['\'', '"']) {
        return Vec::new();
    }
    let mut selected: BTreeSet<&str> = earlier
        .iter()
        .flat_map(|word| word.split(','))
        .map(str::trim)
        .collect();
    // Why not clap's `Arg::value_delimiter`: the engine would pass only the
    // part after the last comma (`rsplit_delimiter`, `clap_complete` 4.6.11
    // `engine/complete.rs`), and sb-go's rules need the whole word: tags
    // earlier in it are left out, and a leading quote gets no candidates.
    let (prefix, suffix) = match current.rfind(',') {
        Some(comma) => {
            selected.extend(current[..comma].split(',').map(str::trim));
            current.split_at(comma + 1)
        }
        None => ("", current),
    };
    // The install command trims each tag; the typed whitespace stays in
    // the word while the rest is matched.
    let typed = suffix.trim_start();
    let kept = &suffix[..suffix.len() - typed.len()];
    let mut out = Vec::new();
    for tag in tags.iter().filter_map(|tag| completion_tag(tag)) {
        if tag.starts_with(typed) && selected.insert(tag) {
            out.push(format!("{prefix}{kept}{tag}"));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// (name, tags, earlier words, current word, candidates).
    type TagCase = (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static str,
        &'static [&'static str],
    );

    /// sb-go's `cmd/tag_completion_test.go`, `TestCompleteInstallTags`, case
    /// for case.
    const TAG_CASES: &[TagCase] = &[
        (
            "sorted unique tags",
            &["sonarr", "plex", "sonarr", "radarr"],
            &[],
            "",
            &["plex", "radarr", "sonarr"],
        ),
        (
            "prefix",
            &["sonarr", "plex", "sabnzbd"],
            &[],
            "so",
            &["sonarr"],
        ),
        (
            "comma prefix",
            &["sonarr", "plex", "radarr"],
            &[],
            "plex,ra",
            &["plex,radarr"],
        ),
        (
            "several commas",
            &["sonarr", "plex", "radarr"],
            &[],
            "plex,radarr,",
            &["plex,radarr,sonarr"],
        ),
        (
            "comma whitespace",
            &["sonarr", "plex", "radarr"],
            &[],
            "plex, so",
            &["plex, sonarr"],
        ),
        (
            "previous arguments and current prefix",
            &["sonarr", "plex", "radarr", "sandbox-plex"],
            &[" plex ,sonarr", "sandbox-plex"],
            "radarr,",
            &[],
        ),
        (
            "repository prefixes are distinct",
            &["plex", "sandbox-plex"],
            &["plex"],
            "",
            &["sandbox-plex"],
        ),
        (
            "shell quoting belongs to adapters",
            &[
                "with space",
                "quote\"tag",
                "apostrophe'tag",
                "$tag",
                "semi;tag",
                "tick`tag",
            ],
            &[],
            "",
            &[
                "$tag",
                "apostrophe'tag",
                "quote\"tag",
                "semi;tag",
                "tick`tag",
                "with space",
            ],
        ),
        (
            "protocol controls and delimiters",
            &[
                "good",
                "\nnew",
                "tab\ttag",
                "null\0tag",
                "del\x7ftag",
                "two,tags",
                "",
                "   ",
            ],
            &[],
            "",
            &["good"],
        ),
        (
            "control in typed prefix",
            &["sonarr"],
            &[],
            "plex\n,so",
            &[],
        ),
        ("raw single quote", &["sonarr"], &[], "'so", &[]),
        ("raw double quote", &["sonarr"], &[], "\"so", &[]),
        (
            "raw single quote before comma",
            &["sonarr"],
            &[],
            "'plex,so",
            &[],
        ),
        (
            "raw double quote before comma",
            &["sonarr"],
            &[],
            "\"plex,so",
            &[],
        ),
        (
            "raw single quote before several commas",
            &["sonarr"],
            &[],
            "'plex,radarr,so",
            &[],
        ),
        (
            "raw double quote before several commas",
            &["sonarr"],
            &[],
            "\"plex,radarr,so",
            &[],
        ),
        ("missing tags", &[], &[], "plex,", &[]),
        ("unmatched prefix", &["sonarr", "plex"], &[], "unknown", &[]),
    ];

    /// Each candidate is a tag in sb's own group, so clap's engine
    /// doesn't format the argument's display text into each one; its
    /// value is the tag.
    #[test]
    fn each_candidate_comes_grouped() {
        let candidate = candidate("plex".to_owned());
        assert_eq!(candidate.get_value(), "plex");
        assert_eq!(
            candidate.get_tag().map(ToString::to_string).as_deref(),
            Some(GROUP)
        );
        assert_eq!(candidate.get_help(), None);
    }

    /// sb-go's cases, [`TAG_CASES`].
    #[test]
    fn sb_gos_tag_cases() {
        for (name, tags, earlier, current, want) in TAG_CASES {
            assert_eq!(
                candidates(&strings(tags), &strings(earlier), current),
                strings(want),
                "{name}"
            );
        }
    }

    /// The research's list beyond sb-go's table (completion spec, Testing
    /// strategy): a C1 control (Go's `unicode.IsControl` and Rust's
    /// `char::is_control` are both Unicode's Cc), Unicode whitespace kept,
    /// a tag already earlier in the word, a cached tag with spaces round it.
    #[test]
    fn controls_whitespace_and_repeats() {
        let tags = strings(&["plex", " sonarr ", "c1\u{85}tag", "radarr"]);
        assert_eq!(
            candidates(&tags, &[], ""),
            strings(&["plex", "radarr", "sonarr"])
        );
        assert_eq!(candidates(&tags, &[], "plex,\u{9f}"), Vec::<String>::new());
        assert_eq!(
            candidates(&tags, &[], "plex,\u{a0}so"),
            strings(&["plex,\u{a0}sonarr"])
        );
        assert_eq!(
            candidates(&tags, &[], "plex,radarr,"),
            strings(&["plex,radarr,sonarr"])
        );
        assert_eq!(
            candidates(&tags, &strings(&["sonarr,plex"]), ""),
            strings(&["radarr"])
        );
    }

    /// sb-go's `TestCompletionCacheRead`: absent, corrupt and empty caches
    /// offer nothing; Saltbox's tags, then Sandbox's with `sandbox-`, mod
    /// tags left out, other values and unusable tags skipped; the read
    /// changes and creates nothing.
    #[test]
    fn sb_gos_cache_read_cases() {
        let routing = r#"{
            "ROOT": {"commit": "a", "tags": ["plex", "sonarr"]},
            "APPDATA/sandbox": {"tags": [" radarr ", 42, "sabnzbd", "", " ", "\tbad", "two,tags"]},
            "APPDATA/saltbox_mod": {"commit": "a", "tags": ["custom"]}
        }"#;
        let cases: &[(&str, Option<&str>, &[&str])] = &[
            ("absent", None, &[]),
            ("corrupt", Some("broken JSON"), &[]),
            ("empty", Some("{}"), &[]),
            (
                "repository routing",
                Some(routing),
                &["plex", "sonarr", "sandbox-radarr", "sandbox-sabnzbd"],
            ),
        ];
        for (name, content, want) in cases {
            let root = tempfile::tempdir().expect("temp dir");
            let appdata = tempfile::tempdir().expect("temp dir");
            let path = root.path().join(sb_ansible::CACHE_FILE);
            let content = content.map(|text| {
                text.replace("ROOT", &root.path().display().to_string())
                    .replace("APPDATA", &appdata.path().display().to_string())
            });
            if let Some(text) = &content {
                std::fs::write(&path, text).expect("cache");
            }
            assert_eq!(cached(root.path(), appdata.path()), strings(want), "{name}");
            assert_eq!(
                std::fs::read_to_string(&path).ok(),
                content,
                "{name}: changed"
            );
            let files = std::fs::read_dir(root.path()).expect("list").count();
            assert_eq!(
                files,
                usize::from(content.is_some()),
                "{name}: created a file"
            );
            assert_eq!(
                std::fs::read_dir(appdata.path()).expect("list").count(),
                0,
                "{name}"
            );
        }
    }

    /// The words before the current one: after the first `--`, up to the
    /// index the bash and zsh stubs pass; without one, all but the last
    /// (fish's stub passes the words up to the cursor).
    #[test]
    fn earlier_words_come_from_the_request() {
        let args = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        let request = args(&[
            "/usr/local/bin/sb",
            "--",
            "sb",
            "install",
            "plex",
            "so",
            "after",
        ]);
        assert_eq!(
            earlier_words(request.clone(), Some(3)),
            args(&["sb", "install", "plex"])
        );
        assert_eq!(
            earlier_words(request.clone(), None),
            args(&["sb", "install", "plex", "so"])
        );
        assert_eq!(earlier_words(request, Some(99)).len(), 5);
        assert_eq!(
            earlier_words(args(&["sb", "install", "plex"]), None),
            Vec::<OsString>::new()
        );
        assert_eq!(
            earlier_words(args(&["sb", "--"]), None),
            Vec::<OsString>::new()
        );
    }

    /// A tree with `install`'s tags as sb has them.
    fn tree() -> clap::Command {
        clap::Command::new("sb")
            .arg(clap::Arg::new("color").long("color").global(true))
            .subcommand(
                clap::Command::new(INSTALL)
                    .arg(clap::Arg::new(TAGS).required(true).num_args(1..))
                    .arg(
                        clap::Arg::new("extra-vars")
                            .short('e')
                            .long("extra-vars")
                            .action(clap::ArgAction::Append)
                            .allow_hyphen_values(true),
                    )
                    .arg(clap::Arg::new("skip-tags").short('s').long("skip-tags")),
            )
            .subcommand(clap::Command::new("list").arg(clap::Arg::new(TAGS)))
    }

    /// The earlier tags are what clap's parse reads as `install`'s tags: a
    /// flag's value isn't one (sb-go's "previous tags with parsed flags"),
    /// a missing required tag is no error, another command has none.
    #[test]
    fn earlier_tags_are_installs_parsed_tags() {
        let words = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "--extra-vars", "sonarr", "plex"])
            ),
            strings(&["plex"])
        );
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "--color", "never", "install", "a,b", "-s", "c", "d"])
            ),
            strings(&["a,b", "d"])
        );
        assert_eq!(
            earlier_tags(tree, &words(&["sb", "install"])),
            Vec::<String>::new()
        );
        assert_eq!(
            earlier_tags(tree, &words(&["sb", "list", "plex"])),
            Vec::<String>::new()
        );
        assert_eq!(
            earlier_tags(tree, &words(&["sb", "install", "--help"])),
            Vec::<String>::new()
        );
        assert_eq!(earlier_tags(tree, &[]), Vec::<String>::new());
    }

    /// bash's and zsh's stubs pass the words as typed, quotes and all
    /// (tests/shells.sh): an earlier tag is read as the shell will pass
    /// it, unquoted, as fish's stub already passes it. A word that isn't
    /// one quoted word stays as typed.
    /// bash splits `--skip-tags=plex` into `--skip-tags`, `=`, `plex`; the
    /// words are joined again before the parse, as bash-completion's
    /// `_get_comp_words_by_ref -n =` does, so `plex` is the flag's value,
    /// not a tag already given; `=` with nothing before it stays.
    #[test]
    fn a_value_bash_split_off_is_joined_again() {
        let words = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "--skip-tags", "=", "plex", "sonarr"])
            ),
            strings(&["sonarr"])
        );
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "-e", "x", "=", "sonarr", "radarr"])
            ),
            strings(&["radarr"])
        );
        assert_eq!(
            rejoined(&words(&["=", "a", "b", "=", "=", "c", "d", "="])),
            words(&["=", "a", "b==c", "d="])
        );
    }

    #[test]
    fn earlier_tags_are_unquoted() {
        let words = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "\"plex\"", "'a, b'", "c\\,d"])
            ),
            strings(&["plex", "a, b", "c,d"])
        );
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "\"open", "two words", "it's"])
            ),
            strings(&["\"open", "two words", "it's"])
        );
        assert_eq!(
            earlier_tags(
                tree,
                &words(&["sb", "install", "-e", "'x=y z'", "\"plex\""])
            ),
            strings(&["plex"])
        );
    }

    /// bash splits `--extra-vars=so` at the `=` (`COMP_WORDBREAKS`), so the
    /// value comes after a word `=` and clap's engine takes it for a tag:
    /// such a word gets none, as sb-go's `--extra-vars=so` case.
    #[test]
    fn a_value_bash_split_off_gets_no_tags() {
        let words = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(split_off_value(&words(&[
            "sb",
            "install",
            "--extra-vars",
            "="
        ])));
        assert!(!split_off_value(&words(&["sb", "install", "--extra-vars"])));
        assert!(!split_off_value(&words(&["sb", "install", "a=b"])));
        assert!(!split_off_value(&[]));
    }
}
