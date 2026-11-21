//! A schema file read into serde's derived rules straight from
//! saphyr-parser's events, the parser every config file goes through
//! ([ADR-028](../../../../docs/decisions/ADR-028-own-schema-reader.md)).
//!
//! Why not serde-saphyr, which read them before: it fingerprints, records
//! and replays every mapping key and keeps comment and budget bookkeeping
//! per event, about 460 instructions a byte of motd.schema.yml against
//! saphyr-parser's 175, a third of validate-config's work and of motd's
//! check at every login (performance round 5, infra finding 1); and it
//! brings a second YAML parser (granit-parser) into the binary. serde's
//! own guide is the pattern followed here
//! (<https://serde.rs/impl-deserializer.html>).
//!
//! The scalars resolve as serde-saphyr 2.0.0 resolved them with its
//! default options, so a schema reads as before (`src/parse_scalars.rs`
//! and `src/de/deserializer.rs` of that release): a plain `~`, `null` (in
//! any case) or empty scalar is null, which reads as none, an empty list
//! or an empty mapping where one of those belongs; a boolean is YAML
//! 1.1's (`yes`, `on`, `y` too, in any case); a whole number is decimal
//! without a leading zero, or `0x`, `0o` or `0b`, with `_` between
//! digits; a float is Rust's decimal form or `.inf` and `.nan`, which
//! aren't taken as a value. Its limits are kept too: 64 nested
//! collections, 250,000 nodes. A duplicate key in any mapping, a second
//! document, an anchor, an alias, a tag and a merge key (`<<`) are
//! refused, naming the line and column: Saltbox's schemas use none of
//! the last four (the maintainer, 2026-11-21).

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;

use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Span, StrInput};
use sb_cli_core::config::yaml;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

/// How deep collections may nest (serde-saphyr's default budget).
const MAX_DEPTH: usize = 64;

/// How many nodes a file may hold (serde-saphyr's default budget).
const MAX_NODES: usize = 250_000;

/// Why a schema file can't be read, at its line and column when known.
#[derive(Debug)]
pub(super) struct Error {
    message: String,
    at: Option<(usize, usize)>,
    /// saphyr-parser's own refusal.
    syntax: bool,
}

