//! The key/value writer (motd spec, Command flow step 7; sb-go's
//! `displayMotd`, `cmd/motd.go:202-339`): empty values hidden, keys padded
//! to the longest shown key plus 2 in the label colour, continuation lines
//! indented to the value column, one blank line at the end.
//!
//! Why not tabled or term-ui's `Table`: sb-go pads only the key and
//! writes each value line as it is, colour codes included; a table gives
//! every cell its column's width and its own separators, so the bytes
//! would no longer be sb-go's.

/// The lines for `results` (keys and values, in display order), keys
/// painted by `label`.
#[must_use]
pub fn render(results: &[(&str, String)], label: impl Fn(&str) -> String) -> String {
    let shown: Vec<(&str, &str)> = results
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let width = shown.iter().map(|(key, _)| key.len()).max().unwrap_or(0) + 2;
    let mut out = String::new();
    for (key, value) in shown {
        let mut lines = value.split('\n');
        let first = lines.next().unwrap_or_default();
        out.push_str(&label(key));
        out.push_str(&" ".repeat(width - key.len()));
        out.push_str(first);
        out.push('\n');
        for line in lines {
            out.push_str(&" ".repeat(width));
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Section;

    fn plain(key: &str) -> String {
        key.to_string()
    }

    #[test]
    fn keys_are_padded_to_the_longest_shown_key_plus_two() {
        let results = [
            (
                Section::Distribution.key(),
                "Ubuntu 24.04 LTS (noble)".to_string(),
            ),
            (Section::Kernel.key(), "6.8.0-1-generic".to_string()),
        ];
        assert_eq!(
            render(&results, plain),
            "Distribution:  Ubuntu 24.04 LTS (noble)\n\
             Kernel:        6.8.0-1-generic\n\
             \n"
        );
    }

    /// A hidden empty value doesn't count towards the padding.
    #[test]
    fn empty_values_are_hidden_and_not_measured() {
        let results = [
            (Section::Kernel.key(), "k".to_string()),
            (Section::Queues.key(), String::new()),
            (Section::Plex.key(), "p".to_string()),
        ];
        assert_eq!(render(&results, plain), "Kernel:  k\nPlex:    p\n\n");
    }

    #[test]
    fn continuation_lines_start_at_the_value_column() {
        let results = [
            (Section::Kernel.key(), "k".to_string()),
            (Section::Disk.key(), "/   10%\n[bar]".to_string()),
        ];
        assert_eq!(
            render(&results, plain),
            "Kernel:      k\nDisk Usage:  /   10%\n             [bar]\n\n"
        );
    }

    /// Padding counts the key, not the colour codes painted around it.
    #[test]
    fn colour_codes_around_a_key_do_not_change_the_padding() {
        let results = [
            (Section::Distribution.key(), "d".to_string()),
            (Section::Kernel.key(), "k".to_string()),
        ];
        let text = render(&results, |key| format!("\x1b[35m{key}\x1b[0m"));
        assert_eq!(
            text,
            "\x1b[35mDistribution:\x1b[0m  d\n\x1b[35mKernel:\x1b[0m        k\n\n"
        );
    }

    #[test]
    fn nothing_shown_is_the_blank_line_alone() {
        assert_eq!(
            render(&[(Section::Kernel.key(), String::new())], plain),
            "\n"
        );
        assert_eq!(render(&[], plain), "\n");
    }
}
