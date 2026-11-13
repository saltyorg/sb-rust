//! YAML as Ansible reads it, before saphyr-parser reads it (hardening plan
//! decisions 3 and 28; findings config-H1 and config-L1).
//!
//! Ansible's loader parses with libyaml (docs/platform-notes/
//! yaml-acceptance.md). saphyr-parser, sb's parser, refuses some layouts
//! libyaml accepts, and accepts some tab layouts libyaml refuses. This pass
//! closes both gaps on the text, before saphyr-parser sees it:
//!
//! - a continuation line inside a flow collection or a quoted scalar is
//!   padded with spaces past the column where that collection or scalar
//!   opened (saphyr wants it deeper than the block indentation; libyaml
//!   doesn't look). A continuation line's leading white space is no part
//!   of the value in either parser (YAML 1.2, 7.3 and 7.4:
//!   <https://yaml.org/spec/1.2.2/#73-flow-scalar-styles>), so the value
//!   is unchanged;
//! - a tab right after a simple key's `:` becomes a space (libyaml takes
//!   it as separation; saphyr refuses it before a letter or `-`);
//! - a tab where libyaml refuses one is refused: in a line's indentation in
//!   block context, after a `-`, `?` or explicit `:` indicator, in a block
//!   scalar's indentation, and in a plain scalar's continuation left of
//!   its indentation. The rules follow libyaml 0.2.5's scanner
//!   (`yaml_parser_scan_to_next_token`, `yaml_parser_scan_plain_scalar`,
//!   `yaml_parser_scan_block_scalar_breaks` in `src/scanner.c`:
//!   <https://github.com/yaml/libyaml/blob/0.2.5/src/scanner.c>), checked
//!   against `PyYAML` 6.0.3's bundled libyaml 0.2.5 on the YAML corpus
//!   (`crates/config/tests/yaml-corpus`).
//!
//! To do that the pass tracks what the scanner tracks: quotes, comments,
//! flow depth, block scalars (whose contents it leaves alone) and the
//! block indentation. It reads the first document only, as sb does; the
//! rest of the file is copied as it is.
//!
//! Why not a libyaml port (`libyaml-safer`, `unsafe-libyaml-norway`): the
//! maintainer chose this pass (decision 28) over Task 42's measured
//! options, which are in the platform note: a second parser beside
//! saphyr's loader adds 61 to 78 KB to the binary, the safe port panics
//! on inputs a differential fuzz found, and the other is `unsafe`
//! throughout. Why not granit-parser 2.0.0, already linked as
//! serde-saphyr's parser: Task 43's library review measured it at 55 of
//! the corpus's 59 layouts (it refuses the column-0 quoted continuation
//! and accepts the three tab layouts libyaml refuses), and its events
//! aren't saphyr's, so it would need an adapter to saphyr's loader as
//! well as this pass's tab checks. This pass covers the corpus with no
//! new dependency.
//!
//! The rewritten text is used only when saphyr refuses the file as
//! written: a file it reads already is read as it is (the tab checks still
//! apply), so a gap in this pass's tracking can't change a value saphyr
//! would have read. Padding is capped at [`PAD_BUDGET`], so a crafted
//! file can't grow without bound. Positions saphyr reports on the padded
//! text go back to the original file through [`Prepared::original`].

use saphyr_parser::{Event, Marker, Parser};

use super::MAX_FILE_BYTES;

/// The most spaces the pass inserts in one file: the padding a file needs
/// is about its opening columns times its continuation lines, which a
/// crafted file can make quadratic. Past it, lines stay as they are and
/// saphyr decides.
pub const PAD_BUDGET: usize = MAX_FILE_BYTES;

/// The text saphyr-parser reads, and how to map its positions back.
pub struct Prepared {
    text: String,
    /// The spaces inserted at the start of each line (0-based line index).
    pads: Vec<usize>,
    /// The spaces inserted before each line, in all the lines above it.
    before: Vec<usize>,
    /// All the spaces inserted.
    total: usize,
}

/// The config text is the user's, secrets included: its debug form shows
/// sizes only.
impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("bytes", &self.text.len())
            .field("padding", &self.total)
            .finish_non_exhaustive()
    }
}

/// A tab libyaml refuses, at a 1-based line and column of the original
/// text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at line {line}, column {column}")]
pub struct Refusal {
    pub line: usize,
    pub column: usize,
    pub message: &'static str,
}

const TAB_INDENT: &str = "a tab in the indentation (YAML indents with spaces)";
const TAB_AFTER_INDICATOR: &str = "a tab after an indicator here (use a space)";
const TAB_BLOCK_SCALAR: &str = "a tab in a block scalar's indentation (YAML indents with spaces)";

impl Prepared {
    /// The text for saphyr-parser.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// `marker`, a position in [`Prepared::text`], in the original text: a
    /// padded line's columns move left by its padding (a column inside
    /// the padding is the line's start), and the character index by all
    /// the padding before it.
    #[must_use]
    pub fn original(&self, marker: &Marker) -> Marker {
        let line = marker.line().saturating_sub(1);
        let pad = self.pads.get(line).copied().unwrap_or(0);
        let before = self.before.get(line).copied().unwrap_or(self.total);
        let in_line = pad.min(marker.col());
        Marker::new(
            marker.index().saturating_sub(before + in_line),
            marker.line(),
            marker.col() - in_line,
        )
    }
}

/// What the scanner is in when a line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Within {
    /// Block or flow context, between tokens.
    Tokens,
    /// A plain scalar that may go on on the next line, whose continuation
    /// can't hold a tab left of `indent` (libyaml's plain indentation:
    /// the block indentation plus one).
    Plain { indent: usize },
    /// A quoted scalar that goes on on the next line.
    Quoted { double: bool },
    /// A block scalar's content, under a block collection indented
    /// `parent` (none at the top), its content indentation known once a
    /// line sets it.
    BlockScalar {
        parent: Option<usize>,
        indent: Option<usize>,
    },
}

