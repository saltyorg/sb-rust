//! `sb install`'s tag validation (ansible spec, `sb install` step 5;
//! module build M33): each requested Saltbox and Sandbox tag checked
//! against the cached tag lists, sb-go's four suggestions for the ones
//! that aren't there (cmd/install.go, `validateAndSuggest`), and their
//! layout under `Tag validation found some issues:` (`formatSuggestions`).

use std::fmt::Write as _;

use sb_cli_core::exit::escaped;
use sb_cli_core::theme::{Role, Theme};
use sb_term_ui::Runner;

use crate::cache::{CacheError, LazyRunner, TagCache};
use crate::{Ansible, Repos, Routed};

/// The most edits a typo may be from the tag it suggests (sb-go's
/// threshold).
const MOST_EDITS: usize = 2;

/// The two repositories whose tags are validated (mod tags never are).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Saltbox,
    Sandbox,
}

impl Side {
    /// The prefix that routes a tag here (spec, step 4).
    fn prefix(self) -> &'static str {
        match self {
            Self::Saltbox => "",
            Self::Sandbox => "sandbox-",
        }
    }

    /// Its name in the suggestions.
    fn name(self) -> &'static str {
        match self {
            Self::Saltbox => "Saltbox",
            Self::Sandbox => "Sandbox",
        }
    }

    /// The other one.
    fn other(self) -> Self {
        match self {
            Self::Saltbox => Self::Sandbox,
            Self::Sandbox => Self::Saltbox,
        }
    }
}

/// What sb suggests for a requested tag its repository doesn't have, in
/// sb-go's order of preference (`suggestionType`), each with the tag it
/// suggests, the prefix that routes it included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// The other repository has it as typed: `Try: sandbox-X (from
    /// Sandbox)`.
    Elsewhere(String),
    /// Its own repository has one within two edits: `Did you mean: Y`.
    Typo(String),
    /// The other repository has one within two edits: `Did you mean:
    /// sandbox-Y (from Sandbox)`.
    TypoElsewhere(String),
    /// Neither has anything close: `Add: --no-cache if developing your
    /// own role`.
    NotFound,
}

/// One requested tag that isn't in its repository's tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The tag as typed, its prefix included (`sandbox-x`).
    pub typed: String,
    /// The repository it was routed to.
    pub repo: Side,
    /// What sb suggests.
    pub hint: Hint,
}

/// Saltbox's and Sandbox's tags, as validation reads them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Known {
    /// Saltbox's tags, never empty.
    pub saltbox: Vec<String>,
    /// Sandbox's tags; none when they couldn't be had.
    pub sandbox: Vec<String>,
}

/// Saltbox's tags couldn't be had, or there are none: nothing can be
/// validated (sb-go's text). A failed rebuild has shown why in its step;
/// a HEAD that can't be read is the cause.
#[derive(Debug, thiserror::Error)]
#[error("saltbox install appears broken: tags cache missing or empty")]
pub struct SaltboxBroken {
    #[source]
    cause: Option<sb_repo::RepoError>,
}

/// Saltbox's and then Sandbox's tags through the cache, each rebuilt in a
/// top-level step of `runner` when stale (spec, step 5: both loaded even
/// when only one repository has requested tags). Sandbox's failure counts
/// as no tags, and is logged: a host without Sandbox has none.
///
/// # Errors
///
/// [`SaltboxBroken`] when Saltbox's checkout or tags couldn't be had, or
/// it has none.
pub async fn known_tags<F: FnOnce() -> Runner>(
    cache: &mut TagCache,
    ansible: &Ansible,
    repos: &Repos,
    runner: &mut LazyRunner<F>,
) -> Result<Known, SaltboxBroken> {
    let broken = |cause| SaltboxBroken { cause };
    let checkout =
        sb_repo::Repo::open("Saltbox", repos.saltbox.dir()).map_err(|e| broken(Some(e)))?;
    let saltbox = match cache
        .tags_on(ansible, &repos.saltbox, &checkout, runner)
        .await
    {
        Ok(listed) => listed.tags,
        Err(CacheError::Head(err)) => return Err(broken(Some(err))),
        // The step has shown why.
        Err(CacheError::Step(_)) => return Err(broken(None)),
    };
    if saltbox.is_empty() {
        return Err(broken(None));
    }
    let sandbox = match sb_repo::Repo::open("Sandbox", repos.sandbox.dir()) {
        Ok(checkout) => cache
            .tags_on(ansible, &repos.sandbox, &checkout, runner)
            .await
            .map(|listed| listed.tags)
            .map_err(anyhow::Error::from),
        Err(err) => Err(err.into()),
    };
    let sandbox = sandbox.unwrap_or_else(|err| {
        tracing::info!(error = %format!("{err:#}"), "Sandbox tags unavailable; counted as none");
        Vec::new()
    });
    Ok(Known { saltbox, sandbox })
}