impl Error {
    fn at(message: impl fmt::Display, marker: Marker) -> Self {
        Self {
            message: message.to_string(),
            at: Some(position(marker)),
            syntax: false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.at {
            Some((line, column)) => write!(f, "{} at line {line}, column {column}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

impl de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self {
            message: message.to_string(),
            at: None,
            syntax: false,
        }
    }
}

/// A marker's line and column, both counted from 1.
fn position(marker: Marker) -> (usize, usize) {
    (marker.line(), marker.col() + 1)
}

/// `text` as a `T`. A text saphyr-parser refuses as written is read
/// again through cli-core's pre-pass, which pads a flow collection's or
/// quoted scalar's continuation line that sits at or left of its block's
/// indentation, as libyaml and yaml.v3 read it (ADR-022; serde-saphyr's
/// parser refused it);
/// only then, so a schema saphyr reads costs nothing more.
///
/// # Errors
///
/// The first thing that keeps `text` from being a `T`.
pub(super) fn from_str<T: de::DeserializeOwned>(text: &str) -> Result<T, Error> {
    characters(text)?;
    match read(text) {
        Err(err) if err.syntax => {
            let Ok(prepared) = yaml::prepare(text) else {
                return Err(err);
            };
            if prepared.text() == text {
                return Err(err);
            }
            read(prepared.text()).map_err(|mut err| {
                err.at = err.at.map(|(line, column)| {
                    position(prepared.original(&Marker::new(0, line, column - 1)))
                });
                err
            })
        }
        read => read,
    }
}

/// `text` as a `T`, read as written.
fn read<'de, T: de::Deserialize<'de>>(text: &'de str) -> Result<T, Error> {
    let mut de = De {
        parser: Parser::new_from_str(text),
        peeked: None,
        last: (1, 1),
        depth: 0,
        nodes: 0,
        documents: 0,
    };
    let value = T::deserialize(&mut de)?;
    // A second document is refused as it is looked for.
    if matches!(de.peek()?, Event::StreamEnd) {
        Ok(value)
    } else {
        Err(de.unexpected("the end of the file"))
    }
}

/// The characters YAML doesn't allow, which serde-saphyr's parser
/// refused and saphyr-parser reads as text: a control character but tab
/// and the line breaks (U+0000 to U+0008, U+000B, U+000C, U+000E to
/// U+001F, U+007F to U+0084, U+0086 to U+009F), U+FFFE and U+FFFF (YAML
/// 1.2's `c-printable`, <https://yaml.org/spec/1.2.2/#51-character-set>),
/// and a byte order mark past the file's start, which the two parsers
/// refuse in different places (cli-core's rule for config files).
/// Read by their UTF-8 bytes, where a lead byte can only start a
/// character. Why not cli-core's check (`config::yaml`'s `characters`):
/// it is private to the pre-pass, and it also refuses NEL, LS and PS,
/// line breaks to Ansible's libyaml, which serde-saphyr's parser read as
/// text in a schema; a schema has no Ansible reader to agree with.
fn characters(text: &str) -> Result<(), Error> {
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let bytes = text.as_bytes();
    let byte = |i: usize| bytes.get(i).copied().unwrap_or(0);
    let refused = |i: usize| match byte(i) {
        b'\t' | b'\n' | b'\r' => false,
        0..=0x1f | 0x7f => true,
        // U+0080 to U+009F, but U+0085 (NEL).
        0xc2 => matches!(byte(i + 1), 0x80..=0x84 | 0x86..=0x9f),
        // U+FEFF; U+FFFE, U+FFFF.
        0xef => matches!(
            (byte(i + 1), byte(i + 2)),
            (0xbb, 0xbf) | (0xbf, 0xbe | 0xbf)
        ),
        _ => false,
    };
    // The bytes that can start one, looked for first: a line break is the
    // only one a schema file has (about a byte in forty).
    let mut from = start;
    let at = loop {
        let Some(found) = bytes[from..]
            .iter()
            .position(|&b| b < 0x20 || b == 0x7f || b == 0xc2 || b == 0xef)
        else {
            return Ok(());
        };
        if refused(from + found) {
            break from + found;
        }
        from += found + 1;
    };
    let before = &text[..at];
    // A line ends at `\n`, `\r\n` or a lone `\r`, as the parser counts.
    let (mut line, mut line_start) = (1, 0);
    for (i, c) in before.char_indices() {
        if c == '\n' || (c == '\r' && !before[i + 1..].starts_with('\n')) {
            (line, line_start) = (line + 1, i + 1);
        }
    }
    let column = before[line_start..].chars().count() + 1;
    let found = u32::from(text[at..].chars().next().unwrap_or_default());
    let message = if found == 0xfeff {
        "syntax error: U+FEFF, a byte order mark, is allowed only as the file's first character"
            .to_string()
    } else {
        format!("syntax error: U+{found:04X}, a character YAML doesn't allow")
    };
    Err(Error {
        message,
        at: Some((line, column)),
        syntax: false,
    })
}

struct De<'de> {
    parser: Parser<'de, StrInput<'de>>,
    /// The next node event, when looked at already.
    peeked: Option<(Event<'de>, Span)>,
    /// Where the last event taken starts.
    last: (usize, usize),
    /// The collections open.
    depth: usize,
    nodes: usize,
    documents: usize,
}

/// A plain scalar's value, as serde-saphyr resolved it.
enum Resolved {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str,
}

/// serde-saphyr's trim before a scalar is read as a value.
fn trim(text: &str) -> &str {
    text.trim_matches([' ', '\t', '\r', '\n'])
}

fn is_null(text: &str, style: ScalarStyle) -> bool {
    style == ScalarStyle::Plain
        && (text.is_empty() || text == "~" || text.eq_ignore_ascii_case("null"))
}

/// YAML 1.1's booleans, in any case.
fn boolean(text: &str) -> Option<bool> {
    let text = trim(text);
    let is = |names: [&str; 4]| names.iter().any(|name| text.eq_ignore_ascii_case(name));
    if is(["true", "yes", "y", "on"]) {
        Some(true)
    } else if is(["false", "no", "n", "off"]) {
        Some(false)
    } else {
        None
    }
}

/// Digits in `radix`, a `_` allowed only between two of them.
fn digits(text: &str, radix: u32) -> Option<u128> {
    let bytes = text.as_bytes();
    let mut value: u128 = 0;
    let mut any = false;
    for (at, &byte) in bytes.iter().enumerate() {
        if byte == b'_' {
            let between = at > 0
                && bytes[at - 1] != b'_'
                && bytes.get(at + 1).is_some_and(|&next| next != b'_');
            if !between {
                return None;
            }
            continue;
        }
        let digit = char::from(byte).to_digit(radix)?;
        value = value
            .checked_mul(u128::from(radix))?
            .checked_add(u128::from(digit))?;
        any = true;
    }
    any.then_some(value)
}

/// A whole number's sign and magnitude: decimal without a leading zero,
/// or `0x`, `0o` or `0b`.
fn whole(text: &str) -> Option<(bool, u128)> {
    let text = trim(text);
    let (negative, rest) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let radix = |prefixes: [&str; 2]| prefixes.iter().find_map(|prefix| rest.strip_prefix(prefix));
    let magnitude = if let Some(hex) = radix(["0x", "0X"]) {
        digits(hex, 16)?
    } else if let Some(octal) = radix(["0o", "0O"]) {
        digits(octal, 8)?
    } else if let Some(binary) = radix(["0b", "0B"]) {
        digits(binary, 2)?
    } else if rest.starts_with('0') && rest != "0" {
        return None;
    } else {
        digits(rest, 10)?
    };
    Some((negative, magnitude))
}

/// A whole number that fits a `u64`, unsigned.
fn unsigned(text: &str) -> Option<u64> {
    match whole(text)? {
        (false, magnitude) => u64::try_from(magnitude).ok(),
        (true, _) => None,
    }
}

/// A float: `.inf` and `.nan` in any case, or Rust's decimal form; a
/// decimal that overflows is infinite.
fn float(text: &str) -> Option<f64> {
    let text = trim(text);
    let is = |names: &[&str]| names.iter().any(|name| text.eq_ignore_ascii_case(name));
    if is(&[".nan", "+.nan", "-.nan"]) {
        return Some(f64::NAN);
    }
    if is(&[".inf", "+.inf"]) {
        return Some(f64::INFINITY);
    }
    if is(&["-.inf"]) {
        return Some(f64::NEG_INFINITY);
    }
    let value = text.parse::<f64>().ok()?;
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    (value.is_finite() || unsigned.starts_with(|c: char| c.is_ascii_digit())).then_some(value)
}

/// A plain scalar's value: null, a boolean, a whole number, a float, or
/// else its text.
fn resolve(text: &str) -> Resolved {
    if is_null(text, ScalarStyle::Plain) {
        return Resolved::Null;
    }
    if let Some(value) = boolean(text) {
        return Resolved::Bool(value);
    }
    // A leading zero (`017`) is no whole number (`whole`): a float.
    let number = match whole(text) {
        Some((true, magnitude)) => i128::try_from(magnitude)
            .ok()
            .and_then(|magnitude| i64::try_from(-magnitude).ok())
            .map(Resolved::Int),
        Some((false, magnitude)) => u64::try_from(magnitude).ok().map(Resolved::Uint),
        None => None,
    };
    number
        .or_else(|| float(text).map(Resolved::Float))
        .unwrap_or(Resolved::Str)
}

/// What an event is, for an error.
fn shape(event: &Event<'_>) -> &'static str {
    match event {
        Event::Scalar(..) => "a scalar",
        Event::MappingStart(..) => "a mapping",
        Event::SequenceStart(..) => "a sequence",
        Event::MappingEnd => "the mapping's end",
        Event::SequenceEnd => "the sequence's end",
        _ => "the end of the file",
    }
}

fn visit_text<'de, V: Visitor<'de>>(visitor: V, text: Cow<'de, str>) -> Result<V::Value, Error> {
    match text {
        Cow::Borrowed(text) => visitor.visit_borrowed_str(text),
        Cow::Owned(text) => visitor.visit_string(text),
    }
}

impl<'de> De<'de> {
    /// `err` at the last event taken, unless it has a place already.
    fn placed(&self, mut err: Error) -> Error {
        if err.at.is_none() {
            err.at = Some(self.last);
        }
        err
    }

    /// The next node event, looked at, not taken: the stream's and the
    /// first document's start and a document's end passed over, a second
    /// document and what schema files don't use refused.
    fn peek(&mut self) -> Result<&Event<'de>, Error> {
        while self.peeked.is_none() {
            let (event, span) = match self.parser.next_event() {
                Some(Ok(found)) => found,
                Some(Err(err)) => {
                    return Err(Error {
                        syntax: true,
                        ..Error::at(format!("syntax error: {}", err.info()), *err.marker())
                    });
                }
                None => (
                    Event::StreamEnd,
                    Span::empty(Marker::new(0, self.last.0, 0)),
                ),
            };
            let refused = match &event {
                Event::StreamStart | Event::DocumentEnd | Event::Nothing => continue,
                Event::DocumentStart(_) => {
                    self.documents += 1;
                    if self.documents == 1 {
                        continue;
                    }
                    "more than one YAML document, the second"
                }
                Event::Scalar(_, _, anchor, tag)
                | Event::MappingStart(anchor, tag)
                | Event::SequenceStart(anchor, tag) => {
                    // An alias needs an anchor, refused before it (an
                    // unknown one is saphyr-parser's syntax error).
                    if *anchor != 0 {
                        "YAML anchors and aliases aren't supported in schema files"
                    } else if tag.is_some() {
                        "YAML tags aren't supported in schema files"
                    } else {
                        ""
                    }
                }
                _ => "",
            };
            if !refused.is_empty() {
                return Err(Error::at(refused, span.start));
            }
            self.peeked = Some((event, span));
        }
        Ok(self
            .peeked
            .as_ref()
            .map_or(&Event::StreamEnd, |(event, _)| event))
    }

    /// The next node event, taken: its place kept for errors, its depth
    /// and nodes counted against the limits.
    fn next(&mut self) -> Result<Event<'de>, Error> {
        self.peek()?;
        let Some((event, span)) = self.peeked.take() else {
            return Ok(Event::StreamEnd);
        };
        self.last = position(span.start);
        match event {
            Event::StreamEnd => {
                // Stays at the end.
                self.peeked = Some((Event::StreamEnd, span));
                return Ok(Event::StreamEnd);
            }
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(Error::at(
                        format!("nested deeper than {MAX_DEPTH} levels"),
                        span.start,
                    ));
                }
            }
            Event::MappingEnd | Event::SequenceEnd => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        if matches!(
            event,
            Event::Scalar(..) | Event::MappingStart(..) | Event::SequenceStart(..)
        ) {
            self.nodes += 1;
            if self.nodes > MAX_NODES {
                return Err(Error::at(
                    format!("more than {MAX_NODES} nodes"),
                    span.start,
                ));
            }
        }
        Ok(event)
    }

    /// An error for the next event, where `expected` belongs, at its place.
    fn unexpected(&mut self, expected: &str) -> Error {
        match self.next() {
            Ok(event) => self.here(format!("expected {expected}, found {}", shape(&event))),
            Err(err) => err,
        }
    }

    /// Whether the next event is a null scalar (taken if it is), or the
    /// end of a file with no document, which serde-saphyr read as null.
    fn null(&mut self) -> Result<bool, Error> {
        let null = match self.peek()? {
            Event::Scalar(text, style, ..) => is_null(text, *style),
            Event::StreamEnd => return Ok(true),
            _ => false,
        };
        if null {
            self.next()?;
        }
        Ok(null)
    }

    /// The next scalar's text and style, where `expected` belongs.
    fn scalar(&mut self, expected: &str) -> Result<(Cow<'de, str>, ScalarStyle), Error> {
        if !matches!(self.peek()?, Event::Scalar(..)) {
            return Err(self.unexpected(expected));
        }
        match self.next()? {
            Event::Scalar(text, style, ..) => Ok((text, style)),
            other => Err(self.here(format!("expected {expected}, found {}", shape(&other)))),
        }
    }

    /// `message`, at the last event taken.
    fn here(&self, message: impl fmt::Display) -> Error {
        Error {
            message: message.to_string(),
            at: Some(self.last),
            syntax: false,
        }
    }
}

impl<'de> de::Deserializer<'de> for &mut De<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let found = match self.next()? {
            Event::Scalar(text, ScalarStyle::Plain, ..) => match resolve(&text) {
                Resolved::Null => visitor.visit_unit(),
                Resolved::Bool(value) => visitor.visit_bool(value),
                Resolved::Int(value) => visitor.visit_i64(value),
                Resolved::Uint(value) => visitor.visit_u64(value),
                Resolved::Float(value) if value.is_finite() => visitor.visit_f64(value),
                Resolved::Float(_) => Err(self.here(format!("`{text}` isn't a finite number"))),
                Resolved::Str => visit_text(visitor, text),
            },
            Event::Scalar(text, ..) => visit_text(visitor, text),
            Event::MappingStart(..) => visitor.visit_map(Map::new(self, false)),
            Event::SequenceStart(..) => visitor.visit_seq(Seq {
                de: self,
                empty: false,
            }),
            other => Err(self.here(format!("expected a value, found {}", shape(&other)))),
        };
        found.map_err(|err| self.placed(err))
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let (text, _) = self.scalar("a boolean")?;
        match boolean(&text) {
            Some(value) => visitor.visit_bool(value).map_err(|err| self.placed(err)),
            None => Err(self.here("invalid boolean")),
        }
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let (text, _) = self.scalar("a whole number")?;
        match unsigned(&text) {
            Some(value) => visitor.visit_u64(value).map_err(|err| self.placed(err)),
            None => Err(self.here("invalid whole number")),
        }
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let (text, style) = self.scalar("a string")?;
        if is_null(&text, style) {
            return Err(self.here("null where a string belongs"));
        }
        visit_text(visitor, text).map_err(|err| self.placed(err))
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.null()? {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let empty = self.null()?;
        if !empty {
            if !matches!(self.peek()?, Event::SequenceStart(..)) {
                return Err(self.unexpected("a sequence"));
            }
            self.next()?;
        }
        visitor
            .visit_seq(Seq {
                de: &mut *self,
                empty,
            })
            .map_err(|err| self.placed(err))
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let empty = self.null()?;
        if !empty {
            if !matches!(self.peek()?, Event::MappingStart(..)) {
                return Err(self.unexpected("a mapping"));
            }
            self.next()?;
        }
        visitor
            .visit_map(Map::new(&mut *self, empty))
            .map_err(|err| self.placed(err))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    // Enums too: `Rule`'s are read as a string, then through serde's
    // `StringDeserializer` (`schema::name`); a field that derives an enum
    // and is read here directly would need `deserialize_enum` with
    // `visit_enum(text.into_deserializer())`
    // (<https://serde.rs/impl-deserializer.html>).
    serde::forward_to_deserialize_any! {
        i8 i16 i32 i64 i128 u8 u16 u32 u128 f32 f64 char bytes byte_buf unit
        unit_struct newtype_struct tuple tuple_struct enum ignored_any
    }
}

/// A mapping key, for the duplicate check: a plain whole number's sign
/// and magnitude, or the key's text.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key<'de> {
    Whole(bool, u128),
    Text(Cow<'de, str>),
}

/// A mapping's keys so far: a list while it is short (a schema's
/// mappings hold a few keys; a comparison each costs less than a tree's
/// allocations), a set past [`FEW`], so a long one stays linear.
enum Keys<'de> {
    Few(Vec<Key<'de>>),
    Many(BTreeSet<Key<'de>>),
}

/// How many keys [`Keys::Few`] holds.
const FEW: usize = 32;

impl<'de> Keys<'de> {
    /// Whether `key` is new, kept.
    fn insert(&mut self, key: Key<'de>) -> bool {
        match self {
            Keys::Few(keys) if keys.contains(&key) => false,
            Keys::Few(keys) if keys.len() < FEW => {
                keys.push(key);
                true
            }
            Keys::Few(keys) => {
                let mut set: BTreeSet<Key<'de>> = std::mem::take(keys).into_iter().collect();
                set.insert(key);
                *self = Keys::Many(set);
                true
            }
            Keys::Many(keys) => keys.insert(key),
        }
    }
}

/// A mapping's entries, each key checked for a duplicate and a merge key.
struct Map<'a, 'de> {
    de: &'a mut De<'de>,
    /// A null read as an empty mapping.
    empty: bool,
    keys: Keys<'de>,
}

impl<'a, 'de> Map<'a, 'de> {
    fn new(de: &'a mut De<'de>, empty: bool) -> Self {
        Self {
            de,
            empty,
            keys: Keys::Few(Vec::new()),
        }
    }
}

impl<'de> MapAccess<'de> for Map<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.empty {
            return Ok(None);
        }
        let refused = match self.de.peek()? {
            Event::MappingEnd => None,
            Event::Scalar(text, ScalarStyle::Plain, ..) if text == "<<" => Some(Some(
                "YAML merge keys aren't supported in schema files".to_string(),
            )),
            Event::Scalar(text, style, ..) => {
                // A plain whole number is the same key however it is
                // spelt (`1`, `0x1`), as serde-saphyr compared them.
                let key = match whole(text) {
                    Some((negative, magnitude)) if *style == ScalarStyle::Plain => {
                        Key::Whole(negative && magnitude != 0, magnitude)
                    }
                    _ => Key::Text(text.clone()),
                };
                Some((!self.keys.insert(key)).then(|| format!("duplicate key `{text}`")))
            }
            _ => Some(None),
        };
        match refused {
            None => {
                self.de.next()?;
                return Ok(None);
            }
            Some(Some(message)) => {
                self.de.next()?;
                return Err(self.de.here(message));
            }
            Some(None) => {}
        }
        seed.deserialize(&mut *self.de)
            .map(Some)
            .map_err(|err| self.de.placed(err))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(&mut *self.de)
            .map_err(|err| self.de.placed(err))
    }
}

