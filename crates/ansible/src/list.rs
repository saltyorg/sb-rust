//! `sb list [query]` (ansible spec, `sb list`; module build M34): the
//! tags of Saltbox and Sandbox through the cache, and of `saltbox_mod`
//! with `-m`, listed fresh; in columns, or searched and shown as one
//! table (sb-go cmd/list.go). A repository that fails doesn't hide the
//! others: its error is collected and reported after the output.

use std::fmt::Write as _;

use sb_cli_core::exit::escaped;
use sb_cli_core::theme::{Profile, Role, Theme};
use sb_term_ui::{Runner, Table};
use term_grid::{Direction, Filling, Grid, GridOptions};
use unicode_width::UnicodeWidthChar;

use crate::cache::{CacheError, LazyRunner, Listed, TagCache};
use crate::{Ansible, Kind, Repo, Repos};

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
    /// Print the tags as JSON
    // New in sb (spec, `--json`; decision 20).
    #[arg(long)]
    pub json: bool,
    /// Rebuild the Saltbox and Sandbox tag caches before listing
    // New in sb (spec, `--refresh`; decision 21).
    #[arg(long)]
    pub refresh: bool,
}

/// One repository's tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Which repository.
    pub repo: Kind,
    /// Its tags, in Ansible's order.
    pub tags: Vec<String>,
    /// Where they came from (`--json`'s `cache`; the run log's `cached`).
    pub cache: Cache,
}

