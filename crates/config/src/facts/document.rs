//! The facts dialect (tui spec, "Editors", Dialect; sb-go
//! facts/document.go): Saltbox's `configparser` settings, read and written
//! so that editing one fact can't reformat or reinterpret another.
//!
//! Saltbox's module reads the files with `configparser.ConfigParser`
//! (`interpolation=None`, `comment_prefixes=("#",)`, no inline comments,
//! `delimiters=("=",)`, `empty_lines_in_values=False`, case kept;
//! <https://raw.githubusercontent.com/saltyorg/Saltbox/master/library/saltbox_facts.py>),
//! and Python's reader rules are
//! <https://docs.python.org/3/library/configparser.html#supported-ini-file-structure>:
//! only `=` separates a key from its value, quotes and `#` inside a value
//! are literal, a line indented deeper than its key continues the value,
//! and a `#` comment or blank line ends it. Saltbox opens the file as
//! text, so a lone `\r` would end a line there: sb refuses one. Its
//! whitespace is Python's `str.isspace` ([`is_space`]), and indents are
//! counted in characters, as Python counts them. `;` comments, which
//! sb-go's older writer produced, are accepted and kept too, except where
//! Python would read the line as a continuation of the value before it.
//!
//! Why not an INI crate (docs/prototypes/proto-facts.md, "Library
//! audit"): rust-ini 0.21.3 and configparser 3.3.0 both drop comments
//! while parsing and rebuild the whole file when writing; rust-ini also
//! writes a newline as a `\n` escape, not a continuation line, and by
//! default unquotes and unescapes values, which can change a password.
//! `ini_core` 0.2.0 has no writer and no continuation lines; ini-roundtrip
//! has very low adoption. Keeping every untouched line byte for byte is
//! Saltbox-specific domain code.

use std::collections::{BTreeMap, HashSet};

use sb_cli_core::config::escape_controls;

use super::{Fact, Instance, Role};

/// The section Saltbox's parser treats as defaults; never an instance.
const DEFAULT: &str = "DEFAULT";

/// Why a fact file can't be read, or an edit can't be written. No
/// message shows a value (a fact can be a secret); names are escaped
/// (cli-core's rule).
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DocumentError {
    #[error("facts must be UTF-8 text without NUL")]
    NotText,
    #[error("carriage return without a line feed at line {0}")]
    LoneCarriageReturn(usize),
    #[error("malformed section at line {0}")]
    MalformedSection(usize),
    #[error("unexpected section suffix at line {0}")]
    SectionSuffix(usize),
    #[error("invalid section at line {0}")]
    InvalidSection(usize),
    #[error("duplicate instance \"{}\"", escape_controls(.0))]
    DuplicateInstance(String),
    #[error("invalid fact at line {0}")]
    InvalidFact(usize),
    #[error("duplicate key \"{}\" in {}", escape_controls(.key), escape_controls(.instance))]
    DuplicateKey { instance: String, key: String },
    #[error("unknown or reserved instance \"{}\"", escape_controls(.0))]
    UnknownInstance(String),
    #[error("unknown key \"{}\"", escape_controls(.0))]
    UnknownKey(String),
    #[error("key name \"{}\" contains unsupported characters", escape_controls(.0))]
    KeyName(String),
    #[error("fact values must not contain carriage returns or NUL")]
    ValueCharacters,
    #[error("conflicting changes in instance \"{}\"", escape_controls(.0))]
    Conflict(String),
    #[error("value whitespace cannot be represented without loss")]
    Whitespace,
    #[error("value for {}/{} cannot be represented without loss", escape_controls(.instance), escape_controls(.key))]
    Lossy { instance: String, key: String },
    /// The result wouldn't parse, or would read other facts differently.
    #[error("the changes cannot be written without altering other facts in the file")]
    Merged,
}

/// One change to a role's file. Renaming and new instances stay out, as
/// in sb-go; deleting the whole file is the session's (`apply`). Its
/// `Debug` leaves the value out (a fact can be a secret).
#[derive(Clone, PartialEq, Eq)]
pub enum Edit {
    /// Change a key's value, or add the key to an existing instance.
    Set {
        instance: String,
        key: String,
        value: String,
    },
    /// Remove a key and its continuation lines.
    DeleteFact { instance: String, key: String },
    /// Remove an instance: its header and every line up to the next one.
    DeleteInstance { instance: String },
}

