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
//!   (`crates/config/tests/yaml-corpus`);
//! - what libyaml refuses and saphyr reads is refused (decision 38;
//!   docs/platform-notes/yaml-acceptance.md): a `:` with no key before it
//!   on its line (`yaml_parser_fetch_value` saves no key there, and the
//!   parser then finds none; an explicit key's `?` takes one such `:`),
//!   an anchor defined twice or whose name libyaml ends where saphyr
//!   doesn't, a flow plain scalar's `:` before a flow indicator, `|` or
//!   `>` starting a flow value, a `?` in a flow collection with no space
//!   after it, a tag not followed by a space, a `%YAML` directive other
//!   than 1.1 or 1.2 and any directive but `%YAML` and `%TAG`, a top-level
//!   block scalar's content at column 0, an end marker before any
//!   document, and a tab in a line's white space after the document's
//!   `...`.
//!
//! To do that the pass tracks what the scanner tracks: quotes, comments,
//! flow depth, block scalars (whose contents it leaves alone) and the
//! block indentation. It reads the first document only; the rest of the
//! file is copied as it is.
//!
//! Then the whole stream, as saphyr-parser reads the prepared text, must
//! be what Ansible's loader takes (hardening plan decision 38): one
//! document (`get_single_node`, which refuses another one, and libyaml's
//! parser, which refuses content after a document's end), scalar mapping
//! keys, only the tags its constructor knows (decision 41), and nesting
//! no deeper than [`MAX_DEPTH`]; before that the text
//! must hold no character libyaml's reader refuses, no NEL, LS or PS
//! (line breaks to libyaml, text to saphyr), and no byte order mark past
//! its first character. The pass limits depth only,
//! never length: the Saltbox inventory is as long as its owner makes it,
//! and cli-core reads it whole (`read_inventory`, decision 41).
//! Its events are loaded through [`Prepared::load`], which gives a block
//! scalar at the end of a file without a final line break libyaml's value.
//! A tree deeper than [`SHALLOW`] is loaded, walked and dropped on a
//! thread with a stack for [`MAX_DEPTH`] ([`with_stack`]): saphyr-parser's
//! `Parser::load`, the tree's clone (an alias) and drop, and sb's walks
//! recurse once per level.
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

use saphyr_parser::{
    Event, Marker, Parser, ScalarStyle, ScanError, Span, SpannedEventReceiver, Tag,
};

use super::MAX_FILE_BYTES;

/// How deep collections may nest, an alias counted as the collections it
/// copies (saphyr's loader clones them; sb's walks recurse into them).
/// Ansible's loader (`PyYAML` 6.0.3 with libyaml 0.2.5) has no limit of
/// its own: its composer recurses in C until the interpreter's 8 MiB
/// stack overflows and the process dies, past about 23,800 nested
/// mappings or 26,170 nested sequences (measured, with the method, in
/// docs/platform-notes/yaml-acceptance.md). sb reads up to a depth that
/// loader always took in the measurement, and refuses deeper.
pub const MAX_DEPTH: usize = 23_000;

/// Up to this depth a tree is handled on the caller's stack: sb's walks
/// held it on a test thread's 2 MiB before the deep stack came.
pub const SHALLOW: usize = 256;

/// The stack [`with_stack`] gives a deep tree: at about 23,800 levels,
/// validate-config's parse and tree took between 16 and 24 MiB in a
/// release build and between 64 and 128 MiB in a debug one (the tests'),
/// so each gets about three times the most measured. UNVERIFIED: that
/// Linux commits a thread's stack pages only as they are touched (demand
/// paging; the `pthread_attr_setstacksize(3)` page doesn't say), checked
/// against the deep tests' peak RSS (under 110 MB in a debug build); a
/// host with strict overcommit charges the whole reservation.
const DEEP_STACK: usize = if cfg!(debug_assertions) {
    384 << 20
} else {
    64 << 20
};

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
    /// The characters taken out of the lines above each line (comment
    /// lines emptied after a top-level block scalar: `top_scalar_ends`),
    /// then the whole count; empty when none were.
    removed: Vec<usize>,
    /// How deep its collections nest, aliases counted as what they copy.
    depth: usize,
    /// Its length in characters, as saphyr-parser's markers count them.
    chars: usize,
    /// Whether it ends, with no final line break, in a block scalar to
    /// which saphyr-parser adds one (`added_break`).
    eof_break: bool,
    /// libyaml's value of a top-level block scalar that a comment at
    /// column 0 ended before any content (`top_scalar_ends`).
    top_scalar: Option<String>,
}

/// The config text is the user's, secrets included: its debug form shows
/// sizes only.
impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("bytes", &self.text.len())
            .field("padding", &self.total)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

/// What Ansible's loader refuses (a tab where libyaml refuses one, a
/// second document, nesting too deep, saphyr-parser's own syntax errors),
/// at a 1-based line and column of the original text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at line {line}, column {column}")]
pub struct Refusal {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

const TAB_INDENT: &str = "a tab in the indentation (YAML indents with spaces)";
const TAB_AFTER_INDICATOR: &str = "a tab after an indicator here (use a space)";
const TAB_BLOCK_SCALAR: &str = "a tab in a block scalar's indentation (YAML indents with spaces)";
const ANCHOR_NAME: &str =
    "an anchor or alias name is letters, digits, `_` and `-`, ending at a space";
const FLOW_BLOCK_INDICATOR: &str =
    "a `|` or `>` can't start a value in a flow collection (quote the value)";
const FLOW_PLAIN_COLON: &str =
    "a `:` right before `,`, `?` or a bracket in a flow collection's plain value (quote the value)";
const TOP_BLOCK_SCALAR: &str =
    "a top-level block scalar's content must be indented (Ansible's parser ends it here)";
const AFTER_TOP_SCALAR: &str = "content after the top-level block scalar that the \
    comment above ended (Ansible's parser reads one node per document)";
const FLOW_QUESTION: &str =
    "a `?` in a flow collection must be followed by a space (quote a value that starts with `?`)";
const TAG_END: &str = "a tag must be followed by a space";
const YAML_VERSION: &str =
    "a %YAML directive for a version Ansible's parser doesn't read (1.1 or 1.2)";
const UNKNOWN_DIRECTIVE: &str = "an unknown directive (Ansible's parser reads %YAML and %TAG)";
const COLLECTION_KEY: &str = "a mapping key must be a scalar, not a collection";
const NO_KEY: &str = "a `:` with no key before it (a key must be on the `:`'s line)";
const LONE_END: &str = "a document end marker (`...`) with no document before it";
const SECOND_DOCUMENT: &str = "a second document (Ansible reads one document per file)";
const AFTER_END: &str = "content after the document's end marker (`...`)";
const NO_CONSTRUCTOR: &str = "a tag Ansible's YAML loader has no constructor for \
     (it takes the YAML types' tags, !unsafe and !vault)";

/// The tags Ansible's loader constructs, as resolved (`!!` is the core
/// prefix): `PyYAML` 6.0.3's `SafeConstructor` and ansible-core 2.21.3's
/// `AnsibleInstrumentedConstructor` and `AnsibleConstructor`
/// (`ansible/_internal/_yaml/_constructor.py`, their
/// `_register_constructors`), checked with its `AnsibleLoader` on libyaml
/// 0.2.5 (docs/platform-notes/yaml-acceptance.md). Any other tag is
/// `construct_undefined`'s error, and a non-specific `!` is resolved as
/// if untagged.
const CONSTRUCTED: [&str; 17] = [
    "tag:yaml.org,2002:binary",
    "tag:yaml.org,2002:bool",
    "tag:yaml.org,2002:float",
    "tag:yaml.org,2002:int",
    "tag:yaml.org,2002:map",
    "tag:yaml.org,2002:null",
    "tag:yaml.org,2002:omap",
    "tag:yaml.org,2002:pairs",
    "tag:yaml.org,2002:python/dict",
    "tag:yaml.org,2002:python/unicode",
    "tag:yaml.org,2002:seq",
    "tag:yaml.org,2002:set",
    "tag:yaml.org,2002:str",
    "tag:yaml.org,2002:timestamp",
    "!unsafe",
    "!vault",
    "!vault-encrypted",
];

/// Whether Ansible's loader constructs a node tagged `tag`: saphyr-parser
/// resolves a tag as libyaml does (its handle replaced by the `%TAG`
/// prefix or the default one, escapes decoded), so the handle and suffix
/// together are libyaml's tag. Why not serde-saphyr 2.0.0's
/// `Options::reject_unsupported_tags`: its set is fixed, without
/// `!unsafe`, `!vault`, `!!omap`, `!!pairs` or `!!set`, and it reads with
/// granit-parser, not the stream this pass checks.
fn constructed(tag: &Tag) -> bool {
    let full = format!("{}{}", tag.handle, tag.suffix);
    full == "!" || CONSTRUCTED.contains(&full.as_str())
}

impl Prepared {
    /// The text for saphyr-parser.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How deep its collections nest, an alias counted as the collections
    /// it copies: at most [`MAX_DEPTH`]. Past [`SHALLOW`], what is built
    /// from it is handled through [`with_stack`].
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// `marker`, a position in [`Prepared::text`], in the original text: a
    /// padded line's columns move left by its padding (a column inside
    /// the padding is the line's start), and the character index by all
    /// the padding before it, less what emptied lines above it lost.
    #[must_use]
    pub fn original(&self, marker: &Marker) -> Marker {
        let line = marker.line().saturating_sub(1);
        let pad = self.pads.get(line).copied().unwrap_or(0);
        let before = self.before.get(line).copied().unwrap_or(self.total);
        // One entry per line and the total after them, so every line the
        // parser names is in it.
        let removed = self.removed.get(line).copied().unwrap_or(0);
        let in_line = pad.min(marker.col());
        Marker::new(
            marker
                .index()
                .saturating_add(removed)
                .saturating_sub(before + in_line),
            marker.line(),
            marker.col() - in_line,
        )
    }

