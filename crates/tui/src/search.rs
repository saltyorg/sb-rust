//! The viewer's search (tui spec, "Search"; decided.md A-Q5): smart-case
//! substring matches over the plain text the screen shows, and their
//! highlight across a line's styled spans.
//!
//! Why not `regex` (with `(?i)` for the caseless case): the spec leaves
//! regex search for later, as a toggle, and rejects the crate for now
//! (Tech Stack, "Rejected"). Caseless matching is std's `char::to_lowercase`
//! (<https://doc.rust-lang.org/std/primitive.char.html#method.to_lowercase>),
//! with each lowered byte mapped back to the character it came from, so a
//! match is a range of the text as shown even where lowering changes a
//! character's length.

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span};

/// Smart case (decided.md A-Q5): a query with an upper-case letter is
/// matched exactly, any other ignoring case. `plain` is what the screen
/// shows (`DisplayText` with colour off). The matches are byte ranges of
/// `plain`, left to right, not overlapping; an empty query has none.
pub(crate) fn matches(plain: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    if query.chars().any(char::is_uppercase) {
        plain
            .match_indices(query)
            .map(|(at, m)| at..at + m.len())
            .collect()
    } else {
        caseless_matches(plain, query)
    }
}

/// `query` in `plain` ignoring case. ASCII text, most log lines, lowers
/// byte for byte (std's `str::to_ascii_lowercase`, which is what
/// `char::to_lowercase` gives an ASCII character), so a match in its
/// lowered copy is the same range of `plain` and no map is built: the
/// map cost a whole-log search a third of its matching (perf round 3).
/// Other text takes [`mapped_matches`].
fn caseless_matches(plain: &str, query: &str) -> Vec<Range<usize>> {
    if !plain.is_ascii() {
        return mapped_matches(plain, query);
    }
    let query = query.to_lowercase();
    plain
        .to_ascii_lowercase()
        .match_indices(&query)
        .map(|(at, m)| at..at + m.len())
        .collect()
}

/// `query` in `plain` ignoring case: `plain` lowered with, for each
/// lowered byte, where the character it came from starts in `plain`.
fn mapped_matches(plain: &str, query: &str) -> Vec<Range<usize>> {
    let mut lowered = String::with_capacity(plain.len());
    let mut from: Vec<usize> = Vec::with_capacity(plain.len());
    for (at, c) in plain.char_indices() {
        for lower in c.to_lowercase() {
            lowered.push(lower);
            from.resize(lowered.len(), at);
        }
    }
    let end_of = |at: usize| at + plain[at..].chars().next().map_or(0, char::len_utf8);
    let query = query.to_lowercase();
    lowered
        .match_indices(&query)
        .map(|(at, m)| from[at]..end_of(from[at + m.len() - 1]))
        .collect()
}

