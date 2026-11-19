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
        Self::parse(bytes, colour, 0, None).0
    }

    /// Sanitizes `bytes` as [`from_bytes`](Self::from_bytes) does, and
    /// says where a parse of them can start again.
    ///
    /// There is a [`Resume`] about every `every` columns of each line.
    /// `column` is the display column the first
    /// line starts at, for its tab stops: a parse resumed at a
    /// [`Resume`] is of the bytes from its `byte` on, from its `column`.
    /// The text that parse makes is the rest of this text from the
    /// resume's character on, line by line, the first line from that
    /// character (the tui log viewer far to the right with wrap off,
    /// which parses only from near the column it shows).
    ///
    /// Each resume is just before a character the parser printed, so the
    /// parser is in its ground state there, with no carriage return
    /// pending, and next to a printable ASCII character (before or
    /// after it), so that it falls between two graphemes in all but a few
    /// cases the caller checks (a prepended or an extending mark beside
    /// it). There are none where the bytes were changed before parsing
    /// (invalid UTF-8, C1), from the first change on, none at a character
    /// the bytes' end splits, and none left on a line a carriage return
    /// took back. A carriage return after the
    /// bytes given can still take a line back: the caller looks for one.
    ///
    /// ```
    /// use sb_term_ui::DisplayText;
    ///
    /// let bytes = b"\x1b[31mred\x1b[0m and plain";
    /// let (text, resumes) = DisplayText::resumable(bytes, true, 0, 4);
    /// let at = resumes[1];
    /// assert_eq!((&bytes[at.byte..], at.column, at.visible), (&b"and plain"[..], 4, 4));
    /// let rest = DisplayText::from_bytes_at(&bytes[at.byte..], true, at.column);
    /// assert!(text.as_str().ends_with(rest.as_str()));
    /// ```
    #[must_use]
    pub fn resumable(
        bytes: &[u8],
        colour: bool,
        column: usize,
        every: usize,
    ) -> (Self, Vec<Resume>) {
        Self::parse(bytes, colour, column, Some(every.max(1)))
    }

    /// Sanitizes `bytes` with the first line starting at display column
    /// `column`: the parse a [`Resume`] starts.
    #[must_use]
    pub fn from_bytes_at(bytes: &[u8], colour: bool, column: usize) -> Self {
        Self::parse(bytes, colour, column, None).0
    }

    /// The parse: [`plain`]'s copy when `bytes` hold nothing to change
    /// and no resume is asked for, else [`DisplayText::parse_full`].
    fn parse(
        bytes: &[u8],
        colour: bool,
        column: usize,
        every: Option<usize>,
    ) -> (DisplayText, Vec<Resume>) {
        if every.is_none()
            && let Some(text) = plain(bytes)
        {
            return (text, Vec::new());
        }
        Self::parse_full(bytes, colour, column, every)
    }

    /// The full parse: [`DisplayText::resumable`]'s, with `every` a resume
    /// about every so many columns, and any text [`plain`] declines.
    fn parse_full(
        bytes: &[u8],
        colour: bool,
        column: usize,
        every: Option<usize>,
    ) -> (DisplayText, Vec<Resume>) {
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
            column,
            after_cr: false,
            last_was_newline: false,
            pending: STAND_IN,
            resumes: every.map(|every| Resumes {
                every,
                next: column,
                ascii: false,
                codes: 0,
                at: Vec::new(),
            }),
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
        let mut resumes = sink.resumes.take().map_or_default(|r| r.at);
        if !resumes.is_empty() {
            // A resume's byte is in the text parsed, which is `bytes` up to
            // the first change made before parsing: invalid UTF-8 or a C1
            // code. A character the end splits leaves the bytes before it
            // as they are, but not the character: the U+FFFD printed for
            // it may stand for one that joins the character before it (a
            // variation selector, a keycap's mark), so no resume is kept
            // there.
            let (valid, split) = match std::str::from_utf8(bytes) {
                Ok(text) => (text.len(), false),
                Err(e) => (e.valid_up_to(), e.error_len().is_none()),
            };
            // `contains` first: it looks for the byte as memchr does, much
            // faster than a scan of pairs, and most text holds no C1 code.
            let same = if bytes[..valid].contains(&0xc2) {
                bytes[..valid]
                    .windows(2)
                    .position(|pair| matches!(pair, [0xc2, 0x80..=0x9f]))
                    .unwrap_or(valid)
            } else {
                valid
            };
            let kept = if split && same == valid {
                same
            } else {
                same + 1
            };
            resumes.retain(|r| r.byte < kept);
        }
        (sink.finish(), resumes)
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

/// Where a parse can start again ([`DisplayText::resumable`]): just
/// before a printed character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    /// The character's first byte in the bytes parsed.
    pub byte: usize,
    /// Its line.
    pub line: usize,
    /// The line's display width before it, tab stops counted.
    pub column: usize,
    /// The line's text before it, in bytes, kept SGRs left out: where it
    /// is in the line's characters as drawn.
    pub visible: usize,
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

/// `bytes` as the full parse leaves them, when it changes nothing: valid
/// UTF-8 with no control character but newline (no escape, carriage
/// return, tab, other C0, DEL or C1), so every character is printed as it
/// is and only newlines split lines, as [`Sink`] splits them (a final one
/// adds none; empty text is one empty line). `None` for anything else,
/// which the parser takes. Most log and table text is such text, and the
/// parser's per-character state machine is most of a log viewer frame's
/// cost there (the performance pass, 2026-11-17). Why not a fast path in
/// the library: anstyle-parse has none; it is a state machine that sees
/// each byte (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/struct.Parser.html>).
fn plain(bytes: &[u8]) -> Option<DisplayText> {
    // C0 but newline, and DEL; ESC, CR and tab are among them.
    if bytes.iter().any(|&b| (b < 0x20 && b != b'\n') || b == 0x7f) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    // C1 (U+0080 to U+009F) is C2 80 to C2 9F in UTF-8. `contains` first,
    // as in `parse_full`: most text holds no C2 byte.
    if bytes.contains(&0xc2)
        && bytes
            .windows(2)
            .any(|pair| matches!(pair, [0xc2, 0x80..=0x9f]))
    {
        return None;
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for (at, _) in text.match_indices('\n') {
        lines.push(start..at);
        start = at + 1;
    }
    let mut text = text.to_owned();
    if start == text.len() && !lines.is_empty() {
        // A final newline adds no line (`Sink::finish`).
        text.pop();
    } else {
        lines.push(start..text.len());
    }
    Some(DisplayText { text, lines })
}

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
    /// The resumes being kept, when asked for.
    resumes: Option<Resumes>,
}

/// The resumes [`DisplayText::resumable`] keeps, and what choosing the
/// next one needs.
struct Resumes {
    every: usize,
    /// The column at or past which the next is kept.
    next: usize,
    /// The line's last visible character is printable ASCII.
    ascii: bool,
    /// The bytes of the kept SGRs on the line so far.
    codes: usize,
    at: Vec<Resume>,
}

impl Sink<'_> {
    /// Something other than CR or newline follows on this line: after a
    /// CR, the line starts again from here.
    fn text_follows(&mut self) {
        if self.after_cr {
            self.after_cr = false;
            self.text.truncate(self.line_start);
            self.column = 0;
            if let Some(resumes) = &mut self.resumes {
                // What the line held is gone, and so is every resume in it.
                let line = self.lines.len();
                resumes.at.retain(|r| r.line != line);
                resumes.start_line();
            }
        }
    }

    fn end_line(&mut self) {
        self.lines.push(self.line_start..self.text.len());
        self.text.push('\n');
        self.line_start = self.text.len();
        self.column = 0;
        self.after_cr = false;
        self.last_was_newline = true;
        if let Some(resumes) = &mut self.resumes {
            resumes.start_line();
        }
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

impl Resumes {
    /// A new line: from its first column, with nothing on it yet.
    fn start_line(&mut self) {
        self.next = 0;
        self.ascii = false;
        self.codes = 0;
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
        if let Some(resumes) = &mut self.resumes {
            // Printed, so the parser is in its ground state: anstyle-parse
            // 1.0.0 prints only from Ground (src/state/codegen.rs, its
            // `Ground` row; `process_utf8` in src/lib.rs stays there),
            // and its `Parser` has no public state to snapshot instead.
            // UNVERIFIED beyond that source, checked by resumed parses in
            // `a_resumed_parse_makes_the_rest_of_the_text`. With a
            // printable ASCII character on one side it is almost surely a
            // grapheme's start, which the caller checks.
            let ascii = c == ' ' || c.is_ascii_graphic();
            if self.column >= resumes.next && (resumes.ascii || ascii) {
                resumes.at.push(Resume {
                    byte: self.at + 1 - c.len_utf8(),
                    line: self.lines.len(),
                    column: self.column,
                    visible: self.text.len() - self.line_start - resumes.codes,
                });
                resumes.next = self.column + resumes.every;
            }
            resumes.ascii = ascii;
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
                if let Some(resumes) = &mut self.resumes {
                    resumes.ascii = true;
                }
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
            if let Some(resumes) = &mut self.resumes {
                resumes.codes += raw.len();
            }
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
///
/// Public for `sb update`'s tag lists (ops spec, "What moved, and the
/// closing hint"), which wrap under their label as the tree's rows do.
#[must_use]
pub fn wrap_line(line: &str, width: usize, words: bool) -> Vec<String> {
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

/// `text` cut to `width` columns, `…` in the last one when it is cut
/// (console's `truncate_str`,
/// <https://docs.rs/console/0.16.6/console/fn.truncate_str.html>); as
/// it is when it fits. For one line in a list that must stay on one
/// line (`sb update`'s local changes question names a few files).
#[must_use]
pub fn fit(text: &str, width: usize) -> String {
    console::truncate_str(text, width, "…").into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn fit_cuts_at_the_width_with_an_ellipsis() {
        assert_eq!(super::fit("roles/a.yml", 20), "roles/a.yml");
        assert_eq!(super::fit("roles/abcdef.yml", 8), "roles/a…");
        assert_eq!(
            console::measure_text_width(&super::fit("日本語のファイル", 7)),
            7
        );
    }

    use super::{DisplayText, wrap_line};

    /// The fast path takes exactly the text the parser leaves as it is:
    /// valid UTF-8 with no control character but newline. Each class at
    /// its edges: space and `~` in; 0x1F, DEL and C1 (U+0080 to U+009F)
    /// out; U+00A0 in.
    #[test]
    fn only_plain_text_takes_the_fast_path() {
        let plain = [
            "",
            "a",
            " ~",
            "é 日 “quoted” 👍",
            "a\nb",
            "a\n",
            "\n\n",
            "x\u{a0}y",
        ];
        for text in plain {
            assert!(super::plain(text.as_bytes()).is_some(), "{text:?}");
        }
        let loud: [&[u8]; 16] = [
            b"a\x1b[31m",
            b"a\rb",
            b"a\tb",
            b"\x00",
            b"\x1f",
            b"\x7f",
            "\u{80}".as_bytes(),
            "\u{9b}".as_bytes(),
            "\u{9f}".as_bytes(),
            b"\xff",
            b"a\xe2\x80",
            // Overlong forms of ESC and CSI, a surrogate, past U+10FFFF, a
            // lone continuation byte: invalid UTF-8, which std refuses.
            b"\xc0\x9b",
            b"\xe0\x80\x9b",
            b"\xed\xa0\x80",
            b"\xf4\x90\x80\x80",
            b"\x80",
        ];
        for bytes in loud {
            assert!(super::plain(bytes).is_none(), "{bytes:?}");
        }
    }

    /// The fast path makes exactly what the full parse makes: over random
    /// byte strings built from what the parser acts on (escapes, CR, tab,
    /// C0, DEL, C1 in both forms, broken UTF-8) and any byte at all, and
    /// over plain text, in colour or not, from column 0 or 3. fastrand's
    /// seeded generator
    /// (<https://docs.rs/fastrand/2.5.0/fastrand/struct.Rng.html#method.with_seed>);
    /// why not proptest, it isn't a dependency (adding one goes to the
    /// maintainer first). A failure prints its bytes.
    #[test]
    fn the_fast_path_matches_the_full_parse() {
        let mut rng = fastrand::Rng::with_seed(0x9e37_79b9_7f4a_7c15);
        let loud: &[&[u8]] = &[
            b"a",
            b" ",
            b"\n",
            b"\r",
            b"\t",
            b"\x1b",
            b"[",
            b"31",
            b";",
            b"m",
            b"]",
            b"\x07",
            b"\x7f",
            b"\x00",
            "\u{9b}".as_bytes(),
            b"\xc2",
            b"\x9c",
            "“".as_bytes(),
            "é".as_bytes(),
            "日".as_bytes(),
            b"\xff",
            b"\xe2\x80",
        ];
        let plain: &[&[u8]] = &[
            b"a",
            b"Z",
            b" ",
            b"~",
            b"\n",
            "é".as_bytes(),
            "日".as_bytes(),
            "“".as_bytes(),
            "\u{a0}".as_bytes(),
            "👍".as_bytes(),
            b"0123456789",
        ];
        let mut fast = 0;
        for round in 0..20_000 {
            let is_loud = round % 2 == 0;
            let pieces = if is_loud { loud } else { plain };
            let mut bytes = Vec::new();
            for _ in 0..rng.usize(..48) {
                // In the loud rounds, one choice in a dozen is any byte.
                if is_loud && rng.usize(..12) == 0 {
                    bytes.push(rng.u8(..));
                } else {
                    bytes.extend_from_slice(pieces[rng.usize(..pieces.len())]);
                }
            }
            fast += usize::from(super::plain(&bytes).is_some());
            for colour in [false, true] {
                for column in [0, 3] {
                    assert_eq!(
                        DisplayText::from_bytes_at(&bytes, colour, column),
                        DisplayText::parse_full(&bytes, colour, column, None).0,
                        "{bytes:?}"
                    );
                }
            }
        }
        // Both paths ran: the plain rounds, half of them, take the fast one.
        assert!((10_000..20_000).contains(&fast), "{fast}");
    }

    /// The same, for every byte and every pair of bytes, inside plain text
    /// and at its ends: each C0 and C1 byte, each C2 pair and each broken
    /// sequence, none left to chance.
    #[test]
    fn every_byte_and_pair_matches_the_full_parse() {
        let check = |bytes: &[u8]| {
            for colour in [false, true] {
                assert_eq!(
                    DisplayText::from_bytes_at(bytes, colour, 3),
                    DisplayText::parse_full(bytes, colour, 3, None).0,
                    "{bytes:?}"
                );
            }
        };
        for a in 0..=u8::MAX {
            check(&[a]);
            check(&[b'x', a, b'\n', b'y']);
            for b in 0..=u8::MAX {
                check(&[b'x', a, b, b'\n']);
            }
        }
    }

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

    /// Where line `line` of `text` is `visible` bytes in, kept SGRs left
    /// out: past the SGRs there, which come before the character.
    fn text_at(text: &DisplayText, line: usize, visible: usize) -> &str {
        let whole = text.lines().nth(line).expect("the resume's line");
        let mut seen = 0;
        let mut at = 0;
        loop {
            if let Some(rest) = whole[at..].strip_prefix("\x1b[") {
                at = whole.len() - rest.len() + rest.find('m').expect("an SGR's end") + 1;
                continue;
            }
            if seen == visible {
                return &whole[at..];
            }
            let c = whole[at..].chars().next().expect("a character");
            seen += c.len_utf8();
            at += c.len_utf8();
        }
    }

    /// A parse resumed at any resume makes the rest of the text from the
    /// resume's character on, the rest of its line and every line after,
    /// colour on and off; resumes are next to printable ASCII, about every
    /// so many columns, and none are left where the bytes changed before
    /// parsing or on a line a carriage return took back.
    #[test]
    fn a_resumed_parse_makes_the_rest_of_the_text() {
        const SYMBOLS: [&str; 12] = [
            "\x1b", "[", "]", "\x07", "1", ";", "m", "a", "\n", "\r", "\t", "日",
        ];
        let colours = (0..60)
            .map(|i| format!("\x1b[3{}mword{i}\x1b[0m ", i % 8))
            .collect::<Vec<_>>()
            .concat();
        let inputs: Vec<Vec<u8>> = vec![
            colours.into_bytes(),
            "lorem ipsum ".repeat(30).into_bytes(),
            "a\tbb\tccc\t".repeat(20).into_bytes(),
            "日本語のログ ".repeat(30).into_bytes(),
            "e\u{301}x".repeat(40).into_bytes(),
            b"osc \x1b]0;a\x07 after\x1b[1;31m bold red\x1b[22m red \x1b[?25l gone".to_vec(),
            b"one\ntwo \x1b[3\n1m split\nthree".to_vec(),
            b"progress 10%\rprogress 20%\r\rdone and more text".to_vec(),
            b"before \xff invalid after".to_vec(),
            "before \u{9b}31m c1 after".into(),
            "split at the end \u{65e5}".as_bytes()[..19].to_vec(),
        ];
        for bytes in &inputs {
            for colour in [true, false] {
                let whole = DisplayText::from_bytes(bytes, colour);
                let (text, resumes) = DisplayText::resumable(bytes, colour, 0, 3);
                assert_eq!(text, whole);
                for r in &resumes {
                    let rest = DisplayText::from_bytes_at(&bytes[r.byte..], colour, r.column);
                    let mut lines = rest.lines();
                    assert_eq!(
                        lines.next(),
                        Some(text_at(&whole, r.line, r.visible)),
                        "{r:?} of {:?}",
                        String::from_utf8_lossy(bytes)
                    );
                    assert!(lines.eq(whole.lines().skip(r.line + 1)), "{r:?}");
                    let next = &bytes[r.byte..];
                    let after = text.lines().nth(r.line).map(|l| l.chars().count());
                    assert!(after.is_some(), "{r:?}");
                    assert!(
                        next.first()
                            .is_some_and(|b| b.is_ascii_graphic() || *b == b' ')
                            || bytes[..r.byte].last().is_some_and(|b| {
                                b.is_ascii_graphic() || *b == b' ' || *b == b'\t' || *b == b'm'
                            }),
                        "{r:?} of {:?}",
                        String::from_utf8_lossy(bytes)
                    );
                }
                let pairs = resumes.windows(2).filter(|w| w[0].line == w[1].line);
                assert!(
                    pairs.clone().all(|w| w[1].column >= w[0].column + 3),
                    "{resumes:?}"
                );
                // Lines of 120 columns and more have a resume every few.
                if bytes.len() > 300 && !bytes.contains(&b'\n') {
                    assert!(resumes.len() > 20, "{resumes:?}");
                }
            }
        }
        let progress = DisplayText::resumable(&inputs[7], true, 0, 3).1;
        assert_eq!(progress.first().map(|r| r.byte), Some(27), "{progress:?}");
        // Up to the first change: a resume at an invalid byte (decoded as
        // U+FFFD from the whole bytes too) is kept, none at a character
        // the end splits.
        for (input, change) in [(8, 7), (9, 6), (10, 16)] {
            let resumes = DisplayText::resumable(&inputs[input], true, 0, 1).1;
            assert_eq!(resumes.last().map(|r| r.byte), Some(change), "{input}");
        }
        // A resumed parse's tab stops count from its column.
        let tabbed = DisplayText::from_bytes_at(b"x\ty", false, 5);
        assert_eq!(tabbed.as_str(), "x  y");
        // Each line has resumes of its own, from its first column.
        let lines = DisplayText::resumable(b"abcdef\nghijkl", false, 0, 4).1;
        let firsts: Vec<_> = lines.iter().map(|r| (r.line, r.column, r.byte)).collect();
        assert_eq!(firsts, [(0, 0, 0), (0, 4, 4), (1, 0, 7), (1, 4, 11)]);
        // A short input over the sequences' alphabet: every resume is good.
        let count = SYMBOLS.len();
        for length in 1..=4u32 {
            for mut index in 0..count.pow(length) {
                let mut input = String::from("a");
                for _ in 0..length {
                    input.push_str(SYMBOLS[index % count]);
                    index /= count;
                }
                input.push('z');
                for colour in [true, false] {
                    let whole = DisplayText::from_bytes(input.as_bytes(), colour);
                    let (_, resumes) = DisplayText::resumable(input.as_bytes(), colour, 0, 1);
                    for r in resumes {
                        let rest = DisplayText::from_bytes_at(
                            &input.as_bytes()[r.byte..],
                            colour,
                            r.column,
                        );
                        let mut lines = rest.lines();
                        assert_eq!(
                            lines.next(),
                            Some(text_at(&whole, r.line, r.visible)),
                            "{input:?} {r:?}"
                        );
                        assert!(lines.eq(whole.lines().skip(r.line + 1)), "{input:?} {r:?}");
                    }
                }
            }
        }
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
