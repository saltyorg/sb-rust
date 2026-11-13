//! Untrusted text made safe to draw (term-ui spec, "Escape handling"):
//! [`DisplayText`].

use std::ops::Range;

use anstyle_parse::{DefaultCharAccumulator, Params, Parser, Perform};

/// Text sanitized for the screen, held as one `String` plus its line
/// ranges. Only colour (SGR) survives, and only with `colour`; every other
/// escape sequence goes with its payload, and so do C0 controls other than
/// tab and newline, DEL and C1. Callers pass term-ui's [`keeps_codes`] for
/// `colour`.
///
/// [`keeps_codes`]: crate::keeps_codes
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayText {
    text: String,
    lines: Vec<Range<usize>>,
}

impl DisplayText {
    /// Sanitize `bytes` for drawing (term-ui spec, "Escape handling"):
    /// - invalid UTF-8 is decoded lossily;
    /// - newline splits lines (a final one adds none, but a line after it
    ///   exists when anything follows, even if all of it is dropped; empty
    ///   text is one empty line), and tab expands to the next 8-column
    ///   stop;
    /// - carriage return keeps only what follows the last one on its line,
    ///   trailing ones ignored (progress output);
    /// - an SGR (`ESC [ [0-9;:]* m`) is kept, as its own bytes (an 8-bit
    ///   CSI in its 7-bit form), only with `colour`;
    /// - every other escape sequence (OSC, DCS, APC, PM, SOS with their
    ///   payloads; cursor, mode and query CSIs; a CSI past the parser's
    ///   parameter limit), other C0 controls, DEL and C1 are dropped.
    ///
    /// The escape sequences are found by anstyle-parse's DEC state machine
    /// (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/struct.Parser.html>).
    ///
    /// ```
    /// use sb_term_ui::DisplayText;
    ///
    /// let text = DisplayText::from_bytes(b"\x1b]0;title\x07\x1b[31mred\x1b[0m\nok", true);
    /// assert_eq!(text.lines().collect::<Vec<_>>(), ["\x1b[31mred\x1b[0m", "ok"]);
    /// let plain = DisplayText::from_bytes(b"\x1b[31mred\x1b[0m", false);
    /// assert_eq!(plain.as_str(), "red");
    /// ```
    #[must_use]
    pub fn from_bytes(bytes: &[u8], colour: bool) -> Self {
        let text = seven_bit(&String::from_utf8_lossy(bytes));
        let input = text.as_bytes();
        let mut sink = Sink {
            input,
            colour,
            start: 0,
            at: 0,
            text: String::with_capacity(input.len()),
            lines: Vec::new(),
            line_start: 0,
            column: 0,
            after_cr: false,
            last_was_newline: false,
            pending: STAND_IN,
        };
        let mut parser = Parser::<DefaultCharAccumulator>::new();
        for (at, c) in text.char_indices() {
            // The character's last byte, so a captured sequence ends there.
            sink.at = at + c.len_utf8() - 1;
            sink.last_was_newline = false;
            match u8::try_from(c) {
                Ok(byte) if byte.is_ascii() => {
                    if byte == ESC {
                        // ESC starts a sequence from any state (the
                        // `Anywhere` row of anstyle-parse 1.0.0's
                        // src/state/codegen.rs; the DEC model,
                        // <https://vt100.net/emu/dec_ansi_parser>), so the
                        // latest one starts whatever is dispatched next.
                        sink.start = at;
                        sink.text_follows();
                    }
                    parser.advance(&mut sink, byte);
                }
                _ => {
                    // Why a stand-in: outside Ground the parser doesn't
                    // decode UTF-8, and its DCS, SOS, PM and APC states
                    // end at a 0x9C byte (the 8-bit ST), which a
                    // character's own UTF-8 can hold (“ is E2 80 9C). The
                    // stand-in's bytes are no-ops in every state but
                    // Ground (and payload in OSC); in Ground they print
                    // the stand-in, which `print` swaps back. UNVERIFIED
                    // beyond anstyle-parse 1.0.0's table, which the tests
                    // pin.
                    sink.pending = c;
                    let mut buf = [0; 4];
                    for &byte in STAND_IN.encode_utf8(&mut buf).as_bytes() {
                        parser.advance(&mut sink, byte);
                    }
                }
            }
        }
        sink.finish()
    }