/// The scanner's state across lines.
struct Scan {
    within: Within,
    /// Block indentation columns, innermost last (libyaml's `indents`;
    /// empty is its -1).
    indents: Vec<usize>,
    /// Open flow collections.
    flow: usize,
    /// The column where the outermost construct still open at the line's
    /// end (a flow collection or a quoted scalar) started.
    open: usize,
    /// Whether a simple key may start here (libyaml's
    /// `simple_key_allowed`): where it may, in block context, a tab can't
    /// separate tokens.
    key_allowed: bool,
    /// Where a possible simple key on this line starts.
    key: Option<usize>,
    /// The possible keys of the flow levels around this one.
    outer: Vec<Option<usize>>,
    /// A document has started (`---`) or has content.
    started: bool,
}

impl Scan {
    /// The block indentation: none at the top (libyaml's -1).
    fn indent(&self) -> Option<usize> {
        self.indents.last().copied()
    }

    /// The column a plain scalar's continuation can't hold a tab left of
    /// (libyaml's `parser->indent + 1`).
    fn plain_indent(&self) -> usize {
        self.indent().map_or(0, |col| col + 1)
    }

    /// A block collection starting at `col` (libyaml's `roll_indent`).
    fn roll(&mut self, col: usize) {
        if self.flow == 0 && self.indents.last().is_none_or(|&top| top < col) {
            self.indents.push(col);
        }
    }

    /// A node starts at `col`: a possible simple key, if one may start
    /// here and none has on this line (libyaml's `save_simple_key`); no
    /// other may follow it.
    fn node(&mut self, col: usize) {
        if self.key_allowed && self.key.is_none() {
            self.key = Some(col);
        }
        self.key_allowed = false;
    }

    /// A token at `col`, in block context, closes the deeper block
    /// collections (`unroll_indent`).
    fn unroll(&mut self, col: usize) {
        while self.indents.last().is_some_and(|&top| top > col) {
            self.indents.pop();
        }
    }
}

fn blank(c: Option<&char>) -> bool {
    matches!(c, None | Some(' ' | '\t'))
}

fn flow_indicator(c: char) -> bool {
    matches!(c, ',' | '[' | ']' | '{' | '}')
}

/// A `---` or `...` at column 0, followed by white space or the end.
fn document_marker(line: &[char]) -> Option<char> {
    match line {
        [a, b, c, rest @ ..] if a == b && b == c && matches!(a, '-' | '.') => {
            blank(rest.first()).then_some(*a)
        }
        _ => None,
    }
}

/// `text` prepared for saphyr-parser, or the first tab libyaml refuses:
/// as it is when saphyr reads its first document already, else padded and
/// with tabs after a key's colon made spaces.
///
/// # Errors
///
/// A [`Refusal`] naming the tab's line and column.
pub fn prepare(text: &str) -> Result<Prepared, Refusal> {
    let rewritten = rewrite(text)?;
    if first_document_parses(text) {
        return Ok(Prepared {
            text: text.to_string(),
            pads: Vec::new(),
            before: Vec::new(),
            total: 0,
        });
    }
    Ok(rewritten)
}

/// Whether saphyr-parser reads `text`'s first document without an error
/// (as `Parser::load` with `multi` off would:
/// <https://docs.rs/saphyr-parser/0.3.3/saphyr_parser/struct.Parser.html#method.load>).
fn first_document_parses(text: &str) -> bool {
    Parser::new_from_str(text)
        .take_while(|event| !matches!(event, Ok((Event::DocumentEnd | Event::StreamEnd, _))))
        .all(|event| event.is_ok())
}

/// The pass itself: `text` padded and with tabs after a key's colon made
/// spaces, or the first tab libyaml refuses.
fn rewrite(text: &str) -> Result<Prepared, Refusal> {
    let mut scan = Scan {
        within: Within::Tokens,
        indents: Vec::new(),
        flow: 0,
        open: 0,
        key_allowed: true,
        key: None,
        outer: Vec::new(),
        started: false,
    };
    let mut out = String::with_capacity(text.len());
    let mut pads = Vec::new();
    let mut before = Vec::new();
    let mut total = 0;
    let mut done = false;
    for (index, (line, end)) in lines(text).into_iter().enumerate() {
        before.push(total);
        let mut chars: Vec<char> = line.chars().collect();
        // A byte order mark starts the stream, not a column (libyaml's
        // reader drops it).
        let bom = index == 0 && chars.first() == Some(&'\u{feff}');
        if bom {
            chars.remove(0);
        }
        let mut pad = 0;
        if !done {
            match scan_line(&mut scan, &mut chars, index + 1)? {
                Line::Pad(n) => pad = n,
                Line::Done => done = true,
            }
        }
        if total + pad > PAD_BUDGET {
            pad = 0;
        }
        pads.push(pad);
        total += pad;
        if bom {
            out.push('\u{feff}');
        }
        out.extend(std::iter::repeat_n(' ', pad));
        out.extend(chars);
        out.push_str(end);
    }
    Ok(Prepared {
        text: out,
        pads,
        before,
        total,
    })
}

/// `text`'s lines, each with its line break (YAML's: `\r\n`, `\r` or
/// `\n`; none at the end). Why not std's `str::lines`: it drops the break
/// and doesn't break at a lone `\r`, which saphyr counts as a line.
fn lines(text: &str) -> Vec<(&str, &str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for piece in text.split_inclusive(['\n', '\r']) {
        // `\r\n` is one break: its `\n` comes as a piece of its own.
        if piece == "\n"
            && let Some(last) = out.last_mut()
            && last.1 == "\r"
        {
            last.1 = "\r\n";
            continue;
        }
        let line = piece.trim_end_matches(['\n', '\r']);
        out.push((line, &piece[line.len()..]));
    }
    out
}

