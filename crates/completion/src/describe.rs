//! bash's descriptions (spec, "Descriptions in bash", decision 13):
//! `name  (description)` entries for a listing, as sb-go's cobra lists
//! show them, written by sb's bash reply (the module `reply`).
//!
//! Why not clap's bash adapter: it writes only each candidate's value
//! (`clap_complete` 4.6.11, `env/shells.rs`, `Bash::write_complete`) and
//! ignores the `COMP_TYPE` it parses; `clap_complete` has no option for
//! bash descriptions. Why not cobra's rule (describe on every call but
//! menu-complete and insert-completions,
//! <https://github.com/spf13/cobra/blob/v1.10.2/bash_completionsV2.go>):
//! an editor that calls completion with `COMP_TYPE` 9 and inserts the
//! reply (ble.sh) put the descriptions into the line.

use clap_complete::engine::CompletionCandidate;
use console::{Alignment, measure_text_width, pad_str};

/// bash's `COMP_TYPE` values that list rather than insert: `?` (63, "for
/// listing completions after successive tabs"), `!` (33, "for listing
/// alternatives on partial word completion", `show-all-if-ambiguous`) and
/// `@` (64, "to list completions if the word is not unmodified",
/// `show-all-if-unmodified`); TAB (9) and `%` (37, menu completion)
/// insert
/// (<https://www.gnu.org/software/bash/manual/html_node/Bash-Variables.html#index-COMP_005fTYPE>).
const LISTING: [&str; 3] = ["63", "33", "64"];

/// The variable sb's bash stub passes `$COLUMNS` in (`quiet_bash`'s
/// sibling patch, `bash_columns`): sb's own, not one of clap's.
pub(crate) const COLUMNS_VAR: &str = "_SB_COMPLETE_COLUMNS";

/// The least room for a description: below it, the names alone (the
/// maintainer, 2026-11-20).
const MIN_DESCRIPTION: usize = 20;

/// The columns a described entry adds to its padded name: two spaces and
/// the parentheses.
const DECORATION: usize = 4;

/// Whether bash's `comp_type` (the stub's `_CLAP_COMPLETE_COMP_TYPE`) is
/// a listing call.
pub(crate) fn listing(comp_type: Option<&str>) -> bool {
    comp_type.is_some_and(|comp_type| LISTING.contains(&comp_type))
}