impl std::fmt::Debug for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Set { instance, key, .. } => f
                .debug_struct("Set")
                .field("instance", instance)
                .field("key", key)
                .finish_non_exhaustive(),
            Self::DeleteFact { instance, key } => f
                .debug_struct("DeleteFact")
                .field("instance", instance)
                .field("key", key)
                .finish(),
            Self::DeleteInstance { instance } => f
                .debug_struct("DeleteInstance")
                .field("instance", instance)
                .finish(),
        }
    }
}

impl Edit {
    fn instance(&self) -> &str {
        match self {
            Self::Set { instance, .. }
            | Self::DeleteFact { instance, .. }
            | Self::DeleteInstance { instance } => instance,
        }
    }
}

/// A parsed fact file that keeps its source lines. Its `Debug` shows the
/// sections' and keys' names, never a line or a value.
#[derive(Clone)]
pub struct Document {
    /// The source split after each `\n`, as Go's `strings.SplitAfter`
    /// splits it: a text that is empty or ends in `\n` has an empty last
    /// line. Keys added to the last section are written after it, so the
    /// writer gives sb-go's bytes (a key added to `[a]\n` gives
    /// `[a]\n\nk = v\n`).
    lines: Vec<String>,
    /// The implicit `DEFAULT` section first (it holds any lines before the
    /// first header), then each header's, in file order.
    sections: Vec<Section>,
    /// `\r\n` when the file has one anywhere, so CRLF files stay CRLF.
    newline: &'static str,
}

#[derive(Clone)]
struct Section {
    name: String,
    /// The section's line range: its header up to the next header.
    start: usize,
    end: usize,
    keys: Vec<Entry>,
}

#[derive(Clone)]
struct Entry {
    name: String,
    value: String,
    /// The key's line and its continuation lines.
    start: usize,
    end: usize,
    /// The key line's leading whitespace, in characters: a deeper line
    /// continues the value.
    indent: usize,
    /// The length of the key line up to its value: the key, `=` and the
    /// spaces after it, which an edit keeps.
    prefix: usize,
}

/// Each section's name and its facts (key → value), in file order.
type Facts = Vec<(String, BTreeMap<String, String>)>;

/// A replacement of the lines `start..end` with `text`.
struct LineEdit {
    start: usize,
    end: usize,
    text: String,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut list = f.debug_map();
        for section in &self.sections {
            let keys: Vec<&str> = section.keys.iter().map(|key| key.name.as_str()).collect();
            list.entry(&section.name, &keys);
        }
        list.finish()
    }
}

impl Section {
    fn new(name: String, start: usize) -> Self {
        Self {
            name,
            start,
            end: start,
            keys: Vec::new(),
        }
    }

    fn key(&self, name: &str) -> Option<&Entry> {
        self.keys.iter().find(|key| key.name == name)
    }
}