/// Where a repository's tags came from (spec, `--json`), its word there
/// serde's `lowercase` of the name
/// (<https://serde.rs/container-attrs.html#rename_all>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cache {
    /// Read from cache.json.
    Cached,
    /// Listed now and recorded (the file written when it changed).
    Rebuilt,
    /// `saltbox_mod`'s, listed fresh and never cached.
    Listed,
    /// `-m`'s `saltbox_mod` isn't there (`--json` only).
    Missing,
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
/// top-level step of `runner` when stale, or with `args.refresh` whatever
/// the cache holds (spec, decision 21); with `args.include_mod`,
/// `saltbox_mod`'s listed fresh, never cached, when its directory is
/// there (sb-go list.go:119-128). A repository that fails is a line in
/// [`Listing::failures`] and the others go on.
pub async fn gather<F: FnOnce() -> Runner>(
    cache: &mut TagCache,
    ansible: &Ansible,
    repos: &Repos,
    args: &ListArgs,
    runner: &mut LazyRunner<F>,
) -> Listing {
    let mut listing = Listing::default();
    // After a Ctrl-C, no repository starts a child it hasn't started yet
    // (sb-go stops at the first interruption): the HEADs below start
    // together, and a Ctrl-C during them stops both (their shared token).
    if runner.token().is_cancelled() {
        return listing;
    }
    let [saltbox, sandbox] = [(&repos.saltbox, "Saltbox"), (&repos.sandbox, "Sandbox")];
    // Both HEADs asked at once: each `git rev-parse` is nearly all its own
    // start, so the second no longer waits for the first. They are used
    // in the order below, as before, so the output and the failures'
    // order are the same. A run that isn't interrupted read both before;
    // one interrupted during Saltbox's turn has now read Sandbox's HEAD
    // too, before the interruption (tokio's `join!`,
    // <https://docs.rs/tokio/1.54.1/tokio/macro.join.html>).
    let token = runner.token();
    let heads = tokio::join!(head_of(saltbox, &token), head_of(sandbox, &token));
    for ((repo, name), head) in [(saltbox, heads.0), (sandbox, heads.1)] {
        if runner.token().is_cancelled() {
            return listing;
        }
        let listed = match head {
            Ok(Ok(head)) if args.refresh => cache
                .refresh_at(ansible, repo, head, runner)
                .await
                .map(|tags| Listed {
                    tags,
                    cached: false,
                })
                .map_err(anyhow::Error::from),
            Ok(Ok(head)) => cache
                .tags_at(ansible, repo, head, runner)
                .await
                .map_err(anyhow::Error::from),
            // As the cache's own read of the HEAD fails.
            Ok(Err(err)) => Err(CacheError::from(err).into()),
            Err(err) => Err(err.into()),
        };
        match listed {
            Ok(listed) => {
                // The listing shows no cache state; the run log has it
                // (spec, decision 19). A rebuild is also its step's records.
                tracing::info!(
                    repo = name,
                    tags = listed.tags.len(),
                    cached = listed.cached,
                    "tags listed"
                );
                listing.sections.push(Section {
                    repo: repo.kind(),
                    tags: listed.tags,
                    cache: if listed.cached {
                        Cache::Cached
                    } else {
                        Cache::Rebuilt
                    },
                });
            }
            Err(err) => listing.failures.push(format!(
                "error running and caching ansible tags for {}: {err:#}",
                repo.dir().display()
            )),
        }
    }
    if args.include_mod && !runner.token().is_cancelled() {
        let repo = &repos.saltbox_mod;
        // std's existence check: missing is `Ok(false)`, any other stat
        // error an error (<https://doc.rust-lang.org/std/path/struct.Path.html#method.try_exists>).
        match repo.dir().try_exists() {
            Ok(false) => listing.mod_missing = true,
            Err(err) => listing
                .failures
                .push(format!("inspect Saltbox-mod directory: {err}")),
            Ok(true) => match list_mod(ansible, repo, runner).await {
                Ok(tags) => listing.sections.push(Section {
                    repo: Kind::Mod,
                    tags,
                    cache: Cache::Listed,
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

/// The HEAD of `repo`'s checkout, named `name` (sb-repo's `head()`), or
/// why the checkout didn't open.
async fn head_of(
    (repo, name): (&Repo, &str),
    token: &tokio_util::sync::CancellationToken,
) -> Result<Result<String, sb_repo::RepoError>, sb_repo::RepoError> {
    Ok(sb_repo::Repo::open(name, repo.dir())?.head(token).await)
}

/// `sb list --json`'s document (spec, `--json`; decision 20): schema 1,
/// each repository with its install prefix, cache state and tags (with a
/// query, its matches in the search's order), and the failures' lines.
/// Fields are only ever added (scripts read them); serde keeps the fields'
/// order.
#[must_use]
pub fn render_json(listing: &Listing, query: Option<&str>) -> String {
    #[derive(serde::Serialize)]
    struct Document<'a> {
        schema: u32,
        query: Option<&'a str>,
        repositories: Vec<Entry<'a>>,
        errors: &'a [String],
    }
    #[derive(serde::Serialize)]
    struct Entry<'a> {
        repository: &'static str,
        prefix: &'static str,
        cache: Cache,
        tags: Vec<&'a str>,
    }
    let found = query.map(|query| search(listing, query));
    let mut repositories: Vec<Entry<'_>> = listing
        .sections
        .iter()
        .map(|section| Entry {
            repository: json_name(section.repo),
            prefix: json_prefix(section.repo),
            cache: section.cache,
            tags: match &found {
                None => section.tags.iter().map(String::as_str).collect(),
                Some(found) => found
                    .iter()
                    .filter(|m| m.repo == section.repo)
                    .map(|m| m.tag.as_str())
                    .collect(),
            },
        })
        .collect();
    if listing.mod_missing {
        repositories.push(Entry {
            repository: json_name(Kind::Mod),
            prefix: json_prefix(Kind::Mod),
            cache: Cache::Missing,
            tags: Vec::new(),
        });
    }
    sb_cli_core::json::line(&Document {
        schema: 1,
        query,
        repositories,
        errors: &listing.failures,
    })
}

/// A repository's `--json` name.
fn json_name(repo: Kind) -> &'static str {
    match repo {
        Kind::Saltbox => "saltbox",
        Kind::Sandbox => "sandbox",
        Kind::Mod => "saltbox_mod",
    }
}

/// What `sb install` takes before a repository's tags (its routing): the
/// prefix the listing's headings show.
fn json_prefix(repo: Kind) -> &'static str {
    named(repo).1
}

/// The step `saltbox_mod`'s listing shows in while it runs (the
/// maintainer's text, 2026-11-14).
const MOD_STEP: &str = "Listing Saltbox_mod tags";

/// `saltbox_mod`'s tags, listed fresh in a top-level step of `runner`,
/// transient when its steps are (`sb list`'s: ansible spec, decision
/// 18): shown while the listing runs; gone at the end when every step
/// succeeded, kept with the whole tree when one failed.
async fn list_mod<F: FnOnce() -> Runner>(
    ansible: &Ansible,
    repo: &Repo,
    runner: &mut LazyRunner<F>,
) -> anyhow::Result<Vec<String>> {
    let labels = runner.labels(MOD_STEP, "Saltbox_mod tags listed");
    runner
        .runner()
        .step(labels, async |step| {
            ansible.list_tags(repo, &step.token()).await
        })
        .await
        // The step has shown why; its `Exit` says only that it ended.
        .map_err(|_| anyhow::anyhow!("listing the tags failed"))
}

/// The listing without a query, the maintainer's "refined columns"
/// (spec, `sb list`, decision 19): sb-go's notice for a missing
/// `saltbox_mod`, if any; a blank line; each repository's heading, a rule
/// across `width` and its tags in [`columns`], a blank line between
/// repositories; then a blank line and the footer hint at the query
/// (`-m` is help's to explain). Every column is as wide as the widest tag
/// of any repository, so the columns line up between them. Styled in
/// `theme`'s roles when `colour` is on and the profile isn't `ascii`;
/// plain otherwise, the rule then in `-`. Nothing but the notice when no
/// repository was listed.
#[must_use]
pub fn render_list(listing: &Listing, theme: &Theme, colour: bool, width: usize) -> String {
    let mut out = String::new();
    if listing.mod_missing {
        out.push_str(MOD_MISSING);
        out.push('\n');
    }
    if listing.sections.is_empty() {
        return out;
    }
    // Each slot in its role from [`COLOURS`], the heading bold. With
    // colour off or under `ascii`, no styling at all, bold included (as
    // help's).
    let styled = colour && theme.profile() != Profile::Ascii;
    let plain = Theme::plain();
    let theme = if styled { theme } else { &plain };
    let heading = theme.style(COLOURS.heading);
    let heading = if styled { heading.bold() } else { heading };
    let (counted, ruled) = (theme.style(COLOURS.count), theme.style(COLOURS.rule));
    let (command, hint) = (theme.style(COLOURS.command), theme.style(COLOURS.hint));
    // Each tag escaped (cli-core's `escaped`: a tag is Ansible's output)
    // and measured as shown.
    let shown: Vec<Vec<String>> = listing
        .sections
        .iter()
        .map(|section| section.tags.iter().map(|tag| escaped(tag)).collect())
        .collect();
    let widest = shown
        .iter()
        .flatten()
        .map(|tag| display_width(tag))
        .max()
        .unwrap_or(0);
    let rule = if styled { "─" } else { "-" }.repeat(width);
    for (section, cells) in listing.sections.iter().zip(shown) {
        let (name, prefix) = named(section.repo);
        let count = section.tags.len();
        let noun = if count == 1 { "tag" } else { "tags" };
        let _ = write!(
            out,
            "\n{heading}{name}{heading:#}{counted}  {count} {noun}{counted:#}"
        );
        if !prefix.is_empty() {
            let _ = write!(
                out,
                "{hint}  ·  install as {hint:#}{command}{prefix}<tag>{command:#}"
            );
        }
        let _ = writeln!(out, "\n{ruled}{rule}{ruled:#}");
        out.push_str(&columns(cells, widest, width));
    }
    let _ = writeln!(
        out,
        "\n{command}sb list <query>{command:#}{hint} search{hint:#}"
    );
    out
}

/// The listing's colours, the one place they are chosen (spec, decision
/// 19, the maintainer 2026-11-16: the canvas's colours): each of the
/// listing's slots names the theme role it is drawn in, so a change is
/// one line here, and nothing else in this file names a role. Roles, not
/// colours: what a role looks like is the theme's (a role's colour can
/// be overridden in sb's config file, `[theme.colors]`; cli-core spec,
/// startup step 6); motd's slots, unlike these, have built-in colours of
/// their own (crates/motd/src/config.rs, `Slot::built_in`). A struct of
/// fields rather than a `match`, so that two slots in one role stay two
/// lines (clippy's pedantic `match_same_arms` would merge them).
const COLOURS: Colours = Colours {
    heading: Role::Service,
    count: Role::Muted,
    rule: Role::Muted,
    command: Role::Accent,
    hint: Role::Muted,
};

/// The theme role of each of the listing's slots ([`COLOURS`]).
struct Colours {
    /// A repository's name (drawn bold too).
    heading: Role,
    /// `  N tags` after it.
    count: Role,
    /// The rule under each heading.
    rule: Role,
    /// A command: `sandbox-<tag>`, `mod-<tag>`, `sb list <query>`.
    command: Role,
    /// The words around them: `  ·  install as `, ` search`.
    hint: Role,
}

/// sb-go's notice when `-m` finds no `saltbox_mod` (its two spaces kept).
const MOD_MISSING: &str =
    "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.";

/// The space between two columns: the grid's default
/// (<https://docs.rs/uutils_term_grid/0.8.1/term_grid/constant.DEFAULT_SEPARATOR_SIZE.html>).
const GAP: usize = term_grid::DEFAULT_SEPARATOR_SIZE;

/// `text`'s width in terminal columns as the grid measures a cell:
/// ansi-width's sum of each character's width (`UnicodeWidthChar`; no
/// escape codes here: the text is escaped). unicode-width's string
/// width can differ, on ligatures such as lam-alef, and padding by it
/// would leave such a column one wider than the rest.
fn display_width(text: &str) -> usize {
    text.chars().filter_map(UnicodeWidthChar::width).sum()
}

/// `cells` (escaped tags) in as many columns as fit `width`, filled top to
/// bottom, every column `widest` plus [`GAP`] wide, through uutils' grid
/// (<https://docs.rs/uutils_term_grid/0.8.1/term_grid/struct.Grid.html>).
/// Why each cell is padded first: the grid sizes each column by its own
/// widest cell and has no fixed column width (`GridOptions` has only
/// `direction`, `filling` and `width`:
/// <https://docs.rs/uutils_term_grid/0.8.1/term_grid/struct.GridOptions.html>),
/// so cells of one width give columns of one width, the same in every
/// repository (spec, decision 19). The grid then fits as many as
/// `(width + 2) / (widest + 2)`. A row's last cell keeps its padding, so
/// each line's trailing spaces are dropped (a tag is trimmed: it never
/// ends in one). Widths are measured as the grid measures
/// ([`display_width`]). One line per row; nothing for no cells.
fn columns(cells: Vec<String>, widest: usize, width: usize) -> String {
    let padded: Vec<String> = cells
        .into_iter()
        .map(|cell| {
            let pad = widest.saturating_sub(display_width(&cell));
            format!("{cell}{:pad$}", "")
        })
        .collect();
    let options = GridOptions {
        direction: Direction::TopToBottom,
        filling: Filling::Spaces(GAP),
        width,
    };
    let grid = Grid::new(padded, options).to_string();
    let mut out = String::with_capacity(grid.len());
    for line in grid.lines() {
        out.push_str(line.trim_end_matches(' '));
        out.push('\n');
    }
    out
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

/// The repository's name and prefix, in the search table (sb-go's) and
/// the listing's headings.
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
    use sb_cli_core::theme::Palette;

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

    fn listing() -> Listing {
        Listing {
            mod_missing: false,
            sections: vec![
                Section {
                    repo: Kind::Saltbox,
                    tags: strings(&SALTBOX),
                    cache: Cache::Cached,
                },
                Section {
                    repo: Kind::Sandbox,
                    tags: strings(&SANDBOX),
                    cache: Cache::Rebuilt,
                },
            ],
            failures: Vec::new(),
        }
    }

    fn plain() -> Theme {
        Theme::resolve(&Palette::default(), Profile::Ascii)
    }

    /// A tag as wide as the longest in the canvas's Saltbox listing.
    const LONG: &str = "generate-traefik-file-template";

    /// Saltbox's tags with [`LONG`], Sandbox's short ones, and with
    /// `with_mod` `saltbox_mod`'s.
    fn wide(with_mod: bool) -> Listing {
        let mut listing = listing();
        listing.sections[0].tags.push(LONG.into());
        if with_mod {
            listing.sections.push(Section {
                repo: Kind::Mod,
                tags: strings(&MOD),
                cache: Cache::Listed,
            });
        }
        listing
    }

    fn colour() -> Theme {
        Theme::resolve(&Palette::default(), Profile::TrueColor)
    }

    /// Escape codes made visible, so a colour snapshot can be read.
    fn visible(text: &str) -> String {
        text.replace('\x1b', "␛")
    }

    /// Columns filled top to bottom, every one `widest` plus 2 wide, as
    /// many as the width holds (the last needs no gap), no trailing
    /// spaces.
    #[test]
    fn the_tags_fill_equal_columns_to_the_width() {
        let tags: Vec<String> = SALTBOX.iter().map(|tag| escaped(tag)).collect();
        insta::assert_snapshot!("columns-40", columns(tags.clone(), 13, 40));
        insta::assert_snapshot!("columns-80", columns(tags.clone(), 13, PIPED_WIDTH));
        for width in [40, 80, 120] {
            let text = columns(tags.clone(), 13, width);
            assert!(
                text.lines().all(|line| line.chars().count() <= width),
                "{width}: {text}"
            );
            assert!(text.lines().all(|line| !line.ends_with(' ')), "{text}");
        }
        // Top to bottom: the second line starts with the second tag; the
        // second column starts 15 on, though `always` is 6 wide.
        let text = columns(tags.clone(), 13, 40);
        assert_eq!(text.lines().next(), Some("always         ombi"), "{text}");
        assert!(
            text.lines()
                .nth(1)
                .is_some_and(|l| l.starts_with("authelia"))
        );
        // 2 columns of 19 fit 40 exactly: the last has no gap.
        assert_eq!(
            columns(strings(&["a", "b", "c"]), 19, 40),
            format!("a{}c\nb\n", " ".repeat(20))
        );
        assert_eq!(columns(Vec::new(), 13, 80), "");
        // Wider than the width: one tag a line.
        assert_eq!(
            columns(strings(&["abcdef", "ghijkl"]), 6, 4),
            "abcdef\nghijkl\n"
        );
        // Widths are display widths: a wide character takes two.
        assert_eq!(
            columns(strings(&["日本", "ab", "cd"]), 4, 12),
            "日本  cd\nab\n"
        );
        // Measured per character, as the grid measures (ansi-width): lam
        // and alef count 2 there, though unicode-width's string width
        // takes them as one ligature; padded so, the column stays 3 wide.
        let ligature = "\u{644}\u{627}x";
        assert_eq!(display_width(ligature), 3);
        assert_eq!(unicode_width::UnicodeWidthStr::width(ligature), 2);
        assert_eq!(
            columns(strings(&[ligature, "abc", "def"]), 3, 12),
            format!("{ligature}  def\nabc\n")
        );
    }

    /// The listing at 80 and 180 columns, plain and coloured: a blank
    /// line, each repository's heading, rule and columns, the footer.
    #[test]
    fn the_listing_has_the_refined_columns() {
        for width in [80, 180] {
            insta::assert_snapshot!(
                format!("list-plain-{width}"),
                render_list(&wide(false), &plain(), true, width)
            );
            insta::assert_snapshot!(
                format!("list-colour-{width}"),
                visible(&render_list(&wide(false), &colour(), true, width))
            );
        }
        insta::assert_snapshot!(
            "list-mod-plain-80",
            render_list(&wide(true), &plain(), true, 80)
        );
    }

    /// One column width across every repository: Saltbox's long tag
    /// widens Sandbox's and `saltbox_mod`'s columns too.
    #[test]
    fn every_repository_shares_the_widest_tag() {
        let text = render_list(&wide(true), &plain(), false, 80);
        let gap = " ".repeat(LONG.len() + 2 - "always".len());
        assert!(text.contains(&format!("\nalways{gap}organizr\n")), "{text}");
        assert!(
            text.contains(&format!("\nalways{gap}plexripper\n")),
            "{text}"
        );
        assert!(
            text.contains(&format!("\nalways{gap}plexguide\n")),
            "{text}"
        );
        // Without it, the widest is `plex-meta-manager`, for all three.
        let text = render_list(&listing(), &plain(), false, 80);
        let gap = " ".repeat("plex-meta-manager".len() + 2 - "always".len());
        assert!(text.contains(&format!("\nalways{gap}")), "{text}");
        assert!(!text.contains(LONG), "{text}");
    }

    /// The headings: the name, the count (`1 tag`), and Sandbox's and
    /// `saltbox_mod`'s install prefix.
    #[test]
    fn the_headings_name_the_count_and_the_prefix() {
        let mut one = wide(true);
        one.sections[2].tags.truncate(1);
        let text = render_list(&one, &plain(), false, 80);
        let headings: Vec<&str> = text
            .lines()
            .filter(|line| line.starts_with("Salt") || line.starts_with("Sand"))
            .collect();
        assert_eq!(
            headings,
            [
                "Saltbox  31 tags",
                "Sandbox  8 tags  ·  install as sandbox-<tag>",
                "Saltbox-mod  1 tag  ·  install as mod-<tag>",
            ]
        );
        // A repository with no tags is its heading and rule alone.
        let mut empty = listing();
        empty.sections[1].tags.clear();
        let text = render_list(&empty, &plain(), false, 80);
        let rule = "-".repeat(80);
        assert!(
            text.contains(&format!(
                "Sandbox  0 tags  ·  install as sandbox-<tag>\n{rule}\n\nsb list"
            )),
            "{text}"
        );
    }

    /// The footer hints at the query only, with or without `-m` (`-m` is
    /// help's to explain; the maintainer, 2026-11-16).
    #[test]
    fn the_footer_hints_at_the_query_only() {
        for listed in [listing(), wide(true)] {
            let text = render_list(&listed, &plain(), false, 80);
            assert!(text.ends_with("\n\nsb list <query> search\n"), "{text}");
            assert!(!text.contains("include Saltbox-mod"), "{text}");
        }
        // Coloured: the command in its slot's role, the word in the
        // hint's.
        let theme = colour();
        let text = render_list(&listing(), &theme, true, 80);
        let (code, hint) = (theme.style(COLOURS.command), theme.style(COLOURS.hint));
        assert!(
            text.ends_with(&format!(
                "\n\n{code}sb list <query>{code:#}{hint} search{hint:#}\n"
            )),
            "{text:?}"
        );
    }

    /// Plain output (colour off, or `ascii` on a terminal) has no escape
    /// codes and a `-` rule; coloured, each slot is in its role from
    /// [`COLOURS`]: the rule a `─` across the width, the name bold.
    #[test]
    fn plain_output_has_no_styling_and_a_dash_rule() {
        for (theme, colour_on) in [(colour(), false), (plain(), true), (plain(), false)] {
            let text = render_list(&wide(false), &theme, colour_on, 80);
            assert!(!text.contains('\x1b'), "{text:?}");
            assert!(text.contains(&format!("\n{}\n", "-".repeat(80))), "{text}");
            assert!(!text.contains('─'), "{text}");
        }
        let theme = colour();
        let text = render_list(&wide(false), &theme, true, 120);
        let heading = theme.style(COLOURS.heading).bold();
        let (count, rule) = (theme.style(COLOURS.count), theme.style(COLOURS.rule));
        assert!(
            text.starts_with(&format!(
                "\n{heading}Saltbox{heading:#}{count}  31 tags{count:#}\n\
                 {rule}{}{rule:#}\n",
                "─".repeat(120)
            )),
            "{text:?}"
        );
        let (code, hint) = (theme.style(COLOURS.command), theme.style(COLOURS.hint));
        assert!(
            text.contains(&format!(
                "{heading}Sandbox{heading:#}{count}  8 tags{count:#}\
                 {hint}  ·  install as {hint:#}{code}sandbox-<tag>{code:#}\n"
            )),
            "{text:?}"
        );
        // The tags themselves are never styled.
        assert!(text.contains("\nalways  "), "{text:?}");
    }

    /// The missing `saltbox_mod` notice comes first, before the blank
    /// line; with no repository listed there is nothing else, nor a
    /// footer.
    #[test]
    fn the_notice_comes_first() {
        let mut missing = listing();
        missing.mod_missing = true;
        let text = render_list(&missing, &plain(), false, 80);
        assert!(
            text.starts_with(
                "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n\
                 \nSaltbox  30 tags\n"
            ),
            "{text}"
        );
        missing.sections.clear();
        assert_eq!(
            render_list(&missing, &plain(), false, 80),
            "Saltbox-mod directory not found, skipping.  Ensure Saltbox-mod is installed.\n"
        );
        assert_eq!(render_list(&Listing::default(), &plain(), false, 80), "");
        // A tag is escaped, and measured as shown.
        let mut forged = listing();
        forged.sections[1].tags = strings(&["a\x1b[2Jb", "c"]);
        let text = render_list(&forged, &plain(), false, 80);
        assert!(text.contains("a\\u{1b}[2Jb"), "{text}");
        assert!(!text.contains('\x1b'), "{text:?}");
    }

    /// A substring is distance 0; within two edits of the whole tag
    /// counts; case is ignored; sorted by distance, repository, tag.
    #[test]
    fn a_search_matches_substrings_and_near_tags() {
        let mut all = listing();
        all.sections.push(Section {
            repo: Kind::Mod,
            tags: strings(&MOD),
            cache: Cache::Listed,
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
        let text = render_search(&listing(), "plex", &plain(), false, None).expect("output");
        insta::assert_snapshot!("search-plex", text);
        let text = render_search(&listing(), "radar", &plain(), false, Some(40)).expect("output");
        insta::assert_snapshot!("search-radar-40", text);
        assert_eq!(
            render_search(&listing(), "nothing", &plain(), false, None).as_deref(),
            Some("No tags found matching 'nothing'\n")
        );
        let mut failed = listing();
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
                    cache: Cache::Cached,
                },
                Section {
                    repo: Kind::Sandbox,
                    tags: strings(&["abc1"]),
                    cache: Cache::Rebuilt,
                },
            ],
            ..Listing::default()
        };
        assert_eq!(search(&apart, "abc")[0].repo, Kind::Sandbox);
        let text = render_search(&apart, "abc", &plain(), false, None).expect("output");
        insta::assert_snapshot!("search-grouped", text);
        // At 80 and 120 columns the table keeps its natural width.
        let piped = render_search(&listing(), "plex", &plain(), false, None);
        for width in [80, 120] {
            assert_eq!(
                render_search(&listing(), "plex", &plain(), false, Some(width)),
                piped
            );
        }
        // The missing `saltbox_mod` notice comes first, in every outcome.
        let mut missing = listing();
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
            render_search(&listing(), "a\x1b[2J", &plain(), false, None).as_deref(),
            Some("No tags found matching 'a\\u{1b}[2J'\n")
        );
    }

    /// Failures: sb-go's headline, then each repository's line, in order.
    #[test]
    fn failures_are_one_error() {
        assert!(failures_error(&listing(), false).is_none());
        let mut failed = listing();
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

    /// `--json`'s document (spec): `schema`, the query, each repository
    /// with its install prefix, cache state and tags, the failures.
    #[test]
    fn json_lists_each_repository_with_its_prefix_and_cache() {
        let mut listing = listing();
        listing.sections[0].tags = strings(&["plex", "sonarr"]);
        listing.sections[1].tags = strings(&["komga"]);
        assert_eq!(
            render_json(&listing, None),
            concat!(
                r#"{"schema":1,"query":null,"repositories":["#,
                r#"{"repository":"saltbox","prefix":"","cache":"cached","tags":["plex","sonarr"]},"#,
                r#"{"repository":"sandbox","prefix":"sandbox-","cache":"rebuilt","tags":["komga"]}],"#,
                r#""errors":[]}"#,
                "\n"
            )
        );
        listing.sections.push(Section {
            repo: Kind::Mod,
            tags: strings(&["custom"]),
            cache: Cache::Listed,
        });
        let text = render_json(&listing, None);
        assert!(
            text.contains(
                r#"{"repository":"saltbox_mod","prefix":"mod-","cache":"listed","tags":["custom"]}"#
            ),
            "{text}"
        );
    }

    /// `-m` with no `saltbox_mod` directory is a `missing` repository with no
    /// tags; a failed repository has no object, its line is under
    /// `errors`; a tag's control characters are escaped.
    #[test]
    fn json_shows_a_missing_mod_and_the_failures() {
        let listing = Listing {
            mod_missing: true,
            sections: vec![Section {
                repo: Kind::Saltbox,
                tags: strings(&["pl\u{1b}[31mex"]),
                cache: Cache::Rebuilt,
            }],
            failures: strings(&["error running and caching ansible tags for /opt/sandbox: boom"]),
        };
        assert_eq!(
            render_json(&listing, None),
            concat!(
                r#"{"schema":1,"query":null,"repositories":["#,
                r#"{"repository":"saltbox","prefix":"","cache":"rebuilt","tags":["pl\u001b[31mex"]},"#,
                r#"{"repository":"saltbox_mod","prefix":"mod-","cache":"missing","tags":[]}],"#,
                r#""errors":["error running and caching ansible tags for /opt/sandbox: boom"]}"#,
                "\n"
            )
        );
    }

    /// A query keeps each repository's matches, in the search's order (by
    /// distance, then tag), and records the query as given.
    #[test]
    fn json_with_a_query_keeps_the_matches() {
        let text = render_json(&listing(), Some("Sonar"));
        assert!(
            text.starts_with(r#"{"schema":1,"query":"Sonar","repositories":["#),
            "{text}"
        );
        assert!(
            text.contains(
                r#"{"repository":"saltbox","prefix":"","cache":"cached","tags":["sonarr"]}"#
            ),
            "{text}"
        );
        assert!(
            text.contains(r#"{"repository":"sandbox","prefix":"sandbox-","cache":"rebuilt","tags":["sonarr4k"]}"#),
            "{text}"
        );
        let none = render_json(&listing(), Some("zzzzzz"));
        assert!(none.contains(r#""tags":[]"#), "{none}");
    }

    /// Schema 1's keys, frozen: a field may be added (append it here), never
    /// renamed or removed, as scripts read them (spec, `--json`).
    #[test]
    fn json_schema_1_keeps_its_keys() {
        let mut listing = listing();
        listing.failures.push("x".into());
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&listing, Some("plex"))).expect("JSON");
        // serde_json's map sorts its keys: the order is the exact-string
        // tests'; this is the set.
        let keys = |value: &serde_json::Value| -> Vec<String> {
            value.as_object().expect("object").keys().cloned().collect()
        };
        assert_eq!(keys(&value), ["errors", "query", "repositories", "schema"]);
        assert_eq!(value["schema"], 1);
        assert_eq!(
            keys(&value["repositories"][0]),
            ["cache", "prefix", "repository", "tags"]
        );
    }
}
