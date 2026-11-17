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

/// `bytes`' first line safe to draw (colour dropped), not trimmed: text
/// sb-go shows as it came (an instance's name, `SABnzbd`'s fields).
#[must_use]
pub(crate) fn first_line(bytes: &[u8]) -> String {
    DisplayText::from_bytes(bytes, false)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `text` cut to sb-config's limit for remote text
/// (`sb_config::http::MAX_TEXT` characters), with `…` when cut.
#[must_use]
pub(crate) fn capped(text: &str) -> String {
    match text.char_indices().nth(sb_config::http::MAX_TEXT) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
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

/// Whether `err` is a permission error: EACCES or EPERM, by errno first
/// (std maps both to `PermissionDenied` today, but doesn't document the
/// mapping as stable).
pub(crate) fn is_denial(err: &std::io::Error) -> bool {
    let errno = err.raw_os_error();
    errno == Some(rustix::io::Errno::ACCESS.raw_os_error())
        || errno == Some(rustix::io::Errno::PERM.raw_os_error())
        || err.kind() == std::io::ErrorKind::PermissionDenied
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
    fn first_line_keeps_its_spaces() {
        assert_eq!(first_line(b" a  \nb"), " a  ");
        assert_eq!(first_line(b"x\x1b[2Jy"), "xy");
        assert_eq!(first_line(b""), "");
    }

    #[test]
    fn capped_cuts_at_the_limit() {
        let limit = sb_config::http::MAX_TEXT;
        let exact = "é".repeat(limit);
        assert_eq!(capped(&exact), exact);
        let long = format!("{exact}tail");
        assert_eq!(capped(&long), format!("{exact}…"));
        assert_eq!(capped(""), "");
    }

    #[test]
    fn block_keeps_every_line_and_trims_the_whole() {
        assert_eq!(block(b"\n  first\x1b[2J\nsecond  \n\n"), "first\nsecond");
        assert_eq!(block(b" \x1b]0;t\x07 "), "");
    }
}