/// Every routed Saltbox and Sandbox tag checked against `known`: Saltbox's
/// suggestions, then Sandbox's, each sorted by the typed tag, as sb-go
/// collects them (one `validateAndSuggest` per repository). Mod tags are
/// never validated. Empty when every tag is there.
#[must_use]
pub fn validate(routed: &Routed, known: &Known) -> Vec<Suggestion> {
    let mut all = check(
        &routed.saltbox,
        Side::Saltbox,
        &known.saltbox,
        &known.sandbox,
    );
    all.extend(check(
        &routed.sandbox,
        Side::Sandbox,
        &known.sandbox,
        &known.saltbox,
    ));
    all
}

/// `requested` (prefixes stripped) checked against `own`, `repo`'s tags,
/// and `other`, the other repository's, in sb-go's order: there, as
/// typed in the other, a typo of its own, a typo of the other's, nowhere.
/// Case counts, as in sb-go (`Plex` is a typo of `plex`).
fn check(requested: &[String], repo: Side, own: &[String], other: &[String]) -> Vec<Suggestion> {
    let there = repo.other().prefix();
    let mut suggestions: Vec<Suggestion> = requested
        .iter()
        .filter(|tag| !own.contains(tag))
        .map(|tag| {
            let hint = if other.contains(tag) {
                Hint::Elsewhere(format!("{there}{tag}"))
            } else if let Some(near) = closest(tag, own) {
                Hint::Typo(format!("{}{near}", repo.prefix()))
            } else if let Some(near) = closest(tag, other) {
                Hint::TypoElsewhere(format!("{there}{near}"))
            } else {
                Hint::NotFound
            };
            Suggestion {
                typed: format!("{}{tag}", repo.prefix()),
                repo,
                hint,
            }
        })
        .collect();
    // Stable, so equal typed tags (duplicates) keep their order; sb-go's
    // `sort.Slice` isn't, but equal ones are the same suggestion.
    suggestions.sort_by(|a, b| a.typed.cmp(&b.typed));
    suggestions
}

