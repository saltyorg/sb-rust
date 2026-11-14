//! `sb list [query]` (ansible spec, `sb list`; module build M34): the
//! tags of Saltbox and Sandbox through the cache, and of `saltbox_mod`
//! with `-m`, listed fresh; in columns, or searched and shown as one
//! table (sb-go cmd/list.go). A repository that fails doesn't hide the
//! others: its error is collected and reported after the output.

use std::fmt::Write as _;

use sb_cli_core::exit::escaped;
use sb_cli_core::theme::Theme;
use sb_term_ui::{Runner, Table};
use term_grid::{Direction, Filling, Grid, GridOptions};

use crate::cache::{LazyRunner, TagCache};
use crate::{Ansible, Kind, Repos};

/// The width the columns fill when stdout isn't a terminal (sb-go's
/// default).
pub const PIPED_WIDTH: usize = 80;

/// The most edits a tag may be from the query (sb-go's, as install's).
const MOST_EDITS: usize = 2;

/// `sb list`'s arguments, sb-go's names and help texts.
#[derive(clap::Args, Clone, Debug, Default, PartialEq, Eq)]
pub struct ListArgs {
    /// A tag, or part of one, to search for
    #[arg(value_name = "query")]
    pub query: Option<String>,
    /// Include Saltbox-mod tags
    #[arg(short = 'm', long)]
    pub include_mod: bool,
}

/// One repository's tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Which repository.
    pub repo: Kind,
    /// Its tags, in Ansible's order.
    pub tags: Vec<String>,
    /// Whether they came from the cache (`(cached)`).
    pub cached: bool,
}

/// What `sb list` found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// `-m` was given and `saltbox_mod`'s directory isn't there.
    pub mod_missing: bool,
    /// Each repository's tags, Saltbox, Sandbox, then `saltbox_mod`.
    pub sections: Vec<Section>,
    /// Each repository that failed, as one line (sb-go's texts).
    pub failures: Vec<String>,
}

/// Saltbox's and Sandbox's tags through the cache, each rebuilt as a
/// top-level step of `runner` when stale; with `include_mod`,
/// `saltbox_mod`'s listed fresh, never cached, when its directory is
/// there (sb-go list.go:119-128). A repository that fails is a line in
/// [`Listing::failures`] and the others go on.
pub async fn gather<F: FnOnce() -> Runner>(
    cache: &mut TagCache,
    ansible: &Ansible,
    repos: &Repos,
    include_mod: bool,
    runner: &mut LazyRunner<F>,
) -> Listing {
    let mut listing = Listing::default();
    for (repo, name) in [(&repos.saltbox, "Saltbox"), (&repos.sandbox, "Sandbox")] {
        // After a Ctrl-C, no other repository starts a child (sb-go stops
        // at the first interruption).
        if runner.token().is_cancelled() {
            return listing;
        }
        let listed = match sb_repo::Repo::open(name, repo.dir()) {
            Ok(checkout) => cache
                .tags_on(ansible, repo, &checkout, runner)
                .await
                .map_err(anyhow::Error::from),
            Err(err) => Err(err.into()),
        };
        match listed {
            Ok(listed) => listing.sections.push(Section {
                repo: repo.kind(),
                tags: listed.tags,
                cached: listed.cached,
            }),
            Err(err) => listing.failures.push(format!(
                "error running and caching ansible tags for {}: {err:#}",
                repo.dir().display()
            )),
        }
    }
    if include_mod && !runner.token().is_cancelled() {
        let repo = &repos.saltbox_mod;
        // std's existence check: missing is `Ok(false)`, any other stat
        // error an error (<https://doc.rust-lang.org/std/path/struct.Path.html#method.try_exists>).
        match repo.dir().try_exists() {
            Ok(false) => listing.mod_missing = true,
            Err(err) => listing
                .failures
                .push(format!("inspect Saltbox-mod directory: {err}")),
            Ok(true) => match ansible.list_tags(repo, &runner.token()).await {
                Ok(tags) => listing.sections.push(Section {
                    repo: Kind::Mod,
                    tags,
                    cached: false,
                }),
                Err(err) => listing.failures.push(format!(
                    "error running ansible list tags for {}: {err:#}",
                    repo.dir().display()
                )),
            },
        }
    }
    listing
}

/// The listing without a query: sb-go's notice for a missing
/// `saltbox_mod`, then each repository's title (`(cached)` when the cache
/// wasn't rebuilt), a blank line and its tags in [`columns`].
#[must_use]
pub fn render_list(listing: &Listing, width: usize) -> String {
    let mut out = String::new();
    if listing.mod_missing {
        out.push_str(MOD_MISSING);
        out.push('\n');
    }
    for section in &listing.sections {
        let title = match section.repo {
            Kind::Saltbox => "Saltbox tags:",
            Kind::Sandbox => "\nSandbox tags (prepend sandbox-):",
            Kind::Mod => "\nSaltbox_mod tags (prepend mod-):",
        };
        let cached = if section.cached { " (cached)" } else { "" };
        let _ = write!(out, "{title}{cached}\n\n");
        out.push_str(&columns(&section.tags, width));
    }
    out
}

