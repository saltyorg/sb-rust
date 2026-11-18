//! sb's `--json` output (`sb version`, `sb docker ps`, `sb motd`, `sb
//! list`, `sb validate-config`): one line of compact JSON, keys in each
//! type's field order, one trailing newline, every control character in a
//! string escaped.

use serde::Serialize;

/// `value` as one line of compact JSON and a newline: `serde_json`'s
/// compact output, with DEL and C1 in strings escaped too
/// ([`EscapeControls`]).
#[must_use]
pub fn line<T: Serialize + ?Sized>(value: &T) -> String {
    // Serialising into a `Vec` can't fail for sb's types (strings,
    // numbers, booleans, options and sequences of them), and the
    // formatter writes only valid UTF-8.
    let mut out = Vec::new();
    let _ = value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut out,
        EscapeControls,
    ));
    let mut text = String::from_utf8(out).unwrap_or_default();
    text.push('\n');
    text
}

/// `serde_json`'s compact output (every method but one is the trait's
/// default, which `CompactFormatter` uses too), with DEL and C1 in strings
/// escaped as `\u00XX` as well: `serde_json` escapes only U+0000 to U+001F,
/// and an 8-bit CSI, OSC or ST in a label would reach a terminal running
/// `--json` raw. The hook for the unescaped parts of a string:
/// <https://docs.rs/serde_json/1.0.154/serde_json/ser/trait.Formatter.html#method.write_string_fragment>.
struct EscapeControls;

impl serde_json::ser::Formatter for EscapeControls {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> std::io::Result<()>
    where
        W: ?Sized + std::io::Write,
    {
        let mut rest = 0;
        for (at, c) in fragment.char_indices().filter(|(_, c)| c.is_control()) {
            writer.write_all(&fragment.as_bytes()[rest..at])?;
            write!(writer, "\\u{:04x}", u32::from(c))?;
            rest = at + c.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[rest..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Shape<'a> {
        schema: u32,
        name: &'a str,
        count: Option<u64>,
    }

    /// Compact, the fields' order, one newline; C0 as `serde_json` escapes it,
    /// DEL and C1 as `\u00XX`, other text as it is.
    #[test]
    fn a_line_of_compact_json_with_every_control_escaped() {
        let shape = Shape {
            schema: 1,
            name: "a\u{1b}[31m\nb\u{7f}c\u{9b}d é ✓",
            count: None,
        };
        assert_eq!(
            line(&shape),
            "{\"schema\":1,\"name\":\"a\\u001b[31m\\nb\\u007fc\\u009bd é ✓\",\"count\":null}\n"
        );
        assert_eq!(line(&[1, 2]), "[1,2]\n");
    }
}