    /// The lines, in order; at least one.
    pub fn lines(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator {
        self.lines.iter().map(|range| &self.text[range.clone()])
    }

    /// The lines joined by newlines.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Writes [`DisplayText::as_str`].
impl std::fmt::Display for DisplayText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

const ESC: u8 = 0x1b;

/// What the parser is given for a non-ASCII character: U+0080, which
/// [`seven_bit`] has already removed from the text, so a print of it is
/// always the stand-in.
const STAND_IN: char = '\u{80}';

/// The 8-bit C1 forms as their 7-bit `ESC` forms, the rest of C1 dropped
/// (term-ui spec, "Escape handling"). Why not the library: anstyle-parse,
/// like vte, "only supports 7-bit codes"
/// (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/>): its Ground
/// table gives the raw C1 bytes no sequence transition, and a C1 code
/// point in UTF-8 (C2 9B) is printed as a character. The text is already
/// valid UTF-8, so a lone 0x80 to 0x9F byte is U+FFFD by now.
fn seven_bit(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{90}' => out.push_str("\x1bP"),
            '\u{98}' => out.push_str("\x1bX"),
            '\u{9b}' => out.push_str("\x1b["),
            '\u{9d}' => out.push_str("\x1b]"),
            '\u{9e}' => out.push_str("\x1b^"),
            '\u{9f}' => out.push_str("\x1b_"),
            '\u{80}'..='\u{9f}' => {}
            c => out.push(c),
        }
    }
    out
}

/// What the parser's callbacks build: the kept text and its lines.
struct Sink<'a> {
    /// The 7-bit text being parsed.
    input: &'a [u8],
    colour: bool,
    /// Where the latest sequence started (its ESC).
    start: usize,
    /// The byte being parsed.
    at: usize,
    text: String,
    lines: Vec<Range<usize>>,
    line_start: usize,
    /// The current line's display width so far, for tab stops.
    column: usize,
    /// A carriage return was seen on this line and nothing since.
    after_cr: bool,
    /// The last character parsed ended a line.
    last_was_newline: bool,
    /// The non-ASCII character the stand-in being parsed stands for.
    pending: char,
}

impl Sink<'_> {
    /// Something other than CR or newline follows on this line: after a
    /// CR, the line starts again from here.
    fn text_follows(&mut self) {
        if self.after_cr {
            self.after_cr = false;
            self.text.truncate(self.line_start);
            self.column = 0;
        }
    }

    fn end_line(&mut self) {
        self.lines.push(self.line_start..self.text.len());
        self.text.push('\n');
        self.line_start = self.text.len();
        self.column = 0;
        self.after_cr = false;
        self.last_was_newline = true;
    }

    fn finish(mut self) -> DisplayText {
        if self.last_was_newline {
            // A final newline adds no line.
            self.text.pop();
        } else {
            self.lines.push(self.line_start..self.text.len());
        }
        DisplayText {
            text: self.text,
            lines: self.lines,
        }
    }
}

/// Whether `raw` is a plain SGR: `ESC [ [0-9;:]* m` (journal spec,
/// "Display").
fn is_sgr(raw: &[u8]) -> bool {
    match raw {
        [ESC, b'[', body @ .., b'm'] => body
            .iter()
            .all(|&b| b.is_ascii_digit() || b == b';' || b == b':'),
        _ => false,
    }
}