    /// The first document's events into `receiver`, as saphyr-parser's
    /// `Parser::load` gives them with `multi` off
    /// (<https://docs.rs/saphyr-parser/0.3.3/saphyr_parser/struct.Parser.html#method.load>;
    /// [`prepare`] has refused a second document), each scalar's text as
    /// libyaml reads it (`Prepared::libyaml`). Spans are this text's:
    /// [`Prepared::original`] moves them into the file.
    ///
    /// # Errors
    ///
    /// saphyr-parser's syntax error, which [`prepare`] has already seen.
    pub fn load<'input, R: SpannedEventReceiver<'input>>(
        &'input self,
        receiver: &mut R,
    ) -> Result<(), ScanError> {
        Parser::new_from_str(&self.text).load(
            &mut Libyaml {
                prepared: self,
                receiver,
            },
            false,
        )
    }

    /// `event`, at `span` in this text, with a scalar's text as libyaml
    /// reads it. One case differs: a block scalar at the end of a text
    /// with no final line break. libyaml 0.2.5 and sb-go's yaml.v3
    /// (go.yaml.in/yaml/v3 v3.0.5, `yaml_parser_scan_block_scalar` in
    /// `scannerc.go`: "Chomp the tail" adds only the line break it read,
    /// <https://github.com/yaml/go-yaml/blob/v3.0.5/scannerc.go>) keep no
    /// line break there; saphyr-parser 0.3.3's `scan_block_scalar` adds
    /// one, and its own case for a scalar with no content line adds one
    /// to a `|+` scalar whose only line is blank. The pass decides from
    /// the text whether it did ([`added_break`]); the scalar is then the
    /// one whose span reaches the text's end, and that break goes. Why
    /// not saphyr's own: neither it, 0.3.4, nor granit-parser 2.0.0 has
    /// an option for it. Checked against libyaml through `PyYAML` 6.0.3
    /// and against yaml.v3 3.0.5 (the platform note).
    fn libyaml<'input>(&self, event: Event<'input>, span: &Span) -> Event<'input> {
        // The top-level scalar a comment ended, the document's only scalar
        // (no collection holds it): libyaml's value, where saphyr reads on
        // over the emptied comment lines. Not found by its span: saphyr
        // starts it where it ends when `...` follows.
        if let Some(value) = &self.top_scalar
            && let Event::Scalar(
                _,
                style @ (ScalarStyle::Literal | ScalarStyle::Folded),
                anchor,
                tag,
            ) = event
        {
            return Event::Scalar(value.clone().into(), style, anchor, tag);
        }
        match event {
            Event::Scalar(
                text,
                style @ (ScalarStyle::Literal | ScalarStyle::Folded),
                anchor,
                tag,
            ) if self.eof_break && span.end.index() == self.chars && text.ends_with('\n') => {
                let mut text = text.into_owned();
                text.pop();
                Event::Scalar(text.into(), style, anchor, tag)
            }
            event => event,
        }
    }
}

/// A receiver that hands each event on as [`Prepared::libyaml`] reads it.
struct Libyaml<'a, R> {
    prepared: &'a Prepared,
    receiver: &'a mut R,
}

impl<'input, R: SpannedEventReceiver<'input>> SpannedEventReceiver<'input> for Libyaml<'_, R> {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        let event = self.prepared.libyaml(event, &span);
        self.receiver.on_event(event, span);
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
    /// line sets it (or its header's indentation indicator); whether a
    /// content line has come; its header's chomping indicator and line
    /// number.
    BlockScalar {
        parent: Option<usize>,
        indent: Option<usize>,
        seen: bool,
        chomp: Chomp,
        header: usize,
    },
}