/// sb-go's notice when `-m` finds no `saltbox_mod` (its two spaces kept).
const MOD_MISSING: &str =
    "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.";

/// `tags` in as many columns as fit `width`, filled top to bottom, 2
/// spaces apart, each column as wide as its widest tag, as `ls` lays out
/// names (spec, difference 12), through uutils' grid
/// (<https://docs.rs/uutils_term_grid/0.8.1/term_grid/struct.Grid.html>).
/// Each tag escaped (cli-core's `escaped`): a tag is Ansible's output. One
/// line per row; nothing for no tags.
#[must_use]
pub fn columns(tags: &[String], width: usize) -> String {
    let cells: Vec<String> = tags.iter().map(|tag| escaped(tag)).collect();
    let options = GridOptions {
        direction: Direction::TopToBottom,
        filling: Filling::Spaces(2),
        width,
    };
    Grid::new(cells, options).to_string()
}

/// One search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The repository it is in.
    pub repo: Kind,
    /// The tag.
    pub tag: String,
    /// 0 for a substring match, else its edits from the query.
    pub distance: usize,
}

/// The tags matching `query`, ignoring case: as a substring (distance 0)
/// or within [`MOST_EDITS`] of the whole tag (`strsim`'s levenshtein,
/// per Unicode scalar, as install's suggestions); sorted by distance, then
/// repository (Saltbox, Sandbox, `saltbox_mod`), then tag (sb-go
/// list.go:230-300).
#[must_use]
pub fn search(listing: &Listing, query: &str) -> Vec<Match> {
    let query = query.to_lowercase();
    let mut matches: Vec<Match> = listing
        .sections
        .iter()
        .flat_map(|section| section.tags.iter().map(move |tag| (section.repo, tag)))
        .filter_map(|(repo, tag)| {
            let lower = tag.to_lowercase();
            let distance = if lower.contains(&query) {
                0
            } else {
                strsim::levenshtein(&query, &lower)
            };
            (distance <= MOST_EDITS).then(|| Match {
                repo,
                tag: tag.clone(),
                distance,
            })
        })
        .collect();
    matches.sort_by(|a, b| {
        (a.distance, order(a.repo), &a.tag).cmp(&(b.distance, order(b.repo), &b.tag))
    });
    matches
}

/// A repository's place in the results.
fn order(repo: Kind) -> u8 {
    match repo {
        Kind::Saltbox => 0,
        Kind::Sandbox => 1,
        Kind::Mod => 2,
    }
}

/// The repository's name and prefix in the search table (sb-go's).
fn named(repo: Kind) -> (&'static str, &'static str) {
    match repo {
        Kind::Saltbox => ("Saltbox", ""),
        Kind::Sandbox => ("Sandbox", "sandbox-"),
        Kind::Mod => ("Saltbox-mod", "mod-"),
    }
}

/// The search's output: `Found N matching tag(s) for 'q':`, a blank line,
/// then one term-ui table, grouped by repository, the first with results
/// as its heading across both columns and each later one a bold row across them
/// (spec, amendment 2), each tag beside the `sb install` that runs it,
/// then a blank line; or `No tags found matching 'q'` (sb-go's texts);
/// either after the missing `saltbox_mod` notice. `None` when nothing
/// matched, a repository failed and there is no notice: the error says
/// enough. The table in `theme`'s colours when `colour` is on, fitted to
/// `width` when given (term-ui's `Table::render`).
#[must_use]
pub fn render_search(
    listing: &Listing,
    query: &str,
    theme: &Theme,
    colour: bool,
    width: Option<usize>,
) -> Option<String> {
    let matches = search(listing, query);
    let shown = escaped(query);
    // sb-go prints the missing `saltbox_mod` notice in both modes.
    let notice = if listing.mod_missing {
        format!("{MOD_MISSING}\n")
    } else {
        String::new()
    };
    // Grouped by repository in sb-go's order, each group in the sorted
    // order (sb-go's `resultsByRepo`): a repository is one section even
    // when its matches sort apart.
    let mut groups = [Kind::Saltbox, Kind::Sandbox, Kind::Mod]
        .into_iter()
        .filter(|repo| matches.iter().any(|m| m.repo == *repo));
    let Some(first) = groups.next() else {
        let none = listing
            .failures
            .is_empty()
            .then(|| format!("No tags found matching '{shown}'\n"));
        return match none {
            Some(none) => Some(format!("{notice}{none}")),
            None => (!notice.is_empty()).then_some(notice),
        };
    };
    let mut table = Table::spanning(named(first).0);
    for repo in std::iter::once(first).chain(groups) {
        let (name, prefix) = named(repo);
        if repo != first {
            table.spanning_row(name);
        }
        for found in matches.iter().filter(|m| m.repo == repo) {
            table.row([
                found.tag.clone(),
                format!("sb install {prefix}{}", found.tag),
            ]);
        }
    }
    Some(format!(
        "{notice}Found {} matching tag(s) for '{shown}':\n\n{}\n\n",
        matches.len(),
        table.render(theme, colour, width)
    ))
}