/// `line` without its `\n` or `\r\n`.
fn without_newline(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Whether a trimmed line is a comment.
fn is_comment(trimmed: &str) -> bool {
    trimmed.starts_with(['#', ';'])
}

/// Python's `str.isspace`, which `strip` and configparser's indent use:
/// Unicode `White_Space` and the separators U+001C to U+001F
/// (<https://docs.python.org/3/library/stdtypes.html#str.isspace>).
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `text` stripped as Python's `str.strip` strips it.
fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

impl Document {
    /// Parses a fact file.
    ///
    /// # Errors
    ///
    /// [`DocumentError`] for text that isn't UTF-8 or holds a NUL, a
    /// malformed or duplicate section, a line that is no key, and a
    /// duplicate key, each with its line or name as sb-go words it.
    pub fn parse(data: &[u8]) -> Result<Self, DocumentError> {
        let text = std::str::from_utf8(data)
            .ok()
            .filter(|text| !text.contains('\0'))
            .ok_or(DocumentError::NotText)?;
        if let Some(at) = text
            .match_indices('\r')
            .find(|(at, _)| !text[at + 1..].starts_with('\n'))
        {
            let line = text[..at.0].matches('\n').count() + 1;
            return Err(DocumentError::LoneCarriageReturn(line));
        }
        let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
        if text.is_empty() || text.ends_with('\n') {
            lines.push(String::new());
        }
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut sections = Vec::new();
        let mut current = Section::new(DEFAULT.to_owned(), 0);
        let mut seen = HashSet::new();
        let mut keys = HashSet::new();
        // Whether the current section's last key can take a continuation.
        let mut continuing = false;
        for (i, line) in lines.iter().enumerate() {
            let raw = without_newline(line);
            let trimmed = strip(raw);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continuing = false;
                continue;
            }
            let indent = raw.chars().take_while(|&c| is_space(c)).count();
            // Before the `;` comments: Python reads `\t; x` after a value as
            // more of the value (its only comment prefix is `#`).
            if let Some(previous) = current
                .keys
                .last_mut()
                .filter(|previous| continuing && indent > previous.indent)
            {
                previous.value.push('\n');
                previous.value.push_str(trimmed);
                previous.end = i + 1;
                continue;
            }
            continuing = false;
            if trimmed.starts_with(';') {
                continue;
            }
            if trimmed.starts_with('[') {
                let name = section_name(trimmed, i + 1)?;
                if !seen.insert(name.to_owned()) {
                    return Err(DocumentError::DuplicateInstance(name.to_owned()));
                }
                keys.clear();
                current.end = i;
                if name == DEFAULT && sections.is_empty() && current.keys.is_empty() {
                    // The first header names the implicit section.
                    current.start = i;
                } else {
                    let next = Section::new(name.to_owned(), i);
                    sections.push(std::mem::replace(&mut current, next));
                }
                continue;
            }
            // The first `=` of the line is the first of its trimmed text.
            let (key, value) = raw
                .split_once('=')
                .ok_or(DocumentError::InvalidFact(i + 1))?;
            let key = strip(key);
            if key.is_empty() || key.chars().any(char::is_control) {
                return Err(DocumentError::InvalidFact(i + 1));
            }
            if !keys.insert(key.to_owned()) {
                return Err(DocumentError::DuplicateKey {
                    instance: current.name.clone(),
                    key: key.to_owned(),
                });
            }
            current.keys.push(Entry {
                name: key.to_owned(),
                value: strip(value).to_owned(),
                start: i,
                end: i + 1,
                indent,
                prefix: raw.len() - value.trim_start_matches(is_space).len(),
            });
            continuing = true;
        }
        current.end = lines.len();
        sections.push(current);
        Ok(Self {
            lines,
            sections,
            newline,
        })
    }

    fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    /// The role `name` as the editor lists it: every instance but
    /// `DEFAULT`, sorted by name, each with its facts sorted by key.
    #[must_use]
    pub fn role(&self, name: &str) -> Role {
        let mut instances: Vec<Instance> = self
            .sections
            .iter()
            .filter(|section| section.name != DEFAULT)
            .map(|section| {
                let mut facts: Vec<Fact> = section
                    .keys
                    .iter()
                    .map(|key| Fact {
                        key: key.name.clone(),
                        value: key.value.clone(),
                    })
                    .collect();
                facts.sort_by(|a, b| a.key.cmp(&b.key));
                Instance {
                    name: section.name.clone(),
                    facts,
                }
            })
            .collect();
        instances.sort_by(|a, b| a.name.cmp(&b.name));
        Role {
            name: name.to_owned(),
            instances,
        }
    }

    /// The file's bytes with `edits` made, every other line as it was.
    ///
    /// An edited key keeps its own `key =` spacing; a new key is written
    /// `key = value` at the end of its instance. A multi-line value is
    /// written with `\t` continuations (Saltbox's writer's form,
    /// `configparser`'s `write`). Setting a key to its value changes
    /// nothing. The result is parsed again: each value set must read back
    /// exactly, and every other fact as it read before.
    ///
    /// # Errors
    ///
    /// [`DocumentError`] for an unknown or `DEFAULT` instance, an unknown
    /// key to delete, a new key name Saltbox's reader would misread, a CR
    /// or NUL in a value, two edits of one key or an edit under a deleted
    /// instance, a value the file can't hold without loss
    /// ([`DocumentError::Whitespace`], [`DocumentError::Lossy`]), and a
    /// result that wouldn't parse or would read another fact differently
    /// ([`DocumentError::Merged`]).
    pub fn apply(&self, edits: &[Edit]) -> Result<Vec<u8>, DocumentError> {
        let mut changes = Vec::new();
        for (i, edit) in edits.iter().enumerate() {
            if let Some(change) = self.line_edit(edit, &edits[..i])? {
                changes.push(change);
            }
        }
        // Stable, by start then end: an insertion at a section's end comes
        // before the deletion of the section that starts there.
        changes.sort_by_key(|change| (change.start, change.end));
        let mut out = String::new();
        let mut cursor = 0;
        for change in changes {
            out.extend(self.lines[cursor..change.start].iter().map(String::as_str));
            out.push_str(&change.text);
            cursor = change.end;
        }
        out.extend(self.lines[cursor..].iter().map(String::as_str));
        // A result that doesn't parse would change how the file reads.
        let parsed = Self::parse(out.as_bytes()).map_err(|_| DocumentError::Merged)?;
        for edit in edits {
            if let Edit::Set {
                instance,
                key,
                value,
            } = edit
            {
                let read_back = parsed
                    .section(instance)
                    .and_then(|section| section.key(key));
                if read_back.is_none_or(|entry| entry.value != *value) {
                    return Err(DocumentError::Lossy {
                        instance: instance.clone(),
                        key: key.clone(),
                    });
                }
            }
        }
        // Every other fact must read as it did. sb-go checks only the values
        // set; a line next to an edit can still change meaning (an indented
        // header after a deleted instance would continue the value before it).
        if parsed.facts() != self.facts_after(edits) {
            return Err(DocumentError::Merged);
        }
        Ok(out.into_bytes())
    }

    /// Each section's name and facts, in file order.
    /// The `DEFAULT` sections are one entry, first, as Python merges
    /// them: deleting the instance between an empty implicit one and a
    /// later `[DEFAULT]` header joins them.
    fn facts(&self) -> Facts {
        let pairs = |section: &Section| {
            section
                .keys
                .iter()
                .map(|key| (key.name.clone(), key.value.clone()))
                .collect::<Vec<_>>()
        };
        let defaults = self
            .sections
            .iter()
            .filter(|section| section.name == DEFAULT)
            .flat_map(pairs)
            .collect();
        std::iter::once((DEFAULT.to_owned(), defaults))
            .chain(
                self.sections
                    .iter()
                    .filter(|section| section.name != DEFAULT)
                    .map(|section| (section.name.clone(), pairs(section).into_iter().collect())),
            )
            .collect()
    }

    /// [`Self::facts`] as `edits` should leave them.
    fn facts_after(&self, edits: &[Edit]) -> Facts {
        let mut facts = self.facts();
        for edit in edits {
            match edit {
                Edit::Set { key, value, .. } => {
                    if let Some(keys) = keys_of(&mut facts, edit.instance()) {
                        keys.insert(key.clone(), value.clone());
                    }
                }
                Edit::DeleteFact { key, .. } => {
                    if let Some(keys) = keys_of(&mut facts, edit.instance()) {
                        keys.remove(key);
                    }
                }
                Edit::DeleteInstance { instance } => facts.retain(|(name, _)| name != instance),
            }
        }
        facts
    }

    /// The line replacement `edit` makes, if any, after sb-go's
    /// `validateChanges` checks of it against the edits `before` it.
    fn line_edit(&self, edit: &Edit, before: &[Edit]) -> Result<Option<LineEdit>, DocumentError> {
        let name = edit.instance();
        let section = self
            .section(name)
            .filter(|_| name != DEFAULT)
            .ok_or_else(|| DocumentError::UnknownInstance(name.to_owned()))?;
        let conflicts = before.iter().any(|prior| {
            prior.instance() == name
                && match (prior, edit) {
                    (Edit::DeleteInstance { .. }, _) | (_, Edit::DeleteInstance { .. }) => true,
                    (
                        Edit::Set { key: a, .. } | Edit::DeleteFact { key: a, .. },
                        Edit::Set { key: b, .. } | Edit::DeleteFact { key: b, .. },
                    ) => a == b,
                }
        });
        if conflicts {
            return Err(DocumentError::Conflict(name.to_owned()));
        }
        let (key, value) = match edit {
            Edit::DeleteInstance { .. } => {
                return Ok(Some(LineEdit {
                    start: section.start,
                    end: section.end,
                    text: String::new(),
                }));
            }
            Edit::DeleteFact { key, .. } => {
                let entry = section
                    .key(key)
                    .ok_or_else(|| DocumentError::UnknownKey(key.clone()))?;
                // A comment ends a value, so a key's lines hold none to keep.
                return Ok(Some(LineEdit {
                    start: entry.start,
                    end: entry.end,
                    text: String::new(),
                }));
            }
            Edit::Set { key, value, .. } => (key, value),
        };
        if value.contains(['\r', '\0']) {
            return Err(DocumentError::ValueCharacters);
        }
        let existing = section.key(key);
        if existing.is_none() {
            check_key(key)?;
        }
        if existing.is_some_and(|entry| entry.value == *value) {
            return Ok(None);
        }
        let value_lines: Vec<&str> = value.split('\n').collect();
        let several = value_lines.len() > 1;
        if value_lines
            .iter()
            .any(|line| *line != strip(line) || (several && line.is_empty()))
        {
            return Err(DocumentError::Whitespace);
        }
        let body = value_lines.join(&format!("{}\t", self.newline));
        if let Some(entry) = existing {
            // The key line up to `=` and the spaces after it, then the value.
            let prefix = &self.lines[entry.start][..entry.prefix];
            let mut text = format!("{prefix}{body}");
            // A key on the last line keeps the file's missing newline.
            if entry.end < self.lines.len() {
                text.push_str(self.newline);
            }
            return Ok(Some(LineEdit {
                start: entry.start,
                end: entry.end,
                text,
            }));
        }
        let start = section.end;
        // The section's last line may lack its newline (end of file).
        let joint = if self.lines[..start]
            .last()
            .is_some_and(|line| !line.ends_with('\n'))
        {
            self.newline
        } else {
            ""
        };
        Ok(Some(LineEdit {
            start,
            end: start,
            text: format!("{joint}{key} = {body}{}", self.newline),
        }))
    }
}