/// anstyle-parse's callbacks
/// (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/trait.Perform.html>):
/// printed characters and the kept SGRs are written; OSC, DCS (`hook`,
/// `put`), plain escapes and the other CSIs fall to the trait's empty
/// defaults, and APC, PM and SOS reach no callback at all.
impl Perform for Sink<'_> {
    fn print(&mut self, c: char) {
        self.text_follows();
        let c = if c == STAND_IN { self.pending } else { c };
        // The ground state prints DEL (0x7F); C1 is gone already.
        if c.is_control() {
            return;
        }
        self.text.push(c);
        self.column += console::measure_text_width(c.encode_utf8(&mut [0; 4]));
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.end_line(),
            b'\r' => self.after_cr = true,
            b'\t' => {
                self.text_follows();
                let spaces = 8 - self.column % 8;
                self.text.extend(std::iter::repeat_n(' ', spaces));
                self.column += spaces;
            }
            _ => self.text_follows(),
        }
    }

    fn csi_dispatch(&mut self, _params: &Params, _intermediates: &[u8], ignore: bool, _action: u8) {
        // Emitted from the captured bytes, never rebuilt from `Params` (or
        // re-rendered through anstream's `WinconBytes`, which models only
        // what `anstyle::Style` holds): that keeps colon forms such as
        // `38:2::r:g:b` byte-exact.
        // `ignore` is set past the parser's parameter limit.
        let raw = &self.input[self.start..=self.at];
        if self.colour && !ignore && is_sgr(raw) {
            self.text.extend(raw.iter().map(|&b| char::from(b)));
        }
    }
}

/// One line of display text wrapped to `width` columns, measured as
/// display width with colour codes left out.
///
/// A line that fits comes back as it is, its indent and a blank line
/// included. A longer one goes through tabled's `Wrap::wrap`
/// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Wrap.html#method.wrap>),
/// at words when `words` is set, else at any character; with tabled's
/// `ansi` feature a colour is closed at each piece's end and re-emitted at
/// the next. A word wider than `width` breaks at any character. A piece
/// holding only spaces is dropped (tabled leaves one where a line ended
/// at the width). In word mode the spaces at each break are taken off:
/// tabled carries the space it broke at to the start of the next piece,
/// which also makes that piece break a column early. The first piece
/// keeps the line's own indent.
///
/// Why not textwrap's `Options::subsequent_indent`
/// (<https://docs.rs/textwrap/0.16.2/textwrap/struct.Options.html#structfield.subsequent_indent>):
/// textwrap isn't a dependency (the term-ui spec asks first for a new
/// one); it doesn't close and reopen a colour at a break, so a child's
/// colour would run into the next row's guide lines; and its indent is
/// one plain string, where the tree paints each row's lead in its own
/// role.
pub(crate) fn wrap_line(line: &str, width: usize, words: bool) -> Vec<String> {
    if console::measure_text_width(line) <= width {
        return vec![line.to_owned()];
    }
    let wrapped = tabled::settings::width::Wrap::wrap(line, width, words);
    let pieces: Vec<&str> = wrapped
        .split('\n')
        .filter(|piece| !console::strip_ansi_codes(piece).trim().is_empty())
        .collect();
    let last = pieces.len().saturating_sub(1);
    pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| {
            let mut piece = (*piece).to_owned();
            if words && i > 0 {
                trim_start(&mut piece);
            }
            if words && i < last {
                trim_end(&mut piece);
            }
            piece
        })
        .collect()
}

/// Take off the spaces `piece`'s visible text starts with, colour codes
/// kept. A colour code holds no space (only SGR codes reach a wrapped
/// line), so while the visible text starts with a space, the first space
/// in the piece is that one. Why not ansi-str's `ansi_trim`
/// (<https://docs.rs/ansi-str/0.9.0/ansi_str/trait.AnsiStr.html#tymethod.ansi_trim>):
/// ansi-str is only tabled's dependency, not term-ui's (a direct one asks
/// first), and it trims both ends, where only a break's side is trimmed.
fn trim_start(piece: &mut String) {
    while console::strip_ansi_codes(piece).starts_with(' ') {
        let Some(at) = piece.find(' ') else { break };
        piece.remove(at);
    }
}

