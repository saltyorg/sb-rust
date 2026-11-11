//! The comparison tools' logic (journal spec, Testing Strategy): the JSON
//! form journalctl prints, the verdicts of `compare`, `more`, `hash-check`
//! and `decode-all`, the query the tools' `--match` builds, and the
//! fixtures `gen-export` and `gen-boot` write. The example
//! (`examples/journal.rs`) is a thin clap wrapper over these, so coverage
//! and cargo-mutants reach the logic (plan, "The `testing` feature").

use crate::file::{JournalFile, hex_id};
use crate::{
    Cursor, FieldName, Journal, Page, Position, Query, QueryError, ReadErrors, Record, Release,
};
use serde_json::{Map, Value};
use std::fmt;
use std::io::{self, Write};

/// Why a tool couldn't give a verdict: its input isn't what it reads.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// A line starting with `{` isn't a JSON object.
    #[error("line {line}: not a JSON object")]
    Json {
        /// The line, from 1.
        line: usize,
        /// `serde_json`'s error.
        #[source]
        source: serde_json::Error,
    },
    /// An entry has no `__CURSOR` string, or one that isn't a cursor.
    #[error("line {line}: no journal cursor")]
    NoCursor {
        /// The line, from 1.
        line: usize,
    },
    /// An entry lacks a field `select` reads, or holds it in another form.
    #[error("line {line}: no {name} as text")]
    Field {
        /// The line, from 1.
        line: usize,
        /// The field.
        name: &'static str,
    },
    /// The page's entries aren't one run of the whole's, in order.
    #[error("the page isn't a run of the whole's entries")]
    NotARun,
    /// An empty page's cursor isn't among the whole's entries, so where
    /// the page lies can't be told.
    #[error("the cursor isn't one of the whole's entries")]
    CursorNotInWhole,
    /// A `--match` without `=`.
    #[error("{0:?}: expected FIELD=value")]
    Match(String),
    /// A query that can't be built.
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// systemd's `utf8_is_printable`: valid UTF-8 without C0 controls other
/// than tab and newline, DEL or C1 controls, and without noncharacters,
/// which `unichar_is_valid` refuses (src/basic/utf8.c): U+FDD0..U+FDEF and
/// every code point ending in FFFE or FFFF.
#[must_use]
pub fn printable(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|s| {
        s.chars().all(|c| {
            let c = u32::from(c);
            !((c < 0x20 && c != 0x09 && c != 0x0a)
                || (0x7f..=0x9f).contains(&c)
                || (0xfdd0..=0xfdef).contains(&c)
                || c & 0xfffe == 0xfffe)
        })
    })
}

/// One record as `journalctl -o json --all` prints it on `release`
/// (`output_json`, src/shared/logs-show.c): the address fields first, then
/// every field; a value that isn't printable UTF-8 is an array of byte
/// values; a field carried more than once is an array of its values.
/// `serde_json`'s map is ordered by key
/// (<https://docs.rs/serde_json/1.0.154/serde_json/map/struct.Map.html>),
/// so field order doesn't count when two forms are compared.
#[must_use]
pub fn json(record: &Record, release: &Release) -> Value {
    let c = &record.cursor;
    let mut m = Map::new();
    m.insert("__CURSOR".into(), Value::String(c.to_string()));
    m.insert(
        "__REALTIME_TIMESTAMP".into(),
        Value::String(c.realtime.to_string()),
    );
    m.insert(
        "__MONOTONIC_TIMESTAMP".into(),
        Value::String(c.monotonic.to_string()),
    );
    m.insert("_BOOT_ID".into(), Value::String(hex_id(&c.boot_id)));
    if release.prints_seqnum() {
        m.insert("__SEQNUM".into(), Value::String(c.seqnum.to_string()));
        m.insert("__SEQNUM_ID".into(), Value::String(hex_id(&c.seqnum_id)));
    }
    let value = |b: &[u8]| {
        if printable(b) {
            Value::String(String::from_utf8_lossy(b).into_owned())
        } else {
            Value::Array(b.iter().map(|&x| Value::from(x)).collect())
        }
    };
    for (name, v) in record.fields() {
        let name = String::from_utf8_lossy(name);
        // The address field comes from the entry header; journalctl skips
        // the data field of the same name (logs-show.c output_json).
        if name == "_BOOT_ID" {
            continue;
        }
        match m.get_mut(name.as_ref()) {
            // Already a list of values (a byte array is a list of numbers).
            Some(Value::Array(a)) if a.first().is_some_and(|x| !x.is_number()) => {
                a.push(value(v));
            }
            Some(existing) => {
                let first = existing.take();
                *existing = Value::Array(vec![first, value(v)]);
            }
            None => {
                m.insert(name.into_owned(), value(v));
            }
        }
    }
    Value::Object(m)
}

/// One `-o json` entry: its fields by name.
type Object = Map<String, Value>;

/// Each line of `text` starting with `{` as a field map, with its line
/// number from 1; other lines (journalctl 249's `-- Journal begins at …
/// --`) are skipped.
fn objects(text: &str) -> Result<Vec<(usize, Object)>, ToolError> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.starts_with('{') {
            let line = i + 1;
            let m = serde_json::from_str(l).map_err(|source| ToolError::Json { line, source })?;
            out.push((line, m));
        }
    }
    Ok(out)
}

/// `compare`'s verdict: two `-o json` streams, field by field, values
/// exact as JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    /// The expected side's entries.
    pub expected: usize,
    /// The actual side's entries.
    pub actual: usize,
    /// Fields that differ, over every entry.
    pub mismatches: usize,
    /// The first differences, as `entry I: field F differs`.
    pub shown: Vec<String>,
}

impl Comparison {
    /// The differences named in full; the rest are counted.
    pub const SHOWN: usize = 20;

    /// The same entries with the same fields and values.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.expected == self.actual && self.mismatches == 0
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.expected != self.actual {
            return write!(
                f,
                "count differs: expected {} entries, got {}",
                self.expected, self.actual
            );
        }
        for line in &self.shown {
            writeln!(f, "{line}")?;
        }
        write!(
            f,
            "compared {} entries: {} mismatches",
            self.expected, self.mismatches
        )
    }
}

/// `expected` and `actual`, each one JSON object per line, compared entry
/// by entry and field by field. A different entry count fails at once:
/// zipping two streams of different lengths would misalign them.
///
/// # Errors
///
/// A line starting with `{` that isn't a JSON object.
pub fn compare(expected: &str, actual: &str) -> Result<Comparison, ToolError> {
    let want: Vec<_> = objects(expected)?.into_iter().map(|(_, m)| m).collect();
    let got: Vec<_> = objects(actual)?.into_iter().map(|(_, m)| m).collect();
    let mut out = Comparison {
        expected: want.len(),
        actual: got.len(),
        mismatches: 0,
        shown: Vec::new(),
    };
    if want.len() != got.len() {
        return Ok(out);
    }
    for (i, (w, g)) in want.iter().zip(&got).enumerate() {
        let mut keys: Vec<&String> = w.keys().chain(g.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            if w.get(k) != g.get(k) {
                out.mismatches += 1;
                if out.shown.len() < Comparison::SHOWN {
                    out.shown.push(format!("entry {i}: field {k} differs"));
                }
            }
        }
    }
    Ok(out)
}

/// A page's `more_older` and `more_newer`, printed as the tools compare
/// them: `more_older=B more_newer=B`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct More {
    /// A matching entry is older than the page's records.
    pub older: bool,
    /// A matching entry is newer than the page's records.
    pub newer: bool,
}

impl More {
    /// `page`'s flags.
    #[must_use]
    pub fn of(page: &Page) -> Self {
        Self {
            older: page.more_older,
            newer: page.more_newer,
        }
    }
}

impl fmt::Display for More {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "more_older={} more_newer={}", self.older, self.newer)
    }
}

/// Each entry's `__CURSOR`.
fn cursors(text: &str) -> Result<Vec<Cursor>, ToolError> {
    objects(text)?
        .into_iter()
        .map(|(line, m)| {
            m.get("__CURSOR")
                .and_then(Value::as_str)
                .and_then(|c| c.parse().ok())
                .ok_or(ToolError::NoCursor { line })
        })
        .collect()
}