/// The repositories that failed as one error, sb-go's headline then a
/// line for each, each a cause in cli-core's error block (a chain of
/// contexts, as sb-docker's `failures_error`: the block models one chain).
/// `None` when none failed.
#[must_use]
pub fn failures_error(listing: &Listing, searched: bool) -> Option<anyhow::Error> {
    let mut lines = listing.failures.iter().rev();
    let last = anyhow::Error::msg(lines.next()?.clone());
    let chain = lines.cloned().fold(last, anyhow::Error::context);
    let doing = if searched { "searching" } else { "listing" };
    Some(chain.context(format!("errors occurred while {doing} tags")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::{Palette, Profile};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    /// Saltbox-like tags, of mixed lengths, in Ansible's order.
    const SALTBOX: [&str; 30] = [
        "always",
        "authelia",
        "autoscan",
        "bazarr",
        "cloudflare",
        "crowdsec",
        "ddclient",
        "docker",
        "emby",
        "glances",
        "jackett",
        "jellyfin",
        "lidarr",
        "mariadb",
        "nzbget",
        "ombi",
        "organizr",
        "overseerr",
        "plex",
        "plex-autoscan",
        "portainer",
        "prowlarr",
        "qbittorrent",
        "radarr",
        "rclone",
        "sabnzbd",
        "sanity-check",
        "sonarr",
        "tautulli",
        "traefik",
    ];
    const SANDBOX: [&str; 8] = [
        "always",
        "autobrr",
        "bazarr4k",
        "plex-meta-manager",
        "plexripper",
        "radarr4k",
        "sonarr4k",
        "tdarr",
    ];
    const MOD: [&str; 2] = ["always", "plexguide"];

    fn listing(cached: bool) -> Listing {
        Listing {
            mod_missing: false,
            sections: vec![
                Section {
                    repo: Kind::Saltbox,
                    tags: strings(&SALTBOX),
                    cached,
                },
                Section {
                    repo: Kind::Sandbox,
                    tags: strings(&SANDBOX),
                    cached,
                },
            ],
            failures: Vec::new(),
        }
    }

    fn plain() -> Theme {
        Theme::resolve(&Palette::default(), Profile::Ascii)
    }

    /// Columns filled top to bottom, each as wide as its widest tag, 2
    /// spaces apart, as many as the width holds; piped is 80.
    #[test]
    fn the_tags_fill_columns_to_the_width() {
        insta::assert_snapshot!("columns-40", columns(&strings(&SALTBOX), 40));
        insta::assert_snapshot!("columns-80", columns(&strings(&SALTBOX), PIPED_WIDTH));
        insta::assert_snapshot!("columns-120", columns(&strings(&SALTBOX), 120));
        for width in [40, 80, 120] {
            let text = columns(&strings(&SALTBOX), width);
            assert!(
                text.lines().all(|line| line.chars().count() <= width),
                "{width}: {text}"
            );
        }
        // Top to bottom: the second line starts with the second tag.
        let text = columns(&strings(&SALTBOX), 40);
        assert!(
            text.lines()
                .nth(1)
                .is_some_and(|l| l.starts_with("authelia"))
        );
        assert_eq!(columns(&[], 80), "");
        // Wider than the width: one tag a line.
        assert_eq!(
            columns(&strings(&["abcdef", "ghijkl"]), 4),
            "abcdef\nghijkl\n"
        );
    }

    /// The listing: titles (`(cached)` only from the cache), a blank line,
    /// the columns; the missing `saltbox_mod` notice first.
    #[test]
    fn the_listing_has_sb_gos_titles() {
        insta::assert_snapshot!("list-cached-80", render_list(&listing(true), 80));
        let mut fresh = listing(false);
        fresh.mod_missing = true;
        fresh.sections.push(Section {
            repo: Kind::Mod,
            tags: strings(&MOD),
            cached: false,
        });
        insta::assert_snapshot!("list-fresh-mod-120", render_list(&fresh, 120));
        let text = render_list(&fresh, 120);
        assert!(text.starts_with(
            "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n\
             Saltbox tags:\n\n"
        ));
        assert!(text.contains("\nSandbox tags (prepend sandbox-):\n\n"));
        assert!(text.contains("\nSaltbox_mod tags (prepend mod-):\n\nalways  plexguide\n"));
        assert!(!text.contains("(cached)"));
    }

    /// A substring is distance 0; within two edits of the whole tag
    /// counts; case is ignored; sorted by distance, repository, tag.
    #[test]
    fn a_search_matches_substrings_and_near_tags() {
        let mut all = listing(true);
        all.sections.push(Section {
            repo: Kind::Mod,
            tags: strings(&MOD),
            cached: false,
        });
        let found: Vec<(Kind, String, usize)> = search(&all, "PLEX")
            .into_iter()
            .map(|m| (m.repo, m.tag, m.distance))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Saltbox, "plex".into(), 0),
                (Kind::Saltbox, "plex-autoscan".into(), 0),
                (Kind::Sandbox, "plex-meta-manager".into(), 0),
                (Kind::Sandbox, "plexripper".into(), 0),
                (Kind::Mod, "plexguide".into(), 0),
            ]
        );
        let found: Vec<(Kind, String, usize)> = search(&all, "Emba")
            .into_iter()
            .map(|m| (m.repo, m.tag, m.distance))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Saltbox, "emby".into(), 1),
                (Kind::Saltbox, "ombi".into(), 2)
            ]
        );
        assert_eq!(search(&all, "zzzzzz"), []);
    }

    /// The search table: the first repository the heading, later ones
    /// spanning rows; no match says so, unless a repository failed.
    #[test]
    fn a_search_prints_one_table() {
        let text = render_search(&listing(true), "plex", &plain(), false, None).expect("output");
        insta::assert_snapshot!("search-plex", text);
        let text =
            render_search(&listing(true), "radar", &plain(), false, Some(40)).expect("output");
        insta::assert_snapshot!("search-radar-40", text);
        assert_eq!(
            render_search(&listing(true), "nothing", &plain(), false, None).as_deref(),
            Some("No tags found matching 'nothing'\n")
        );
        let mut failed = listing(true);
        failed.failures.push("error".into());
        assert_eq!(
            render_search(&failed, "nothing", &plain(), false, None),
            None
        );
        // Each repository is one section, though its matches sort apart:
        // Sandbox's substring first, Saltbox's typo after it.
        let apart = Listing {
            sections: vec![
                Section {
                    repo: Kind::Saltbox,
                    tags: strings(&["abd"]),
                    cached: true,
                },
                Section {
                    repo: Kind::Sandbox,
                    tags: strings(&["abc1"]),
                    cached: true,
                },
            ],
            ..Listing::default()
        };
        assert_eq!(search(&apart, "abc")[0].repo, Kind::Sandbox);
        let text = render_search(&apart, "abc", &plain(), false, None).expect("output");
        insta::assert_snapshot!("search-grouped", text);
        // At 80 and 120 columns the table keeps its natural width.
        let piped = render_search(&listing(true), "plex", &plain(), false, None);
        for width in [80, 120] {
            assert_eq!(
                render_search(&listing(true), "plex", &plain(), false, Some(width)),
                piped
            );
        }
        // The missing `saltbox_mod` notice comes first, in every outcome.
        let mut missing = listing(true);
        missing.mod_missing = true;
        let notice =
            "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n";
        for (query, then) in [("plex", "Found"), ("nothing", "No tags")] {
            let text = render_search(&missing, query, &plain(), false, None).expect("output");
            assert!(text.starts_with(&format!("{notice}{then}")), "{text}");
        }
        missing.failures.push("error".into());
        assert_eq!(
            render_search(&missing, "nothing", &plain(), false, None).as_deref(),
            Some(notice)
        );
        // The query is escaped, as any text sb echoes.
        assert_eq!(
            render_search(&listing(true), "a\x1b[2J", &plain(), false, None).as_deref(),
            Some("No tags found matching 'a\\u{1b}[2J'\n")
        );
    }

    /// Failures: sb-go's headline, then each repository's line, in order.
    #[test]
    fn failures_are_one_error() {
        assert!(failures_error(&listing(true), false).is_none());
        let mut failed = listing(true);
        failed.failures = strings(&["first", "second"]);
        let err = failures_error(&failed, false).expect("an error");
        let chain: Vec<String> = err.chain().map(ToString::to_string).collect();
        assert_eq!(
            chain,
            ["errors occurred while listing tags", "first", "second"]
        );
        let err = failures_error(&failed, true).expect("an error");
        assert_eq!(err.to_string(), "errors occurred while searching tags");
    }
}