/// The entries for `candidates` listed in a terminal `columns` wide, each
/// `name  (description)` with the names padded to the longest and the
/// description cut with `…` so the line fits `columns` less one (a line
/// as wide as the terminal can wrap where wrapping isn't deferred); or
/// `None` for the names alone: one candidate, one without help (a tag),
/// or less than [`MIN_DESCRIPTION`] columns left for descriptions.
/// Widths are display columns, as the terminal draws them, through
/// console (`measure_text_width`, `pad_str`,
/// <https://docs.rs/console/0.16.6/console/fn.pad_str.html>, as term-ui's
/// tree pads); the cut is term-ui's [`sb_term_ui::fit`] (console's
/// `truncate_str`).
pub(crate) fn described(candidates: &[CompletionCandidate], columns: usize) -> Option<Vec<String>> {
    if candidates.len() < 2 {
        return None;
    }
    let mut entries = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let help = candidate.get_help()?.to_string();
        let help = help.lines().next().unwrap_or_default().trim().to_owned();
        if help.is_empty() {
            return None;
        }
        entries.push((candidate.get_value().to_string_lossy().into_owned(), help));
    }
    let longest = entries
        .iter()
        .map(|(name, _)| measure_text_width(name))
        .max()
        .unwrap_or_default();
    let room = columns
        .saturating_sub(1)
        .saturating_sub(longest + DECORATION);
    if room < MIN_DESCRIPTION {
        return None;
    }
    let lines = entries
        .iter()
        .map(|(name, help)| {
            let name = pad_str(name, longest, Alignment::Left, None);
            format!("{name}  ({})", sb_term_ui::fit(help, room))
        })
        .collect();
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(value: &str, help: Option<&str>) -> CompletionCandidate {
        CompletionCandidate::new(value).help(help.map(|help| help.to_owned().into()))
    }

    #[test]
    fn only_the_listing_types_list() {
        for comp_type in ["63", "33", "64"] {
            assert!(listing(Some(comp_type)), "{comp_type}");
        }
        for comp_type in ["9", "37", "42", "", "063", "x"] {
            assert!(!listing(Some(comp_type)), "{comp_type}");
        }
        assert!(!listing(None));
    }

    #[test]
    fn names_are_padded_and_descriptions_kept_when_they_fit() {
        let candidates = [
            candidate("start", Some("Start it")),
            candidate("stop", Some("Stop it")),
        ];
        assert_eq!(
            described(&candidates, 80).expect("described"),
            ["start  (Start it)", "stop   (Stop it)"]
        );
    }

    /// The line fits the width less one: the room left after the longest
    /// name and the four columns.
    #[test]
    fn a_long_description_is_cut_with_an_ellipsis() {
        let candidates = [
            candidate("ab", Some(&"x".repeat(40))),
            candidate("c", Some("short")),
        ];
        // 31 columns: 30 for the line, 2 + 4 for the name and decoration,
        // 24 for the description.
        let lines = described(&candidates, 31).expect("described");
        assert_eq!(
            lines,
            [
                format!("ab  ({}…)", "x".repeat(23)),
                "c   (short)".to_owned()
            ]
        );
        assert_eq!(measure_text_width(&lines[0]), 30);
        // Exactly the room: kept whole.
        let exact = [
            candidate("ab", Some(&"x".repeat(24))),
            candidate("c", Some("y")),
        ];
        assert_eq!(
            described(&exact, 31).expect("described")[0],
            format!("ab  ({})", "x".repeat(24))
        );
    }

    /// Fewer than 20 columns for descriptions: the names alone.
    #[test]
    fn a_narrow_terminal_gets_the_names() {
        let candidates = [candidate("ab", Some("one")), candidate("c", Some("two"))];
        // 2 + 4 + 20 + 1 = 27 columns is the narrowest that describes.
        assert!(described(&candidates, 27).is_some());
        assert!(described(&candidates, 26).is_none());
        assert!(described(&candidates, 0).is_none());
    }

    #[test]
    fn one_candidate_or_one_without_help_gets_the_names() {
        assert!(described(&[candidate("update", Some("Update"))], 80).is_none());
        assert!(described(&[], 80).is_none());
        let mixed = [
            candidate("--no-cache", Some("Skip")),
            candidate("plex", None),
        ];
        assert!(described(&mixed, 80).is_none());
        let blank = [
            candidate("a", Some("A")),
            candidate("b", Some("  \nsecond")),
        ];
        assert!(described(&blank, 80).is_none());
    }

    /// Only the help's first line, trimmed; widths in display columns.
    #[test]
    fn first_lines_and_wide_characters() {
        let candidates = [candidate("a", Some(" one\ntwo")), candidate("b", Some("ü"))];
        assert_eq!(
            described(&candidates, 80).expect("described"),
            ["a  (one)", "b  (ü)"]
        );
        // A double-width character is two columns: it is cut whole.
        let wide = "界".repeat(30);
        let candidates = [candidate("a", Some(&wide)), candidate("b", Some("b"))];
        let lines = described(&candidates, 31).expect("described");
        // 30 - 1 - 4 = 25 columns: 12 characters (24 columns) and `…`.
        assert_eq!(lines[0], format!("a  ({}…)", "界".repeat(12)));
        assert!(measure_text_width(&lines[0]) <= 30, "{}", lines[0]);
        // A name wider in columns than in bytes pads by columns.
        let candidates = [candidate("界", Some("x")), candidate("abc", Some("y"))];
        assert_eq!(
            described(&candidates, 80).expect("described"),
            ["界   (x)", "abc  (y)"]
        );
    }
}