/// Each entry's `name` field as text, one per entry, in order: a string
/// as it is, any other JSON value (a byte array, a list of values) as its
/// JSON text, and an entry without the field as an empty string. The
/// scripts read cursors and messages through it, not by matching the
/// JSON text. A string holding a newline spans lines when printed, so a
/// line per entry holds only for fields without one (cursors, and the
/// boot fixtures' messages, which is all the scripts read).
///
/// # Errors
///
/// A line starting with `{` that isn't a JSON object.
pub fn fields(text: &str, name: &str) -> Result<Vec<String>, ToolError> {
    Ok(objects(text)?
        .into_iter()
        .map(|(_, m)| match m.get(name) {
            Some(Value::String(v)) => v.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        })
        .collect())
}

/// What `select` keeps of a `-o json` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Selection {
    /// Every entry except systemd-coredump's (its `MESSAGE_ID`) whose
    /// `_UID` isn't one of these: journalctl's `-u` output as sb-journal
    /// matches it where Difference 1 applies.
    KeepCoredumpsOf(Vec<u32>),
    /// Only systemd-coredump's entries whose `_UID` is this.
    CoredumpsOf(u32),
    /// Only entries whose `__REALTIME_TIMESTAMP` is later than this.
    AfterRealtime(u64),
}

/// The text of a string field.
fn string<'a>(m: &'a Object, name: &str) -> Option<&'a str> {
    m.get(name).and_then(Value::as_str)
}

/// The lines of `text` holding the entries `selection` keeps, as they
/// are, in order.
///
/// # Errors
///
/// A line starting with `{` that isn't a JSON object; for
/// `AfterRealtime`, an entry without a decimal `__REALTIME_TIMESTAMP`.
pub fn select<'a>(text: &'a str, selection: &Selection) -> Result<Vec<&'a str>, ToolError> {
    let entries = objects(text)?;
    let lines: Vec<&str> = text.lines().collect();
    let coredump = |m: &Object| string(m, "MESSAGE_ID") == Some(COREDUMP);
    // journald writes `_UID` in decimal; anything else is no uid.
    let uid = |m: &Object| string(m, "_UID").and_then(|u| u.parse::<u32>().ok());
    let mut out = Vec::new();
    for (line, m) in entries {
        let keep = match selection {
            Selection::KeepCoredumpsOf(uids) => {
                !coredump(&m) || uid(&m).is_some_and(|u| uids.contains(&u))
            }
            Selection::CoredumpsOf(of) => coredump(&m) && uid(&m) == Some(*of),
            Selection::AfterRealtime(t) => {
                let realtime = string(&m, "__REALTIME_TIMESTAMP")
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or(ToolError::Field {
                        line,
                        name: "__REALTIME_TIMESTAMP",
                    })?;
                realtime > *t
            }
        };
        if keep {
            out.push(lines[line - 1]);
        }
    }
    Ok(out)
}

/// The flags a page at `at` should have, from `whole`: every entry the
/// query matches, in the order journalctl prints them. A page with records
/// has a matching entry older (newer) than them when `whole` has one
/// before its first (after its last). An empty page is placed by its
/// cursor, which must be in `whole`: for `Before(c)`, `c` itself is newer
/// and what precedes it older; for `After(c)`, `c` and what precedes it
/// are older and what follows newer. An empty tail page has nothing newer,
/// and something older only if anything matched (journal spec, "`Page`").
///
/// # Errors
///
/// An entry without a cursor; a page that isn't a run of `whole`, in
/// order; an empty page whose cursor `whole` doesn't hold.
pub fn more(whole: &str, page: &str, at: &Position) -> Result<More, ToolError> {
    let whole = cursors(whole)?;
    let page = cursors(page)?;
    if let Some(first) = page.first() {
        let start = whole
            .iter()
            .position(|c| c == first)
            .ok_or(ToolError::NotARun)?;
        if whole.get(start..start + page.len()) != Some(&page[..]) {
            return Err(ToolError::NotARun);
        }
        return Ok(More {
            older: start > 0,
            newer: start + page.len() < whole.len(),
        });
    }
    let at_cursor = |c: &Cursor| {
        whole
            .iter()
            .position(|w| w == c)
            .ok_or(ToolError::CursorNotInWhole)
    };
    Ok(match at {
        Position::Tail => More {
            older: !whole.is_empty(),
            newer: false,
        },
        Position::Before(c) => More {
            older: at_cursor(c)? > 0,
            newer: true,
        },
        Position::After(c) => More {
            older: true,
            newer: at_cursor(c)? + 1 < whole.len(),
        },
    })
}

/// The query of the tools' `--match FIELD=value` list: one term per field,
/// combined by AND, and a field given twice matches either value (`Query::or`), as
/// journalctl's matches combine. `None` for an empty list: every entry.
///
/// # Errors
///
/// A match without `=`, a field name journald doesn't allow, or past
/// `QUERY_GROUPS_MAX` groups.
pub fn matches(list: &[String]) -> Result<Option<Query>, ToolError> {
    let mut by_field: Vec<(&str, Query)> = Vec::new();
    for m in list {
        let (field, value) = m
            .split_once('=')
            .ok_or_else(|| ToolError::Match(m.clone()))?;
        let q = Query::field(&FieldName::new(field.as_bytes())?, value.as_bytes());
        match by_field.iter().position(|(f, _)| *f == field) {
            Some(i) => {
                let (f, prev) = by_field.remove(i);
                by_field.insert(i, (f, prev.or(q)));
            }
            None => by_field.push((field, q)),
        }
    }
    let mut terms = by_field.into_iter().map(|(_, q)| q);
    let Some(first) = terms.next() else {
        return Ok(None);
    };
    Ok(Some(terms.try_fold(first, Query::and)?))
}

/// The query matching every entry, as journalctl with no match.
#[must_use]
pub fn every_entry() -> Query {
    Query::every()
}

/// Write `records` to `out` as `-o json --all` lines on `release`, then
/// flush it, and `errors` to `err`, a line each (`sb-journal: ` and the
/// error with its sources), then the count of the omitted ones: what the
/// `follow` command prints for its page and each drain's batch, as they
/// come, so the scripts read them while it runs.
///
/// # Errors
///
/// Writing to either.
pub fn write_records(
    records: &[Record],
    errors: &ReadErrors,
    release: &Release,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<()> {
    for e in &errors.shown {
        writeln!(err, "sb-journal: {}", text(e))?;
    }
    if errors.omitted > 0 {
        writeln!(err, "sb-journal: {} more errors", errors.omitted)?;
    }
    for r in records {
        writeln!(out, "{}", json(r, release))?;
    }
    out.flush()
}

/// The files `journal`'s pages would read, opened as a page opens them,
/// with the errors listing and opening them gave (one per file).
#[must_use]
pub fn files(journal: &Journal) -> (Vec<JournalFile>, ReadErrors) {
    let mut errors = ReadErrors::default();
    let files = crate::walk::open_listed(|| journal.list(), &journal.opener, &mut errors);
    (files, errors)
}

/// An error with its sources, as one line.
fn text(e: &crate::ReadError) -> String {
    crate::key(e)
}

/// `hash-check`'s verdict: every DATA object's hash computed again and
/// compared with the one it stores.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HashCheck {
    /// In the order met: `error: …` lines, and one line per file, `NAME:
    /// flags F, header S bytes, keyed K, N bad hashes`.
    pub lines: Vec<String>,
    /// Objects whose hash matches.
    pub matched: u64,
    /// Objects whose hash differs.
    pub differ: u64,
    /// Files, listings and objects that couldn't be read.
    pub errors: u64,
}

impl HashCheck {
    /// No hash differs and nothing failed to read.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.differ == 0 && self.errors == 0
    }
}

impl fmt::Display for HashCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.lines {
            writeln!(f, "{line}")?;
        }
        write!(
            f,
            "hash-check: {} match, {} differ, {} errors",
            self.matched, self.differ, self.errors
        )
    }
}