/// Take off the spaces `piece`'s visible text ends with, as `trim_start`.
fn trim_end(piece: &mut String) {
    while console::strip_ansi_codes(piece).ends_with(' ') {
        let Some(at) = piece.rfind(' ') else { break };
        piece.remove(at);
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayText, wrap_line};

    /// Wrapping counts display width: colour codes count nothing and stay
    /// whole on each piece, a wide character counts two; no piece starts
    /// or ends with the space it was broken at (ui-L4).
    #[test]
    fn wrapped_lines_are_measured_in_display_width() {
        let red = "\x1b[31mred red red red\x1b[0m";
        let pieces = wrap_line(red, 7, true);
        let plain: Vec<String> = pieces
            .iter()
            .map(|p| console::strip_ansi_codes(p).into_owned())
            .collect();
        // tabled carries the break's space to the next piece, so it
        // breaks a column early: "red" alone, then "red".
        assert_eq!(plain, ["red red", "red", "red"], "{pieces:?}");
        assert!(
            pieces.iter().all(|p| p.starts_with("\x1b[31m")),
            "{pieces:?}"
        );
        let wide = "漢字漢字 漢字漢字漢字";
        let pieces = wrap_line(wide, 6, true);
        assert!(
            pieces.iter().all(|p| console::measure_text_width(p) <= 6),
            "{pieces:?}"
        );
        assert_eq!(pieces.concat(), "漢字漢字 漢字漢字漢字", "{pieces:?}");
        assert_eq!(wrap_line("fits", 4, true), ["fits"]);
    }

    /// A line's own spaces stay: one that fits comes back as it is, blank
    /// or indented, and a wrapped one keeps its indent on its first piece;
    /// only the spaces at a break go.
    #[test]
    fn a_lines_own_indent_and_blank_lines_stay() {
        assert_eq!(wrap_line("    ^ here", 40, true), ["    ^ here"]);
        assert_eq!(wrap_line("", 40, true), [""]);
        assert_eq!(wrap_line("   ", 40, true), ["   "]);
        assert_eq!(
            wrap_line("  key: one two three", 10, true),
            ["  key: one", "two three"]
        );
    }

    /// The lines of `text` sanitized with `colour`.
    fn shown(text: &str, colour: bool) -> Vec<String> {
        shown_bytes(text.as_bytes(), colour)
    }

    fn shown_bytes(bytes: &[u8], colour: bool) -> Vec<String> {
        DisplayText::from_bytes(bytes, colour)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// A kept SGR is the input's own bytes, colon forms included; 8-bit
    /// CSI becomes its 7-bit form.
    #[test]
    fn sgr_is_kept_byte_exact_with_colour() {
        for sgr in [
            "\x1b[38:2::10:20:30m",
            "\x1b[38;5;196m",
            "\x1b[1;4:3m",
            "\x1b[m",
            "\x1b[0m",
            "\x1b[007;;m",
        ] {
            let text = format!("a{sgr}b");
            assert_eq!(shown(&text, true), [text.as_str()], "{sgr:?}");
        }
        assert_eq!(
            shown("a\u{9b}38:2::10:20:30mb\u{9b}0m", true),
            ["a\x1b[38:2::10:20:30mb\x1b[0m"]
        );
    }

    #[test]
    fn sgr_is_dropped_without_colour() {
        assert_eq!(shown("a\x1b[38:2::10:20:30mb\x1b[0m", false), ["ab"]);
        assert_eq!(shown("a\u{9b}31mb\u{9b}m", false), ["ab"]);
    }

    /// OSC (title, OSC 8 link, OSC 52 clipboard) goes with its payload,
    /// ended by BEL or ST, colour on or off.
    #[test]
    fn osc_is_dropped_with_its_payload() {
        for colour in [true, false] {
            assert_eq!(shown("a\x1b]0;title\x07b", colour), ["ab"]);
            assert_eq!(
                shown(
                    "a\x1b]8;;https://example.test\x1b\\link\x1b]8;;\x1b\\b",
                    colour
                ),
                ["alinkb"]
            );
            assert_eq!(shown("a\x1b]52;c;ZGF0YQ==\x07b", colour), ["ab"]);
            assert_eq!(shown("a\u{9d}0;title\x07b", colour), ["ab"]);
            assert_eq!(shown("a\u{9d}52;c;ZGF0YQ==\x1b\\b", colour), ["ab"]);
        }
    }

    #[test]
    fn dcs_is_dropped_with_its_payload() {
        for colour in [true, false] {
            assert_eq!(shown("a\x1bP1$qm\x1b\\b", colour), ["ab"]);
            assert_eq!(shown("a\x1bPq#0;2;0;0;0#0~~@@\x1b\\b", colour), ["ab"]);
            assert_eq!(shown("a\u{90}1$qm\x1b\\b", colour), ["ab"]);
        }
    }

    /// APC, PM and SOS: the parser's string state ignores all up to ST.
    #[test]
    fn apc_pm_and_sos_are_dropped_with_their_payloads() {
        for colour in [true, false] {
            for intro in ["\x1b_", "\x1b^", "\x1bX", "\u{9f}", "\u{9e}", "\u{98}"] {
                let text = format!("a{intro}Gf=24,payload\x1b\\b");
                assert_eq!(shown(&text, colour), ["ab"], "{intro:?}");
            }
        }
    }

    /// Cursor moves, erases, modes and queries go, 7-bit and 8-bit.
    #[test]
    fn cursor_and_mode_csis_are_dropped() {
        for code in [
            "[2J", "[1A", "[10;5H", "[K", "[?25l", "[?1049h", "[6n", "[>c", "[c", "[4h", "[1 q",
            "[2\"q", "[1;31x",
        ] {
            let seven = format!("a\x1b{code}b");
            let eight = format!("a\u{9b}{}b", &code[1..]);
            assert_eq!(shown(&seven, true), ["ab"], "{seven:?}");
            assert_eq!(shown(&eight, true), ["ab"], "{eight:?}");
        }
    }

    /// Other escapes (`ESC 7`, `ESC c`, charset designations) go too.
    #[test]
    fn plain_escapes_are_dropped() {
        for code in [
            "\x1b7", "\x1b8", "\x1bc", "\x1b(0", "\x1b#8", "\x1bM", "\x1b=",
        ] {
            let text = format!("a{code}b");
            assert_eq!(shown(&text, true), ["ab"], "{code:?}");
        }
    }

    /// The parser keeps 32 parameters; past that it flags the CSI
    /// ignored, and an ignored SGR is dropped.
    #[test]
    fn a_csi_past_the_parameter_limit_is_dropped() {
        let at_limit = format!("\x1b[{}m", vec!["1"; 32].join(";"));
        let past = format!("\x1b[{}m", vec!["1"; 33].join(";"));
        assert_eq!(
            shown(&format!("a{at_limit}b"), true),
            [format!("a{at_limit}b")]
        );
        assert_eq!(shown(&format!("a{past}b"), true), ["ab"]);
        let past_eight = format!("\u{9b}{}m", vec!["1"; 33].join(";"));
        assert_eq!(shown(&format!("a{past_eight}b"), true), ["ab"]);
    }

    /// Only `ESC [ [0-9;:]* m` is SGR: a private marker or an
    /// intermediate makes it something else.
    #[test]
    fn only_plain_parameters_make_an_sgr() {
        for code in ["[?1m", "[>1m", "[=1m", "[<1m", "[1 m", "[1$m", "[1\"m"] {
            let seven = format!("a\x1b{code}b");
            let eight = format!("a\u{9b}{}b", &code[1..]);
            assert_eq!(shown(&seven, true), ["ab"], "{seven:?}");
            assert_eq!(shown(&eight, true), ["ab"], "{eight:?}");
        }
    }

    /// Tab and newline are kept, CR follows its rule; every other C0
    /// control, and DEL, is dropped.
    #[test]
    fn other_c0_controls_and_del_are_dropped() {
        for byte in (0x00..0x20u8).chain([0x7f]) {
            if matches!(byte, b'\t' | b'\n' | b'\r' | 0x1b) {
                continue;
            }
            let bytes = [b'a', byte, b'b'];
            assert_eq!(shown_bytes(&bytes, true), ["ab"], "{byte:#04x}");
        }
    }

    /// The C1 code points other than the six that start a sequence go
    /// (NEL, IND, ST among them); raw 0x80 to 0x9F bytes are invalid
    /// UTF-8 and show as U+FFFD.
    #[test]
    fn c1_controls_are_dropped() {
        for code in 0x80..0xa0u32 {
            if matches!(code, 0x90 | 0x98 | 0x9b | 0x9d | 0x9e | 0x9f) {
                continue;
            }
            let c = char::from_u32(code).expect("a C1 code point");
            assert_eq!(shown(&format!("a{c}b"), true), ["ab"], "{code:#x}");
        }
        for byte in 0x80..0xa0u8 {
            let bytes = [b'a', byte, b'b'];
            assert_eq!(shown_bytes(&bytes, true), ["a\u{fffd}b"], "{byte:#04x}");
        }
    }

    /// A non-ASCII character inside a string sequence is payload, even
    /// when its UTF-8 holds a byte the parser's string states read as the
    /// 8-bit ST (0x9C: “, Ü, ✓, М). Every two-byte character, and three-
    /// and four-byte samples, after every string introducer, terminated
    /// and not.
    #[test]
    fn non_ascii_text_never_ends_a_string() {
        let chars = ('\u{a0}'..='\u{7ff}')
            .chain('\u{2000}'..='\u{20ff}')
            .chain('\u{2700}'..='\u{27ff}')
            .chain('\u{7700}'..='\u{77ff}')
            .chain(['\u{1f642}', '\u{1009c}']);
        let intros = [
            "\x1b]0;", "\x1bP1$q", "\x1b_", "\x1b^", "\x1bX", "\u{9d}0;", "\u{90}", "\u{9f}",
            "\u{9e}", "\u{98}",
        ];
        for c in chars {
            for intro in intros {
                for colour in [true, false] {
                    let ended = format!("a{intro}x{c}LEAK\x1b\\b");
                    assert_eq!(shown(&ended, colour), ["ab"], "{ended:?}");
                    let open = format!("a{intro}x{c}LEAK\nnext");
                    assert_eq!(shown(&open, colour), ["a"], "{open:?}");
                }
            }
        }
    }

    /// Non-ASCII text inside a CSI makes it no SGR; outside sequences it is
    /// printed as itself.
    #[test]
    fn non_ascii_text_in_a_csi_drops_it() {
        assert_eq!(shown("a\x1b[3✓1mb", true), ["ab"]);
        assert_eq!(shown("a\x1b[3Ü1mb", true), ["ab"]);
        assert_eq!(shown("“✓Üм\u{1009c}”", true), ["“✓Üм\u{1009c}”"]);
    }

    /// CAN and SUB end a sequence, as in the DEC model.
    #[test]
    fn can_and_sub_end_a_sequence() {
        assert_eq!(shown("a\x1b]0;title\x18b", true), ["ab"]);
        assert_eq!(shown("a\x1b_payload\x1ab", true), ["ab"]);
    }

    /// An unterminated string sequence takes the rest of the text, newlines
    /// included: its payload is never shown.
    #[test]
    fn an_unterminated_string_takes_the_rest() {
        assert_eq!(shown("a\x1b]52;c;payload\nsecond", true), ["a"]);
        assert_eq!(shown("a\x1b_payload\nsecond", true), ["a"]);
        assert_eq!(shown("a\x1b[31", true), ["a"]);
    }

    /// CR: trailing ones are ignored, then the text after the last one is
    /// kept (progress output); a dropped sequence after it counts as text.
    #[test]
    fn carriage_return_keeps_what_follows_the_last_one() {
        assert_eq!(shown("10%\r50%\r100%", true), ["100%"]);
        assert_eq!(shown("10%\r50%\r", true), ["50%"]);
        assert_eq!(shown("10%\r50%\r\r", true), ["50%"]);
        assert_eq!(shown("a\r\nb\r\n", true), ["a", "b"]);
        assert_eq!(shown("abc\r\x1b[K", true), [""]);
        assert_eq!(shown("abc\r\x07", true), [""]);
        assert_eq!(shown("abc\r\x1b[32mok", true), ["\x1b[32mok"]);
        assert_eq!(shown("abc\r\tx", true), ["        x"]);
        // A CR inside a string's payload is payload.
        assert_eq!(shown("abc\x1b]0;t\rx\x07d", true), ["abcd"]);
    }

    #[test]
    fn invalid_utf8_is_decoded_lossily() {
        assert_eq!(shown_bytes(b"a\xffb", true), ["a\u{fffd}b"]);
        assert_eq!(shown_bytes(b"a\xe2\x82", true), ["a\u{fffd}"]);
        assert_eq!(shown("é漢🙂", true), ["é漢🙂"]);
    }

    /// Bidi and zero-width characters pass unmarked (journal decision 2).
    #[test]
    fn bidi_and_zero_width_pass() {
        assert_eq!(shown("a\u{202e}b\u{200b}c", true), ["a\u{202e}b\u{200b}c"]);
    }

    /// Newline splits lines, a final one adds none, and empty text is one
    /// empty line; `as_str` holds the lines joined by newlines.
    #[test]
    fn newlines_split_lines() {
        let text = DisplayText::from_bytes(b"a\n\nb\n", true);
        assert_eq!(text.lines().collect::<Vec<_>>(), ["a", "", "b"]);
        assert_eq!(text.as_str(), "a\n\nb");
        assert_eq!(shown("", true), [""]);
        assert_eq!(shown("\n", true), [""]);
        assert_eq!(shown("a", true), ["a"]);
        // A line after the last newline exists when anything follows it,
        // even if all of it is dropped (as `str::lines` splits).
        assert_eq!(shown("a\n\x07", true), ["a", ""]);
        assert_eq!(shown("a\n\x1b[K", true), ["a", ""]);
        assert_eq!(shown("a\n\r", true), ["a", ""]);
        assert_eq!(DisplayText::from_bytes(b"", true).as_str(), "");
        let coloured = DisplayText::from_bytes(b"\x1b[31ma\nb\x1b[0m", true);
        assert_eq!(coloured.as_str(), "\x1b[31ma\nb\x1b[0m");
        assert_eq!(coloured.lines().len(), 2);
    }

    /// `Display` writes `as_str`.
    #[test]
    fn display_writes_the_text() {
        let text = DisplayText::from_bytes(b"a\x1b]0;t\x07\nb", false);
        assert_eq!(text.to_string(), "a\nb");
        assert_eq!(text.to_string(), text.as_str());
    }

    /// Tabs go to the next 8-column stop, by display width; a kept SGR
    /// takes no column.
    #[test]
    fn tabs_expand_to_the_next_stop() {
        assert_eq!(shown("ab\tc", true), ["ab      c"]);
        assert_eq!(shown("a\tb\tc", true), ["a       b       c"]);
        assert_eq!(
            shown("\x1b[31mab\x1b[0m\tc", true),
            ["\x1b[31mab\x1b[0m      c"]
        );
        assert_eq!(shown("漢\tx", true), ["漢      x"]);
        assert_eq!(shown("a\nb\tc", true), ["a", "b       c"]);
        assert_eq!(shown("12345678\tx", true), ["12345678        x"]);
    }

    /// Every input over a small alphabet of sequence starts, ends and
    /// controls, up to four symbols long: no line holds a control
    /// character, and with colour on every ESC left starts a plain SGR.
    #[test]
    fn no_short_input_gets_a_control_through() {
        const SYMBOLS: [&str; 19] = [
            "\x1b", "[", "]", "P", "_", "X", "\\", "\x07", "1", ";", "?", "m", "a", "\n", "\r",
            "\u{9b}", "\u{9d}", "✓", "Ü",
        ];
        let count = SYMBOLS.len();
        let mut checked = 0;
        for length in 1..=4u32 {
            for mut index in 0..count.pow(length) {
                let mut input = String::new();
                for _ in 0..length {
                    input.push_str(SYMBOLS[index % count]);
                    index /= count;
                }
                for colour in [true, false] {
                    for line in shown(&input, colour) {
                        check_line(&line, colour, &input);
                    }
                }
                checked += 1;
            }
        }
        assert_eq!(checked, 19 + 19 * 19 + 19usize.pow(3) + 19usize.pow(4));
    }

    /// A line holds no control character other than the ESC of a plain
    /// SGR, and that only with colour on.
    fn check_line(line: &str, colour: bool, input: &str) {
        let mut rest = line;
        while let Some(at) = rest.find('\x1b') {
            assert!(colour, "{input:?} gave {line:?}");
            let after = &rest[at + 1..];
            let body = after
                .strip_prefix('[')
                .unwrap_or_else(|| panic!("{input:?} gave {line:?}"));
            let end = body
                .find('m')
                .unwrap_or_else(|| panic!("{input:?} gave {line:?}"));
            assert!(
                body[..end]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b == b';' || b == b':'),
                "{input:?} gave {line:?}"
            );
            assert!(
                !rest[..at].chars().any(char::is_control),
                "{input:?} gave {line:?}"
            );
            rest = &body[end + 1..];
        }
        assert!(
            !rest.chars().any(char::is_control),
            "{input:?} gave {line:?}"
        );
    }
}