/// What became of a line.
enum Line {
    /// Padded with this many spaces (0: as it was; block scalar content
    /// always is).
    Pad(usize),
    /// The first document has ended here: this line and the rest are
    /// copied as they are.
    Done,
}

fn refuse(line: usize, col: usize, message: &'static str) -> Refusal {
    Refusal {
        line,
        column: col + 1,
        message,
    }
}

/// Scans `chars`, line `number`, turning a tab after a colon into a space;
/// says how much to pad it.
fn scan_line(scan: &mut Scan, chars: &mut [char], number: usize) -> Result<Line, Refusal> {
    let spaces = chars.iter().take_while(|&&c| c == ' ').count();
    let first = chars
        .iter()
        .position(|&c| c != ' ' && c != '\t')
        .unwrap_or(chars.len());
    let content = first < chars.len();
    scan.key = None;
    if let Within::BlockScalar { parent, indent } = scan.within {
        let tab = chars.get(spaces) == Some(&'\t');
        // The least content indentation: one past the parent's. At the
        // top, column 0 is content to saphyr (libyaml, which says 1,
        // refuses the stream then), so it is left alone.
        let least = parent.map_or(0, |col| col + 1);
        match indent {
            Some(n) if spaces >= n => return Ok(Line::Pad(0)),
            _ if tab => return Err(refuse(number, spaces, TAB_BLOCK_SCALAR)),
            _ if !content => return Ok(Line::Pad(0)),
            None if spaces >= least => {
                scan.within = Within::BlockScalar {
                    parent,
                    indent: Some(spaces),
                };
                return Ok(Line::Pad(0));
            }
            _ => scan.within = Within::Tokens,
        }
        // A line left of the content ends the block scalar: it starts
        // tokens in block context.
        scan.key_allowed = true;
    }
    if document_marker(chars).is_some() && (scan.flow > 0 || scan.within != Within::Tokens) {
        // A marker ends a plain scalar's document; inside a quoted scalar
        // or flow collection both parsers refuse it.
        return Ok(Line::Done);
    }
    let mut pad = 0;
    let continuing = scan.flow > 0 || matches!(scan.within, Within::Quoted { .. });
    if let Within::Plain { indent } = scan.within {
        if let Some(tab) = chars[..first].iter().position(|&c| c == '\t')
            && tab < indent
        {
            return Err(refuse(number, tab, TAB_INDENT));
        }
        if content && (chars[first] == '#' || (scan.flow == 0 && first < indent)) {
            scan.within = Within::Tokens;
            scan.key_allowed = true;
        }
    } else if scan.flow == 0 && scan.within == Within::Tokens {
        if let Some(tab) = chars[..first].iter().position(|&c| c == '\t') {
            return Err(refuse(number, tab, TAB_INDENT));
        }
        scan.key_allowed = true;
    }
    if continuing {
        // A continuation line's leading white space is separation, no part
        // of the value (YAML 1.2, 7.3), and saphyr refuses tabs in it.
        chars[..first].fill(' ');
    }
    if continuing && content {
        // A directive line in a flow collection is refused by both; a
        // quoted scalar can hold one.
        let directive = first == 0 && chars[0] == '%' && scan.within == Within::Tokens;
        if !directive && first <= scan.open {
            pad = scan.open + 1 - first;
        }
    }
    let mut at = first;
    if let Within::Quoted { double } = scan.within {
        match quoted_end(chars, at, double) {
            Some(end) => {
                scan.within = Within::Tokens;
                scan.key_allowed = false;
                at = end;
            }
            None => return Ok(Line::Pad(pad)),
        }
    } else if let Within::Plain { .. } = scan.within {
        // The plain scalar goes on: the rest of it is on this line.
        at = plain_end(scan, chars, at);
        if at >= chars.len() {
            return Ok(Line::Pad(pad));
        }
        scan.within = Within::Tokens;
    } else if content && scan.flow == 0 {
        if first == 0 {
            if chars[0] == '%' && !scan.started {
                return Ok(Line::Pad(0));
            }
            match document_marker(chars) {
                Some('-') if !scan.started => {
                    scan.started = true;
                    scan.indents.clear();
                    scan.key_allowed = false;
                    at = 3;
                }
                Some(_) => return Ok(Line::Done),
                None => {}
            }
        }
        // A comment is no token: it closes no collection.
        if chars.get(at) != Some(&'#') {
            scan.unroll(at);
        }
    }
    tokens(scan, chars, at, number)?;
    Ok(Line::Pad(pad))
}

/// Where the quoted scalar that `chars[at..]` continues ends (after its
/// closing quote), or `None` when it goes on past the line.
fn quoted_end(chars: &[char], at: usize, double: bool) -> Option<usize> {
    let quote = if double { '"' } else { '\'' };
    let mut rest = chars.iter().enumerate().skip(at);
    while let Some((i, &c)) = rest.next() {
        if double && c == '\\' {
            // An escape: the next character is the escaped one.
            rest.next();
        } else if c == quote {
            if double || chars.get(i + 1) != Some(&'\'') {
                return Some(i + 1);
            }
            // `''` is a quote in a single-quoted scalar.
            rest.next();
        }
    }
    None
}

/// Where the plain scalar at `chars[at]` ends: before a `:` indicator,
/// before white space and a comment, at a flow indicator in a flow
/// collection, or at the line's end (its trailing white space included).
fn plain_end(scan: &Scan, chars: &[char], at: usize) -> usize {
    let flow = scan.flow > 0;
    let ends = |&(i, &c): &(usize, &char)| {
        let next = chars.get(i + 1);
        let after_blank = i.checked_sub(1).is_some_and(|p| blank(chars.get(p)));
        (c == ':' && (blank(next) || (flow && next.is_some_and(|&n| flow_indicator(n)))))
            || (flow && flow_indicator(c))
            || (c == '#' && after_blank)
    };
    chars
        .iter()
        .enumerate()
        .skip(at)
        .find(ends)
        .map_or(chars.len(), |(i, _)| i)
}

