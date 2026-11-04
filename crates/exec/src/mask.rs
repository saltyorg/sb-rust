//! Secrets out of text (spec: "Secrets in output"): every `secret`
//! argument and `secret_env` value, empty ones ignored, replaced by
//! cli-core's placeholder before anything is shown or logged. Matching is
//! aho-corasick's overlapping search
//! (https://docs.rs/aho-corasick/1.1.5/aho_corasick/struct.AhoCorasick.html#method.find_overlapping_iter),
//! so a secret inside another, or two overlapping in the text, leave no
//! part of either. Why not `AhoCorasick::replace_all`: it replaces
//! non-overlapping matches only, so the end of an overlapping secret
//! would show. Why not one `str::replace` per secret: the same, and a
//! shorter secret replaced first can cut a longer one apart.

use aho_corasick::AhoCorasick;
use sb_cli_core::log::WITHHELD;

/// The shortest line of a multi-line secret masked on its own.
const MIN_PIECE: usize = 4;

/// The secrets of one command, ready to take out of its text.
#[derive(Debug)]
pub(crate) struct Mask {
    longest: usize,
    search: Search,
}

#[derive(Debug)]
enum Search {
    /// No secrets: text passes unchanged.
    Nothing,
    Secrets(AhoCorasick),
    /// The searcher couldn't be built (aho-corasick's size limits): every
    /// text is withheld whole rather than shown unmasked.
    Unbuilt,
}

impl Mask {
    /// A mask for `secrets`; empty ones are ignored (they would match
    /// everywhere). A secret with a newline (a credential read from a file,
    /// a multi-line key) is masked whole and also line by line, or it would
    /// never match output split at newlines (stream's lines) or printed
    /// without its trailing newline. A line of it shorter than
    /// [`MIN_PIECE`] once trimmed (a JSON value's `{` and `}`) isn't masked
    /// on its own: it would turn every such character in the output into
    /// the placeholder, corrupting what callers read, and a fragment that
    /// short gives nothing of the secret away.
    pub(crate) fn new<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Self {
        let secrets: Vec<&str> = secrets
            .into_iter()
            .flat_map(|secret| {
                let lines = secret
                    .contains('\n')
                    .then(|| {
                        secret
                            .lines()
                            .map(str::trim)
                            .filter(|l| l.len() >= MIN_PIECE)
                    })
                    .into_iter()
                    .flatten();
                // Whole, without trailing newlines, so the output's own
                // line ends stay.
                std::iter::once(secret.trim_end_matches(['\n', '\r'])).chain(lines)
            })
            .filter(|s| !s.trim().is_empty())
            .collect();
        let longest = secrets.iter().map(|s| s.len()).max().unwrap_or(0);
        let search = if secrets.is_empty() {
            Search::Nothing
        } else {
            AhoCorasick::new(&secrets).map_or(Search::Unbuilt, Search::Secrets)
        };
        Self { longest, search }
    }

    /// The longest secret's length in bytes: how far a cut can reach into
    /// one (see the output caps).
    pub(crate) fn longest(&self) -> usize {
        self.longest
    }