/// Every DATA object of `files`, its payload hashed as its file hashes
/// (`JournalFile::hash`). A file or object that can't be read is an error,
/// counted with `opened`'s, and the rest are still checked.
#[must_use]
pub fn hash_check(files: &[JournalFile], opened: &ReadErrors) -> HashCheck {
    let mut out = HashCheck::default();
    for e in &opened.shown {
        out.errors += 1;
        out.lines.push(format!("error: {}", text(e)));
    }
    for f in files {
        let mut bad = 0;
        let all = f.all_data().unwrap_or_else(|e| {
            out.errors += 1;
            out.lines.push(format!("error: {}", text(&e)));
            Vec::new()
        });
        for (offset, stored) in all {
            match f.data_payload(offset) {
                Ok(p) if f.hash(&p) == stored => out.matched += 1,
                Ok(_) => bad += 1,
                Err(e) => {
                    out.errors += 1;
                    out.lines.push(format!("error: {}", text(&e)));
                }
            }
        }
        out.differ += bad;
        out.lines.push(format!(
            "{}: flags {:#x}, header {} bytes, keyed {}, {bad} bad hashes",
            f.path.file_name().unwrap_or_default().to_string_lossy(),
            f.header.incompatible,
            f.header.size,
            f.header.keyed(),
        ));
    }
    out
}

/// `decode-all`'s verdict: every entry of every file decoded, each item's
/// payload too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodeAll {
    /// Open errors as the page reports them (one per distinct error), then
    /// the first `SHOWN` read errors.
    pub lines: Vec<String>,
    /// Files read.
    pub files: usize,
    /// Entries decoded.
    pub entries: u64,
    /// Everything that couldn't be read, open errors included.
    pub errors: u64,
}

impl DecodeAll {
    /// Read errors named in full; the rest are counted.
    pub const SHOWN: u64 = 10;

    /// Nothing failed to read.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.errors == 0
    }
}

impl fmt::Display for DecodeAll {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.lines {
            writeln!(f, "{line}")?;
        }
        write!(
            f,
            "decode-all: {} files, {} entries, {} errors",
            self.files, self.entries, self.errors
        )
    }
}

/// Every entry of `files`, newest first through each file's main list, and
/// every DATA object it names. An error in the list is reported and the
/// walk goes on with the older arrays; each entry must end before the next
/// (objects never overlap), so a corrupt size can't make each read the
/// rest of the file.
#[must_use]
pub fn decode_all(files: &[JournalFile], opened: &ReadErrors) -> DecodeAll {
    let mut out = DecodeAll {
        files: files.len(),
        ..DecodeAll::default()
    };
    for e in &opened.shown {
        out.errors += 1;
        out.lines.push(format!("open error: {}", text(e)));
    }
    let mut read_errors = 0;
    let mut fail = |out: &mut DecodeAll, what: &str, e: &crate::ReadError| {
        out.errors += 1;
        read_errors += 1;
        if read_errors <= DecodeAll::SHOWN {
            out.lines.push(format!("{what} error: {}", text(e)));
        }
    };
    for f in files {
        let mut offsets = Vec::new();
        for r in f.all_entries_rev() {
            match r {
                Ok(offset) => offsets.push(offset),
                Err(e) => fail(&mut out, "entry array", &e),
            }
        }
        offsets.reverse();
        for (k, &offset) in offsets.iter().enumerate() {
            let end = offsets.get(k + 1).copied().unwrap_or(u64::MAX);
            match f.entry_before(offset, end) {
                Ok(e) => {
                    out.entries += 1;
                    for item in e.items {
                        if let Err(e) = f.data_payload(item) {
                            fail(&mut out, "data", &e);
                        }
                    }
                }
                Err(e) => fail(&mut out, "entry", &e),
            }
        }
    }
    out
}

/// One export-format field (<https://systemd.io/JOURNAL_EXPORT_FORMATS/>):
/// text as `NAME=value`; a value with a newline or a byte that isn't
/// printable as `NAME`, newline, its little-endian 64-bit length, the
/// bytes, newline.
fn export_field(out: &mut impl Write, name: &str, value: &[u8]) -> io::Result<()> {
    out.write_all(name.as_bytes())?;
    if value.contains(&b'\n') || !printable(value) {
        out.write_all(b"\n")?;
        out.write_all(&(value.len() as u64).to_le_bytes())?;
    } else {
        out.write_all(b"=")?;
    }
    out.write_all(value)?;
    out.write_all(b"\n")
}

/// The fixtures' first realtime, in microseconds (2023-11-14).
pub const EXPORT_BASE: u64 = 1_700_000_000_000_000;

/// systemd-coredump's `MESSAGE_ID` (catalog `SD_MESSAGE_COREDUMP`).
const COREDUMP: &str = "fc2e22bc6ee647b6b90729ab34a250b1";