/// A block scalar's chomping indicator: none (clip), `+` or `-`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chomp {
    Clip,
    Keep,
    Strip,
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
    /// The possible keys of the flow levels around this one, each with
    /// its level's pending `?`.
    outer: Vec<(Option<usize>, bool)>,
    /// The columns of the `?`s in block context whose `:` hasn't come,
    /// innermost last.
    questions: Vec<usize>,
    /// A `?` in this flow level whose `:` hasn't come.
    flow_question: bool,
    /// A document has started (`---`) or has content.
    started: bool,
    /// Each anchor defined so far, where (1-based line and column).
    anchors: std::collections::HashMap<String, (usize, usize)>,
    /// A top-level block scalar ended at a comment (`top_scalar_ends`):
    /// its value. Only comments, blank lines and an end marker may follow
    /// in the document.
    top_scalar: Option<String>,
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

    /// The `?`s right of `col` are answered or abandoned: a token at `col`
    /// is outside their entries.
    fn close_questions(&mut self, col: usize) {
        while self.questions.last().is_some_and(|&top| top >= col) {
            self.questions.pop();
        }
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

/// `text` prepared for saphyr-parser, or the first thing Ansible's loader
/// refuses in it: as it is when saphyr reads its first document already,
/// else padded and with tabs after a key's colon made spaces.
///
/// # Errors
///
/// A [`Refusal`] naming its line and column: a NEL, LS or PS, a tab or a
/// `:` libyaml refuses, a document end marker with no document, a
/// second document or content after the first one's end, nesting
/// deeper than [`MAX_DEPTH`], saphyr-parser's own syntax error, or a tag
/// Ansible's loader has no constructor for.
pub fn prepare(text: &str) -> Result<Prepared, Refusal> {
    characters(text)?;
    // The scan runs either way (it refuses what libyaml refuses); its
    // rewritten text is built only when saphyr can't read the original.
    let parses = first_document_parses(text);
    let mut rewritten = rewrite(text, !parses)?;
    // A top-level block scalar ended at a comment: saphyr reads the
    // comment as content, so its text is the rewritten one either way.
    let blanked = rewritten.top_scalar.is_some();
    if parses && blanked {
        rewritten = rewrite(text, true)?;
    }
    let mut prepared = if parses && !blanked {
        Prepared {
            text: text.to_string(),
            pads: Vec::new(),
            before: Vec::new(),
            total: 0,
            removed: Vec::new(),
            depth: 0,
            chars: 0,
            eof_break: rewritten.eof_break,
            top_scalar: None,
        }
    } else {
        rewritten
    };
    prepared.depth = stream(&prepared)?;
    prepared.chars = prepared.text.chars().count();
    Ok(prepared)
}

/// `f`, run where a tree `depth` levels deep can be built, walked and
/// dropped: on the caller's stack up to [`SHALLOW`], else on a thread with
/// a stack for [`MAX_DEPTH`] (`Builder::stack_size` and `spawn_scoped`:
/// <https://doc.rust-lang.org/std/thread/struct.Builder.html#method.spawn_scoped>).
/// A panic in `f` goes on in the caller.
///
/// # Errors
///
/// The thread couldn't be started (the system's thread or memory limits).
pub fn with_stack<T: Send>(depth: usize, f: impl FnOnce() -> T + Send) -> std::io::Result<T> {
    if depth <= SHALLOW {
        return Ok(f());
    }
    std::thread::scope(|scope| {
        // The caller's span goes along (`crate::spawn`).
        let builder = std::thread::Builder::new()
            .name("sb-yaml".to_string())
            .stack_size(DEEP_STACK);
        let handle = crate::spawn::scoped_thread(builder, scope, f)?;
        Ok(handle
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
    })
}

/// The first character in `text` that libyaml's reader refuses or reads
/// as saphyr doesn't. Its reader refuses control characters but tab and
/// the line breaks, and U+FFFE and U+FFFF ("control characters are not
/// allowed", `yaml_parser_update_buffer` in `reader.c`). NEL, LS and PS
/// are line breaks to libyaml (`IS_BREAK` in `yaml_private.h`:
/// <https://github.com/yaml/libyaml/blob/0.2.5/src/yaml_private.h>) and
/// text to saphyr: wherever one sits, a value or the structure around it
/// reads differently. A byte order mark (U+FEFF) is refused past the
/// stream's first character (decision 38): libyaml's reader drops only
/// that one (`yaml_parser_determine_encoding`), its scanner skips one at
/// a line's start but counts it as a column (`yaml_parser_scan_to_next_token`
/// in `scanner.c`), and elsewhere it is text. The pre-pass dropped only
/// the first and saphyr reads the others its own way, so some files
/// libyaml refuses read (the corpus's `bom-*` cases); one rule refuses
/// them all, the few libyaml reads as saphyr does too. Escaped in a double-quoted scalar each is
/// text to both. The message names the code point, never the text around
/// it; the stream's own mark is no column.
fn characters(text: &str) -> Result<(), Refusal> {
    let refused = |c: char| {
        matches!(c, '\0'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
            || matches!(
                c,
                '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}' | '\u{feff}'
            )
    };
    let mark = text
        .strip_prefix('\u{feff}')
        .map_or(0, |_| '\u{feff}'.len_utf8());
    let Some(at) = text[mark..].find(refused).map(|at| at + mark) else {
        return Ok(());
    };
    let before = &text[..at];
    let mut line = 1;
    let mut start = mark;
    for (i, c) in before.char_indices() {
        // `\r\n` is one break: its `\n` starts the line again.
        if c == '\n' || (c == '\r' && !before[i + 1..].starts_with('\n')) {
            line += 1;
            start = i + 1;
        } else if c == '\r' {
            start = i + 1;
        }
    }
    let found = text[at..].chars().next().unwrap_or_default();
    let code = u32::from(found);
    let name = match found {
        '\u{85}' => "NEL",
        '\u{2028}' => "LS",
        '\u{2029}' => "PS",
        _ => "",
    };
    let message = if found == '\u{feff}' {
        "U+FEFF, a byte order mark, is allowed only as the file's first character \
         (remove it, or in a double-quoted string write it as \\ufeff)"
            .to_string()
    } else if name.is_empty() {
        format!(
            "U+{code:04X}, a control character Ansible's YAML parser refuses \
             (in a double-quoted string, write it as \\u{code:04x})"
        )
    } else {
        format!(
            "U+{code:04X} ({name}) is a line break to Ansible's YAML parser and not to sb's \
             (in a double-quoted string, write it as \\u{code:04x})"
        )
    };
    Err(Refusal {
        line,
        column: before[start..].chars().count() + 1,
        message,
    })
}

/// The prepared text's whole stream, as saphyr-parser reads it: its
/// depth (an alias counted as the height of its anchor's collection, which
/// the loader copies), or what Ansible's loader refuses: a second
/// document, a collection as a mapping key, nesting past [`MAX_DEPTH`],
/// a syntax error, or a tag its constructor doesn't know ([`CONSTRUCTED`];
/// the first in the file, where `PyYAML` names the first it constructs,
/// which for two in different collections may be another).
///
/// Why not saphyr's own limits: saphyr-parser 0.3.3 has no depth or document option (only its `u8` flow level), and
/// `YamlLoader::alias_limit` counts nodes, not depth; serde-saphyr's
/// `check_yaml_budget` scans with another parser (granit-parser), counts
/// no alias heights and knows nothing of the pre-pass's positions.
fn stream(prepared: &Prepared) -> Result<usize, Refusal> {
    let at = |marker: &Marker, message: &str| {
        let marker = prepared.original(marker);
        Refusal {
            line: marker.line(),
            column: marker.col() + 1,
            message: message.to_string(),
        }
    };
    let mut open: Vec<Open> = Vec::new();
    // Each anchor's height.
    let mut heights: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut documents = 0;
    let mut deepest = 0;
    // The first tag Ansible's loader can't construct: its constructor runs
    // after the parser, so a syntax error anywhere comes first.
    let mut unconstructed = None;
    let mut top_scalars = 0;
    for event in Parser::new_from_str(prepared.text()) {
        let (event, span) = event.map_err(|err| at(err.marker(), err.info()))?;
        // A comment ended the top-level scalar (`top_scalar_ends`): the
        // first document must be that one scalar, its lines blank, or the
        // scan and saphyr have read the text apart and libyaml's value
        // can't stand in for saphyr's.
        if prepared.top_scalar.is_some() && documents == 1 {
            let blank = match &event {
                Event::Scalar(text, ScalarStyle::Literal | ScalarStyle::Folded, ..) => {
                    top_scalars += 1;
                    top_scalars == 1 && text.chars().all(|c| c == '\n' || c == ' ')
                }
                Event::MappingStart(..)
                | Event::SequenceStart(..)
                | Event::Scalar(..)
                | Event::Alias(_) => false,
                _ => true,
            };
            if !blank {
                return Err(at(&span.start, AFTER_TOP_SCALAR));
            }
        }
        if let Event::Scalar(.., Some(tag))
        | Event::MappingStart(_, Some(tag))
        | Event::SequenceStart(_, Some(tag)) = &event
            && unconstructed.is_none()
            && !constructed(tag)
        {
            unconstructed = Some(at(&span.start, NO_CONSTRUCTOR));
        }
        // A collection (or an alias to one) where a mapping wants a key:
        // `PyYAML`'s constructor refuses it ("found unhashable key"), and
        // saphyr's loader would hash it whole at every level.
        let as_key = open
            .last()
            .is_some_and(|parent| parent.key_next == Some(true));
        let height = match event {
            Event::DocumentStart(explicit) => {
                documents += 1;
                if documents > 1 {
                    let message = if explicit { SECOND_DOCUMENT } else { AFTER_END };
                    return Err(at(&span.start, message));
                }
                continue;
            }
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                if as_key {
                    return Err(at(&span.start, COLLECTION_KEY));
                }
                let mapping = matches!(event, Event::MappingStart(..));
                open.push(Open {
                    anchor,
                    tallest: 0,
                    key_next: mapping.then_some(true),
                });
                if open.len() > MAX_DEPTH {
                    return Err(at(&span.start, &too_deep()));
                }
                deepest = deepest.max(open.len());
                continue;
            }
            Event::MappingEnd | Event::SequenceEnd => {
                let Some(ended) = open.pop() else {
                    continue;
                };
                if ended.anchor != 0 {
                    heights.insert(ended.anchor, ended.tallest + 1);
                }
                ended.tallest + 1
            }
            Event::Alias(anchor) => {
                let height = heights.get(&anchor).copied().unwrap_or(0);
                if as_key && height > 0 {
                    return Err(at(&span.start, COLLECTION_KEY));
                }
                if open.len() + height > MAX_DEPTH {
                    return Err(at(&span.start, &too_deep()));
                }
                deepest = deepest.max(open.len() + height);
                height
            }
            Event::Scalar(..) => 0,
            _ => continue,
        };
        // A node has ended in its parent: a mapping's key gives way to its
        // value, and its value to the next key.
        if let Some(parent) = open.last_mut() {
            parent.tallest = parent.tallest.max(height);
            parent.key_next = parent.key_next.map(|key| !key);
        }
    }
    unconstructed.map_or(Ok(deepest), Err)
}

/// An open collection in [`stream`].
struct Open {
    /// Its anchor (0: none).
    anchor: usize,
    /// The tallest node in it so far.
    tallest: usize,
    /// A mapping's: whether a key comes next. `None` for a sequence.
    key_next: Option<bool>,
}

fn too_deep() -> String {
    format!("nested deeper than {MAX_DEPTH} levels")
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
/// spaces, or the first tab libyaml refuses. Without `build`, only the
/// scan runs: the text and its padding are left empty, and only its
/// refusals and `eof_break` count.
fn rewrite(text: &str, build: bool) -> Result<Prepared, Refusal> {
    let mut scan = Scan {
        within: Within::Tokens,
        indents: Vec::new(),
        flow: 0,
        open: 0,
        key_allowed: true,
        key: None,
        outer: Vec::new(),
        questions: Vec::new(),
        flow_question: false,
        started: false,
        anchors: std::collections::HashMap::new(),
        top_scalar: None,
    };
    let mut out = String::with_capacity(if build { text.len() } else { 0 });
    let mut pads = Vec::new();
    let mut before = Vec::new();
    let mut total = 0;
    let mut removed = Vec::new();
    let mut gone = 0;
    let mut done = false;
    // The first document ended at a `...` line.
    let mut after_end = false;
    let mut eof_break = false;
    for (index, (line, end)) in lines(text).enumerate() {
        if build {
            before.push(total);
            removed.push(gone);
        }
        let mut chars: Vec<char> = line.chars().collect();
        // A byte order mark starts the stream, not a column (libyaml's
        // reader drops it).
        let bom = index == 0 && chars.first() == Some(&'\u{feff}');
        if bom {
            chars.remove(0);
        }
        let mut pad = 0;
        let mut emptied = false;
        if !done {
            match scan_line(&mut scan, &mut chars, index + 1)? {
                Line::Pad(n) => pad = n,
                Line::Blank => {
                    gone += chars.len();
                    chars.clear();
                    emptied = true;
                }
                Line::Done => {
                    done = true;
                    after_end = document_marker(&chars) == Some('.');
                }
            }
        } else if document_marker(&chars) == Some('-') {
            // A second document: refused whole, after this pass.
            after_end = false;
        } else if after_end {
            // Between documents a line's white space is block context's:
            // libyaml takes no tab in it.
            let first = chars.iter().position(|&c| c != ' ' && c != '\t');
            if let Some(tab) = chars[..first.unwrap_or(chars.len())]
                .iter()
                .position(|&c| c == '\t')
            {
                return Err(refuse(index + 1, tab, TAB_INDENT));
            }
        }
        if total + pad > PAD_BUDGET {
            pad = 0;
        }
        total += pad;
        // Only the last line has no break.
        eof_break = !done && end.is_empty() && added_break(&scan, &chars, index + 1);
        if build {
            pads.push(pad);
            if bom {
                out.push('\u{feff}');
            }
            // A lone `\r` before an emptied line ending in `\n` would make
            // one break of two: it becomes a `\n`, the count unchanged.
            if emptied && end.starts_with('\n') && out.ends_with('\r') {
                out.pop();
                out.push('\n');
            }
            out.extend(std::iter::repeat_n(' ', pad));
            out.extend(chars);
            out.push_str(end);
        }
    }
    // Past the last line, all that was taken out; nothing taken out, no
    // list to keep.
    if gone == 0 {
        removed = Vec::new();
    } else {
        removed.push(gone);
    }
    Ok(Prepared {
        text: out,
        pads,
        before,
        total,
        removed,
        depth: 0,
        chars: 0,
        eof_break,
        top_scalar: scan.top_scalar,
    })
}

/// `text`'s lines, each with its line break (YAML's: `\r\n`, `\r` or
/// `\n`; none at the end), one at a time. Why not std's `str::lines`: it
/// drops the break and doesn't break at a lone `\r`, which saphyr counts
/// as a line.
fn lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (line, end) = match rest.find(['\n', '\r']) {
            Some(at) => {
                let width = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..at], &rest[at..at + width])
            }
            None => (rest, ""),
        };
        rest = &rest[line.len() + end.len()..];
        Some((line, end))
    })
}

/// What became of a line.
enum Line {
    /// Padded with this many spaces (0: as it was; block scalar content
    /// always is).
    Pad(usize),
    /// The first document has ended here: this line and the rest are
    /// copied as they are.
    Done,
    /// Emptied, its line break kept: a comment after a top-level block
    /// scalar that libyaml has ended and saphyr would read on
    /// (`top_scalar_ends`).
    Blank,
}

fn refuse(line: usize, col: usize, message: &str) -> Refusal {
    Refusal {
        line,
        column: col + 1,
        message: message.to_string(),
    }
}