/// `line` with every match of `query` in its text patched with `hit`
/// (`Format::hit`): spans are split at the matches' edges, so a match
/// across styled spans is highlighted whole and each piece keeps its own
/// style beneath the hit's. Why not ratatui's `Line::patch_style` or
/// `Span::patch_style`
/// (<https://docs.rs/ratatui/0.30.2/ratatui/text/struct.Span.html#method.patch_style>):
/// they patch a whole line or span; ratatui has no styling by range, so
/// the spans are split here first.
pub(crate) fn highlight(line: &mut TextLine<'static>, query: &str, hit: Style) {
    let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let found = matches(&plain, query);
    if found.is_empty() {
        return;
    }
    // The matches are in order and don't overlap: one pass over the
    // spans and the matches together.
    let mut spans = Vec::with_capacity(line.spans.len() + 2 * found.len());
    let mut next = found.iter().peekable();
    let mut start = 0;
    for span in std::mem::take(&mut line.spans) {
        let end = start + span.content.len();
        let mut from = start;
        while from < end {
            while next.peek().is_some_and(|m| m.end <= from) {
                next.next();
            }
            let (to, inside) = match next.peek() {
                Some(m) if m.start <= from => (m.end.min(end), true),
                Some(m) => (m.start.min(end), false),
                None => (end, false),
            };
            let style = if inside {
                span.style.patch(hit)
            } else {
                span.style
            };
            spans.push(Span::styled(
                span.content[from - start..to - start].to_owned(),
                style,
            ));
            from = to;
        }
        start = end;
    }
    line.spans = spans;
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;

    fn found<'a>(plain: &'a str, query: &str) -> Vec<&'a str> {
        matches(plain, query)
            .into_iter()
            .map(|m| &plain[m])
            .collect()
    }

    #[test]
    fn lower_case_matches_any_case_and_capitals_match_exactly() {
        let plain = "Error: error ERROR eRRor";
        assert_eq!(found(plain, "error"), ["Error", "error", "ERROR", "eRRor"]);
        assert_eq!(found(plain, "Error"), ["Error"]);
        assert_eq!(found(plain, "ERROR"), ["ERROR"]);
        assert_eq!(found(plain, "rr"), ["rr", "rr", "RR", "RR"]);
    }

    #[test]
    fn several_matches_in_a_line_and_none_overlapping() {
        assert_eq!(matches("aaaa", "aa"), [0..2, 2..4]);
        assert_eq!(matches("a.b.c", "."), [1..2, 3..4]);
        assert_eq!(matches("abc", ""), Vec::<Range<usize>>::new());
        assert_eq!(matches("abc", "x"), Vec::<Range<usize>>::new());
    }

    /// Lowering can change a character's length (`İ` lowers to `i̇`, two
    /// characters; `K`, the Kelvin sign, to one byte): the ranges are of
    /// the text as shown.
    #[test]
    fn a_caseless_match_is_a_range_of_the_shown_text() {
        let plain = "xİy KELVIN \u{212A}m";
        assert_eq!(found(plain, "i̇y"), ["İy"]);
        assert_eq!(found(plain, "km"), ["\u{212A}m"]);
        assert_eq!(found(plain, "kelvin"), ["KELVIN"]);
        // A query that starts inside a lowered character's expansion
        // takes the whole character.
        assert_eq!(found("İ", "\u{307}"), ["İ"]);
    }

    /// ASCII text matches as the mapped path matches it, whatever the
    /// query: one that lowers to ASCII (the Kelvin sign to `k`) finds the
    /// same ranges, one that doesn't finds none.
    #[test]
    fn ascii_text_matches_as_the_mapped_path_does() {
        let texts = [
            "",
            "plain lower case",
            "Error: error ERROR eRRor",
            "GET /api/v1/Items?Page=2 HTTP/1.1 200 \t KM km Km",
            "aaaa",
        ];
        let queries = [
            "e",
            "error",
            "rr",
            "km",
            "\u{212A}m",
            "i\u{307}",
            "aa",
            "/api",
            "x",
            "é",
        ];
        for plain in texts {
            for query in queries {
                assert_eq!(
                    caseless_matches(plain, query),
                    mapped_matches(plain, query),
                    "{plain:?} {query:?}"
                );
            }
        }
        // The Kelvin sign is upper case, so `matches` takes it exactly;
        // ignoring case it lowers to `k`.
        assert_eq!(caseless_matches("KM km", "\u{212A}m"), [0..2, 3..5]);
        assert_eq!(found("Café CAFÉ", "café"), ["Café", "CAFÉ"]);
    }

    fn hit() -> Style {
        Style::new().bg(Color::Yellow)
    }

    fn pieces(line: &TextLine<'_>) -> Vec<(String, Style)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    /// A match across a span boundary is highlighted whole: each side of
    /// it patched, its own colour kept beneath.
    #[test]
    fn a_match_across_styled_spans_highlights_whole() {
        let green = Style::new().fg(Color::Green);
        let mut line = TextLine::from(vec![
            Span::styled("[Info]", green),
            Span::raw(" Housekeeping: done"),
        ]);
        highlight(&mut line, "info] house", hit());
        assert_eq!(
            pieces(&line),
            [
                ("[".into(), green),
                ("Info]".into(), green.patch(hit())),
                (" House".into(), hit()),
                ("keeping: done".into(), Style::new()),
            ]
        );
    }

    #[test]
    fn every_match_is_highlighted_and_nothing_else_changes() {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut line = TextLine::from(vec![Span::styled("ab ab", bold), Span::raw(" ab")]);
        highlight(&mut line, "ab", hit());
        assert_eq!(
            pieces(&line),
            [
                ("ab".into(), bold.patch(hit())),
                (" ".into(), bold),
                ("ab".into(), bold.patch(hit())),
                (" ".into(), Style::new()),
                ("ab".into(), hit()),
            ]
        );
        let mut untouched = TextLine::from(vec![Span::raw("nothing here")]);
        highlight(&mut untouched, "zz", hit());
        assert_eq!(pieces(&untouched), [("nothing here".into(), Style::new())]);
    }
}