/// A sequence's items.
struct Seq<'a, 'de> {
    de: &'a mut De<'de>,
    /// A null read as an empty sequence.
    empty: bool,
}

impl<'de> SeqAccess<'de> for Seq<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.empty {
            return Ok(None);
        }
        if matches!(self.de.peek()?, Event::SequenceEnd) {
            self.de.next()?;
            return Ok(None);
        }
        seed.deserialize(&mut *self.de)
            .map(Some)
            .map_err(|err| self.de.placed(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value `text` resolves to, as a plain scalar.
    fn value(text: &str) -> String {
        match resolve(text) {
            Resolved::Null => "null".to_string(),
            Resolved::Bool(value) => format!("bool {value}"),
            Resolved::Int(value) => format!("int {value}"),
            Resolved::Uint(value) => format!("uint {value}"),
            Resolved::Float(value) => format!("float {value}"),
            Resolved::Str => format!("str {text}"),
        }
    }

    /// serde-saphyr 2.0.0's answers for each form, taken from its
    /// `from_str` into `serde_json::Value` (the differential harness of
    /// ADR-028).
    #[test]
    fn plain_scalars_resolve_as_serde_saphyr_resolved_them() {
        for (text, expected) in [
            ("", "null"),
            ("~", "null"),
            ("nUlL", "null"),
            ("yes", "bool true"),
            ("ON", "bool true"),
            ("Y", "bool true"),
            ("True", "bool true"),
            ("no", "bool false"),
            ("Off", "bool false"),
            ("n", "bool false"),
            ("FALSE", "bool false"),
            ("0", "uint 0"),
            ("-0", "int 0"),
            ("+5", "uint 5"),
            ("-5", "int -5"),
            ("1_000", "uint 1000"),
            ("0x1F", "uint 31"),
            ("0Xff", "uint 255"),
            ("-0x10", "int -16"),
            ("0o17", "uint 15"),
            ("0O7", "uint 7"),
            ("0b101", "uint 5"),
            ("0B1", "uint 1"),
            ("18446744073709551615", "uint 18446744073709551615"),
            ("-9223372036854775808", "int -9223372036854775808"),
            // Past i64 or u64: a float.
            ("-9223372036854775809", "float -9223372036854776000"),
            ("18446744073709551616", "float 18446744073709552000"),
            // A leading zero is no whole number: a float.
            ("017", "float 17"),
            ("-017", "float -17"),
            ("1.5", "float 1.5"),
            (".5", "float 0.5"),
            ("5.", "float 5"),
            ("1e3", "float 1000"),
            ("+.inf", "float inf"),
            (".INF", "float inf"),
            ("-.inf", "float -inf"),
            ("1e999", "float inf"),
            ("-1e999", "float -inf"),
            (".NaN", "float NaN"),
            // Rust's spellings that aren't YAML's: text.
            ("inf", "str inf"),
            ("nan", "str nan"),
            ("infinity", "str infinity"),
            ("_1", "str _1"),
            ("1_", "str 1_"),
            ("1__0", "str 1__0"),
            ("0x", "str 0x"),
            ("0x_1", "str 0x_1"),
            ("0b2", "str 0b2"),
            ("0o8", "str 0o8"),
            ("0xg", "str 0xg"),
            ("--5", "str --5"),
            ("+-5", "str +-5"),
            ("yes please", "str yes please"),
        ] {
            assert_eq!(value(text), expected, "{text:?}");
        }
    }

    #[test]
    fn whole_numbers_read_where_one_belongs() {
        assert_eq!(unsigned(" 7 "), Some(7));
        assert_eq!(unsigned("0x10"), Some(16));
        assert_eq!(unsigned("-0"), None);
        assert_eq!(unsigned("-1"), None);
        assert_eq!(unsigned("18446744073709551616"), None);
        assert_eq!(unsigned("1.0"), None);
        assert_eq!(whole("340282366920938463463374607431768211456"), None);
    }

    #[test]
    fn booleans_are_yaml_1_1_trimmed() {
        assert_eq!(boolean(" yes\n"), Some(true));
        assert_eq!(boolean("\toff"), Some(false));
        assert_eq!(boolean("maybe"), None);
        assert_eq!(boolean(""), None);
    }

    /// Only a plain scalar is null.
    #[test]
    fn null_is_plain() {
        assert!(is_null("~", ScalarStyle::Plain));
        assert!(!is_null("~", ScalarStyle::SingleQuoted));
        assert!(!is_null("", ScalarStyle::DoubleQuoted));
        assert!(!is_null("nil", ScalarStyle::Plain));
    }

    /// The error for `text`, as shown.
    fn refused(text: &str) -> String {
        from_str::<serde::de::IgnoredAny>(text)
            .expect_err("refused")
            .to_string()
    }

    #[test]
    fn characters_yaml_does_not_allow_are_refused_where_they_stand() {
        assert_eq!(
            refused("a: 1\nb: x\u{1}\n"),
            "syntax error: U+0001, a character YAML doesn't allow at line 2, column 5"
        );
        assert_eq!(
            refused("a:\r\n é\u{7f}"),
            "syntax error: U+007F, a character YAML doesn't allow at line 2, column 3"
        );
        assert_eq!(
            refused("a: 1\rb: \u{9f}"),
            "syntax error: U+009F, a character YAML doesn't allow at line 2, column 4"
        );
        assert_eq!(
            refused("a: \u{80}"),
            "syntax error: U+0080, a character YAML doesn't allow at line 1, column 4"
        );
        assert_eq!(
            refused("a: \u{ffff}"),
            "syntax error: U+FFFF, a character YAML doesn't allow at line 1, column 4"
        );
        assert_eq!(
            refused("a: \u{fffe}"),
            "syntax error: U+FFFE, a character YAML doesn't allow at line 1, column 4"
        );
        assert_eq!(
            refused("# a\u{feff}\n"),
            "syntax error: U+FEFF, a byte order mark, is allowed only as the file's first \
             character at line 1, column 4"
        );
        // Tab, the line breaks, NEL, a no-break space, U+FFFD and a byte
        // order mark first are text.
        assert!(
            from_str::<serde::de::IgnoredAny>("\u{feff}a: \"\t\u{85}\u{a0}\u{fffd}\"\r\n").is_ok()
        );
    }

    /// A long mapping's keys move from the list to the set; a duplicate
    /// is found in either.
    #[test]
    fn a_duplicate_is_found_in_a_long_mapping() {
        let keys = |count: usize, again: &str| {
            let mut text = String::new();
            for i in 0..count {
                text.push('k');
                text.push_str(&i.to_string());
                text.push_str(": 1\n");
            }
            text.push_str(again);
            text
        };
        assert!(from_str::<serde::de::IgnoredAny>(&keys(FEW, "")).is_ok());
        assert!(from_str::<serde::de::IgnoredAny>(&keys(FEW * 3, "")).is_ok());
        for count in [FEW - 1, FEW, FEW * 3] {
            let message = refused(&keys(count, "k0: 2\n"));
            assert_eq!(
                message,
                format!("duplicate key `k0` at line {}, column 1", count + 1)
            );
        }
    }

    #[test]
    fn the_limits_are_serde_saphyrs() {
        let deep =
            |levels: usize| format!("a: {}{}\n", "[".repeat(levels - 1), "]".repeat(levels - 1));
        assert!(from_str::<serde::de::IgnoredAny>(&deep(MAX_DEPTH)).is_ok());
        assert_eq!(
            refused(&deep(MAX_DEPTH + 1)),
            format!(
                "nested deeper than 64 levels at line 1, column {}",
                4 + MAX_DEPTH - 1
            )
        );
        // The root mapping, its key and the list are three nodes.
        let wide = |items: usize| format!("a: [{}]\n", vec!["1"; items].join(","));
        assert!(from_str::<serde::de::IgnoredAny>(&wide(MAX_NODES - 3)).is_ok());
        assert!(
            refused(&wide(MAX_NODES - 2)).starts_with("more than 250000 nodes at line 1, column ")
        );
    }
}