/// Scans the tokens of `chars` from `at` to the line's end.
fn tokens(
    scan: &mut Scan,
    chars: &mut [char],
    mut at: usize,
    number: usize,
) -> Result<(), Refusal> {
    loop {
        let white = &chars[at..];
        let word = white
            .iter()
            .position(|&c| c != ' ' && c != '\t')
            .unwrap_or(white.len());
        if scan.flow == 0
            && scan.key_allowed
            && let Some(tab) = white[..word].iter().position(|&c| c == '\t')
        {
            return Err(refuse(number, at + tab, TAB_AFTER_INDICATOR));
        }
        at += word;
        // Each token moves on; anything else ends the line's scan.
        match token(scan, chars, at) {
            Some(next) if next > at => at = next,
            _ => return Ok(()),
        }
    }
}

/// Scans the token at `chars[at]`: where the next one may start, or `None`
/// when the line holds no more (a comment, a scalar going on past it, the
/// end).
fn token(scan: &mut Scan, chars: &mut [char], at: usize) -> Option<usize> {
    let c = *chars.get(at)?;
    let next = chars.get(at + 1).copied();
    let indicator = blank(next.as_ref());
    let flow = scan.flow > 0;
    if c == '#' {
        return None;
    }
    scan.started = true;
    match c {
        '[' | '{' => {
            scan.node(at);
            if !flow {
                scan.open = at;
            }
            // Each flow level has its own possible key (libyaml's
            // `simple_keys` stack): the collection may be the outer key.
            scan.outer.push(scan.key.take());
            scan.flow += 1;
            scan.key_allowed = true;
        }
        ']' | '}' => {
            scan.flow = scan.flow.saturating_sub(1);
            scan.key = scan.outer.pop().flatten();
            scan.key_allowed = false;
        }
        ',' if flow => scan.key_allowed = true,
        '-' | '?' if indicator && !flow => {
            scan.roll(at);
            scan.key_allowed = true;
            scan.key = None;
        }
        '?' if indicator => scan.key = None,
        // In a flow collection a `:` starting a token is always one
        // (after a quoted key: `{"k":"v"}`); a plain scalar's own `:`
        // never starts one.
        ':' if indicator || flow => {
            let simple = scan.key.take();
            if let Some(key) = simple {
                scan.roll(key);
                scan.key_allowed = false;
            } else {
                scan.key_allowed = !flow;
            }
            // After a simple key, or anywhere in a flow collection, libyaml
            // takes a tab as separation; after an explicit `:` in block
            // context it refuses one, which the caller checks.
            if next == Some('\t') && (simple.is_some() || flow) {
                chars[at + 1] = ' ';
            }
        }
        '|' | '>' if !flow => {
            block_scalar(scan, chars, at);
            return None;
        }
        '\'' | '"' => {
            scan.node(at);
            if let Some(end) = quoted_end(chars, at + 1, c == '"') {
                return Some(end);
            }
            if !flow {
                scan.open = at;
            }
            scan.within = Within::Quoted { double: c == '"' };
            return None;
        }
        '&' | '*' => {
            // An anchor's or alias's name: letters, digits, `_` and `-`
            // (libyaml's `yaml_parser_scan_anchor`).
            scan.node(at);
            let name = chars[at + 1..]
                .iter()
                .take_while(|&&c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                .count();
            return Some(at + 1 + name);
        }
        '!' => {
            scan.node(at);
            let name = chars[at + 1..]
                .iter()
                .take_while(|&&c| c != ' ' && c != '\t' && !(flow && flow_indicator(c)))
                .count();
            return Some(at + 1 + name);
        }
        _ => {
            scan.node(at);
            let end = plain_end(scan, chars, at + 1);
            if end < chars.len() {
                return Some(end);
            }
            scan.within = Within::Plain {
                indent: scan.plain_indent(),
            };
            return None;
        }
    }
    Some(at + 1)
}

/// A block scalar's header at `chars[at]`: its content starts on the next
/// line, indented by its indentation indicator past the block
/// indentation, or as its first non-empty line sets.
fn block_scalar(scan: &mut Scan, chars: &[char], at: usize) {
    let parent = scan.indent();
    let explicit = chars[at + 1..]
        .iter()
        .take_while(|&&c| c != ' ' && c != '\t')
        .find_map(|c| c.to_digit(10))
        .filter(|&n| n > 0)
        .and_then(|n| usize::try_from(n).ok());
    let indent = explicit.map(|n| parent.map_or(n, |p| p + n));
    scan.within = Within::BlockScalar { parent, indent };
    scan.key_allowed = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pass's rewrite, whether or not saphyr reads the text as it is.
    fn text(input: &str) -> String {
        rewrite(input)
            .unwrap_or_else(|refusal| panic!("{input:?}: {refusal}"))
            .text
    }

    fn refused(input: &str) -> String {
        prepare(input).expect_err(input).to_string()
    }

    /// The layouts saphyr refuses and libyaml accepts (config-H1): each
    /// continuation line comes out deeper than the construct's opening.
    #[test]
    fn continuation_lines_are_padded_past_the_opening() {
        assert_eq!(text("list: [\n]\n"), "list: [\n       ]\n");
        assert_eq!(text("map: {\n}\n"), "map: {\n      }\n");
        assert_eq!(text("t: \"a\nb\"\n"), "t: \"a\n    b\"\n");
        assert_eq!(text("t: 'a\n b'\n"), "t: 'a\n    b'\n");
        // Already deep enough: as it was.
        assert_eq!(text("l: [a,\n      b]\n"), "l: [a,\n      b]\n");
        // Nested: the outermost opening counts.
        assert_eq!(
            text("l: [[a,\nb], {c: \"d\ne\"}]\n"),
            "l: [[a,\n    b], {c: \"d\n    e\"}]\n"
        );
        // Blank lines stay blank; leading tabs become spaces.
        assert_eq!(text("t: 'a\n\n\tb'\n"), "t: 'a\n\n    b'\n");
        assert_eq!(text("l: [a,\n \t\n  \tb]\n"), "l: [a,\n  \n    b]\n");
    }

    /// Escapes and doubled quotes don't end a quoted scalar early.
    #[test]
    fn quotes_end_at_their_closing_quote() {
        assert_eq!(
            text("a: \"x\\\"\ny\"\nb: 1\n"),
            "a: \"x\\\"\n    y\"\nb: 1\n"
        );
        assert_eq!(text("a: 'it''s\nx'\nb: 1\n"), "a: 'it''s\n    x'\nb: 1\n");
        assert_eq!(
            text("a: \"x\\\ny\\\\\"\nb: [\n]\n"),
            "a: \"x\\\n    y\\\\\"\nb: [\n    ]\n"
        );
        // A quote inside a plain scalar opens nothing.
        assert_eq!(text("a: it's\nb: [\n]\n"), "a: it's\nb: [\n    ]\n");
    }

    /// A tab after a key's `:` is separation to libyaml: a space for saphyr.
    #[test]
    fn a_tab_after_a_key_colon_becomes_a_space() {
        assert_eq!(text("key:\tvalue\n"), "key: value\n");
        assert_eq!(text("a:\n  b:\t\tc\n"), "a:\n  b: \tc\n");
        assert_eq!(text("{a:\tb}\n"), "{a: b}\n");
        // Not inside a scalar or a comment, nor in a plain scalar's `:x`.
        assert_eq!(text("a: 'x:\ty'\n"), "a: 'x:\ty'\n");
        assert_eq!(text("a: b # c:\td\n"), "a: b # c:\td\n");
        assert_eq!(text("a: |\n  k:\tv\n"), "a: |\n  k:\tv\n");
    }

    /// libyaml's tab refusals (config-L1), at the tab's position.
    #[test]
    fn tabs_libyaml_refuses_are_refused() {
        let indent = "a tab in the indentation (YAML indents with spaces)";
        let after = "a tab after an indicator here (use a space)";
        assert_eq!(
            refused("a: 1\n\t\nb: 2\n"),
            format!("{indent} at line 2, column 1")
        );
        assert_eq!(refused("\ta: 1\n"), format!("{indent} at line 1, column 1"));
        assert_eq!(
            refused("a: \"1\"\n  \t\nb: 2\n"),
            format!("{indent} at line 2, column 3")
        );
        assert_eq!(
            refused("a: 1\n\t# c\n"),
            format!("{indent} at line 2, column 1")
        );
        assert_eq!(
            refused("list:\n  -\tvalue\n"),
            format!("{after} at line 2, column 4")
        );
        assert_eq!(refused("- \tx\n"), format!("{after} at line 1, column 3"));
        assert_eq!(
            refused("? a\n:\tb\n"),
            format!("{after} at line 2, column 2")
        );
        assert_eq!(refused("? \ta\n"), format!("{after} at line 1, column 3"));
    }

    /// A plain scalar's continuation may hold a tab from its indentation
    /// on (the block indentation plus one), as libyaml allows.
    #[test]
    fn a_plain_continuation_holds_tabs_past_its_indentation() {
        assert_eq!(text("a: x\n  \tb\n"), "a: x\n  \tb\n");
        assert_eq!(text("a: 1\n \t\nb: 2\n"), "a: 1\n \t\nb: 2\n");
        assert_eq!(text("a: 1\n  \t# c\nb: 2\n"), "a: 1\n  \t# c\nb: 2\n");
        let indent = "a tab in the indentation (YAML indents with spaces)";
        assert_eq!(
            refused("a:\n  b: 1\n  \t\n  c: 2\n"),
            format!("{indent} at line 3, column 3")
        );
        assert_eq!(
            refused("a:\n  b: 1\n\t\n"),
            format!("{indent} at line 3, column 1")
        );
        // In a flow collection too.
        assert_eq!(
            refused("a: [x\n\ty]\n"),
            format!("{indent} at line 2, column 1")
        );
        assert_eq!(text("a: [x\n \ty]\n"), "a: [x\n    y]\n");
        // A comment or a shallower line ends it: a tab there is refused.
        assert_eq!(
            refused("a: x # c\n \tb: 1\n"),
            format!("{indent} at line 2, column 2")
        );
        assert_eq!(
            refused("a:\n  b: x\nc: 1\n\t\n"),
            format!("{indent} at line 4, column 1")
        );
    }

    /// After a plain scalar ends on its line, the next line starts tokens.
    #[test]
    fn a_plain_scalar_ending_on_its_line_does_not_go_on() {
        assert_eq!(text("a: x:\n  y\n"), "a: x:\n  y\n");
        assert_eq!(text("[a, b]\n"), "[a, b]\n");
        let indent = "a tab in the indentation (YAML indents with spaces)";
        assert_eq!(
            refused("a: [b]\n \t\n"),
            format!("{indent} at line 2, column 2")
        );
        assert_eq!(
            refused("a: x  # c\n \t\n"),
            format!("{indent} at line 2, column 2")
        );
        assert_eq!(text("a: x\n  y: [\n]\n"), "a: x\n  y: [\n      ]\n");
    }

    /// Block scalars' contents are left alone, tabs and quotes included,
    /// and their indentation is checked as libyaml checks it.
    #[test]
    fn block_scalars_are_left_alone() {
        let kept = "a: |\n  x: \"\n  \ty:\tz\n\n  [\nb: 1\n";
        assert_eq!(text(kept), kept);
        let folded = "- >-\n  x\n   y\n- [\n]\n";
        assert_eq!(text(folded), "- >-\n  x\n   y\n- [\n   ]\n");
        // An indentation indicator sets the content's indentation.
        let explicit = "a:\n  b: |2\n      \tx\n    y\n  c: [\n]\n";
        assert_eq!(
            text(explicit),
            "a:\n  b: |2\n      \tx\n    y\n  c: [\n      ]\n"
        );
        assert_eq!(text("--- |1\n x\n"), "--- |1\n x\n");
        let message = "a tab in a block scalar's indentation (YAML indents with spaces)";
        assert_eq!(
            refused("a: |\n  x\n \ty\n"),
            format!("{message} at line 3, column 2")
        );
        assert_eq!(
            refused("a: |\n  \tx\n"),
            format!("{message} at line 2, column 3")
        );
        assert_eq!(
            refused("a: |\n\tx\n"),
            format!("{message} at line 2, column 1")
        );
        assert_eq!(
            refused("a: |2\n \tx\n"),
            format!("{message} at line 2, column 2")
        );
        // A shallower line ends it, and is checked as tokens.
        assert_eq!(
            text("a: |\n  x\nb: \"\nc\"\n"),
            "a: |\n  x\nb: \"\n    c\"\n"
        );
        let indent = "a tab in the indentation (YAML indents with spaces)";
        assert_eq!(
            refused("a: |\n  x\nb: 1\n\t\n"),
            format!("{indent} at line 4, column 1")
        );
        // A first line no deeper than the parent ends it at once.
        assert_eq!(
            text("a:\n  b: |\n  c: [\n]\n"),
            "a:\n  b: |\n  c: [\n      ]\n"
        );
        assert_eq!(text("a: >\nb: [\n]\n"), "a: >\nb: [\n    ]\n");
        // Top level: column 0 is content, as saphyr reads it.
        assert_eq!(text("|\nx\n[\n"), "|\nx\n[\n");
    }

    /// The first document only: the rest is copied as it is.
    #[test]
    fn only_the_first_document_is_prepared() {
        let rest = "a: 1\n---\n\tb: [\n]\n";
        assert_eq!(text(rest), rest);
        let ended = "a: 1\n...\n\tb\n";
        assert_eq!(text(ended), ended);
        // A leading marker, a directive and comments start the first.
        assert_eq!(
            text("%YAML 1.2\n# c\n---\na: [\n]\n"),
            "%YAML 1.2\n# c\n---\na: [\n    ]\n"
        );
        assert_eq!(text("--- [a,\nb]\n"), "--- [a,\n     b]\n");
        // A marker inside a scalar or flow collection is refused by both
        // parsers: left for saphyr to refuse.
        let inside = "a: \"x\n---\ny\"\n";
        assert_eq!(text(inside), inside);
        let flow = "a: [x,\n...\n]\n";
        assert_eq!(text(flow), flow);
        assert_eq!(text("a: x\n---\n\t\n"), "a: x\n---\n\t\n");
    }

    /// A directive line in a flow collection isn't padded: both parsers
    /// refuse it. In a quoted scalar it is text.
    #[test]
    fn a_directive_in_a_flow_collection_stays() {
        assert_eq!(text("a: [\n%x\n]\n"), "a: [\n%x\n    ]\n");
        assert_eq!(text("a: \"\n%x\"\n"), "a: \"\n    %x\"\n");
    }

    /// Block indentation follows keys, dashes and `?` as libyaml's does.
    #[test]
    fn block_indentation_follows_the_collections() {
        let indent = "a tab in the indentation (YAML indents with spaces)";
        // `- k: 1` nests the mapping at column 2: its plain value's
        // continuation can't hold a tab left of column 3.
        assert_eq!(
            refused("- k: 1\n  \t\n"),
            format!("{indent} at line 2, column 3")
        );
        assert_eq!(text("- k: 1\n   \t\n"), "- k: 1\n   \t\n");
        assert_eq!(
            refused("- x\n\t\n"),
            format!("{indent} at line 2, column 1")
        );
        assert_eq!(text("- x\n \t\n"), "- x\n \t\n");
        assert_eq!(
            refused("? x\n\t\n"),
            format!("{indent} at line 2, column 1")
        );
        // An anchor, a tag or a quoted key starts the key.
        assert_eq!(
            refused("  &a k: 1\n  \t\n"),
            format!("{indent} at line 2, column 3")
        );
        assert_eq!(
            refused("  !t k: 1\n  \t\n"),
            format!("{indent} at line 2, column 3")
        );
        assert_eq!(
            refused("  'k': 1\n  \t\n"),
            format!("{indent} at line 2, column 3")
        );
        assert_eq!(text("  &a k: 1\n   \t\n"), "  &a k: 1\n   \t\n");
        // A comment closes no collection; a key at a shallower column
        // closes the deeper ones.
        assert_eq!(
            refused("a:\n  b:\n# c\n    x\n  \t\n"),
            format!("{indent} at line 5, column 3")
        );
        assert_eq!(
            refused("a:\n    b: 1\nc: 1\n\t\n"),
            format!("{indent} at line 4, column 1")
        );
        // A plain scalar at the top has no indentation to be left of.
        assert_eq!(text("x\n\ty\n"), "x\n\ty\n");
        // An alias's name ends at white space, a flow indicator in flow.
        assert_eq!(text("a: *x\nb: [*y, [\n]]\n"), "a: *x\nb: [*y, [\n    ]]\n");
        // A shallower line closes the deeper collections: here a plain
        // scalar's continuation may hold a tab from column 1 on.
        assert_eq!(
            text("a:\n    b: 1\nc: 1\n \t\n"),
            "a:\n    b: 1\nc: 1\n \t\n"
        );
        // A key in a flow collection rolls nothing: `d`'s indentation is
        // still the top mapping's.
        assert_eq!(text("a: {b: c}\n    d\n  \t\n"), "a: {b: c}\n    d\n  \t\n");
        // A plain scalar in a flow collection runs to a flow indicator
        // (`#` inside it is no comment), so the collection closes.
        assert_eq!(
            text("aaaa: {k: x#y}\nb: [\n]\n"),
            "aaaa: {k: x#y}\nb: [\n    ]\n"
        );
        // Flow collections don't roll the block indentation.
        assert_eq!(
            refused("a: {b: 1}\n\t\n"),
            format!("{indent} at line 2, column 1")
        );
    }

    /// Positions in the padded text map back into the original.
    #[test]
    fn positions_map_back_to_the_original() {
        let prepared = prepare("a: [\nb,\n  c]\nd: 1\n").expect("prepares");
        assert_eq!(prepared.text, "a: [\n    b,\n    c]\nd: 1\n");
        let at = |index, line, col| {
            let m = prepared.original(&Marker::new(index, line, col));
            (m.index(), m.line(), m.col())
        };
        // Line 1 isn't padded.
        assert_eq!(at(3, 1, 3), (3, 1, 3));
        // Line 2 has 4 spaces before `b`: column 4 is column 0.
        assert_eq!(at(9, 2, 4), (5, 2, 0));
        assert_eq!(at(10, 2, 5), (6, 2, 1));
        // Inside the padding: the line's start.
        assert_eq!(at(6, 2, 1), (5, 2, 0));
        // Line 3 has 2 spaces of padding, after 4 above.
        assert_eq!(at(16, 3, 4), (10, 3, 2));
        // Lines after are moved by all 6.
        assert_eq!(at(19, 4, 0), (13, 4, 0));
        // The end of the stream, past the last line.
        assert_eq!(at(25, 5, 0), (19, 5, 0));
    }

    /// A text saphyr reads as it is goes to it as it is: the rewrite is
    /// for what saphyr refuses. The tab checks still apply.
    #[test]
    fn a_text_saphyr_reads_is_left_as_it_is() {
        for input in [
            "a: [x,\n    y]\n",
            "a: 'x\n  y'\nb: 1\n",
            "a: 1\n---\nb: [\n",
        ] {
            let prepared = prepare(input).expect("prepares");
            assert_eq!(prepared.text, input);
            let marker = prepared.original(&Marker::new(7, 2, 3));
            assert_eq!((marker.index(), marker.line(), marker.col()), (7, 2, 3));
        }
        // saphyr refuses the first: rewritten. The second has a tab
        // libyaml refuses, which saphyr would read.
        assert_eq!(
            prepare("a: [\n]\n").expect("prepares").text,
            "a: [\n    ]\n"
        );
        assert!(prepare("a: 1\n\t\nb: 2\n").is_err());
    }

    /// Padding stops at its budget: a crafted file can't grow without
    /// bound (security audit of Task 43).
    #[test]
    fn padding_is_capped() {
        let input = format!("{}[\n{}]\n", " ".repeat(5000), "1,\n".repeat(1000));
        let prepared = rewrite(&input).expect("prepares");
        assert!(prepared.total <= PAD_BUDGET, "{}", prepared.total);
        assert!(prepared.total > PAD_BUDGET - 5002, "{}", prepared.total);
        assert_eq!(prepared.text.len(), input.len() + prepared.total);
        // The prepared text is then saphyr's to refuse.
        assert!(prepare(&input).is_ok());
    }

    /// The debug form shows no config text (it may hold secrets).
    #[test]
    fn the_debug_form_shows_sizes_only() {
        let prepared = prepare("password: hunter2\n").expect("prepares");
        assert_eq!(
            format!("{prepared:?}"),
            "Prepared { bytes: 18, padding: 0, .. }"
        );
    }

    /// A `:` starting a token in a flow collection is a value indicator,
    /// so the quoted value after it is scanned as one.
    #[test]
    fn a_colon_after_a_quoted_flow_key_is_an_indicator() {
        let json = "x: {\"q\":\"a #b\", \"k\":'c:\td', 'e':\"[\"}\ny: [\n]\n";
        assert_eq!(
            text(json),
            "x: {\"q\":\"a #b\", \"k\":'c:\td', 'e':\"[\"}\ny: [\n    ]\n"
        );
        assert_eq!(text("x: [\"a\":\"b,\n]\"]\n"), "x: [\"a\":\"b,\n    ]\"]\n");
        // A tab after it is separation, after an explicit key too.
        assert_eq!(text("{\"a\":\tb}\n"), "{\"a\": b}\n");
        assert_eq!(text("{? a\n :\tb}\n"), "{? a\n : b}\n");
    }

    /// A flow collection can be a key: the `:` after it rolls the block
    /// indentation, and a tab after it is separation.
    #[test]
    fn a_flow_collection_key_keeps_its_place() {
        assert_eq!(text("{i: j}:\tv\n"), "{i: j}: v\n");
        assert_eq!(text("- [a, {b: c}]: \tv\n"), "- [a, {b: c}]: \tv\n");
        let indent = "a tab in the indentation (YAML indents with spaces)";
        assert_eq!(
            refused("  {i: j}: v\n  \t\n"),
            format!("{indent} at line 2, column 3")
        );
    }

    /// An anchor's or alias's name ends where libyaml's does; a tag's at
    /// white space.
    #[test]
    fn anchor_names_end_as_libyaml_ends_them() {
        let indent = "a tab in the indentation (YAML indents with spaces)";
        // `&a` then the plain `:b`: the plain scalar goes on.
        assert_eq!(text("- &a:b\n \tc\n"), "- &a:b\n \tc\n");
        // `&b` then a `:` with no key on its line: libyaml refuses it.
        assert_eq!(
            refused("a: &b:\tc\n"),
            "a tab after an indicator here (use a space) at line 1, column 7"
        );
        assert_eq!(text("a: !t:x\tc\n"), "a: !t:x\tc\n");
        assert_eq!(
            refused("- *a_-1\n\t\n"),
            format!("{indent} at line 2, column 1")
        );
    }

    /// Line breaks are kept as they were, `\r\n` and `\r` too, and a byte
    /// order mark isn't a column.
    #[test]
    fn breaks_and_a_byte_order_mark_are_kept() {
        assert_eq!(text("a: [\r\n]\r\n"), "a: [\r\n    ]\r\n");
        assert_eq!(text("a: [\r]\r"), "a: [\r    ]\r");
        assert_eq!(text("\u{feff}a: [\n]"), "\u{feff}a: [\n    ]");
        assert_eq!(
            refused("\u{feff}\ta: 1\n"),
            "a tab in the indentation (YAML indents with spaces) at line 1, column 1"
        );
        assert_eq!(text(""), "");
    }

    /// Untrusted text never panics the pass: every string of up to four
    /// characters from YAML's indicators, white space and breaks, and a
    /// prepared text maps every position back inside the original.
    #[test]
    fn no_short_input_panics() {
        let alphabet: Vec<char> = " \t\n\r-:?[]{},\"'#|>&*!%.\\a\u{feff}".chars().collect();
        let mut input = String::new();
        for len in 0..=4 {
            let mut counter = [0usize; 4];
            loop {
                input.clear();
                input.extend(counter[..len].iter().map(|&i| alphabet[i]));
                if let Ok(prepared) = prepare(&input) {
                    let chars = prepared.text.chars().count();
                    let original = prepared.original(&Marker::new(chars, 9, 99));
                    assert!(original.index() <= input.chars().count(), "{input:?}");
                }
                let Some(digit) = counter[..len].iter().rposition(|&i| i + 1 < alphabet.len())
                else {
                    break;
                };
                counter[digit] += 1;
                counter[digit + 1..len].fill(0);
            }
        }
    }

    /// The scan's tracking, edge by edge: each case reads differently if
    /// one rule slips (found by mutation testing).
    #[test]
    fn the_tracking_holds_at_its_edges() {
        let indent = "a tab in the indentation (YAML indents with spaces)";
        let block = "a tab in a block scalar's indentation (YAML indents with spaces)";
        // A plain continuation may hold a tab at its indentation exactly,
        // after spaces too.
        assert_eq!(text("a: x\n \tb\n"), "a: x\n \tb\n");
        assert_eq!(text("a:\n  b: 1\n   \t\n"), "a:\n  b: 1\n   \t\n");
        // A token at a collection's own column keeps it open.
        assert_eq!(
            refused("a:\n  b:\n  x\n  \t\n"),
            format!("{indent} at line 4, column 3")
        );
        // A plain scalar goes on at its indentation (a `[` there is
        // text) and ends left of it (a `- [` there is tokens).
        assert_eq!(text("a: x\n [\nb\n"), "a: x\n [\nb\n");
        assert_eq!(text("a: x\n- [\n]\n"), "a: x\n- [\n   ]\n");
        // A second `---` ends the first document after any value.
        assert_eq!(
            text("a: \"1\"\n---\n\tb: [\n]\n"),
            "a: \"1\"\n---\n\tb: [\n]\n"
        );
        // `-x` and `?x` are plain scalars, not indicators.
        assert_eq!(text("  -x\n  \t\n"), "  -x\n  \t\n");
        assert_eq!(text("  ?x\n  \t\n"), "  ?x\n  \t\n");
        // `%` starts a directive only before the document.
        assert_eq!(text("a: 1\n%x: [\n]\n"), "a: 1\n%x: [\n     ]\n");
        // In a block plain scalar a flow indicator is text.
        assert_eq!(text("a: x{y\nb: [\n]\n"), "a: x{y\nb: [\n    ]\n");
        assert_eq!(text("a: x y:z w\nb: [\n]\n"), "a: x y:z w\nb: [\n    ]\n");
        // In a flow collection `>` and `|` start no block scalar.
        assert_eq!(text("a: [>, b,\n  c]\nd: 1\n"), "a: [>, b,\n    c]\nd: 1\n");
        // Anchors, aliases and tags before a flow collection.
        assert_eq!(text("a: &x [\nb]\n"), "a: &x [\n       b]\n");
        assert_eq!(text("a: &x_y-9 [\nb]\n"), "a: &x_y-9 [\n           b]\n");
        assert_eq!(text("a: !t [\nb]\n"), "a: !t [\n       b]\n");
        assert_eq!(text("a: [!t, [\nb]]\n"), "a: [!t, [\n    b]]\n");
        assert_eq!(text("a: [*x, [\nb]]\n"), "a: [*x, [\n    b]]\n");
        // Quotes: a backslash escapes only in double quotes, and each
        // style closes at its own quote.
        assert_eq!(text("a: 'x\\'\nb: [\n]\n"), "a: 'x\\'\nb: [\n    ]\n");
        assert_eq!(text("a: 'x\"y'\nb: [\n]\n"), "a: 'x\"y'\nb: [\n    ]\n");
        assert_eq!(text("a: \"x'y\"\nb: [\n]\n"), "a: \"x'y\"\nb: [\n    ]\n");
        assert_eq!(
            text("a: \"x\\\"y\"\nb: [\n]\n"),
            "a: \"x\\\"y\"\nb: [\n    ]\n"
        );
        // A block scalar's indentation indicator is in its header, not
        // in a comment after it.
        assert_eq!(
            refused("a: | # 2\n    x\n  \tb\n"),
            format!("{block} at line 3, column 3")
        );
        assert_eq!(text("a: |2 # c\n  x\n"), "a: |2 # c\n  x\n");
    }

    /// Padding up to the budget exactly is allowed; one space more isn't.
    #[test]
    fn the_budget_is_inclusive() {
        // 1024 continuation lines padded by 1024 fill it exactly; the
        // closing line's padding is over it.
        let input = format!("{}[\n{}]\n", " ".repeat(1023), "1,\n".repeat(1024));
        let prepared = rewrite(&input).expect("prepares");
        assert_eq!(prepared.total, PAD_BUDGET);
        assert!(prepared.text.ends_with("\n]\n"));
    }
}