/// The matrix's fixture, in export format for systemd-journal-remote:
/// `entries` entries over two boots, covering every `-u` branch for
/// `demo.service` with decoys, ANSI and binary values, large (compressed)
/// and repeated fields. `generation` 0 is the matrix's fixture; 1 and up
/// are what a host writes after an upgrade, later and on other boots, to
/// merge with files from before it (compat.sh).
///
/// With `coredump_uid`, half the uid-1000 coredump decoys (entry `i` with
/// `i % 24 == 20`) are logged by that uid instead: on 26.04 the local
/// journal's `-u` takes them, on 22.04 and 24.04 it doesn't (decisions 1
/// and 4).
///
/// # Errors
///
/// Writing to `out`.
pub fn gen_export(
    out: &mut impl Write,
    entries: u64,
    generation: u8,
    coredump_uid: Option<u32>,
) -> io::Result<()> {
    // Synthetic boot ids, their first byte XORed with the generation (so
    // generation 0 keeps the matrix's ids).
    let boots = [
        format!("{generation:02x}112233445566778899aabbccddeeff"),
        format!("{:02x}eeddccbbaa99887766554433221100", 0xff ^ generation),
    ];
    // Each generation starts 10^6 s (about 11.6 days) after the last.
    let base = EXPORT_BASE + u64::from(generation) * 1_000_000_000_000;
    // A fixed-seed xorshift: the same fixtures on every run and release.
    // Why not fastrand (in the tree through tempfile): its seeded stream
    // isn't promised to stay the same across its versions, and the
    // fixtures must not change with a dependency update.
    let mut rng: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for i in 0..entries {
        let boot = &boots[usize::from(i >= entries / 2)];
        let f = &mut *out;
        export_field(
            f,
            "__REALTIME_TIMESTAMP",
            (base + i * 1_000_000).to_string().as_bytes(),
        )?;
        export_field(
            f,
            "__MONOTONIC_TIMESTAMP",
            (1_000_000 + i * 1000).to_string().as_bytes(),
        )?;
        export_field(f, "_BOOT_ID", boot.as_bytes())?;
        export_field(f, "_TRANSPORT", b"journal")?;
        let (fields, pid, uid) = branch(i, coredump_uid);
        for (k, v) in &fields {
            export_field(f, k, v.as_bytes())?;
        }
        export_field(f, "_PID", pid.as_bytes())?;
        export_field(f, "_UID", uid.as_bytes())?;
        export_field(f, "SYSLOG_IDENTIFIER", b"sb-eval")?;
        export_field(f, "PRIORITY", (i % 8).to_string().as_bytes())?;
        let message = format!("entry {i}");
        match i % 7 {
            0 => export_field(
                f,
                "MESSAGE",
                format!("\x1b[31m{message} red\x1b[0m").as_bytes(),
            )?,
            1 => export_field(f, "MESSAGE", &[0xff, 0xfe, b'b', b'i', b'n', 0x00, 0x01])?,
            // Large and repetitive: compressed by journald (> 512 bytes).
            2 => export_field(
                f,
                "MESSAGE",
                format!("{message} {}", "compress me ".repeat(900)).as_bytes(),
            )?,
            3 => {
                // Large and random: stored as written or compressed, per writer.
                let noise: String = (0..6000)
                    .map(|_| char::from(b'a'.saturating_add(letter(next()))))
                    .collect();
                export_field(f, "MESSAGE", format!("{message} {noise}").as_bytes())?;
            }
            4 => {
                export_field(
                    f,
                    "MESSAGE",
                    format!("{message}\nsecond line\tTab").as_bytes(),
                )?;
                export_field(f, "EXTRA", b"one")?;
                export_field(f, "EXTRA", b"two")?;
                // The same FIELD=value twice: systemd 249 links the entry
                // twice in the value's entry array.
                export_field(f, "SAME", b"again")?;
                export_field(f, "SAME", b"again")?;
            }
            5 => export_field(
                f,
                "MESSAGE",
                "unicode \u{00e6}\u{00f8}\u{00e5} \u{1f980} \u{0085}c1".as_bytes(),
            )?,
            _ => export_field(f, "MESSAGE", message.as_bytes())?,
        }
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// One of 26 letters, from a random number.
fn letter(r: u64) -> u8 {
    u8::try_from(r % 26).expect("below 26")
}

/// Entry `i`'s unit fields, pid and uid: the branch of `-u demo.service`
/// it exercises. Decoys match on no release, or only on some (noted).
fn branch(
    i: u64,
    coredump_uid: Option<u32>,
) -> (Vec<(&'static str, String)>, &'static str, String) {
    let unit = |k: &'static str| vec![(k, "demo.service".to_owned())];
    let coredump = || {
        vec![
            ("MESSAGE_ID", COREDUMP.to_owned()),
            ("COREDUMP_UNIT", "demo.service".to_owned()),
        ]
    };
    let root = "0".to_owned();
    let user = "1000".to_owned();
    match i % 12 {
        0..=2 => (unit("_SYSTEMD_UNIT"), "4242", user),
        3 => (unit("UNIT"), "1", root),
        // Decoy: not PID 1.
        4 => (unit("UNIT"), "77", root),
        5 => (unit("OBJECT_SYSTEMD_UNIT"), "99", root),
        // Decoy: not root.
        6 => (unit("OBJECT_SYSTEMD_UNIT"), "99", user),
        7 => (coredump(), "500", root),
        // The systemd-coredump user: 26.04's local `-u` takes it.
        8 if i % 24 == 20 && coredump_uid.is_some() => (
            coredump(),
            "501",
            coredump_uid.unwrap_or_default().to_string(),
        ),
        // Decoy everywhere since decision 1 (sb); journalctl 259 with
        // `--file` or without the systemd-coredump user takes it.
        8 => (coredump(), "501", user),
        // 259.1 and later: a match (init.scope helper); 249 and 255: a
        // decoy (not PID 1).
        9 if i % 24 == 9 => {
            let mut f = unit("UNIT");
            f.push(("_SYSTEMD_CGROUP", "/init.scope".to_owned()));
            (f, "88", root)
        }
        // Decoy: no MESSAGE_ID.
        9 => (unit("COREDUMP_UNIT"), "502", root),
        10 => (
            vec![("_SYSTEMD_UNIT", "other.service".to_owned())],
            "4343",
            user,
        ),
        // Decoy: a prefix.
        _ => (
            vec![("_SYSTEMD_UNIT", "demodemo.service".to_owned())],
            "4444",
            user,
        ),
    }
}

/// A boot fixture in export format: one entry per element of
/// `realtimes_s`, at `EXPORT_BASE` plus that many seconds, all on `boot`
/// (32 hex digits), monotonic time rising by a millisecond from one
/// second, each with `MESSAGE=<prefix><k>` counting from 1 and
/// `SYSLOG_IDENTIFIER=sb-boots`. Two such files with overlapping realtimes
/// are two boots whose clocks overlap (decision 11, Difference 20);
/// realtimes that step back in one file are a clock set back within a
/// boot (decision 10).
///
/// With `machine`, the entries carry `_MACHINE_ID=` that id, as
/// systemd-journal-remote keeps a source machine's own (its file's header
/// names the receiving machine): 26.04's boot table takes the tail entry's
/// (decision 11, issue #34169). `machine.untagged_tail` leaves it off the
/// last entry, whose header id then stays.
///
/// # Errors
///
/// Writing to `out`.
pub fn gen_boot(
    out: &mut impl Write,
    boot: &str,
    prefix: &str,
    realtimes_s: &[u64],
    machine: Option<SourceMachine<'_>>,
) -> io::Result<()> {
    let last = realtimes_s.len();
    for (k, &s) in (1u64..).zip(realtimes_s) {
        export_field(
            out,
            "__REALTIME_TIMESTAMP",
            (EXPORT_BASE + s * 1_000_000).to_string().as_bytes(),
        )?;
        export_field(
            out,
            "__MONOTONIC_TIMESTAMP",
            (1_000_000 + k * 1000).to_string().as_bytes(),
        )?;
        export_field(out, "_BOOT_ID", boot.as_bytes())?;
        if let Some(m) = machine {
            let tail = usize::try_from(k).is_ok_and(|k| k == last);
            if !(tail && m.untagged_tail) {
                export_field(out, "_MACHINE_ID", m.id.as_bytes())?;
            }
        }
        export_field(out, "_TRANSPORT", b"journal")?;
        export_field(out, "_PID", b"4242")?;
        export_field(out, "_UID", b"0")?;
        export_field(out, "SYSLOG_IDENTIFIER", b"sb-boots")?;
        export_field(out, "PRIORITY", b"6")?;
        export_field(out, "MESSAGE", format!("{prefix}{k}").as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// The source machine of a `gen_boot` fixture.
#[derive(Debug, Clone, Copy)]
pub struct SourceMachine<'a> {
    /// Its id, 32 hex digits, for each entry's `_MACHINE_ID=`.
    pub id: &'a str,
    /// The last entry carries none.
    pub untagged_tail: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted;
    use crate::{Limits, Source};

    #[test]
    fn printable_follows_systemd() {
        assert!(printable(b"plain\ttext\n"));
        assert!(!printable(b"\x1b[31mred\x1b[0m"));
        assert!(!printable(&[0xff, 0xfe]));
        assert!(!printable("\u{85}".as_bytes()));
        // Noncharacters: valid UTF-8, refused by systemd's unichar_is_valid.
        assert!(!printable("\u{fdd0}".as_bytes()));
        assert!(!printable("\u{fffe}".as_bytes()));
        assert!(!printable("\u{1ffff}".as_bytes()));
        assert!(printable("\u{fdcf}\u{fff0}".as_bytes()));
    }

    /// The edges of each refused range.
    #[test]
    fn printable_ranges_end_where_systemds_do() {
        for refused in [0x00, 0x08, 0x0b, 0x1f, 0x7f, 0x9f, 0xfdd0, 0xfdef, 0xffff] {
            let c = char::from_u32(refused).expect("a char");
            assert!(!printable(c.to_string().as_bytes()), "{refused:#x}");
        }
        for taken in [0x20, 0x7e, 0xa0, 0xfdcf, 0xfdf0, 0xfffd] {
            let c = char::from_u32(taken).expect("a char");
            assert!(printable(c.to_string().as_bytes()), "{taken:#x}");
        }
    }

    /// The records of a crafted file whose entries carry `fields`.
    fn records(fields: &[&[&[u8]]]) -> Vec<Record> {
        let (mut c, _) = crafted::journal_of(fields);
        let t = c.file();
        let release = Release::parse("24.04").expect("a row");
        let j =
            Journal::open(Source::Files(vec![t.path().to_path_buf()]), &release).expect("opens");
        let page = j.page(&every_entry(), &Position::Tail, 10, &Limits::default());
        assert!(page.errors.is_empty(), "{:?}", page.errors);
        page.records
    }

    /// journalctl's `-o json` shape: the header fields, strings and byte
    /// arrays, repeated fields as a list, and `_BOOT_ID` from the header.
    #[test]
    fn json_is_journalctls_form() {
        let r = records(&[&[
            b"A=1",
            b"B=\x1b",
            b"A=2",
            b"A=3",
            b"C=\xff",
            b"C=x",
            b"_BOOT_ID=ignored",
        ]]);
        let v = json(&r[0], &Release::parse("22.04").expect("a row"));
        let c = r[0].cursor();
        let want = serde_json::json!({
            "__CURSOR": c.to_string(),
            "__REALTIME_TIMESTAMP": "1000001",
            "__MONOTONIC_TIMESTAMP": "1",
            "_BOOT_ID": "07070707070707070707070707070707",
            "MESSAGE": "m0",
            "A": ["1", "2", "3"],
            "B": [27],
            "C": [[255], "x"],
        });
        assert_eq!(v, want);
    }

    /// 24.04 and 26.04 print the sequence number and its id too.
    #[test]
    fn json_prints_the_seqnum_where_the_release_does() {
        let r = records(&[&[]]);
        let v = json(&r[0], &Release::parse("26.04").expect("a row"));
        assert_eq!(v["__SEQNUM"], "1");
        assert_eq!(v["__SEQNUM_ID"], "00000000000000000000000000000000");
        let v = json(&r[0], &Release::parse("22.04").expect("a row"));
        assert!(v.get("__SEQNUM").is_none() && v.get("__SEQNUM_ID").is_none());
    }

    /// A field whose first value is a byte array, repeated: a list of the
    /// array and the next value, not the array extended.
    #[test]
    fn json_lists_a_repeated_byte_array_value() {
        let r = records(&[&[b"B=\x01", b"B=\x02", b"B=t"]]);
        let v = json(&r[0], &Release::parse("22.04").expect("a row"));
        assert_eq!(v["B"], serde_json::json!([[1], [2], "t"]));
    }

    #[test]
    fn equal_streams_compare_equal_whatever_the_field_order() {
        let a = "-- Journal begins --\n{\"A\":\"1\",\"B\":[1,2]}\n{\"A\":\"2\"}\n";
        let b = "{\"B\":[1,2],\"A\":\"1\"}\n{\"A\":\"2\"}\n";
        let c = compare(a, b).expect("compares");
        assert!(c.passed());
        assert_eq!((c.expected, c.actual, c.mismatches), (2, 2, 0));
        assert_eq!(c.to_string(), "compared 2 entries: 0 mismatches");
    }

    /// A value's shape counts (a string against a byte array), a field
    /// missing on one side counts, and so does a list's order.
    #[test]
    fn every_differing_field_counts() {
        let a = "{\"A\":\"1\",\"B\":\"x\",\"L\":[\"a\",\"b\"]}\n";
        let b = "{\"A\":[49],\"L\":[\"b\",\"a\"]}\n";
        let c = compare(a, b).expect("compares");
        assert!(!c.passed());
        assert_eq!(c.mismatches, 3);
        assert_eq!(
            c.to_string(),
            "entry 0: field A differs\nentry 0: field B differs\nentry 0: field L differs\n\
             compared 1 entries: 3 mismatches"
        );
    }

    #[test]
    fn a_different_count_fails_at_once() {
        let c = compare("{}\n{}\n", "{}\n").expect("compares");
        assert!(!c.passed());
        assert_eq!(c.to_string(), "count differs: expected 2 entries, got 1");
        // Equal entries, but one more on the actual side.
        assert!(!compare("{}\n", "{}\n{}\n").expect("compares").passed());
    }

    /// Twenty differences are named, the rest counted.
    #[test]
    fn the_first_twenty_differences_are_named() {
        let a = "{\"A\":\"1\"}\n".repeat(25);
        let b = "{\"A\":\"2\"}\n".repeat(25);
        let c = compare(&a, &b).expect("compares");
        assert_eq!(c.mismatches, 25);
        assert_eq!(c.shown.len(), Comparison::SHOWN);
        assert_eq!(c.shown[19], "entry 19: field A differs");
    }

    #[test]
    fn a_line_that_isnt_an_object_is_an_error() {
        assert!(matches!(
            compare("{}\n{nope\n", "{}\n"),
            Err(ToolError::Json { line: 2, .. })
        ));
        assert!(matches!(
            compare("{}\n", "x\n{}\n[]\n"),
            Ok(Comparison { actual: 1, .. })
        ));
        assert!(matches!(
            compare("{}\n", "{}\n{\"a\":1}\n{1}"),
            Err(ToolError::Json { line: 3, .. })
        ));
    }

    /// Both readers report lines from 1.
    #[test]
    fn errors_name_their_line() {
        let e = compare("{}\n", "{]").expect_err("not JSON");
        assert_eq!(e.to_string(), "line 1: not a JSON object");
        let e = more("x\n{\"a\":1}", "", &Position::Tail).expect_err("no cursor");
        assert_eq!(e.to_string(), "line 2: no journal cursor");
        let e = more("{\"__CURSOR\":\"x\"}", "", &Position::Tail).expect_err("bad cursor");
        assert_eq!(e.to_string(), "line 1: no journal cursor");
    }

    #[test]
    fn fields_are_text_whatever_their_form() {
        let text = "x\n{\"M\":\"a \\\"q\\\"\"}\n{\"M\":[1,2]}\n{}\n{\"M\":[\"a\",\"b\"]}\n";
        let f = fields(text, "M").expect("reads");
        assert_eq!(f, ["a \"q\"", "[1,2]", "", "[\"a\",\"b\"]"]);
        assert!(matches!(
            fields("{x", "M"),
            Err(ToolError::Json { line: 1, .. })
        ));
    }

    fn coredump_line(uid: &str) -> String {
        format!("{{\"MESSAGE_ID\":\"{COREDUMP}\",\"_UID\":\"{uid}\",\"M\":\"_UID=0\"}}")
    }

    /// Difference 1's selection reads the fields, not the text: a value
    /// holding `"_UID":"0"` doesn't make a uid-1000 coredump root's.
    #[test]
    fn coredumps_are_selected_by_their_uid_field() {
        let other = "{\"_UID\":\"1000\",\"MESSAGE\":\"\\\"_UID\\\":\\\"0\\\"\"}";
        let text = [
            coredump_line("0"),
            coredump_line("1000"),
            coredump_line("997"),
            other.to_owned(),
            "{\"MESSAGE_ID\":[1],\"_UID\":\"5\"}".to_owned(),
        ]
        .join("\n");
        let keep = |uids: &[u32]| {
            select(&text, &Selection::KeepCoredumpsOf(uids.to_vec())).expect("selects")
        };
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(keep(&[0]), [lines[0], lines[3], lines[4]]);
        assert_eq!(keep(&[0, 997]), [lines[0], lines[2], lines[3], lines[4]]);
        let of = select(&text, &Selection::CoredumpsOf(997)).expect("selects");
        assert_eq!(of, [lines[2]]);
        let of = select(&text, &Selection::CoredumpsOf(5)).expect("selects");
        assert_eq!(of, [] as [&str; 0]);
        // A coredump entry without a uid is no uid's.
        let bare = format!("{{\"MESSAGE_ID\":\"{COREDUMP}\"}}");
        assert_eq!(
            select(&bare, &Selection::KeepCoredumpsOf(vec![0])).expect("selects"),
            [] as [&str; 0]
        );
    }

    #[test]
    fn entries_after_a_realtime_are_selected() {
        let text = "{\"__REALTIME_TIMESTAMP\":\"5\"}\n{\"__REALTIME_TIMESTAMP\":\"6\"}\n\
                    {\"__REALTIME_TIMESTAMP\":\"7\"}";
        let lines: Vec<&str> = text.lines().collect();
        let after = select(text, &Selection::AfterRealtime(5)).expect("selects");
        assert_eq!(after, [lines[1], lines[2]]);
        let e = select(
            "{\"__REALTIME_TIMESTAMP\":[1]}",
            &Selection::AfterRealtime(0),
        )
        .expect_err("not text");
        assert_eq!(e.to_string(), "line 1: no __REALTIME_TIMESTAMP as text");
        assert!(matches!(
            select(
                "{\"__REALTIME_TIMESTAMP\":\"1\"}\n{\"__REALTIME_TIMESTAMP\":\"x\"}",
                &Selection::AfterRealtime(0)
            ),
            Err(ToolError::Field { line: 2, .. })
        ));
    }

    /// Cursors for `more`'s tests, the entries' JSON lines and the text of
    /// the run `range` of them.
    fn entries(n: u64) -> Vec<String> {
        (1..=n)
            .map(|i| {
                format!(
                    "{{\"__CURSOR\":\"s={};i={i:x};b={};m={i:x};t={i:x};x=0\"}}",
                    "1".repeat(32),
                    "2".repeat(32)
                )
            })
            .collect()
    }

    fn cursor(lines: &[String], k: usize) -> Cursor {
        let text = lines[k]
            .strip_prefix("{\"__CURSOR\":\"")
            .and_then(|s| s.strip_suffix("\"}"))
            .expect("a cursor line");
        text.parse().expect("a cursor")
    }

    #[test]
    fn a_pages_flags_are_what_the_whole_holds_beyond_it() {
        let all = entries(5);
        let whole = all.join("\n");
        let flags = |range: std::ops::Range<usize>| {
            more(&whole, &all[range].join("\n"), &Position::Tail).expect("a run")
        };
        let m = |older, newer| More { older, newer };
        assert_eq!(flags(0..5), m(false, false));
        assert_eq!(flags(0..2), m(false, true));
        assert_eq!(flags(1..4), m(true, true));
        assert_eq!(flags(3..5), m(true, false));
        assert_eq!(
            m(true, false).to_string(),
            "more_older=true more_newer=false"
        );
    }

    #[test]
    fn a_page_out_of_the_wholes_order_is_refused() {
        let all = entries(5);
        let whole = all.join("\n");
        let page = [all[1].clone(), all[3].clone()].join("\n");
        assert!(matches!(
            more(&whole, &page, &Position::Tail),
            Err(ToolError::NotARun)
        ));
        // Running past the whole's end.
        let past = [all[4].clone(), all[0].clone()].join("\n");
        assert!(matches!(
            more(&whole, &past, &Position::Tail),
            Err(ToolError::NotARun)
        ));
        let other = entries(7)[6].clone();
        assert!(matches!(
            more(&whole, &other, &Position::Tail),
            Err(ToolError::NotARun)
        ));
    }

    /// An empty page lies at its cursor.
    #[test]
    fn an_empty_pages_flags_are_placed_by_its_cursor() {
        let all = entries(3);
        let whole = all.join("\n");
        let m = |older, newer| More { older, newer };
        let at = |p: Position| more(&whole, "", &p).expect("placed");
        assert_eq!(at(Position::Tail), m(true, false));
        assert_eq!(
            more("", "", &Position::Tail).expect("placed"),
            m(false, false)
        );
        assert_eq!(at(Position::Before(cursor(&all, 0))), m(false, true));
        assert_eq!(at(Position::Before(cursor(&all, 1))), m(true, true));
        assert_eq!(at(Position::After(cursor(&all, 2))), m(true, false));
        assert_eq!(at(Position::After(cursor(&all, 1))), m(true, true));
        let elsewhere = cursor(&entries(4), 3);
        assert!(matches!(
            more(&whole, "", &Position::After(elsewhere)),
            Err(ToolError::CursorNotInWhole)
        ));
        assert!(matches!(
            more(&whole, "", &Position::Before(elsewhere)),
            Err(ToolError::CursorNotInWhole)
        ));
    }

    /// A page's own flags, as `More` prints them.
    #[test]
    fn more_of_a_page_is_its_flags() {
        let (mut c, _) = crafted::journal(3);
        let t = c.file();
        let release = Release::parse("22.04").expect("a row");
        let j =
            Journal::open(Source::Files(vec![t.path().to_path_buf()]), &release).expect("opens");
        let page = j.page(&every_entry(), &Position::Tail, 2, &Limits::default());
        assert_eq!(
            More::of(&page),
            More {
                older: true,
                newer: false
            }
        );
        let back = Position::Before(page.records[0].cursor());
        let page = j.page(&every_entry(), &back, 1, &Limits::default());
        assert_eq!(
            More::of(&page),
            More {
                older: false,
                newer: true
            }
        );
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|&s| s.to_owned()).collect()
    }

    /// One term per field, a field given twice either value: `A=1 B=2 A=3` is
    /// `(A=1 OR A=3) AND B=2`.
    #[test]
    fn matches_and_fields_and_or_values() {
        let q = matches(&strings(&["A=1", "B=2", "A=3"]))
            .expect("builds")
            .expect("a query");
        let name = |n: &[u8]| FieldName::new(n).expect("a name");
        let want = Query::field(&name(b"A"), b"1")
            .or(Query::field(&name(b"A"), b"3"))
            .and(Query::field(&name(b"B"), b"2"))
            .expect("builds");
        assert_eq!(q, want);
        // A value may hold `=`: only the first one splits.
        let q = matches(&strings(&["A=x=y"])).expect("builds");
        assert_eq!(q, Some(Query::field(&name(b"A"), b"x=y")));
        assert_eq!(matches(&[]).expect("builds"), None);
    }

    #[test]
    fn a_match_must_be_field_and_value() {
        assert!(matches!(
            matches(&strings(&["A"])),
            Err(ToolError::Match(m)) if m == "A"
        ));
        assert!(matches!(
            matches(&strings(&["a=1"])),
            Err(ToolError::Query(QueryError::FieldName(_)))
        ));
        let many: Vec<String> = (0..7)
            .flat_map(|f| [format!("F{f}=1"), format!("F{f}=2")])
            .collect();
        assert!(matches!(
            matches(&many),
            Err(ToolError::Query(QueryError::TooManyGroups { groups: 128 }))
        ));
    }

    /// `files` opens what a page would, reporting a file that can't be
    /// opened, once.
    #[test]
    fn files_are_a_pages_files() {
        let (mut c, _) = crafted::journal(2);
        let t = c.file();
        let missing = t.path().with_extension("gone");
        let release = Release::parse("22.04").expect("a row");
        let source = Source::Files(vec![t.path().to_path_buf(), missing.clone()]);
        let j = Journal::open(source, &release).expect("opens");
        let (files, errors) = files(&j);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, t.path());
        assert_eq!(errors.shown.len(), 1);
        assert_eq!(errors.shown[0].path(), Some(missing.as_path()));
    }

    fn opened(c: &mut crafted::Crafted) -> (tempfile::NamedTempFile, JournalFile) {
        c.write()
    }

    #[test]
    fn hash_check_passes_a_sound_file() {
        let (mut c, _) = crafted::journal(3);
        let (t, f) = opened(&mut c);
        let name = t.path().file_name().expect("a name").to_string_lossy();
        let h = hash_check(&[f], &ReadErrors::default());
        assert!(h.passed());
        // U=x and three messages.
        assert_eq!((h.matched, h.differ, h.errors), (4, 0, 0));
        assert_eq!(
            h.to_string(),
            format!(
                "{name}: flags 0x0, header 272 bytes, keyed false, 0 bad hashes\n\
                 hash-check: 4 match, 0 differ, 0 errors"
            )
        );
    }

    /// A stored hash that isn't the payload's differs; a payload that
    /// can't be read is an error; both fail the check.
    #[test]
    fn hash_check_counts_bad_hashes_and_errors() {
        let mut c = crafted::Crafted::new();
        c.hash_table(1);
        c.data(b"A=1");
        c.data_raw(0, b"B=1", 99);
        let (_t, f) = opened(&mut c);
        let h = hash_check(&[f], &ReadErrors::default());
        assert!(!h.passed());
        assert_eq!((h.matched, h.differ, h.errors), (1, 1, 0));
        assert!(h.lines[0].ends_with(", 1 bad hashes"), "{:?}", h.lines);

        let mut c = crafted::Crafted::new();
        c.hash_table(1);
        let d = c.data(b"A=1");
        c.set(d + crafted::O_SIZE, 20);
        let (_t, f) = opened(&mut c);
        let h = hash_check(&[f], &ReadErrors::default());
        assert!(!h.passed());
        assert_eq!((h.matched, h.differ, h.errors), (0, 0, 1));
        assert!(h.lines[0].starts_with("error: "), "{:?}", h.lines);
        assert!(h.lines[0].len() > "error: ".len() + 20, "{:?}", h.lines);
    }

    /// An object that is listed but whose payload doesn't decode (xz flag,
    /// not xz) is an error of its own; the sound one still matches.
    #[test]
    fn hash_check_counts_a_payload_that_doesnt_decode() {
        let mut c = crafted::Crafted::new();
        c.hash_table(1);
        c.data(b"A=1");
        c.data_raw(1, b"not xz at all", 5);
        let (_t, f) = opened(&mut c);
        let h = hash_check(&[f], &ReadErrors::default());
        assert!(!h.passed());
        assert_eq!((h.matched, h.differ, h.errors), (1, 0, 1));
        assert_eq!(h.lines.len(), 2, "{:?}", h.lines);
        assert!(h.lines[0].starts_with("error: "), "{:?}", h.lines);
        assert!(h.lines[1].ends_with(", 0 bad hashes"), "{:?}", h.lines);
    }

    /// A hash table that can't be read is one error, and the open errors
    /// count.
    #[test]
    fn hash_check_reports_a_bad_table_and_open_errors() {
        let mut c = crafted::Crafted::new();
        c.hash_table(1);
        c.set(crafted::H_HASH_TABLE_SIZE, 7);
        let (_t, f) = opened(&mut c);
        let (_, opened_errors) = open_errors();
        let h = hash_check(&[f], &opened_errors);
        assert!(!h.passed());
        assert_eq!((h.matched, h.differ, h.errors), (0, 0, 2));
        // The open error first, naming its file, then the table's.
        assert!(h.lines[0].starts_with("error: "), "{:?}", h.lines);
        assert!(h.lines[0].contains("none.journal"), "{:?}", h.lines);
        assert!(h.lines[1].starts_with("error: "), "{:?}", h.lines);
    }

    /// The errors of opening a file that doesn't exist.
    fn open_errors() -> (Vec<JournalFile>, ReadErrors) {
        let dir = tempfile::tempdir().expect("a directory");
        let release = Release::parse("22.04").expect("a row");
        let source = Source::Files(vec![dir.path().join("none.journal")]);
        let j = Journal::open(source, &release).expect("opens");
        files(&j)
    }

    #[test]
    fn decode_all_reads_every_entry() {
        let (mut c, _) = crafted::journal(4);
        let (_t, f) = opened(&mut c);
        let (mut c, _) = crafted::journal(2);
        let (_t2, g) = opened(&mut c);
        let d = decode_all(&[f, g], &ReadErrors::default());
        assert!(d.passed());
        assert_eq!((d.files, d.entries, d.errors), (2, 6, 0));
        assert_eq!(d.to_string(), "decode-all: 2 files, 6 entries, 0 errors");
    }

    /// An unreadable entry, an unreadable payload, an open error: each
    /// counted, named, and the rest still read.
    #[test]
    fn decode_all_counts_what_it_cant_read() {
        let (mut c, layout) = crafted::journal(3);
        // The second entry's size, too small for an item.
        c.set(layout.entries[1] + crafted::O_SIZE, 64);
        // The third entry's message, cut short.
        c.set(layout.messages[2] + crafted::O_SIZE, 20);
        let (_t, f) = opened(&mut c);
        let (_, opened_errors) = open_errors();
        let d = decode_all(&[f], &opened_errors);
        assert!(!d.passed());
        assert_eq!((d.files, d.entries, d.errors), (1, 2, 3));
        assert!(d.lines[0].starts_with("open error: "), "{:?}", d.lines);
        assert!(d.lines[1].starts_with("entry error: "), "{:?}", d.lines);
        assert!(d.lines[2].starts_with("data error: "), "{:?}", d.lines);
    }

    /// A broken main list is reported and the older arrays still read.
    #[test]
    fn decode_all_reports_a_broken_list() {
        let (mut c, layout) = crafted::journal(2);
        c.set(layout.main_array + crafted::A_ITEMS, 8);
        let (_t, f) = opened(&mut c);
        let d = decode_all(&[f], &ReadErrors::default());
        assert!(!d.passed());
        assert_eq!(d.entries, 1);
        assert!(
            d.lines.iter().any(|l| l.starts_with("entry array error: ")),
            "{:?}",
            d.lines
        );
    }

    /// Ten read errors are named; the rest only counted.
    #[test]
    fn decode_all_names_ten_errors() {
        let (mut c, layout) = crafted::journal(12);
        for &m in &layout.messages {
            c.set(m + crafted::O_SIZE, 20);
        }
        let (_t, f) = opened(&mut c);
        let d = decode_all(&[f], &ReadErrors::default());
        assert_eq!((d.entries, d.errors), (12, 12));
        assert_eq!(d.lines.len(), 10);
        // Open errors are all named, outside the ten.
        let (_, opened_errors) = open_errors();
        let (mut c, layout) = crafted::journal(12);
        for &m in &layout.messages {
            c.set(m + crafted::O_SIZE, 20);
        }
        let (_t, f) = opened(&mut c);
        let d = decode_all(&[f], &opened_errors);
        assert_eq!((d.errors, d.lines.len()), (13, 11));
    }

    #[test]
    fn export_fields_are_text_or_binary() {
        let mut out = Vec::new();
        export_field(&mut out, "A", b"text").expect("writes");
        export_field(&mut out, "B", b"two\nlines").expect("writes");
        export_field(&mut out, "C", b"\x1b").expect("writes");
        let mut want = b"A=text\nB\n".to_vec();
        want.extend_from_slice(&9u64.to_le_bytes());
        want.extend_from_slice(b"two\nlines\nC\n");
        want.extend_from_slice(&1u64.to_le_bytes());
        want.extend_from_slice(b"\x1b\n");
        assert_eq!(out, want);
    }

    /// The fields of each export entry, by name, text values only.
    fn exported(bytes: &[u8]) -> Vec<Vec<(String, String)>> {
        let text = String::from_utf8_lossy(bytes);
        text.split("\n\n")
            .filter(|e| !e.is_empty())
            .map(|e| {
                e.lines()
                    .filter_map(|l| l.split_once('='))
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect()
            })
            .collect()
    }

    fn field<'a>(entry: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        entry
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The fixture's times, boots and branches, entry by entry.
    #[test]
    fn the_export_fixture_is_laid_out() {
        let mut out = Vec::new();
        gen_export(&mut out, 48, 0, None).expect("writes");
        let e = exported(&out);
        assert_eq!(e.len(), 48);
        assert_eq!(field(&e[0], "__REALTIME_TIMESTAMP"), ["1700000000000000"]);
        assert_eq!(field(&e[47], "__REALTIME_TIMESTAMP"), ["1700000047000000"]);
        assert_eq!(field(&e[3], "__MONOTONIC_TIMESTAMP"), ["1003000"]);
        assert_eq!(
            field(&e[23], "_BOOT_ID"),
            ["00112233445566778899aabbccddeeff"]
        );
        assert_eq!(
            field(&e[24], "_BOOT_ID"),
            ["ffeeddccbbaa99887766554433221100"]
        );
        assert_eq!(field(&e[5], "PRIORITY"), ["5"]);
        assert_eq!(field(&e[6], "MESSAGE"), ["entry 6"]);
        assert_eq!(field(&e[11], "SYSLOG_IDENTIFIER"), ["sb-eval"]);
        let shape = |i: usize| {
            (
                field(&e[i], "_PID")[0],
                field(&e[i], "_UID")[0],
                field(&e[i], "_SYSTEMD_UNIT"),
            )
        };
        assert_eq!(shape(0), ("4242", "1000", vec!["demo.service"]));
        assert_eq!(shape(2), ("4242", "1000", vec!["demo.service"]));
        assert_eq!(shape(10), ("4343", "1000", vec!["other.service"]));
        assert_eq!(shape(11), ("4444", "1000", vec!["demodemo.service"]));
        assert_eq!(field(&e[3], "UNIT"), ["demo.service"]);
        assert_eq!(
            (field(&e[3], "_PID"), field(&e[4], "_PID")),
            (vec!["1"], vec!["77"])
        );
        assert_eq!(field(&e[5], "OBJECT_SYSTEMD_UNIT"), ["demo.service"]);
        assert_eq!(field(&e[6], "OBJECT_SYSTEMD_UNIT"), ["demo.service"]);
        assert_eq!(field(&e[6], "_PID"), ["99"]);
        assert_eq!(
            (field(&e[5], "_UID"), field(&e[6], "_UID")),
            (vec!["0"], vec!["1000"])
        );
        for i in [7, 8, 20] {
            assert_eq!(field(&e[i], "MESSAGE_ID"), [COREDUMP], "{i}");
            assert_eq!(field(&e[i], "COREDUMP_UNIT"), ["demo.service"], "{i}");
        }
        assert_eq!(
            (
                field(&e[7], "_UID"),
                field(&e[8], "_UID"),
                field(&e[20], "_UID")
            ),
            (vec!["0"], vec!["1000"], vec!["1000"])
        );
        assert_eq!(field(&e[9], "_SYSTEMD_CGROUP"), ["/init.scope"]);
        assert_eq!(field(&e[9], "_PID"), ["88"]);
        assert_eq!(field(&e[21], "_SYSTEMD_CGROUP"), [] as [&str; 0]);
        assert_eq!(field(&e[21], "COREDUMP_UNIT"), ["demo.service"]);
        assert_eq!(field(&e[21], "MESSAGE_ID"), [] as [&str; 0]);
        // Entry 4: two lines (binary), repeated fields.
        assert_eq!(field(&e[4], "EXTRA"), ["one", "two"]);
        assert_eq!(field(&e[4], "SAME"), ["again", "again"]);
        // Entry 5: C1 (U+0085) makes it binary too, as systemd prints it.
        assert_eq!(field(&e[5], "MESSAGE"), [] as [&str; 0]);
    }

    /// Each message variant: ANSI and binary values are binary fields,
    /// the large ones over 512 bytes, the same on every run.
    #[test]
    fn the_export_messages_vary() {
        let mut out = Vec::new();
        gen_export(&mut out, 7, 0, None).expect("writes");
        let text = String::from_utf8_lossy(&out);
        // ANSI (0), binary (1), two lines (4) and C1 (5) are
        // length-prefixed.
        assert_eq!(text.matches("\nMESSAGE\n").count(), 4);
        let e = exported(&out);
        assert!(field(&e[2], "MESSAGE")[0].starts_with("entry 2 compress me "));
        assert_eq!(
            field(&e[2], "MESSAGE")[0].len(),
            "entry 2 ".len() + 12 * 900
        );
        let noise = field(&e[3], "MESSAGE")[0];
        assert_eq!(noise.len(), "entry 3 ".len() + 6000);
        // The fixed seed's stream: the fixtures never change.
        assert!(
            noise.starts_with("entry 3 xavoihyhxdozbqebmaxsmdvhuarlpsbl"),
            "{}",
            &noise[..40]
        );
        assert!(
            noise["entry 3 ".len()..]
                .bytes()
                .all(|b| b.is_ascii_lowercase())
        );
        let mut again = Vec::new();
        gen_export(&mut again, 7, 0, None).expect("writes");
        assert_eq!(out, again);
        assert!(
            out.windows(7)
                .any(|w| w == [0xff, 0xfe, b'b', b'i', b'n', 0x00, 0x01])
        );
        assert!(text.contains("\x1b[31mentry 0 red\x1b[0m"));
        assert!(text.contains("unicode \u{e6}\u{f8}\u{e5} \u{1f980} \u{85}c1"));
    }

    /// The noise uses every letter.
    #[test]
    fn letters_cover_the_alphabet() {
        assert_eq!(
            (letter(0), letter(25), letter(26), letter(51)),
            (0, 25, 0, 25)
        );
    }

    /// Later generations: later times, other boots.
    #[test]
    fn a_later_generation_moves_on() {
        let mut out = Vec::new();
        gen_export(&mut out, 2, 1, None).expect("writes");
        let e = exported(&out);
        assert_eq!(field(&e[0], "__REALTIME_TIMESTAMP"), ["1701000000000000"]);
        assert_eq!(
            field(&e[0], "_BOOT_ID"),
            ["01112233445566778899aabbccddeeff"]
        );
        assert_eq!(
            field(&e[1], "_BOOT_ID"),
            ["feeeddccbbaa99887766554433221100"]
        );
    }

    /// With the systemd-coredump uid, every other uid-1000 coredump decoy
    /// is that user's.
    #[test]
    fn the_coredump_users_entries_replace_half_the_decoys() {
        let mut out = Vec::new();
        gen_export(&mut out, 48, 0, Some(997)).expect("writes");
        let e = exported(&out);
        assert_eq!(field(&e[20], "_UID"), ["997"]);
        assert_eq!(field(&e[20], "_PID"), ["501"]);
        assert_eq!(field(&e[20], "MESSAGE_ID"), [COREDUMP]);
        assert_eq!(
            (field(&e[8], "_UID"), field(&e[44], "_UID")),
            (vec!["1000"], vec!["997"])
        );
        assert_eq!(field(&e[32], "_UID"), ["1000"]);
    }

    #[test]
    fn a_boot_fixture_is_one_boot_at_the_times_given() {
        let mut out = Vec::new();
        gen_boot(&mut out, &"a".repeat(32), "a", &[100, 300, 5], None).expect("writes");
        let e = exported(&out);
        assert_eq!(e.len(), 3);
        let times: Vec<&str> = e
            .iter()
            .map(|x| field(x, "__REALTIME_TIMESTAMP")[0])
            .collect();
        assert_eq!(
            times,
            ["1700000100000000", "1700000300000000", "1700000005000000"]
        );
        let mono: Vec<&str> = e
            .iter()
            .map(|x| field(x, "__MONOTONIC_TIMESTAMP")[0])
            .collect();
        assert_eq!(mono, ["1001000", "1002000", "1003000"]);
        let messages: Vec<&str> = e.iter().map(|x| field(x, "MESSAGE")[0]).collect();
        assert_eq!(messages, ["a1", "a2", "a3"]);
        for x in &e {
            assert_eq!(field(x, "_BOOT_ID"), ["a".repeat(32)]);
            assert_eq!(field(x, "SYSLOG_IDENTIFIER"), ["sb-boots"]);
            assert_eq!(
                (
                    field(x, "_TRANSPORT"),
                    field(x, "_PID"),
                    field(x, "_UID"),
                    field(x, "PRIORITY")
                ),
                (vec!["journal"], vec!["4242"], vec!["0"], vec!["6"])
            );
            assert_eq!(field(x, "_MACHINE_ID"), [] as [&str; 0]);
        }
    }

    /// With a source machine, each entry names it in `_MACHINE_ID=`, the
    /// last one too unless it is to be untagged.
    #[test]
    fn a_boot_fixture_names_its_source_machine() {
        let id = "c".repeat(32);
        for untagged_tail in [false, true] {
            let mut out = Vec::new();
            let machine = SourceMachine {
                id: &id,
                untagged_tail,
            };
            gen_boot(&mut out, &"a".repeat(32), "a", &[1, 2, 3], Some(machine)).expect("writes");
            let e = exported(&out);
            let ids: Vec<Vec<&str>> = e.iter().map(|x| field(x, "_MACHINE_ID")).collect();
            let tail = if untagged_tail {
                vec![]
            } else {
                vec![id.as_str()]
            };
            assert_eq!(ids, [vec![id.as_str()], vec![id.as_str()], tail]);
        }
    }

    /// `write_records`: the records as JSON lines on `out`, flushed, and the
    /// errors with their sources on `err`, then the omitted count.
    #[test]
    fn records_and_errors_are_written_as_lines() {
        let records = records(&[&[b"A=1"], &[b"A=2"]]);
        let mut errors = ReadErrors::default();
        errors.add(crate::ReadError {
            path: Some("/j/x.journal".into()),
            kind: crate::ReadErrorKind::Io {
                context: "read at 0x10".into(),
                source: io::Error::other("gone"),
            },
        });
        errors.omitted = 2;
        let release = Release::parse("24.04").expect("a row");
        let mut out = io::BufWriter::new(Vec::new());
        let mut err = Vec::new();
        write_records(&records, &errors, &release, &mut out, &mut err).expect("writes");
        // Flushed: the lines are in the buffer's vector, not held.
        let lines = String::from_utf8(out.get_ref().clone()).expect("utf-8");
        let want: Vec<String> = records
            .iter()
            .map(|r| json(r, &release).to_string())
            .collect();
        assert_eq!(lines.lines().collect::<Vec<_>>(), want);
        assert_eq!(
            String::from_utf8(err).expect("utf-8"),
            "sb-journal: /j/x.journal: read at 0x10: gone\nsb-journal: 2 more errors\n"
        );
    }

    /// No errors, no lines on `err`.
    #[test]
    fn records_without_errors_write_nothing_else() {
        let records = records(&[&[b"A=1"]]);
        let release = Release::parse("22.04").expect("a row");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        write_records(
            &records,
            &ReadErrors::default(),
            &release,
            &mut out,
            &mut err,
        )
        .expect("writes");
        let out = String::from_utf8(out).expect("utf-8");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert_eq!(err, b"");
    }
}
