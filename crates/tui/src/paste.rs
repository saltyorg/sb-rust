//! A paste's text for the screens' text boxes (bracketed paste: the
//! terminal sends a paste as one event, crossterm's `Event::Paste`,
//! <https://docs.rs/crossterm/0.29.0/crossterm/event/enum.Event.html#variant.Paste>,
//! which goes into a box through ratatui-textarea's `insert_str`,
//! <https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html#method.insert_str>).
//! A paste's line ends come as `\r\n`, `\r` or `\n`: tmux's
//! `paste-buffer` turns each LF into CR unless told otherwise (tmux(1),
//! `paste-buffer -r`, <https://man7.org/linux/man-pages/man1/tmux.1.html>);
//! UNVERIFIED for other terminals, which may send a copied text's own
//! line ends; all three are handled. `insert_str` takes `\n` and `\r\n`
//! as new lines but not `\r`.
//!
//! A paste can also carry control characters typing can't (ESC, BEL,
//! DEL; crossterm passes the bracketed text through as it came). The
//! screen wouldn't show them (ratatui's buffer drops them), but they
//! would reach a fact file, so they are dropped here; a tab stays.

/// `text` for a box of several lines: every line end a `\n`, a last one
/// dropped (a copied token often carries one).
pub(crate) fn lines(text: &str) -> String {
    let text: String = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect();
    text.strip_suffix('\n').unwrap_or(&text).to_owned()
}

/// `text` for a one-line box: as [`lines`], each line end left a space.
pub(crate) fn one_line(text: &str) -> String {
    lines(text).replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ends_are_newlines_and_a_last_one_goes() {
        assert_eq!(lines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(lines("tok\r\n"), "tok");
        assert_eq!(lines("tok\n\n"), "tok\n");
        assert_eq!(lines(""), "");
    }

    /// Control characters a paste can carry are dropped; a tab stays.
    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(lines("a\x1b[31mb\x07\x7fc\td\u{9b}e"), "a[31mbc\tde");
    }

    #[test]
    fn a_one_line_box_gets_spaces() {
        assert_eq!(one_line("a\nb\r\nc\n"), "a b c");
        assert_eq!(one_line("rad\n"), "rad");
    }
}