/// The facts of the section `name` in `facts`.
fn keys_of<'a>(facts: &'a mut Facts, name: &str) -> Option<&'a mut BTreeMap<String, String>> {
    let (_, keys) = facts.iter_mut().find(|(section, _)| section == name)?;
    Some(keys)
}

/// The header's name; `trimmed` starts with `[`. A comment may follow
/// the `]`; the name holds no control character and no bracket.
fn section_name(trimmed: &str, line: usize) -> Result<&str, DocumentError> {
    let end = trimmed
        .rfind(']')
        .filter(|&end| end >= 2)
        .ok_or(DocumentError::MalformedSection(line))?;
    let tail = strip(&trimmed[end + 1..]);
    if !tail.is_empty() && !is_comment(tail) {
        return Err(DocumentError::SectionSuffix(line));
    }
    let name = &trimmed[1..end];
    if name.contains(|c: char| c.is_control() || c == '[' || c == ']') {
        return Err(DocumentError::InvalidSection(line));
    }
    Ok(name)
}

/// A new key's name: one Saltbox's reader reads back as that key (sb-go's
/// `validateKey`). Brackets inside it are fine (`headers[Authorization]`);
/// a leading `[` would read as a header.
fn check_key(key: &str) -> Result<(), DocumentError> {
    if key.is_empty()
        || strip(key) != key
        || key.contains(|c: char| c.is_control() || c == '=')
        || key.starts_with(['[', '#', ';'])
    {
        return Err(DocumentError::KeyName(key.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