/// Scans `chars`, line `number`, turning a tab after a colon into a space;
/// says how much to pad it.
fn scan_line(scan: &mut Scan, chars: &mut [char], number: usize) -> Result<Line, Refusal> {
    let first = chars
        .iter()
        .position(|&c| c != ' ' && c != '\t')
        .unwrap_or(chars.len());
    let content = first < chars.len();
    scan.key = None;
    // A possible key lasts one line, in every flow level (libyaml's
    // `yaml_parser_stale_simple_keys`).
    for (key, _) in &mut scan.outer {
        *key = None;
    }
    if scan.top_scalar.is_some() {
        return after_top_scalar(chars, first, number);
    }
    if let Within::BlockScalar { parent, indent, .. } = scan.within
        && block_scalar_line(scan, chars, parent, indent, number)?
    {
        // The comment that ended a top-level scalar goes too.
        return Ok(if scan.top_scalar.is_some() {
            Line::Blank
        } else {
            Line::Pad(0)
        });
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
        let end = plain_end(scan, chars, at);
        flow_plain_colon(scan, chars, at, end, number)?;
        at = end;
        if at >= chars.len() {
            return Ok(Line::Pad(pad));
        }
        scan.within = Within::Tokens;
    } else if content && scan.flow == 0 {
        if first == 0 {
            if chars[0] == '%' && !scan.started {
                directive(chars, number)?;
                return Ok(Line::Pad(0));
            }
            match document_marker(chars) {
                Some('-') if !scan.started => {
                    scan.started = true;
                    scan.indents.clear();
                    scan.key_allowed = false;
                    at = 3;
                }
                // libyaml's parser wants a node before an end marker.
                Some('.') if !scan.started => return Err(refuse(number, 0, LONE_END)),
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

/// A line, `number`, after the top-level scalar a comment ended
/// (`top_scalar_ends`), as libyaml reads it: a comment or blank line is
/// emptied (saphyr would read it as the scalar's content), `...` ends the
/// document, `---` starts a second one, and anything else is a second
/// node. A tab in its leading white space is refused there: libyaml is
/// back in block context. `first` is its first character that isn't
/// white space.
fn after_top_scalar(chars: &[char], first: usize, number: usize) -> Result<Line, Refusal> {
    if let Some(tab) = chars[..first].iter().position(|&c| c == '\t') {
        return Err(refuse(number, tab, TAB_INDENT));
    }
    match document_marker(chars) {
        Some('-') => Err(refuse(number, 0, SECOND_DOCUMENT)),
        Some(_) => Ok(Line::Done),
        None if chars.get(first).is_some_and(|&c| c != '#') => {
            Err(refuse(number, first, AFTER_TOP_SCALAR))
        }
        None => Ok(Line::Blank),
    }
}

/// A line under a block scalar's header, on line `number`: whether it is
/// the scalar's content (copied as it is). A line left of the content
/// ends the scalar and starts tokens in block context.
fn block_scalar_line(
    scan: &mut Scan,
    chars: &[char],
    parent: Option<usize>,
    indent: Option<usize>,
    number: usize,
) -> Result<bool, Refusal> {
    let spaces = chars.iter().take_while(|&&c| c == ' ').count();
    let content = chars.iter().any(|&c| c != ' ' && c != '\t');
    let tab = chars.get(spaces) == Some(&'\t');
    // The least content indentation: one past the parent's. At the top,
    // column 0 is content to saphyr; libyaml says 1, and ends the scalar
    // there, so content at column 0 is refused.
    let least = parent.map_or(0, |col| col + 1);
    match indent {
        Some(n) if spaces >= n => {
            // A line past the indentation is content, spaces or not
            // (saphyr's and libyaml's content loop starts at the column).
            if chars.len() > n
                && let Within::BlockScalar { seen, .. } = &mut scan.within
            {
                *seen = true;
            }
            return Ok(true);
        }
        _ if tab => return Err(refuse(number, spaces, TAB_BLOCK_SCALAR)),
        _ if !content => return Ok(true),
        // A `---` there starts a second document in libyaml, where saphyr
        // reads it as the scalar's content.
        None if parent.is_none() && document_marker(chars) == Some('-') => {
            return Err(refuse(number, 0, SECOND_DOCUMENT));
        }
        None if parent.is_none() && spaces == 0 && chars[0] == '#' => {
            top_scalar_ends(scan, number);
            return Ok(true);
        }
        None if parent.is_none()
            && spaces == 0
            && chars[0] != '#'
            && document_marker(chars).is_none() =>
        {
            return Err(refuse(number, 0, TOP_BLOCK_SCALAR));
        }
        // An end marker ends the scalar (saphyr ends it there too): what
        // follows is between documents.
        None if parent.is_none() && document_marker(chars) == Some('.') => {
            scan.within = Within::Tokens;
        }
        None if spaces >= least => {
            if let Within::BlockScalar { indent, seen, .. } = &mut scan.within {
                *indent = Some(spaces);
                *seen = true;
            }
            return Ok(true);
        }
        _ => scan.within = Within::Tokens,
    }
    scan.key_allowed = true;
    Ok(false)
}

/// A comment at column 0 under a top-level block scalar with no content
/// yet, on line `number`: libyaml's least indentation at the top is 1, so
/// the comment ends the scalar there, which then holds the blank lines
/// above it, kept only by `+` (libyaml 0.2.5, `yaml_parser_scan_block_scalar`
/// in `src/scanner.c`; checked through `PyYAML` 6.0.3 on the corpus).
/// saphyr's least indentation there is 0, so the comment would be content.
fn top_scalar_ends(scan: &mut Scan, number: usize) {
    if let Within::BlockScalar { chomp, header, .. } = scan.within {
        let breaks = number - header - 1;
        let value = if chomp == Chomp::Keep {
            "\n".repeat(breaks)
        } else {
            String::new()
        };
        scan.top_scalar = Some(value);
    }
    scan.within = Within::Tokens;
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

/// In a flow collection, a plain scalar from `chars[from]` to `end` (and
/// the `:` that may end it) holding a `:` right before `,`, `?`, `[`, `]`,
/// `{` or `}`: libyaml refuses it ("found unexpected ':'" in
/// `yaml_parser_scan_plain_scalar`), saphyr reads a key and its value.
fn flow_plain_colon(
    scan: &Scan,
    chars: &[char],
    from: usize,
    end: usize,
    number: usize,
) -> Result<(), Refusal> {
    if scan.flow == 0 {
        return Ok(());
    }
    let last = end.min(chars.len().saturating_sub(1));
    match (from..=last).find(|&i| {
        chars.get(i) == Some(&':')
            && matches!(chars.get(i + 1), Some(',' | '?' | '[' | ']' | '{' | '}'))
    }) {
        Some(i) => Err(refuse(number, i, FLOW_PLAIN_COLON)),
        None => Ok(()),
    }
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
        match token(scan, chars, at, number)? {
            Some(next) if next > at => at = next,
            _ => return Ok(()),
        }
    }
}

/// Scans the token at `chars[at]`, on line `number`: where the next one may
/// start, or `None` when the line holds no more (a comment, a scalar going
/// on past it, the end).
fn token(
    scan: &mut Scan,
    chars: &mut [char],
    at: usize,
    number: usize,
) -> Result<Option<usize>, Refusal> {
    let Some(&c) = chars.get(at) else {
        return Ok(None);
    };
    let next = chars.get(at + 1).copied();
    let indicator = blank(next.as_ref());
    let flow = scan.flow > 0;
    if c == '#' {
        return Ok(None);
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
            scan.outer.push((scan.key.take(), scan.flow_question));
            scan.flow_question = false;
            scan.flow += 1;
            scan.key_allowed = true;
        }
        ']' | '}' => {
            scan.flow = scan.flow.saturating_sub(1);
            let (key, question) = scan.outer.pop().unwrap_or_default();
            scan.key = key;
            scan.flow_question = question;
            scan.key_allowed = false;
        }
        ',' if flow => {
            // An entry's end: no key or `?` reaches past it.
            scan.key_allowed = true;
            scan.key = None;
            scan.flow_question = false;
        }
        '-' | '?' if indicator && !flow => {
            scan.roll(at);
            scan.key_allowed = true;
            scan.key = None;
            // A `-` left of a `?`'s column starts another entry; a `?` at
            // or left of one starts another key.
            scan.close_questions(at + usize::from(c == '-'));
            if c == '?' {
                scan.questions.push(at);
            }
        }
        '?' if indicator => {
            scan.key = None;
            scan.flow_question = true;
        }
        // libyaml takes `?` for a key in a flow collection whatever
        // follows it, saphyr for a plain scalar's start.
        '?' if flow => return Err(refuse(number, at, FLOW_QUESTION)),
        // In a flow collection a `:` starting a token is always one
        // (after a quoted key: `{"k":"v"}`); a plain scalar's own `:`
        // never starts one.
        ':' if indicator || flow => colon(scan, chars, at, number)?,
        '|' | '>' if !flow => {
            block_scalar(scan, chars, at, number);
            return Ok(None);
        }
        // In a flow collection libyaml starts no block scalar and no plain
        // scalar at `|` or `>`; saphyr reads a plain one.
        '|' | '>' => return Err(refuse(number, at, FLOW_BLOCK_INDICATOR)),
        '\'' | '"' => {
            scan.node(at);
            if let Some(end) = quoted_end(chars, at + 1, c == '"') {
                return Ok(Some(end));
            }
            if !flow {
                scan.open = at;
            }
            scan.within = Within::Quoted { double: c == '"' };
            return Ok(None);
        }
        '&' | '*' => return anchor(scan, chars, at, number).map(Some),
        '!' => return tag(scan, chars, at, number).map(Some),
        _ => {
            scan.node(at);
            let end = plain_end(scan, chars, at + 1);
            flow_plain_colon(scan, chars, at, end, number)?;
            if end < chars.len() {
                return Ok(Some(end));
            }
            scan.within = Within::Plain {
                indent: scan.plain_indent(),
            };
            return Ok(None);
        }
    }
    Ok(Some(at + 1))
}

/// A `:` value indicator at `chars[at]`, on line `number`: it closes a
/// simple key, or answers an explicit one; with neither it is refused.
fn colon(scan: &mut Scan, chars: &mut [char], at: usize, number: usize) -> Result<(), Refusal> {
    let flow = scan.flow > 0;
    let next = chars.get(at + 1).copied();
    let simple = scan.key.take();
    if let Some(key) = simple {
        scan.roll(key);
        scan.key_allowed = false;
        // A key at or left of a `?`'s column ends its entry.
        if !flow {
            scan.close_questions(key);
        }
    } else {
        // No key on this line: only an explicit key's `:` (its
        // `?` at this column in block context, or in this flow
        // level), as libyaml's parser takes it. Where no key may
        // start in block context, saphyr refuses it already.
        let explicit = if flow {
            std::mem::take(&mut scan.flow_question)
        } else if scan.key_allowed {
            scan.close_questions(at + 1);
            scan.questions.pop() == Some(at)
        } else {
            true
        };
        if !explicit {
            return Err(refuse(number, at, NO_KEY));
        }
        scan.key_allowed = !flow;
    }
    // After a simple key, or anywhere in a flow collection, libyaml
    // takes a tab as separation; after an explicit `:` in block
    // context it refuses one, which the caller checks.
    if next == Some('\t') && (simple.is_some() || flow) {
        chars[at + 1] = ' ';
    }
    Ok(())
}

/// An anchor (`&`) or alias (`*`) at `chars[at]`, on line `number`: where
/// its name ends.
fn anchor(scan: &mut Scan, chars: &[char], at: usize, number: usize) -> Result<usize, Refusal> {
    let c = chars[at];
    // An anchor's or alias's name: letters, digits, `_` and `-`,
    // then white space or an indicator libyaml allows there
    // (`yaml_parser_scan_anchor`), where saphyr takes more.
    scan.node(at);
    let name: String = chars[at + 1..]
        .iter()
        .take_while(|&&c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        .collect();
    let end = at + 1 + name.len();
    // libyaml also ends a name at `?`, `:`, `%`, `@` and a
    // backtick, which saphyr takes into it: refused, as the two
    // would read different names.
    let ends = |c: &char| matches!(c, ',' | ']' | '}');
    if name.is_empty() || !(blank(chars.get(end)) || chars.get(end).is_some_and(ends)) {
        return Err(refuse(number, end, ANCHOR_NAME));
    }
    if c == '&' {
        // The composer refuses a name defined twice (`PyYAML`'s
        // `compose_node`: "found duplicate anchor").
        if let Some(&(line, column)) = scan.anchors.get(&name) {
            return Err(Refusal {
                line: number,
                column: at + 1,
                // Not its name, which may be a value written twice
                // (`pass: &secret`): positions only.
                message: format!("an anchor defined again (first at line {line}, column {column})"),
            });
        }
        scan.anchors.insert(name, (number, at + 1));
    }
    Ok(end)
}

/// A tag at `chars[at]`, on line `number`: where it ends. libyaml reads a
/// handle and suffix of URI characters (`,`, `[` and `]` among them only
/// in block context), or `!<…>`, and wants white space after it, or `,` in
/// a flow collection (`yaml_parser_scan_tag`, `yaml_parser_scan_tag_uri`).
fn tag(scan: &mut Scan, chars: &[char], at: usize, number: usize) -> Result<usize, Refusal> {
    scan.node(at);
    let flow = scan.flow > 0;
    let end = if chars.get(at + 1) == Some(&'<') {
        chars[at..]
            .iter()
            .position(|&c| c == '>')
            .map_or(chars.len(), |close| at + close + 1)
    } else {
        let uri = |c: &char| {
            c.is_ascii_alphanumeric()
                || ";/?:@&=+$.%!~*'()-_".contains(*c)
                || (!flow && matches!(c, ',' | '[' | ']'))
        };
        at + 1 + chars[at + 1..].iter().take_while(|c| uri(c)).count()
    };
    if blank(chars.get(end)) || (flow && chars.get(end) == Some(&',')) {
        Ok(end)
    } else {
        Err(refuse(number, end, TAG_END))
    }
}

/// A directive line before the document: `%YAML` 1.1 or 1.2, or `%TAG`,
/// as libyaml reads them (`yaml_parser_scan_directive`, and its parser's
/// version check).
fn directive(chars: &[char], number: usize) -> Result<(), Refusal> {
    let line: String = chars.iter().collect();
    let mut words = line[1..].split([' ', '\t']).filter(|w| !w.is_empty());
    match words.next() {
        Some("TAG") => Ok(()),
        Some("YAML") if matches!(words.next(), Some("1.1" | "1.2")) => Ok(()),
        Some("YAML") => Err(refuse(number, 0, YAML_VERSION)),
        _ => Err(refuse(number, 0, UNKNOWN_DIRECTIVE)),
    }
}

/// A block scalar's header at `chars[at]`, on line `header`: its content
/// starts on the next line, indented by its indentation indicator past the
/// block indentation, or as its first non-empty line sets.
fn block_scalar(scan: &mut Scan, chars: &[char], at: usize, header: usize) {
    let parent = scan.indent();
    let indicators = || {
        chars[at + 1..]
            .iter()
            .take_while(|&&c| c != ' ' && c != '\t')
    };
    let explicit = indicators()
        .find_map(|c| c.to_digit(10))
        .filter(|&n| n > 0)
        .and_then(|n| usize::try_from(n).ok());
    let indent = explicit.map(|n| parent.map_or(n, |p| p + n));
    let chomp = match indicators().find(|&&c| c == '+' || c == '-') {
        Some('+') => Chomp::Keep,
        Some(_) => Chomp::Strip,
        None => Chomp::Clip,
    };
    scan.within = Within::BlockScalar {
        parent,
        indent,
        seen: false,
        chomp,
        header,
    };
    scan.key_allowed = true;
}

/// Whether saphyr-parser 0.3.3 gives the block scalar that `scan` is in at
/// the end of a text whose last line, `last` (line `number`), has no line
/// break, one more line break than libyaml. Its `scan_block_scalar` adds
/// one there when the last line reaches the content's indentation, unless
/// the scalar strips its breaks ("Chomp the tail"), and gives a `|+`
/// scalar with no content line `"\n"` when its one line after the header
/// is blank (its early return for a scalar that ends before content).
fn added_break(scan: &Scan, last: &[char], number: usize) -> bool {
    let Within::BlockScalar {
        indent,
        seen,
        chomp,
        header,
        ..
    } = scan.within
    else {
        return false;
    };
    match indent {
        _ if chomp == Chomp::Strip => false,
        Some(indent) if seen => last.len() >= indent.max(1),
        _ => chomp == Chomp::Keep && number == header + 1 && !last.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pass's rewrite, whether or not saphyr reads the text as it is.
    fn text(input: &str) -> String {
        rewrite(input, true)
            .unwrap_or_else(|refusal| panic!("{input:?}: {refusal}"))
            .text
    }

    fn refused(input: &str) -> String {
        prepare(input).expect_err(input).to_string()
    }

    /// Lines keep their breaks: `\n`, `\r\n` and a lone `\r` (a line to
    /// saphyr), none on a last line without one, nothing for no text.
    #[test]
    fn lines_keep_their_breaks() {
        let all: Vec<_> = lines("a\nb\r\nc\rd\r\n\r\ne").collect();
        assert_eq!(
            all,
            [
                ("a", "\n"),
                ("b", "\r\n"),
                ("c", "\r"),
                ("d", "\r\n"),
                ("", "\r\n"),
                ("e", "")
            ]
        );
        assert_eq!(lines("x\n").collect::<Vec<_>>(), [("x", "\n")]);
        assert_eq!(lines("\r").collect::<Vec<_>>(), [("", "\r")]);
        assert_eq!(lines("").count(), 0);
    }

    /// A text saphyr reads as it is isn't rebuilt: the scan alone runs,
    /// and still refuses what libyaml refuses.
    #[test]
    fn the_scan_alone_builds_nothing() {
        let scanned = rewrite("a:\tb\nc: [\n 1]\n", false).expect("scans");
        assert!(scanned.text.is_empty() && scanned.pads.is_empty() && scanned.before.is_empty());
        let built = rewrite("a:\tb\nc: [\n 1]\n", true).expect("builds");
        assert_eq!(built.text, "a: b\nc: [\n    1]\n");
        assert_eq!(built.pads.len(), 3);
        assert_eq!(
            rewrite("a:\n\t- b\n", false).map(|_| ()),
            rewrite("a:\n\t- b\n", true).map(|_| ())
        );
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
        // Top level: column 0 ends the scalar to libyaml, which then
        // refuses the stream (saphyr reads it as content).
        assert!(refused("|\nx\n[\n").starts_with("a top-level block scalar's content"));
    }

    /// The first document only: the rest is copied as it is.
    #[test]
    fn only_the_first_document_is_prepared() {
        let rest = "a: 1\n---\n\tb: [\n]\n";
        assert_eq!(text(rest), rest);
        // After an end marker a tab in a line's white space is refused,
        // as in block context; before a second document's `---`.
        assert_eq!(
            refused("a: 1\n...\n\tb\n"),
            "a tab in the indentation (YAML indents with spaces) at line 3, column 1"
        );
        assert_eq!(
            refused("a: x\n...\n  # c\n \t# d\n"),
            "a tab in the indentation (YAML indents with spaces) at line 4, column 2"
        );
        assert!(prepare("a: 1\n...\n  # c\n\n").is_ok());
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
            "a: 1\n...\n# [\n",
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
        let prepared = rewrite(&input, true).expect("prepares");
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
            "Prepared { bytes: 18, padding: 0, depth: 1, .. }"
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
        // libyaml reads `&a` then the plain `:b`, saphyr the anchor `a:b`:
        // refused where the names part.
        let name = "an anchor or alias name is letters, digits, `_` and `-`, ending at a space";
        assert_eq!(
            refused("- &a:b\n \tc\n"),
            format!("{name} at line 1, column 5")
        );
        assert_eq!(
            refused("a: &b:\tc\n"),
            format!("{name} at line 1, column 6")
        );
        // At a flow indicator both end it.
        assert_eq!(text("a: [&b, *b]\n"), "a: [&b, *b]\n");
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
        // In a flow collection `>` and `|` start no block scalar (nor a
        // plain one, to libyaml).
        assert_eq!(
            refused("a: [>, b,\n  c]\nd: 1\n"),
            "a `|` or `>` can't start a value in a flow collection (quote the value) \
             at line 1, column 5"
        );
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

    /// A `:` with no key before it on its line, which libyaml refuses
    /// (saphyr reads an empty key), refused at the `:` as libyaml says.
    #[test]
    fn a_colon_with_no_key_is_refused() {
        let no_key = "a `:` with no key before it (a key must be on the `:`'s line)";
        for (input, at) in [
            (": x\n", "line 1, column 1"),
            ("a: 1\n: x\n", "line 2, column 1"),
            ("a: 1\n:\n", "line 2, column 1"),
            ("a:\n  : x\n", "line 2, column 3"),
            ("- : x\n", "line 1, column 3"),
            ("a: |\n  x\n: b\n", "line 3, column 1"),
            ("a:\n  - b\n  : c\n", "line 3, column 3"),
            // An explicit key takes one value, in its own collection.
            ("? a\n: b\n: c\n", "line 3, column 1"),
            ("? a\n:\n:\n", "line 3, column 1"),
            ("- ? a\n- : b\n", "line 2, column 3"),
            ("? a\nb: c\n: d\n", "line 3, column 1"),
            // In a flow collection, and after a key on an earlier line.
            ("x: {a: 1, : 2}\n", "line 1, column 11"),
            ("[a, : b]\n", "line 1, column 5"),
            ("{: x}\n", "line 1, column 2"),
            ("{\"k\n\": 1}\n", "line 2, column 2"),
            ("a: {\"k\n  x\": 1}\n", "line 2, column 5"),
            ("x: {a\n  : 1}\n", "line 2, column 3"),
            ("{\"a\"\n: 1}\n", "line 2, column 1"),
            ("{? a : b, : c}\n", "line 1, column 11"),
        ] {
            assert_eq!(refused(input), format!("{no_key} at {at}"), "{input:?}");
        }
        // libyaml accepts these: an explicit key's value, keys before
        // their `:` on the line, an anchor as the key.
        for input in [
            "? a\n: b\n",
            "?\n:\n",
            "- ? a\n  : b\n",
            "? a\n? b\n: c\n",
            "? |\n  x\n: b\n",
            "{? a : b}\n",
            "[? a : b]\n",
            "{? a\n: b}\n",
            "&x : 1\n",
            "\"a\": 1\n",
            "{\"a\":1}\n",
            "a: [b, c]\n",
            "a: x:y\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// A file holds one document, as Ansible's loader wants: a second one,
    /// explicit or after the first one's end, is refused at its start.
    #[test]
    fn a_second_document_is_refused() {
        let second = "a second document (Ansible reads one document per file)";
        // saphyr-parser's error, in libyaml's words.
        let after = "did not find expected <document start>";
        assert_eq!(
            refused("a: 1\n---\nb: 2\n"),
            format!("{second} at line 2, column 1")
        );
        assert_eq!(
            refused("a: 1\n---\n"),
            format!("{second} at line 2, column 1")
        );
        assert_eq!(
            refused("a: 1\n...\n---\nb: 1\n"),
            format!("{second} at line 3, column 1")
        );
        // A broken second document is the second document.
        assert_eq!(
            refused("a: 1\n---\n[unclosed\n"),
            format!("{second} at line 2, column 1")
        );
        assert_eq!(
            refused("  a: 1\nb: 2\n"),
            format!("{after} at line 2, column 1")
        );
        assert_eq!(
            refused("a: 1\n...\nb: 2\n"),
            "content after the document's end marker (`...`) at line 3, column 1"
        );
        // An end marker and comments after the document are no document.
        for input in [
            "a: 1\n...\n",
            "a: 1\n...\n# c\n",
            "a: 1\n... # c\n",
            "---\n...\n",
            "a: 1\n...\n...\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// Anchors as libyaml's scanner and `PyYAML`'s composer take them: a
    /// name of letters, digits, `_` and `-` ending at white space or an
    /// indicator, defined once.
    #[test]
    fn anchors_are_named_and_defined_once() {
        let name = "an anchor or alias name is letters, digits, `_` and `-`, ending at a space";
        assert_eq!(
            refused("a: &x.y 1\n"),
            format!("{name} at line 1, column 6")
        );
        assert_eq!(refused("a: *x.y\n"), format!("{name} at line 1, column 6"));
        assert_eq!(refused("a: & 1\n"), format!("{name} at line 1, column 5"));
        assert_eq!(
            refused("a: &x:y 1\n"),
            format!("{name} at line 1, column 6")
        );
        assert_eq!(
            refused("- &a x\n- [&a y]\n"),
            "an anchor defined again (first at line 1, column 3) at line 2, column 4"
        );
        for input in [
            "a: &x_y-9 1\nb: *x_y-9\n",
            "a: [&x 1, *x]\n",
            "{&x a: 1}\n",
            "a: &x\n  b: 1\n",
            "&a : 1\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// In a flow collection a plain value's `:` right before a flow
    /// indicator or `?` is refused, as libyaml refuses it; other `:`s in
    /// it are text.
    #[test]
    fn a_colon_before_a_flow_indicator_is_refused_in_plain() {
        let colon = "a `:` right before `,`, `?` or a bracket in a flow collection's plain \
                     value (quote the value)";
        assert_eq!(
            refused("x: {key:}\n"),
            format!("{colon} at line 1, column 8")
        );
        assert_eq!(
            refused("x: [a:?]\n"),
            format!("{colon} at line 1, column 6")
        );
        assert_eq!(
            refused("x: [a:b:]\n"),
            format!("{colon} at line 1, column 8")
        );
        assert_eq!(
            refused("x: [a,\n  b:,c]\n"),
            format!("{colon} at line 2, column 4")
        );
        for input in [
            "{a:b}\n",
            "x: [a:b]\n",
            "{\"a\":}\n",
            "a: x:]\n",
            "x: [a: b]\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// A collection as a mapping key, which Ansible's loader refuses
    /// ("found unhashable key"), is refused where it starts: nested, its
    /// hashing would cost the depth squared.
    #[test]
    fn a_collection_key_is_refused() {
        let key = "a mapping key must be a scalar, not a collection";
        assert_eq!(
            refused("? [a]\n: 1\n"),
            format!("{key} at line 1, column 3")
        );
        assert_eq!(refused("[a]: 1\n"), format!("{key} at line 1, column 1"));
        assert_eq!(
            refused("x: {[a]: 1}\n"),
            format!("{key} at line 1, column 5")
        );
        assert_eq!(
            refused("a: 1\n? ? b\n"),
            format!("{key} at line 2, column 3")
        );
        assert_eq!(
            refused("a: &x [1]\nb:\n  *x : 1\n"),
            format!("{key} at line 3, column 3")
        );
        let nested = format!("{}v\n: 1\n", "? ".repeat(MAX_DEPTH - 1));
        assert_eq!(refused(&nested), format!("{key} at line 1, column 3"));
        // Values may be collections; an alias to a scalar may be a key.
        for input in [
            "a: [1]\nb: {c: [2]}\n",
            "a: &x 1\n*x : 2\n",
            "? a\n: [1]\n",
            "- [a]\n- {b: 1}\n",
            "? \n: x\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// Characters libyaml's reader refuses are refused at the first, by
    /// code point only.
    #[test]
    fn control_characters_are_refused() {
        assert_eq!(
            refused("a: 1\nb: \"\u{1b}[0m\"\n"),
            "U+001B, a control character Ansible's YAML parser refuses (in a \
             double-quoted string, write it as \\u001b) at line 2, column 5"
        );
        for input in [
            "a: x\u{1}y\n",
            "# \u{7}\n",
            "a: x\u{7f}y\n",
            "a: x\u{80}y\n",
            "a: x\u{fffe}\n",
            "\0",
        ] {
            assert!(refused(input).starts_with("U+"), "{input:?}");
        }
        for input in [
            "a:\tb\n",
            "a: 1\r\nb: 2\r\n",
            "a: x\u{a0}y\n",
            "\u{feff}a: 1\n",
            "a: \u{1f600}\n",
            "a: \u{fffd}\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// Directives as libyaml reads them: `%YAML` 1.1 or 1.2, and `%TAG`.
    #[test]
    fn directives_are_read_as_libyaml_reads_them() {
        let version = "a %YAML directive for a version Ansible's parser doesn't read (1.1 or 1.2)";
        let unknown = "an unknown directive (Ansible's parser reads %YAML and %TAG)";
        for input in [
            "%YAML 1.0\n---\na: 1\n",
            "%YAML 1.3\n---\na: 1\n",
            "%YAML 2.0\n---\n",
            "%YAML\n---\n",
        ] {
            assert_eq!(
                refused(input),
                format!("{version} at line 1, column 1"),
                "{input:?}"
            );
        }
        assert_eq!(
            refused("# c\n%FOO bar\n---\n"),
            format!("{unknown} at line 2, column 1")
        );
        for input in [
            "%YAML 1.1\n---\na: 1\n",
            "%YAML  1.2 # c\n---\na: 1\n",
            "%TAG !e! tag:yaml.org,2002:\n---\na: !e!str 1\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// A tag ends where libyaml's does, at white space or a flow `,`.
    #[test]
    fn a_tag_is_followed_by_a_space() {
        let end = "a tag must be followed by a space";
        assert_eq!(refused("a: [!t]\n"), format!("{end} at line 1, column 7"));
        assert_eq!(
            refused("a: {b: !!str}\n"),
            format!("{end} at line 1, column 13")
        );
        assert_eq!(refused("a: !t# c\n"), format!("{end} at line 1, column 6"));
        assert_eq!(
            refused("a: [!t[x]]\n"),
            format!("{end} at line 1, column 7")
        );
        for input in [
            "a: [!unsafe, 1]\n",
            "a: !unsafe #c\n",
            "a: !<tag:yaml.org,2002:str> 1\n",
            "a: !!str x\n",
            "a: !\n",
            "a: ! x\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// In a flow collection libyaml reads `?` as a key's indicator whatever
    /// follows it: one without a space after it is refused.
    #[test]
    fn a_flow_question_mark_needs_a_space() {
        let question = "a `?` in a flow collection must be followed by a space (quote a \
                        value that starts with `?`)";
        assert_eq!(
            refused("x: [?]\n"),
            format!("{question} at line 1, column 5")
        );
        assert_eq!(
            refused("x: {?x: 1}\n"),
            format!("{question} at line 1, column 5")
        );
        for input in ["x: [? a]\n", "x: ?a\n", "x: [\"?a\"]\n", "x: [a?b]\n"] {
            assert!(prepare(input).is_ok(), "{input:?}");
        }
    }

    /// A top-level block scalar's content at column 0 is refused: libyaml
    /// ends the scalar there and finds no document start.
    #[test]
    fn a_top_level_block_scalar_is_indented() {
        let top = "a top-level block scalar's content must be indented (Ansible's \
                   parser ends it here)";
        assert_eq!(refused("|\nfoo\n"), format!("{top} at line 2, column 1"));
        assert_eq!(
            refused("--- >\n\nfoo\n"),
            format!("{top} at line 3, column 1")
        );
        for input in ["--- |\n foo\n", "|\n# c\n", "|\n---\n", "a: |\n  x\nb: 1\n"] {
            let prepared = prepare(input);
            assert!(
                !prepared.as_ref().is_err_and(|r| r.message == top),
                "{input:?}: {prepared:?}"
            );
        }
    }

    /// A possible key lasts one line in every flow level, not only the
    /// innermost: a flow collection closed on a later line is no key.
    #[test]
    fn a_flow_key_lasts_one_line() {
        let no_key = "a `:` with no key before it (a key must be on the `:`'s line)";
        assert_eq!(
            refused("x: {[a\n]: c}\n"),
            format!("{no_key} at line 2, column 2")
        );
        assert_eq!(
            refused("x: {{a: 1\n}: c}\n"),
            format!("{no_key} at line 2, column 2")
        );
    }

    /// An end marker with no document before it: libyaml finds no node.
    #[test]
    fn an_end_marker_with_no_document_is_refused() {
        let lone = "a document end marker (`...`) with no document before it";
        assert_eq!(refused("...\n"), format!("{lone} at line 1, column 1"));
        assert_eq!(refused("# c\n...\n"), format!("{lone} at line 2, column 1"));
        assert!(prepare("").is_ok());
        assert!(prepare("# c\n").is_ok());
    }

    /// NEL, LS and PS are line breaks to libyaml and not to saphyr: a file
    /// holding one would read differently, so it is refused at the first.
    #[test]
    fn unicode_line_breaks_are_refused() {
        assert_eq!(
            refused("a: x\u{85}- y\n"),
            "U+0085 (NEL) is a line break to Ansible's YAML parser and not to sb's \
             (in a double-quoted string, write it as \\u0085) at line 1, column 5"
        );
        assert_eq!(
            refused("a: 1\r\nb: \"x\u{2028}y\"\n"),
            "U+2028 (LS) is a line break to Ansible's YAML parser and not to sb's \
             (in a double-quoted string, write it as \\u2028) at line 2, column 6"
        );
        assert!(refused("# \u{2029}\n").ends_with("at line 1, column 3"));
        assert!(refused("a\rb\u{2029}\n").ends_with("at line 2, column 2"));
        // Escaped, it is text.
        assert!(prepare("a: \"x\\u2028y\"\n").is_ok());
    }

    /// A byte order mark past the stream's first character is refused
    /// (decision 38): libyaml skips one at a line's start but counts it
    /// as a column, and reads one elsewhere as text, where saphyr reads
    /// it otherwise. The stream's own mark is no column.
    #[test]
    fn a_byte_order_mark_past_the_start_is_refused() {
        let bom = "U+FEFF, a byte order mark, is allowed only as the file's first \
                   character (remove it, or in a double-quoted string write it as \\ufeff)";
        assert_eq!(
            refused("\u{feff}\u{feff}a: 1\nb: 2\n"),
            format!("{bom} at line 1, column 1")
        );
        assert_eq!(
            refused("a: 1\n\u{feff}b: 2\n"),
            format!("{bom} at line 2, column 1")
        );
        assert_eq!(
            refused("a: \"x\u{feff}y\"\n"),
            format!("{bom} at line 1, column 6")
        );
        assert_eq!(
            refused("\u{feff}a: x\u{feff}\n"),
            format!("{bom} at line 1, column 5")
        );
        // The stream's own mark isn't a column for other refusals either.
        assert!(refused("\u{feff}a: \u{1}\n").ends_with("at line 1, column 4"));
        assert!(prepare("\u{feff}a: 1\n").is_ok());
        assert!(prepare("a: \"x\\ufeffy\"\n").is_ok());
    }

    /// Nesting is limited, never length: `MAX_DEPTH` collections are read,
    /// one more is refused where it starts, and an alias counts as the
    /// collections it copies.
    #[test]
    fn nesting_is_limited_by_depth() {
        let deep = |n: usize| format!("a:\n  {}x\n", "- ".repeat(n));
        assert_eq!(
            prepare(&deep(MAX_DEPTH - 1)).expect("at the limit").depth(),
            MAX_DEPTH
        );
        let message = format!("nested deeper than {MAX_DEPTH} levels");
        assert_eq!(
            refused(&deep(MAX_DEPTH)),
            format!("{message} at line 2, column {}", 3 + 2 * (MAX_DEPTH - 1))
        );
        // Flow nesting stops at saphyr-parser's own limit, with its error.
        let flow = format!("a: {}x{}\n", "[".repeat(256), "]".repeat(256));
        assert!(
            refused(&flow).starts_with("recursion limit exceeded"),
            "{}",
            refused(&flow)
        );
        // An alias copies its anchor's height.
        let tall = format!("a: &x\n  {}x\n", "- ".repeat(MAX_DEPTH - 2));
        assert_eq!(
            prepare(&format!("{tall}b: *x\n")).expect("as deep").depth(),
            MAX_DEPTH - 1
        );
        assert_eq!(
            prepare(&format!("{tall}b:\n  - *x\n"))
                .expect("at it")
                .depth(),
            MAX_DEPTH
        );
        assert_eq!(
            refused(&format!("{tall}b: [[*x]]\n")),
            format!("{message} at line 3, column 6")
        );
        // Depth is the deepest branch, not the count of collections; the
        // text's length isn't limited.
        assert_eq!(
            prepare("a:\n  b: [1]\nc: [[1]]\n").expect("wide").depth(),
            3
        );
        assert_eq!(prepare("a: 1\n").expect("flat").depth(), 1);
        assert_eq!(prepare("").expect("empty").depth(), 0);
    }

    /// A tree past `SHALLOW` levels is handled on the deep stack, and one
    /// as deep as `MAX_DEPTH` fits it; a panic there comes back.
    #[test]
    fn deep_trees_get_the_deep_stack() {
        assert_eq!(
            with_stack(SHALLOW, || std::thread::current()
                .name()
                .map(str::to_string))
            .expect("runs"),
            std::thread::current().name().map(str::to_string)
        );
        assert_eq!(
            with_stack(SHALLOW + 1, || std::thread::current()
                .name()
                .map(str::to_string))
            .expect("runs"),
            Some("sb-yaml".to_string())
        );
        let text = format!("a:\n  {}x\n", "- ".repeat(MAX_DEPTH - 1));
        let prepared = prepare(&text).expect("at the limit");
        let loaded = with_stack(prepared.depth(), || {
            let mut loader = saphyr::YamlLoader::<saphyr::YamlOwned>::default();
            Parser::new_from_str(prepared.text())
                .load(&mut loader, false)
                .expect("loads");
            loader.into_documents().len()
        });
        assert_eq!(loaded.expect("runs"), 1);
        let caught = std::panic::catch_unwind(|| with_stack(SHALLOW + 1, || panic!("inside")));
        assert!(caught.is_err());
    }

    /// Padding up to the budget exactly is allowed; one space more isn't.
    #[test]
    fn the_budget_is_inclusive() {
        // 1024 continuation lines padded by 1024 fill it exactly; the
        // closing line's padding is over it.
        let input = format!("{}[\n{}]\n", " ".repeat(1023), "1,\n".repeat(1024));
        let prepared = rewrite(&input, true).expect("prepares");
        assert_eq!(prepared.total, PAD_BUDGET);
        assert!(prepared.text.ends_with("\n]\n"));
    }

    /// A tag Ansible's loader has no constructor for is refused at its
    /// node, as `PyYAML`'s constructor refuses it ("could not determine a
    /// constructor for the tag"), checked with ansible-core 2.21.3's
    /// `AnsibleLoader` on `PyYAML` 6.0.3 and libyaml 0.2.5 (the platform
    /// note): a custom tag on a scalar, a collection, a key or the root,
    /// a core tag `SafeConstructor` lacks, and `%TAG` handles that
    /// resolve to one. The first in the file is named.
    #[test]
    fn a_tag_ansible_cannot_construct_is_refused() {
        for (input, line, column) in [
            ("a: !custom x\n", 1, 4),
            ("a: !custom {b: 1}\n", 1, 4),
            ("a: !custom [1]\n", 1, 4),
            ("!custom k: 1\n", 1, 1),
            ("!custom\na: 1\n", 1, 1),
            ("a: !!merge x\n", 1, 4),
            ("a: !!value x\n", 1, 4),
            ("a: !!python/str x\n", 1, 4),
            ("a: !!foo x\n", 1, 4),
            ("a: !<tag:x,2000:y> x\n", 1, 4),
            ("%TAG !e! tag:x,2000:\n---\na: !e!y 1\n", 3, 4),
            ("%TAG ! tag:yaml.org,2002:\n---\na: !unsafe x\n", 3, 4),
            ("%TAG !! tag:x,2000:\n---\na: !!str x\n", 3, 4),
            ("a: &x !custom 1\nb: *x\n", 1, 4),
            ("a: !unsafe {b: !custom 1}\n", 1, 16),
            ("a: !one 1\nb: !two 2\n", 1, 4),
        ] {
            let refusal = prepare(input).expect_err(input);
            assert_eq!(
                (refusal.line, refusal.column, refusal.message.as_str()),
                (line, column, NO_CONSTRUCTOR),
                "{input:?}"
            );
        }
        // A syntax error later in the file is named first, as libyaml's
        // parser stops before Ansible's constructor runs.
        let refusal = prepare("a: !custom x\nb: [\n").expect_err("unclosed");
        assert_ne!(refusal.message, NO_CONSTRUCTOR);
    }

    /// The tags Ansible's loader constructs pass: the non-specific `!`,
    /// `SafeConstructor`'s core tags, ansible-core's `python/dict` and
    /// `python/unicode`, and its own `!unsafe`, `!vault` and
    /// `!vault-encrypted`, however they are written.
    #[test]
    fn the_tags_ansible_constructs_pass() {
        for input in [
            "a: ! 12\n",
            "a: ! {b: 1}\n",
            "a: !<!> x\n",
            "a: !!str 12\n",
            "a: !!int 12\n",
            "a: !!float 1\n",
            "a: !!bool true\n",
            "a: !!null ''\n",
            "a: !!binary aGk=\n",
            "a: !!timestamp 2001-01-01\n",
            "a: !!map {b: 1}\n",
            "a: !!seq [1]\n",
            "a: !!omap [b: 1]\n",
            "a: !!pairs [b: 1]\n",
            "a: !!set {b}\n",
            "a: !!python/dict {b: 1}\n",
            "a: !!python/unicode x\n",
            "a: !unsafe x\n",
            "a: !unsafe {b: 1}\n",
            "a: !vault x\n",
            "a: !vault-encrypted x\n",
            "a: !<tag:yaml.org,2002:str> x\n",
            "a: !<tag:yaml.org,2002:%73tr> x\n",
            "a: !<!unsafe> x\n",
            "a: !%75nsafe x\n",
            "%TAG !e! tag:yaml.org,2002:\n---\na: !e!str x\n",
            "%TAG ! tag:yaml.org,2002:\n---\na: !str x\n",
            "%TAG ! !\n---\na: !unsafe x\n",
        ] {
            assert!(prepare(input).is_ok(), "{input:?}: {:?}", prepare(input));
        }
    }

    /// Every scalar's text as [`Prepared::load`] hands it on.
    fn loaded(input: &str) -> Vec<String> {
        struct Texts(Vec<String>);
        impl<'input> saphyr_parser::SpannedEventReceiver<'input> for Texts {
            fn on_event(&mut self, event: Event<'input>, _: saphyr_parser::Span) {
                if let Event::Scalar(text, ..) = event {
                    self.0.push(text.into_owned());
                }
            }
        }
        let prepared = prepare(input).unwrap_or_else(|refusal| panic!("{input:?}: {refusal}"));
        let mut texts = Texts(Vec::new());
        prepared.load(&mut texts).expect("loads");
        texts.0
    }

    /// A comment at column 0 under a top-level block scalar with no
    /// content yet ends the scalar, as libyaml ends it, where saphyr reads
    /// it as content: the scalar holds the blank lines above (kept only by
    /// `+`), the comments after it stay comments, and content after it is
    /// refused where libyaml refuses it. Each value and position is
    /// libyaml 0.2.5's through `PyYAML` 6.0.3; an indented comment stays
    /// content in both.
    #[test]
    fn a_comment_at_column_0_ends_a_top_level_block_scalar() {
        for (input, value) in [
            ("--- >\n\n# c\n", ""),
            ("--- |+\n\n# c\n", "\n"),
            ("--- |+\n\n\n# c\n\n", "\n\n"),
            ("--- >\n   \n# c\n", ""),
            ("|+\n\n# c\n", "\n"),
            (">\n# c\n", ""),
            ("|+\n\n# c\n\n\n", "\n"),
            ("|+\n# c", ""),
            ("--- |-\n\n# c\n# d\n", ""),
            ("--- |+\n\n# c\n...\n", "\n"),
            ("--- |-\n\n# c\n# d\n...\n", ""),
            (">+\n\n # c\n", "\n# c\n"),
        ] {
            assert_eq!(loaded(input), [value], "{input:?}");
        }
        let after = "content after the top-level block scalar that the comment above \
                     ended (Ansible's parser reads one node per document)";
        assert_eq!(
            refused(">\n# c\nfoo\n"),
            format!("{after} at line 3, column 1")
        );
        assert_eq!(
            refused("|\n# c\n  x\n"),
            format!("{after} at line 3, column 3")
        );
        // A `---` under it, or under the comment, is a second document
        // (saphyr read it as content).
        let second = "a second document (Ansible reads one document per file)";
        assert_eq!(
            refused("|\n---\nb\n"),
            format!("{second} at line 2, column 1")
        );
        assert_eq!(
            refused("|\n\n---\n"),
            format!("{second} at line 3, column 1")
        );
        assert_eq!(
            refused("|\n# c\n---\nb\n"),
            format!("{second} at line 3, column 1")
        );
        assert_eq!(loaded("|\n...\n"), [""]);
        // A tab in a line's leading white space after it is refused, as
        // libyaml refuses it in block context, at the tab.
        for (input, line, column) in [
            ("|\n# c\n\t# d\n", 3, 1),
            ("|\n# c\n\t\n", 3, 1),
            ("|\n# c\n \n\t\n", 4, 1),
            ("|\n# c\n \tfoo\n", 3, 2),
        ] {
            assert_eq!(
                refused(input),
                format!("{TAB_INDENT} at line {line}, column {column}"),
                "{input:?}"
            );
        }
        // Positions after the emptied lines are the file's, a lone `\r`
        // before one included.
        assert_eq!(
            refused("|\n# c\n...\n---\nb\n"),
            refused("|\n\n...\n---\nb\n")
        );
        assert_eq!(
            refused("|\r# c\n...\n---\nb\n"),
            format!("{second} at line 4, column 1")
        );
        // An end marker ends the scalar there: what follows it is between
        // documents, where libyaml takes no tab.
        assert_eq!(
            refused("|\n...\n\t\n"),
            format!("{TAB_INDENT} at line 3, column 1")
        );
        assert_eq!(loaded("|\n...\n# c\n"), [""]);
        // Where the scan and saphyr read such a text apart (here saphyr
        // reads the indented `>` lines as the scalar's and the next `>` as
        // more content; found by a fuzz), libyaml's value doesn't stand in:
        // refused.
        let read_apart = "|\n  >\n\r\n>\n\n# c\n";
        assert!(
            refused(read_apart).starts_with(AFTER_TOP_SCALAR),
            "{:?}",
            prepare(read_apart)
        );
        // The fuzz's first such text held a second byte order mark, which
        // the character check now refuses before the scan.
        let apart = "\u{feff}\u{feff}&a \n\n>\r\r\n>##&a :\n>\n#\u{feff}%#:";
        assert!(prepare(apart).is_err(), "{:?}", prepare(apart));
        // The comment lines are emptied, what they held counted.
        let prepared = prepare("|\n# c\n  # d\n").expect("reads");
        assert_eq!(prepared.text(), "|\n\n\n");
        let end = prepared.original(&Marker::new(4, 4, 0));
        assert_eq!((end.index(), end.line(), end.col()), (12, 4, 0));
        let mid = prepared.original(&Marker::new(3, 3, 0));
        assert_eq!(mid.index(), 6);
    }

    /// A block scalar at the end of a file without a final line break
    /// ends without one, as in libyaml and yaml.v3 (go.yaml.in/yaml/v3
    /// v3.0.5, `yaml_parser_scan_block_scalar`: only a line break it read
    /// is kept); saphyr-parser 0.3.3 adds one. Each expected value is
    /// libyaml 0.2.5's through `PyYAML` 6.0.3, and yaml.v3's where the
    /// platform note shows it.
    #[test]
    fn a_block_scalar_at_the_end_keeps_no_break_it_lacks() {
        for (input, value) in [
            ("a: |\n  x", "x"),
            ("a: |+\n  x", "x"),
            ("a: |-\n  x", "x"),
            ("a: >\n  x", "x"),
            ("a: >\n  x\n\n  y", "x\ny"),
            ("a: |2\n   x", " x"),
            ("a: |\n\n  x", "\nx"),
            ("- |\n x", "x"),
            ("|\n x", "x"),
            // A last line of spaces only: content past the indentation,
            // a blank line at it, nothing left of it.
            ("a: |\n  x\n    ", "x\n  "),
            ("a: |-\n  x\n    ", "x\n  "),
            ("a: |\n  x\n  ", "x\n"),
            ("a: |+\n  x\n  ", "x\n"),
            ("a: |\n  x\n ", "x\n"),
            ("a: |+\n  x\n ", "x\n"),
            ("a:\n  b: |\n    x\n  ", "x\n"),
            ("a: |\n  x\r\n  ", "x\n"),
            // No content line.
            ("a: |+\n  ", ""),
            ("a: |\n  ", ""),
            ("a: |+\n\n  ", "\n"),
            ("a: |+\n", ""),
            ("a: |+", ""),
            // A tag or an anchor before the header.
            ("k: !vault |\n  x", "x"),
            ("k: !vault |\n  x\n  ", "x\n"),
            ("a: &x |\n  x\n ", "x\n"),
            ("a: &x |1\n  ", " "),
            ("a: !!str >\n     - z\n\n\n   ", "- z\n"),
            ("a: !unsafe |+\n  ", ""),
            ("a: |2\n    x\n  ", "  x\n"),
            ("a:\n  - |1\n    x\n   ", " x\n"),
            ("a: |+\n\n\n\n   ", "\n\n\n"),
            // An indentation indicator and no content line.
            ("- a: |+1\n  ", ""),
            ("- a: >+1 #c\n  ", ""),
            ("a: !!str >+1  \n\n ", "\n"),
            ("- - >+1\n\n\n\n   ", "\n\n\n"),
            // With a final line break, as saphyr reads it.
            ("a: |\n  x\n", "x\n"),
            ("a: |\n  x\n\n", "x\n"),
            ("a: |+\n  x\n\n", "x\n\n"),
        ] {
            assert_eq!(
                loaded(input).last().map(String::as_str),
                Some(value),
                "{input:?}"
            );
        }
        // Not at the end: as saphyr reads it, a tagged one and one with no
        // content line included; only the last scalar loses its break.
        assert_eq!(loaded("a: |\n  x\nb: 1"), ["a", "x\n", "b", "1"]);
        assert_eq!(
            loaded(
                "a: !vault |\n          $ANSIBLE_VAULT;1.1;AES256\n          6162\n     # note\nb: 1\n"
            ),
            ["a", "$ANSIBLE_VAULT;1.1;AES256\n6162\n", "b", "1"]
        );
        assert_eq!(
            loaded("a:\n  b: |+\n\n\n  c: 1\n"),
            ["a", "b", "\n\n", "c", "1"]
        );
        assert_eq!(loaded("a: |\n  x\nb: |\n  y"), ["a", "x\n", "b", "y"]);
        // Padding before it doesn't move the end.
        assert_eq!(
            loaded("a: [1,\n2]\nb: |\n  x").last().map(String::as_str),
            Some("x")
        );
        // A byte order mark doesn't either.
        assert_eq!(
            loaded("\u{feff}a: |\n  x").last().map(String::as_str),
            Some("x")
        );
    }
}