    /// The `keep` part of `window` with every secret replaced by the
    /// placeholder: each stretch that matches overlap one placeholder,
    /// matches only next to each other one each; a secret crossing an edge
    /// of `keep` becomes the placeholder.
    /// For output cut at a cap: the window reaches [`Mask::longest`]
    /// bytes past the cut, so every secret touching the kept part is whole
    /// in it.
    pub(crate) fn bytes(&self, window: &[u8], keep: std::ops::Range<usize>) -> Vec<u8> {
        let searcher = match &self.search {
            Search::Nothing => return window[keep].to_vec(),
            Search::Unbuilt => return WITHHELD.as_bytes().to_vec(),
            Search::Secrets(searcher) => searcher,
        };
        let mut found: Vec<(usize, usize)> = searcher
            .find_overlapping_iter(window)
            .map(|m| (m.start(), m.end()))
            .collect();
        found.sort_unstable();
        let mut stretches: Vec<(usize, usize)> = Vec::new();
        for (start, end) in found {
            match stretches.last_mut() {
                Some((_, last)) if start < *last => *last = (*last).max(end),
                _ => stretches.push((start, end)),
            }
        }
        let mut masked = Vec::with_capacity(keep.len());
        let mut shown = keep.start;
        for (from, to) in stretches {
            if to <= keep.start {
                continue;
            }
            if from >= keep.end {
                break;
            }
            masked.extend_from_slice(&window[shown..from.max(shown)]);
            masked.extend_from_slice(WITHHELD.as_bytes());
            shown = to.min(keep.end);
        }
        masked.extend_from_slice(&window[shown..keep.end]);
        masked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Mask {
        /// `text` whole, secrets taken out.
        pub(crate) fn text(&self, text: &str) -> String {
            String::from_utf8_lossy(&self.bytes(text.as_bytes(), 0..text.len())).into_owned()
        }
    }

    #[test]
    fn a_secret_is_replaced_wherever_it_appears() {
        let mask = Mask::new(["hunter2"]);
        assert_eq!(
            mask.text("token hunter2, again hunter2."),
            "token [REDACTED], again [REDACTED]."
        );
    }

    #[test]
    fn text_without_secrets_is_unchanged() {
        assert_eq!(Mask::new(["hunter2"]).text("nothing here"), "nothing here");
        assert_eq!(Mask::new([]).text("nothing here"), "nothing here");
    }

    /// An empty secret would match between every two characters.
    #[test]
    fn empty_secrets_are_ignored() {
        let mask = Mask::new(["", "abc"]);
        assert_eq!(mask.text("xabcx"), "x[REDACTED]x");
        assert_eq!(mask.longest(), 3);
        assert_eq!(Mask::new([""]).longest(), 0);
    }

    /// One secret inside another: no part of the longer one is left.
    #[test]
    fn a_secret_inside_another_leaves_nothing_of_either() {
        let mask = Mask::new(["abc", "abcdef"]);
        assert_eq!(mask.text("-abcdef-"), "-[REDACTED]-");
        let mask = Mask::new(["abcdef", "cd"]);
        assert_eq!(mask.text("-abcdef-cd-"), "-[REDACTED]-[REDACTED]-");
    }

    /// Two secrets overlapping in the text: the whole stretch goes, so
    /// neither's end is left showing.
    #[test]
    fn overlapping_secrets_are_taken_out_together() {
        let mask = Mask::new(["xy", "yz"]);
        assert_eq!(mask.text("axyzb"), "a[REDACTED]b");
        // Next to each other, not overlapping: one placeholder each.
        assert_eq!(mask.text("xyyz"), "[REDACTED][REDACTED]");
    }

    #[test]
    fn multi_byte_text_around_a_secret_is_kept() {
        let mask = Mask::new(["sécret"]);
        assert_eq!(mask.text("✓ sécret ✗"), "✓ [REDACTED] ✗");
    }

    /// Only the kept part of a window is rendered; a secret crossing an
    /// edge of it becomes the placeholder, so no part of it shows.
    #[test]
    fn a_cut_window_shows_only_what_is_kept() {
        let mask = Mask::new(["secret"]);
        // Kept: the first 6 bytes of "ab secret cd"; the secret crosses
        // the cut, after "ab " and before "cret cd".
        assert_eq!(mask.bytes(b"ab secret cd", 0..6), b"ab [REDACTED]");
        // Kept: the last 6; the secret crosses the start.
        assert_eq!(mask.bytes(b"ab secret cd", 6..12), b"[REDACTED] cd");
        // A secret wholly outside the kept part isn't rendered at all.
        assert_eq!(mask.bytes(b"ab cd secret", 0..5), b"ab cd");
        assert_eq!(mask.bytes(b"secret ab cd", 7..12), b"ab cd");
        // Placeholders shorter than their secrets never pull in bytes from
        // past the kept part.
        let long = Mask::new(["a-very-long-secret-value"]);
        assert_eq!(
            long.bytes(b"a-very-long-secret-value tail", 0..25),
            b"[REDACTED] "
        );
    }

    #[test]
    fn without_secrets_a_window_is_cut_as_kept() {
        assert_eq!(Mask::new([]).bytes(b"abcdef", 2..4), b"cd");
    }

    /// A secret with a newline in it (a credential read from a file, a
    /// multi-line key) is masked line by line: output split at newlines,
    /// or printed without the trailing one, still hides every piece.
    #[test]
    fn a_secret_with_newlines_is_masked_piece_by_piece() {
        let mask = Mask::new(["tok-secret\n"]);
        assert_eq!(mask.text("tok-secret"), "[REDACTED]");
        assert_eq!(mask.text("a tok-secret\n"), "a [REDACTED]\n");
        let mask = Mask::new(["first-part\r\nsecond-part"]);
        assert_eq!(mask.text("first-part"), "[REDACTED]");
        assert_eq!(mask.text("second-part"), "[REDACTED]");
        // A secret of newlines only is empty once split: ignored.
        assert_eq!(Mask::new(["\n\n"]).text("text"), "text");
        // Short lines of a multi-line secret (JSON's braces) aren't masked
        // on their own, or they would corrupt every brace in the output;
        // the whole secret, and its longer lines, still are.
        let json = Mask::new(["{\n  \"key\": \"s3cr3t-not-real\"\n}\n"]);
        assert_eq!(json.text("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(json.text("\"key\": \"s3cr3t-not-real\""), "[REDACTED]");
        // A secret without newlines is masked whatever its length.
        assert_eq!(Mask::new(["ab"]).text("xaby"), "x[REDACTED]y");
        assert_eq!(Mask::new(["tok-secret\n"]).longest(), "tok-secret".len());
    }

    #[test]
    fn the_placeholder_is_cli_cores() {
        assert_eq!(Mask::new(["a"]).text("a"), WITHHELD);
    }
}
