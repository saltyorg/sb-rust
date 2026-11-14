//! Text sb didn't write, made safe to show (motd decision 24).

use sb_term_ui::DisplayText;

/// `bytes` as one line safe to draw (term-ui's `DisplayText`, colour
/// dropped): its first line, trimmed.
#[must_use]
pub(crate) fn one_line(bytes: &[u8]) -> String {
    DisplayText::from_bytes(bytes, false)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `bytes` as lines safe to draw (colour dropped), the whole text
/// trimmed first, as sb-go trims a child's output
/// (`motd/utils.go`, `ExecCommand`).
#[must_use]
pub(crate) fn block(bytes: &[u8]) -> String {
    DisplayText::from_bytes(bytes.trim_ascii(), false)
        .as_str()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_keeps_the_first_line_trimmed() {
        // The tab expands to the next 8-column stop before the trim.
        assert_eq!(one_line(b"  a\tb \nsecond"), "a     b");
        assert_eq!(one_line(b"\x1b]0;title\x07x\x1b[31my"), "xy");
        assert_eq!(one_line(b""), "");
    }

    #[test]
    fn block_keeps_every_line_and_trims_the_whole() {
        assert_eq!(block(b"\n  first\x1b[2J\nsecond  \n\n"), "first\nsecond");
        assert_eq!(block(b" \x1b]0;t\x07 "), "");
    }
}