/// The first of `tags` with the fewest edits from `tag`, if at most
/// [`MOST_EDITS`]: a later tag replaces it only when strictly closer
/// (sb-go's loop). Edits are counted per Unicode scalar, as sb-go's
/// agnivade/levenshtein counts runes
/// (<https://docs.rs/strsim/0.11.1/strsim/fn.levenshtein.html>).
fn closest<'a>(tag: &str, tags: &'a [String]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in tags {
        let edits = strsim::levenshtein(tag, candidate);
        if edits <= MOST_EDITS && best.is_none_or(|(fewest, _)| edits < fewest) {
            best = Some((edits, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// The suggestions as one error, sb-go's layout in sb's error block's
/// shape: `✗` in the problem colour and the heading in the warning
/// colour, then two lines for each suggestion, indented as the block's
/// lines are, with a blank line before each (sb-go 514ecd2, 5a8135e),
/// then the block's `details:` line with the run log's `pointer`, if any.
/// sb-go's colours by palette role: the labels (`Tag:`, `Try:`, `Did you
/// mean:`, `Add:`) muted, as the block's labels; the typed tag in the
/// problem colour and the suggestion in the ok colour, both bold;
/// `--no-cache` as code. Bold only with colour, as cli-core's help: the
/// plain theme writes no styling. Every tag is escaped (cli-core's
/// `escaped`): a typed tag is anyone's text. Why not cli-core's
/// `error_block`: it escapes each line whole, so it can't carry the
/// styled tags or the blank lines between suggestions.
#[must_use]
pub fn render(suggestions: &[Suggestion], theme: &Theme, pointer: Option<&str>) -> String {
    let bold = |role| match theme.color(role) {
        Some(_) => theme.style(role).bold(),
        None => theme.style(role),
    };
    let (mark, heading) = (theme.style(Role::Problem), theme.style(Role::Warn));
    let (label, typed, suggested) = (
        theme.style(Role::Muted),
        bold(Role::Problem),
        bold(Role::Ok),
    );
    let code = theme.style(Role::Code);
    let mut out =
        format!("{mark}✗{mark:#} {heading}Tag validation found some issues:{heading:#}\n");
    for suggestion in suggestions {
        let tag = escaped(&suggestion.typed);
        let (here, from) = (suggestion.repo.name(), suggestion.repo.other().name());
        let missing_from = match suggestion.hint {
            Hint::NotFound => "Saltbox or Sandbox",
            _ => here,
        };
        let _ = writeln!(
            out,
            "\n  {label}Tag:{label:#} {typed}{tag}{typed:#} not present in {missing_from}"
        );
        let shown = |near: &str| format!("{suggested}{}{suggested:#}", escaped(near));
        let _ = match &suggestion.hint {
            Hint::Elsewhere(near) => {
                writeln!(out, "  {label}Try:{label:#} {} (from {from})", shown(near))
            }
            Hint::Typo(near) => writeln!(out, "  {label}Did you mean:{label:#} {}", shown(near)),
            Hint::TypoElsewhere(near) => writeln!(
                out,
                "  {label}Did you mean:{label:#} {} (from {from})",
                shown(near)
            ),
            Hint::NotFound => writeln!(
                out,
                "  {label}Add:{label:#} {code}--no-cache{code:#} if developing your own role"
            ),
        };
    }
    if let Some(pointer) = pointer {
        let _ = writeln!(out, "\n  {label}details:{label:#} {}", escaped(pointer));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    fn known(saltbox: &[&str], sandbox: &[&str]) -> Known {
        Known {
            saltbox: strings(saltbox),
            sandbox: strings(sandbox),
        }
    }

    fn suggestions(tags: &[&str], known: &Known) -> Vec<(String, Hint)> {
        validate(&Routed::route(&strings(tags)), known)
            .into_iter()
            .map(|s| (s.typed, s.hint))
            .collect()
    }

    fn one(typed: &str, hint: Hint) -> (String, Hint) {
        (typed.to_string(), hint)
    }

    const SALTBOX: [&str; 6] = ["plex", "sonarr", "radarr", "traefik", "jellyfin", "emby"];
    const SANDBOX: [&str; 4] = ["tautulli", "bazarr", "autobrr", "plex-sandbox"];

    /// A tag its repository has needs nothing; mod tags are never
    /// checked.
    #[test]
    fn present_tags_and_mod_tags_need_nothing() {
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["plex", "sandbox-tautulli", "mod-anything"], &known),
            []
        );
    }

    /// Typed as the other repository has it: `Try:` with the other's
    /// prefix, both ways.
    #[test]
    fn a_tag_of_the_other_repo_suggests_its_prefix() {
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["tautulli", "sandbox-sonarr"], &known),
            [
                one("tautulli", Hint::Elsewhere("sandbox-tautulli".into())),
                one("sandbox-sonarr", Hint::Elsewhere("sonarr".into())),
            ]
        );
    }

    /// Up to two edits from a tag of its own repository; three is too
    /// many; the other repository's exact match wins over its own typo.
    #[test]
    fn a_typo_suggests_its_own_repos_tag() {
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["plx", "sonar", "sandbox-tautuli", "traefikkk"], &known),
            [
                one("plx", Hint::Typo("plex".into())),
                one("sonar", Hint::Typo("sonarr".into())),
                one("traefikkk", Hint::Typo("traefik".into())),
                one("sandbox-tautuli", Hint::Typo("sandbox-tautulli".into())),
            ]
        );
        assert_eq!(
            suggestions(&["traefikxyz"], &known),
            [one("traefikxyz", Hint::NotFound)]
        );
        // Sandbox has `bazarr` as typed and Saltbox has `bazar`, one edit
        // away: the exact match in the other repository wins.
        let both = Known {
            saltbox: strings(&["plex", "bazar"]),
            sandbox: strings(&["bazarr"]),
        };
        assert_eq!(
            suggestions(&["bazarr"], &both),
            [one("bazarr", Hint::Elsewhere("sandbox-bazarr".into()))]
        );
    }

    /// A typo of the other repository's tag, when its own has none close.
    #[test]
    fn a_typo_of_the_other_repos_tag_suggests_it() {
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["autobr", "sandbox-sonar"], &known),
            [
                one("autobr", Hint::TypoElsewhere("sandbox-autobrr".into())),
                one("sandbox-sonar", Hint::TypoElsewhere("sonarr".into())),
            ]
        );
    }

    /// Nothing close anywhere.
    #[test]
    fn a_tag_close_to_nothing_is_not_found() {
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["kubernetes", "sandbox-zzz"], &known),
            [
                one("kubernetes", Hint::NotFound),
                one("sandbox-zzz", Hint::NotFound),
            ]
        );
    }

    /// A tie goes to the first lowest in cache order; a later tag wins
    /// only when strictly closer.
    #[test]
    fn ties_go_to_the_first_in_cache_order() {
        let tags = strings(&["abcd", "abce", "abc"]);
        // "abcx": one edit from each of the first two, one from "abc".
        assert_eq!(closest("abcx", &tags), Some("abcd"));
        let tags = strings(&["axxd", "abce", "abcf"]);
        // Two edits from the first, one from the second and third.
        assert_eq!(closest("abcd", &tags), Some("abce"));
        assert_eq!(closest("abcd", &strings(&["wxyz"])), None);
        // Exactly two edits is close enough.
        assert_eq!(closest("ab", &strings(&["abcd"])), Some("abcd"));
        assert_eq!(closest("a", &strings(&["abcd"])), None);
        assert_eq!(closest("x", &[]), None);
    }

    /// Edits count Unicode scalars, as sb-go's runes: `é` for `e` is one.
    #[test]
    fn edits_count_unicode_scalars() {
        assert_eq!(closest("plé", &strings(&["plex"])), Some("plex"));
        assert_eq!(closest("名前xy", &strings(&["名前"])), Some("名前"));
        assert_eq!(closest("名前xyz", &strings(&["名前"])), None);
    }

    /// Saltbox's suggestions first, then Sandbox's, each sorted by the
    /// typed tag; duplicates kept.
    #[test]
    fn suggestions_sort_by_typed_tag_within_each_repo() {
        let known = known(&SALTBOX, &SANDBOX);
        let typed: Vec<String> = suggestions(
            &["zzz", "sandbox-b1", "aaa", "sandbox-a1", "mmm", "aaa"],
            &known,
        )
        .into_iter()
        .map(|(typed, _)| typed)
        .collect();
        assert_eq!(
            typed,
            strings(&["aaa", "aaa", "mmm", "zzz", "sandbox-a1", "sandbox-b1"])
        );
    }

    /// With no Sandbox tags (they couldn't be had), a Sandbox tag that
    /// Saltbox has is suggested from Saltbox, and Saltbox tags still get
    /// their own typos.
    #[test]
    fn no_sandbox_tags_still_validates() {
        let known = known(&SALTBOX, &[]);
        assert_eq!(
            suggestions(&["sandbox-plex", "sandbox-sonar", "plx"], &known),
            [
                one("plx", Hint::Typo("plex".into())),
                one("sandbox-plex", Hint::Elsewhere("plex".into())),
                one("sandbox-sonar", Hint::TypoElsewhere("sonarr".into())),
            ]
        );
    }

    /// A typo of its own repository's tag wins over a closer one of the
    /// other's (sb-go checks its own first); case counts, as sb-go's
    /// `TestLevenshteinDistance` has it (`Plex` is one edit from `plex`).
    #[test]
    fn its_own_typo_wins_and_case_counts() {
        let close_to_both = Known {
            saltbox: strings(&["abcdef"]),
            sandbox: strings(&["abcdxy"]),
        };
        // Two edits from Saltbox's, one from Sandbox's.
        assert_eq!(
            suggestions(&["abcdxz"], &close_to_both),
            [one("abcdxz", Hint::Typo("abcdef".into()))]
        );
        let known = known(&SALTBOX, &SANDBOX);
        assert_eq!(
            suggestions(&["Plex"], &known),
            [one("Plex", Hint::Typo("plex".into()))]
        );
    }

    /// sb-go's layout and texts, plainly: the heading, then each
    /// suggestion's two lines after a blank line.
    #[test]
    fn the_layout_is_sb_gos_in_sbs_block() {
        let known = known(&SALTBOX, &SANDBOX);
        let all = validate(
            &Routed::route(&strings(&[
                "plx",
                "tautulli",
                "autobr",
                "nothing-here",
                "sandbox-plex",
            ])),
            &known,
        );
        assert_eq!(
            render(&all, &Theme::plain(), None),
            "✗ Tag validation found some issues:\n\
             \n  Tag: autobr not present in Saltbox\
             \n  Did you mean: sandbox-autobrr (from Sandbox)\n\
             \n  Tag: nothing-here not present in Saltbox or Sandbox\
             \n  Add: --no-cache if developing your own role\n\
             \n  Tag: plx not present in Saltbox\
             \n  Did you mean: plex\n\
             \n  Tag: tautulli not present in Saltbox\
             \n  Try: sandbox-tautulli (from Sandbox)\n\
             \n  Tag: sandbox-plex not present in Sandbox\
             \n  Try: plex (from Saltbox)\n"
        );
    }

    /// Colour from the palette's roles, bold only with colour; a typed
    /// tag's control characters escaped, so it can't forge a line.
    #[test]
    fn the_layout_is_coloured_by_role_and_escaped() {
        let theme = Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::Ansi,
        );
        let all = [
            Suggestion {
                typed: "pl\x1b[2Jx\n✗ forged".into(),
                repo: Side::Saltbox,
                hint: Hint::Typo("plex".into()),
            },
            Suggestion {
                typed: "zzz".into(),
                repo: Side::Sandbox,
                hint: Hint::NotFound,
            },
        ];
        let text = render(&all, &theme, Some("/var/log/sb/sb.log (run 7)"));
        let style = |role| theme.style(role);
        let bold = |role: Role| theme.style(role).bold();
        let (problem, warn, muted) = (style(Role::Problem), style(Role::Warn), style(Role::Muted));
        let (typed, ok, code) = (bold(Role::Problem), bold(Role::Ok), style(Role::Code));
        assert!(text.starts_with(&format!(
            "{problem}✗{problem:#} {warn}Tag validation found some issues:{warn:#}\n"
        )));
        assert!(text.contains(&format!(
            "  {muted}Tag:{muted:#} {typed}pl\\u{{1b}}[2Jx\\n✗ forged{typed:#} not present in Saltbox\n"
        )), "{text:?}");
        assert!(text.contains(&format!("  {muted}Did you mean:{muted:#} {ok}plex{ok:#}\n")));
        assert!(text.contains(&format!(
            "  {muted}Add:{muted:#} {code}--no-cache{code:#} if developing your own role\n"
        )));
        assert!(
            text.ends_with(&format!(
                "\n\n  {muted}details:{muted:#} /var/log/sb/sb.log (run 7)\n"
            )),
            "{text:?}"
        );
        assert_eq!(text.lines().count(), 9, "{text:?}");
    }
}
