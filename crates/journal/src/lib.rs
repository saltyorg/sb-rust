//! sb's own read-only reader of systemd's journal files: pread only, pure
//! Rust, exact against each supported release's journalctl except the
//! differences the journal spec lists (docs/specs/journal.md). Why sb owns
//! the reader rather than using a crate or a `journalctl` child:
//! docs/decisions/ADR-013-own-journal-reader.md.
//!
//! A [`Journal`] names a [`Source`]; [`Journal::page`] reads the newest
//! entries matching a [`Query`], or the page before a [`Cursor`], as
//! [`Record`]s holding raw bytes.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// One declaration each, not a `pub` and a private one under opposite
// `cfg`s: cargo-mutants walks every `mod` without evaluating `cfg`, so two
// declarations made it test each of file.rs's mutants twice.
mod file;
mod hash;
mod rules;
#[cfg(feature = "testing")]
pub mod testing;
mod walk;

pub use rules::{
    FieldName, QUERY_GROUPS_MAX, Query, QueryError, RELEASES, Release, ReleaseError, Uid, UnitName,
    UnitRule,
};

/// The output limit's default, our own bound on the payload bytes one
/// query reads and decodes, not a journald value: a corrupt file can give
/// each entry a decompression bomb, or an entry listing one object
/// millions of times, and the records are held for output. Failed reads
/// spend it too, so it bounds work as well as memory. 256 MiB is far above
/// an `sb logs` page (500 entries are typically well under a MiB). It is
/// below journald's `ENTRY_SIZE_MAX` (770 MiB), so it also bounds one entry:
/// only an entry carrying a coredump stored in the journal comes near it,
/// and it is left out with the limit reported, where journalctl would
/// print it (journal spec, Difference 5).
const OUTPUT_DEFAULT: usize = 256 << 20;

/// The bytes of payload one query (a page, or one file's share of a
/// follow's wake) may read and decode. A page also holds at most this many
/// bytes of candidate entries before it decodes the ones it shows. A limit
/// below one candidate's size holds none: such a page shows nothing and
/// reports the held limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    output: usize,
}

impl Default for Limits {
    /// 256 MiB.
    fn default() -> Self {
        Self {
            output: OUTPUT_DEFAULT,
        }
    }
}

impl Limits {
    /// An output limit of `bytes`, capped at the journal format's own
    /// limit on one DATA object, systemd's `DATA_SIZE_MAX` (768 MiB): a
    /// larger payload is corruption or a bomb whatever the limit.
    #[must_use]
    pub fn with_output(bytes: usize) -> Self {
        Self {
            output: bytes.min(file::DATA_SIZE_MAX),
        }
    }

    /// The output limit in bytes.
    #[must_use]
    pub fn output(&self) -> usize {
        self.output
    }
}

/// A payload larger than the caller allowed. Not corruption by itself: a
/// lookup reads a candidate only as far as the value it looks for, and a
/// query stops once its output limit is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("payload over its limit of {limit} bytes")]
pub struct OverLimit {
    /// The bytes the read was allowed.
    pub limit: usize,
}

/// An entry lists a DATA object this query already failed to read: a
/// definitive error, not one to wait for, since the object had its read
/// (and, following, its wait) already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("more entries list data {item:#x}, which can't be read; left out too")]
pub struct KnownBad {
    /// The DATA object's offset in its file.
    pub item: u64,
}

/// What went wrong reading a journal, or what a page left out. The file,
/// when there is one, is the `ReadError`'s, so no message here names it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadErrorKind {
    /// A payload over the read's limit (`OverLimit`), with where it was
    /// met (for example `data 0x1f8: zstd`).
    #[error("{}", prefixed(context, over))]
    OverLimit {
        /// Where the payload was met; empty when nothing more is known.
        context: String,
        /// The limit the payload went over.
        over: OverLimit,
    },
    /// A DATA object this query already failed to read (`KnownBad`).
    #[error(transparent)]
    KnownBad(#[from] KnownBad),
    /// The file's content breaks the format: a bad offset, size, link,
    /// header or compressed payload.
    #[error("{0}")]
    Corrupt(String),
    /// The system refused a read.
    #[error("{context}")]
    Io {
        /// What was being read.
        context: String,
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// An entry larger than the whole output limit, met before anything
    /// else was read: left out, and the page goes on past it (Difference
    /// 5).
    #[error("{context}: an entry larger than the output limit of {limit} bytes, left out")]
    TooLarge {
        /// The read that went over (an `OverLimit`'s message).
        context: String,
        /// The output limit.
        limit: usize,
    },
    /// The output limit ended the page at an entry that didn't fit, after
    /// others were read: the older entries are left out, with no gap
    /// (Difference 5). No file: the limit is the page's.
    #[error("output limit of {limit} bytes reached; {left_out} older entries left out")]
    OutputLimit {
        /// The output limit.
        limit: usize,
        /// The entries chosen but not read.
        left_out: usize,
    },
    /// The page held as many candidate entries as its limit allows; the
    /// older ones are left out. No file: the limit is the page's.
    #[error("limit of held entries reached; older entries left out")]
    HeldLimit,
    /// The journal's file list changed every time it was read again after
    /// opening (rotations in a row). No file: the list is the source's.
    #[error("the journal's file list kept changing while it was opened; entries may be missing")]
    ListChanging,
}

/// `context: what`, or `what` alone without a context.
fn prefixed(context: &str, what: &dyn fmt::Display) -> String {
    if context.is_empty() {
        what.to_string()
    } else {
        format!("{context}: {what}")
    }
}

/// An error reading a journal: the file, when it is about one, and what
/// went wrong.
///
/// `Display` writes the path escaped (`str::escape_debug`), so control
/// bytes in a file name can't reach a terminal (security audit L5). No
/// message carries a field's name or value.
///
/// `Display` gives the file and the context only. For `ReadErrorKind::Io`
/// the system's error is not in it but is the `source()`, so callers
/// render the whole chain (anyhow's `{:#}`, or walking `source()`) to show
/// it.
#[derive(Debug)]
pub struct ReadError {
    path: Option<PathBuf>,
    kind: ReadErrorKind,
}

impl ReadError {
    /// An error about no one file: the page's own limits, the file list.
    fn of_page(kind: ReadErrorKind) -> Self {
        Self { path: None, kind }
    }

    /// The file the error is about; `None` for one about the page as a
    /// whole (`OutputLimit`, `HeldLimit`, `ListChanging`).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// What went wrong, for matching (`ReadErrorKind::OverLimit`,
    /// `ReadErrorKind::KnownBad`).
    #[must_use]
    pub fn kind(&self) -> &ReadErrorKind {
        &self.kind
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(
                f,
                "{}: {}",
                path.to_string_lossy().escape_debug(),
                self.kind
            ),
            None => write!(f, "{}", self.kind),
        }
    }
}

impl std::error::Error for ReadError {
    /// The kind's own source (the system's error for `Io`): the kind's
    /// message is already part of this error's.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&self.kind)
    }
}

/// The errors of one page: the first ten distinct read errors in full,
/// then a count of the further ones, so a badly broken file can't flood
/// the output or grow the list without bound. Errors opening or listing
/// files are one per file and shown outside that cap.
#[derive(Debug, Default)]
pub struct ReadErrors {
    /// The errors shown, in the order they were met.
    pub shown: Vec<ReadError>,
    /// The read errors past the first ten distinct ones.
    pub omitted: usize,
    /// The shown errors' text with their sources: what "distinct" compares.
    keys: Vec<String>,
    /// The shown errors under the cap (not open or listing errors).
    capped: usize,
}

impl ReadErrors {
    /// Read errors shown in full.
    pub const SHOWN: usize = 10;

    /// A read error: shown if it is new and fewer than ten are, else
    /// counted.
    fn add(&mut self, e: ReadError) {
        let key = key(&e);
        if self.keys.contains(&key) {
            return;
        }
        if self.capped < Self::SHOWN {
            self.capped += 1;
            self.keys.push(key);
            self.shown.push(e);
        } else {
            self.omitted += 1;
        }
    }

    /// An error opening or listing a file: shown, outside the cap.
    fn add_open(&mut self, e: ReadError) {
        let key = key(&e);
        if !self.keys.contains(&key) {
            self.keys.push(key);
            self.shown.push(e);
        }
    }

    /// No error shown or omitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shown.is_empty() && self.omitted == 0
    }
}

/// An error's message and its sources' messages: two errors are the same
/// when all of it is. Why not std's `Error::sources`: it is unstable
/// (`error_iter`).
fn key(e: &ReadError) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// Where a journal's pages read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// The local machine's journal, as journalctl reads it by default
    /// (`SD_JOURNAL_LOCAL_ONLY`): /run/log/journal/MACHINE-ID, then
    /// /var/log/journal/MACHINE-ID, in sd-journal's search order, which
    /// decides which copy of a flushed entry is shown.
    Local,
    /// A directory's `*.journal` and `*.journal~` files and those of its
    /// machine-id subdirectories, as `journalctl -D` reads them.
    Directory(PathBuf),
    /// These files, in this order, as `journalctl --file` reads them.
    Files(Vec<PathBuf>),
}

/// Where a page is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Position {
    /// The newest entries.
    Tail,
    /// The entries before this one, which isn't shown.
    Before(Cursor),
}

/// A page of entries, oldest first, and what went wrong reading it.
/// Built by `Journal::page` only: more fields can come.
#[derive(Debug)]
#[non_exhaustive]
pub struct Page {
    /// The entries, oldest first.
    pub records: Vec<Record>,
    /// What couldn't be read, and what the limits left out.
    pub errors: ReadErrors,
}

/// A journal that can't be read at all. Files that fail to open on a later
/// page are errors of that page, not an `OpenError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The machine id couldn't be read (`Source::Local`).
    #[error("read {}", .path.to_string_lossy().escape_debug())]
    MachineId {
        /// The machine-id file.
        path: PathBuf,
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// The directory couldn't be read (`Source::Directory`).
    #[error("read the journal directory {}", .path.to_string_lossy().escape_debug())]
    Directory {
        /// The directory.
        path: PathBuf,
        /// The system's error.
        #[source]
        source: io::Error,
    },
}

/// The files a journal's pages read, found again on every page.
#[derive(Debug, Clone)]
enum Roots {
    /// The local journal's directories, in search order; either may be
    /// missing.
    Local([PathBuf; 2]),
    Directory(PathBuf),
    Files(Vec<PathBuf>),
}

/// A source to read pages from. It holds no descriptors: every page lists
/// the files and opens them again, and closes them before it returns, so
/// a rotation or vacuum between pages is seen by the next one. That also
/// makes it `Send + Sync`, which a file's reader (keeping its arena limit
/// in a `Cell`) is not.
#[derive(Debug)]
pub struct Journal {
    roots: Roots,
}

/// The local journal's roots, in sd-journal's search order.
const LOCAL_ROOTS: [&str; 2] = ["/run/log/journal", "/var/log/journal"];

impl Journal {
    /// Checks the source: the machine id for `Source::Local`, the directory
    /// for `Source::Directory`.
    ///
    /// # Errors
    ///
    /// `OpenError` for a source that can't be read at all.
    pub fn open(source: Source) -> Result<Self, OpenError> {
        let roots = match source {
            Source::Local => Roots::Local(local_dirs(
                Path::new("/etc/machine-id"),
                LOCAL_ROOTS.map(Path::new),
            )?),
            Source::Directory(path) => match std::fs::read_dir(&path) {
                Ok(_) => Roots::Directory(path),
                Err(source) => return Err(OpenError::Directory { path, source }),
            },
            Source::Files(paths) => Roots::Files(paths),
        };
        Ok(Self { roots })
    }

    /// The `n` matching entries at `at`, oldest first: the newest ones
    /// (`Position::Tail`), as `journalctl -n` finds them, or those before a
    /// cursor. Every call lists and reopens the files, so an `Io` error in
    /// `errors` from a file cut short while the page read it is gone from
    /// the next page, which reads the file's header again.
    #[must_use]
    pub fn page(&self, q: &Query, at: &Position, n: usize, limits: &Limits) -> Page {
        let mut errors = ReadErrors::default();
        let files = walk::open_listed(|| self.list(), &mut errors);
        let before = match at {
            Position::Tail => None,
            Position::Before(c) => Some(c),
        };
        let bounds = walk::Bounds {
            payload: limits.output(),
            held: limits.output(),
        };
        let records = walk::tail(&files, q.groups(), n, before, bounds, &mut errors);
        Page { records, errors }
    }

    fn list(&self) -> walk::Listing {
        let mut out = walk::Listing::default();
        match &self.roots {
            Roots::Local(dirs) => {
                for d in dirs {
                    walk::journal_files_in(d, true, &mut out);
                }
            }
            Roots::Directory(d) => walk::directory_files(d, &mut out),
            Roots::Files(paths) => out.paths.clone_from(paths),
        }
        out
    }
}

/// The local journal's directories: each root joined with the machine id
/// read from `machine_id`. The id isn't validated here: `MachineId`
/// (security audit L1) is the plan's Task 4.
fn local_dirs(machine_id: &Path, roots: [&Path; 2]) -> Result<[PathBuf; 2], OpenError> {
    let id = std::fs::read_to_string(machine_id).map_err(|source| OpenError::MachineId {
        path: machine_id.to_path_buf(),
        source,
    })?;
    let id = id.trim();
    Ok(roots.map(|r| r.join(id)))
}

/// A position in the merged stream, and what a cursor names: sd-journal's
/// cursor text, `s=SEQNUM_ID;i=SEQNUM;b=BOOT_ID;m=MONOTONIC;t=REALTIME;x=XOR_HASH`
/// (`sd_journal_get_cursor`, src/libsystemd/sd-journal/sd-journal.c): the
/// ids as 32 lowercase hex digits, the numbers in hex. journalctl prints the
/// same text as `__CURSOR`. `Display` writes it and `FromStr` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
    seqnum_id: [u8; 16],
    seqnum: u64,
    boot_id: [u8; 16],
    monotonic: u64,
    realtime: u64,
    xor_hash: u64,
}

impl Cursor {
    /// sd-journal's order between two entries
    /// (`journal_file_compare_locations`): the same sequence-number space
    /// orders by seqnum; the same boot by monotonic time; otherwise
    /// realtime; the XOR hash breaks ties. It isn't transitive across
    /// seqnum ids and boots, so it is never handed to a sort.
    fn order(&self, other: &Self) -> std::cmp::Ordering {
        if self.seqnum_id == other.seqnum_id {
            let o = self.seqnum.cmp(&other.seqnum);
            if o.is_ne() {
                return o;
            }
        }
        if self.boot_id == other.boot_id {
            let o = self.monotonic.cmp(&other.monotonic);
            if o.is_ne() {
                return o;
            }
        }
        self.realtime
            .cmp(&other.realtime)
            .then(self.xor_hash.cmp(&other.xor_hash))
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "s={};i={:x};b={};m={:x};t={:x};x={:x}",
            file::hex_id(&self.seqnum_id),
            self.seqnum,
            file::hex_id(&self.boot_id),
            self.monotonic,
            self.realtime,
            self.xor_hash
        )
    }
}

impl std::str::FromStr for Cursor {
    type Err = QueryError;

    /// Each of the six keys once, in any order; other keys are ignored
    /// (sd-journal refuses a key longer than one letter; sb parses only
    /// cursors it or journalctl printed).
    fn from_str(text: &str) -> Result<Self, QueryError> {
        let mut c = Self {
            seqnum_id: [0; 16],
            seqnum: 0,
            boot_id: [0; 16],
            monotonic: 0,
            realtime: 0,
            xor_hash: 0,
        };
        // One bit per key: a repeated key can't stand in for a missing one.
        let mut seen = 0u8;
        for part in text.split(';') {
            let (k, v) = part.split_once('=').ok_or(QueryError::BadCursor)?;
            let bit = match k {
                "s" => {
                    c.seqnum_id = parse_id(v)?;
                    1
                }
                "i" => {
                    c.seqnum = parse_hex(v)?;
                    2
                }
                "b" => {
                    c.boot_id = parse_id(v)?;
                    4
                }
                "m" => {
                    c.monotonic = parse_hex(v)?;
                    8
                }
                "t" => {
                    c.realtime = parse_hex(v)?;
                    16
                }
                "x" => {
                    c.xor_hash = parse_hex(v)?;
                    32
                }
                _ => continue,
            };
            seen |= bit;
        }
        if seen == 63 {
            Ok(c)
        } else {
            Err(QueryError::BadCursor)
        }
    }
}

/// A 128-bit id: exactly 32 hex digits
/// (<https://docs.rs/hex/0.4.3/hex/fn.decode_to_slice.html>).
fn parse_id(v: &str) -> Result<[u8; 16], QueryError> {
    let mut out = [0u8; 16];
    hex::decode_to_slice(v, &mut out).map_err(|_| QueryError::BadCursor)?;
    Ok(out)
}

/// A cursor number: hex digits only. Deliberately stricter than systemd
/// 249's `sscanf("%llx")`, which also takes `+3`, `0x3` and trailing junk:
/// sb only parses cursors it or journalctl printed.
fn parse_hex(v: &str) -> Result<u64, QueryError> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(QueryError::BadCursor);
    }
    u64::from_str_radix(v, 16).map_err(|_| QueryError::BadCursor)
}

/// A boot's id, from the entry header (journalctl prints it as
/// `_BOOT_ID`). `Display` writes 32 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BootId([u8; 16]);

impl BootId {
    /// The id's 16 bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for BootId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&file::hex_id(&self.0))
    }
}

/// A syslog priority: journald's `PRIORITY` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Priority {
    /// 0, `LOG_EMERG`.
    Emergency,
    /// 1, `LOG_ALERT`.
    Alert,
    /// 2, `LOG_CRIT`.
    Critical,
    /// 3, `LOG_ERR`.
    Error,
    /// 4, `LOG_WARNING`.
    Warning,
    /// 5, `LOG_NOTICE`.
    Notice,
    /// 6, `LOG_INFO`.
    Info,
    /// 7, `LOG_DEBUG`.
    Debug,
}

impl Priority {
    /// A `PRIORITY` value as journalctl reads it: one digit, 0 to 7
    /// (`output_short`, src/shared/logs-show.c v255).
    fn parse(value: &[u8]) -> Option<Self> {
        Some(match value {
            b"0" => Self::Emergency,
            b"1" => Self::Alert,
            b"2" => Self::Critical,
            b"3" => Self::Error,
            b"4" => Self::Warning,
            b"5" => Self::Notice,
            b"6" => Self::Info,
            b"7" => Self::Debug,
            _ => return None,
        })
    }
}

/// One entry read for output: where it is, and its fields in item order,
/// raw (journal spec, assumption 5). Turning them into terminal text is
/// sb-term-ui's `DisplayText`.
#[derive(Debug, Clone)]
pub struct Record {
    cursor: Cursor,
    fields: Vec<Field>,
}

/// One `FIELD=value` DATA payload. The records of a page that carry the
/// same DATA object share its bytes: most fields of a unit's entries
/// repeat.
#[derive(Debug, Clone)]
struct Field {
    payload: Arc<[u8]>,
    /// Where the first `=` is.
    eq: usize,
}

impl Field {
    /// A payload split at its first `=`; `None` for a payload without one,
    /// which isn't a field.
    fn new(payload: Vec<u8>) -> Option<Self> {
        let eq = payload.iter().position(|&b| b == b'=')?;
        Some(Self {
            payload: payload.into(),
            eq,
        })
    }

    fn name(&self) -> &[u8] {
        &self.payload[..self.eq]
    }

    fn value(&self) -> &[u8] {
        &self.payload[self.eq + 1..]
    }
}

impl Record {
    /// Where the entry is: a page before it starts from here.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// The entry's wall-clock time (`__REALTIME_TIMESTAMP`); `None` when
    /// it is out of jiff's range, as a corrupt or foreign file's can be.
    #[must_use]
    pub fn realtime(&self) -> Option<jiff::Timestamp> {
        let micros = i64::try_from(self.cursor.realtime).ok()?;
        jiff::Timestamp::from_microsecond(micros).ok()
    }

    /// The boot the entry was written in.
    #[must_use]
    pub fn boot_id(&self) -> BootId {
        BootId(self.cursor.boot_id)
    }

    /// The first `MESSAGE` value.
    #[must_use]
    pub fn message(&self) -> Option<&[u8]> {
        self.value_of(b"MESSAGE")
    }

    /// The first value of the field `name`.
    #[must_use]
    pub fn field(&self, name: &FieldName) -> Option<&[u8]> {
        self.value_of(name.as_bytes())
    }

    /// Every field, as raw name and value bytes in item order, a repeated
    /// field once per value. Names aren't checked against journald's rule
    /// (Difference 14): a corrupt or foreign file's are handed out as they
    /// are, for the viewer to sanitize.
    pub fn fields(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        self.fields.iter().map(|f| (f.name(), f.value()))
    }

    /// The `PRIORITY` field as journalctl reads it; `None` without one, or
    /// for a value other than one digit from 0 to 7.
    #[must_use]
    pub fn priority(&self) -> Option<Priority> {
        self.value_of(b"PRIORITY").and_then(Priority::parse)
    }

    fn value_of(&self, name: &[u8]) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|f| f.name() == name)
            .map(Field::value)
    }
}

// What a caller moves into `spawn_blocking` or shares across threads
// (journal spec, assumption 3).
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    const fn send_static<T: Send + 'static>() {}
    send_sync::<Journal>();
    send_static::<Page>();
    send_static::<Record>();
    send_static::<Query>();
    send_static::<Position>();
    send_static::<Limits>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_output_limit_is_256_mib() {
        assert_eq!(Limits::default().output(), 256 * 1024 * 1024);
    }

    #[test]
    fn the_output_limit_is_capped_at_data_size_max() {
        assert_eq!(Limits::with_output(4096).output(), 4096);
        assert_eq!(
            Limits::with_output(file::DATA_SIZE_MAX).output(),
            768 * 1024 * 1024
        );
        assert_eq!(Limits::with_output(usize::MAX).output(), 768 * 1024 * 1024);
    }

    fn error(path: &str, kind: ReadErrorKind) -> ReadError {
        ReadError {
            path: Some(PathBuf::from(path)),
            kind,
        }
    }

    /// A file name carrying escape sequences and other controls is
    /// written escaped: no control byte of it reaches a terminal (L5).
    #[test]
    fn display_escapes_control_bytes_in_the_path() {
        let e = error(
            "/run/log/journal/x\u{1b}]0;title\u{7}\u{9b}31m\n.journal",
            ReadErrorKind::Corrupt("bad header size".into()),
        );
        let shown = e.to_string();
        assert_eq!(
            shown,
            "/run/log/journal/x\\u{1b}]0;title\\u{7}\\u{9b}31m\\n.journal: bad header size"
        );
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert_eq!(
            e.path(),
            Some(Path::new(
                "/run/log/journal/x\u{1b}]0;title\u{7}\u{9b}31m\n.journal"
            ))
        );
    }

    #[test]
    fn display_names_the_file_then_the_kind() {
        let over = error(
            "/var/log/journal/m/system.journal",
            ReadErrorKind::OverLimit {
                context: "data 0x1f8: zstd".into(),
                over: OverLimit { limit: 5 },
            },
        );
        assert_eq!(
            over.to_string(),
            "/var/log/journal/m/system.journal: data 0x1f8: zstd: payload over its limit of 5 bytes"
        );
        let bare = ReadErrorKind::OverLimit {
            context: String::new(),
            over: OverLimit { limit: 5 },
        };
        assert_eq!(bare.to_string(), "payload over its limit of 5 bytes");
        let known = error("/f", KnownBad { item: 0x2a0 }.into());
        assert_eq!(
            known.to_string(),
            "/f: more entries list data 0x2a0, which can't be read; left out too"
        );
        assert!(matches!(
            known.kind(),
            ReadErrorKind::KnownBad(KnownBad { item: 0x2a0 })
        ));
    }

    /// The system's error is the source, not repeated in the message.
    #[test]
    fn an_io_error_is_the_source() {
        let e = error(
            "/f",
            ReadErrorKind::Io {
                context: "read at 0x110".into(),
                source: std::io::Error::other("gone"),
            },
        );
        assert_eq!(e.to_string(), "/f: read at 0x110");
        let source = std::error::Error::source(&e).expect("a source");
        assert_eq!(source.to_string(), "gone");
        let corrupt = error("/f", ReadErrorKind::Corrupt("x".into()));
        assert!(std::error::Error::source(&corrupt).is_none());
    }

    /// Synthetic ids: a cursor built here, not copied from a journal.
    const CURSOR: &str = "s=00112233445566778899aabbccddeeff;i=3;b=ffeeddccbbaa99887766554433221100;m=1f4;t=5f5e100;x=abc";

    fn parse(text: &str) -> Result<Cursor, QueryError> {
        text.parse()
    }

    #[test]
    fn cursor_round_trips() {
        assert_eq!(parse(CURSOR).expect("parses").to_string(), CURSOR);
    }

    #[test]
    fn cursor_numbers_are_hex_digits_only() {
        let bad = |t: String| assert_eq!(parse(&t), Err(QueryError::BadCursor), "{t}");
        bad(CURSOR.replace("i=3", "i=+3"));
        bad(CURSOR.replace("s=00", "s=+0"));
        bad(CURSOR.replace(";x=abc", ""));
        // A repeated key doesn't make up for the missing one.
        bad(CURSOR.replace(";x=abc", ";i=4"));
    }

    /// journalctl's `__CURSOR` (`-o json`), in its own form: lowercase hex,
    /// numbers without leading zeros. Synthetic values. Each key lands in
    /// its own field, and the text prints back the same.
    #[test]
    fn a_journalctl_cursor_parses() {
        let text = "s=6d3f4a2c1b0e49f8a7d6c5b4a3928170;i=1a2b;b=0f1e2d3c4b5a69788796a5b4c3d2e1f0;m=3b9aca00;t=5f5e1000abcd;x=9f8e7d6c5b4a3921";
        let c = parse(text).expect("parses");
        assert_eq!(
            file::hex_id(&c.seqnum_id),
            "6d3f4a2c1b0e49f8a7d6c5b4a3928170"
        );
        assert_eq!(
            BootId(c.boot_id).to_string(),
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0"
        );
        assert_eq!(
            (c.seqnum, c.monotonic, c.realtime, c.xor_hash),
            (0x1a2b, 0x3b9a_ca00, 0x5f5e_1000_abcd, 0x9f8e_7d6c_5b4a_3921)
        );
        assert_eq!(c.to_string(), text);
    }

    /// Keys in another order, and keys sd-journal doesn't know, still
    /// parse; the text printed is the canonical one. A part without `=`,
    /// an id of the wrong length, a number past 64 bits or the empty text
    /// don't.
    #[test]
    fn cursor_keys_parse_in_any_order() {
        let shuffled = "x=abc;t=5f5e100;z=1;m=1f4;b=ffeeddccbbaa99887766554433221100;i=3;s=00112233445566778899aabbccddeeff";
        assert_eq!(parse(shuffled).expect("parses").to_string(), CURSOR);
        for bad in [
            "",
            &format!("{CURSOR};junk"),
            &CURSOR.replace("s=00", "s=0"),
            &CURSOR.replace("b=ff", "b=ffff"),
            &CURSOR.replace("i=3", "i=10000000000000000"),
            &CURSOR.replace("m=1f4", "m="),
            &CURSOR.replace("t=5f5e100", "t=5g"),
            &CURSOR.replace("x=abc", "x=0xabc"),
        ] {
            assert_eq!(parse(bad), Err(QueryError::BadCursor), "{bad}");
        }
        assert_eq!(QueryError::BadCursor.to_string(), "not a journal cursor");
    }

    fn page_error(kind: ReadErrorKind) -> ReadError {
        ReadError::of_page(kind)
    }

    /// Ten distinct errors are shown, the rest counted; a repeat of one
    /// shown is neither, before the cap or after.
    #[test]
    fn read_errors_show_ten_distinct_then_count() {
        let mut errors = ReadErrors::default();
        assert!(errors.is_empty());
        for i in 0..14 {
            errors.add(page_error(ReadErrorKind::Corrupt(format!("e{i}"))));
            errors.add(page_error(ReadErrorKind::Corrupt("e0".into())));
        }
        let shown: Vec<String> = errors.shown.iter().map(ToString::to_string).collect();
        let want: Vec<String> = (0..10).map(|i| format!("e{i}")).collect();
        assert_eq!((shown, errors.omitted), (want, 4));
        assert!(!errors.is_empty());
        let mut only_omitted = ReadErrors {
            omitted: 1,
            ..ReadErrors::default()
        };
        assert!(!only_omitted.is_empty());
        only_omitted.omitted = 0;
        assert!(only_omitted.is_empty());
    }

    /// Two errors with the same message but different sources are
    /// distinct; the same file and message twice is one.
    #[test]
    fn read_errors_are_distinct_by_their_sources_too() {
        let io = |source: &str| ReadError {
            path: Some(PathBuf::from("/f")),
            kind: ReadErrorKind::Io {
                context: "read at 0x110".into(),
                source: io::Error::other(source.to_owned()),
            },
        };
        let mut errors = ReadErrors::default();
        errors.add(io("gone"));
        errors.add(io("busy"));
        errors.add(io("gone"));
        assert_eq!(errors.shown.len(), 2);
        assert_eq!(key(&errors.shown[1]), "/f: read at 0x110: busy");
    }

    /// Open and listing errors are shown outside the cap: one per file,
    /// however many read errors came first.
    #[test]
    fn open_errors_are_outside_the_cap() {
        let mut errors = ReadErrors::default();
        for i in 0..12 {
            errors.add(page_error(ReadErrorKind::Corrupt(format!("e{i}"))));
        }
        errors.add_open(error("/a", ReadErrorKind::Corrupt("open".into())));
        errors.add_open(error("/b", ReadErrorKind::Corrupt("open".into())));
        errors.add_open(error("/b", ReadErrorKind::Corrupt("open".into())));
        assert_eq!((errors.shown.len(), errors.omitted), (12, 2));
        // Open errors don't use up the cap either.
        let mut first = ReadErrors::default();
        for f in 0..3 {
            first.add_open(error(
                &format!("/{f}"),
                ReadErrorKind::Corrupt("open".into()),
            ));
        }
        for i in 0..10 {
            first.add(page_error(ReadErrorKind::Corrupt(format!("e{i}"))));
        }
        assert_eq!((first.shown.len(), first.omitted), (13, 0));
    }

    /// An error about no one file is its kind alone.
    #[test]
    fn a_page_error_has_no_path() {
        let e = page_error(ReadErrorKind::OutputLimit {
            limit: 5,
            left_out: 2,
        });
        assert_eq!(e.path(), None);
        assert_eq!(
            e.to_string(),
            "output limit of 5 bytes reached; 2 older entries left out"
        );
        assert_eq!(
            page_error(ReadErrorKind::HeldLimit).to_string(),
            "limit of held entries reached; older entries left out"
        );
        let too = error(
            "/f",
            ReadErrorKind::TooLarge {
                context: "entry 4's fields: data 0x1f8: payload over its limit of 9 bytes".into(),
                limit: 9,
            },
        );
        assert_eq!(
            too.to_string(),
            "/f: entry 4's fields: data 0x1f8: payload over its limit of 9 bytes: an entry larger than the output limit of 9 bytes, left out"
        );
    }

    use crate::testing::crafted::{
        self, Crafted, D_ENTRY_OFFSET, D_N_ENTRIES, H_ENTRY_ARRAY_OFFSET, H_N_ENTRIES,
    };

    /// A journal of one entry carrying the payloads `fields`, each its own
    /// DATA object, written to a temporary file.
    fn one_entry(fields: &[&[u8]]) -> (Crafted, u64) {
        let mut c = Crafted::new();
        c.hash_table(7);
        let items: Vec<u64> = fields.iter().map(|f| c.data(f)).collect();
        let e = c.entry(1, &items);
        for &d in &items {
            c.set(d + D_ENTRY_OFFSET, e);
            c.set(d + D_N_ENTRIES, 1);
        }
        let main = c.array(&[e], 1, 0);
        c.set(H_N_ENTRIES, 1);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        (c, e)
    }

    fn files(paths: &[&Path]) -> Journal {
        Journal::open(Source::Files(
            paths.iter().map(|p| p.to_path_buf()).collect(),
        ))
        .expect("opens")
    }

    fn name(n: &str) -> FieldName {
        FieldName::new(n.as_bytes()).expect("a field name")
    }

    fn tail(j: &Journal, q: &Query, n: usize) -> Page {
        j.page(q, &Position::Tail, n, &Limits::default())
    }

    /// Difference 3: a field of 5 KiB comes back whole, as bytes (where
    /// `journalctl -o json` without `--all` prints `null` from 4096).
    #[test]
    fn a_5_kib_field_is_read_back_whole() {
        let mut big = b"BIG=".to_vec();
        big.extend((0..5 * 1024).map(|i| u8::try_from(i % 251).expect("small")));
        let (mut c, _e) = one_entry(&[b"MESSAGE=m", &big]);
        let t = c.file();
        let j = files(&[t.path()]);
        let page = tail(&j, &Query::field(&name("MESSAGE"), b"m"), 1);
        assert!(page.errors.is_empty(), "{:?}", page.errors);
        assert_eq!(page.records[0].field(&name("BIG")), Some(&big[4..]));
        assert_eq!(page.records[0].field(&name("NONE")), None);
    }

    /// Difference 14: a field name journald's rule refuses is handed out
    /// raw, as are values; a repeated field once per value, in item order.
    #[test]
    fn fields_are_raw_bytes_in_item_order() {
        let (mut c, _e) = one_entry(&[
            b"MESSAGE=first",
            b"bad name\x1b[31m=v\xff",
            b"MESSAGE=second",
            b"lower=x=y",
        ]);
        let t = c.file();
        let page = tail(
            &files(&[t.path()]),
            &Query::field(&name("MESSAGE"), b"first"),
            1,
        );
        let r = &page.records[0];
        let got: Vec<(&[u8], &[u8])> = r.fields().collect();
        let want: [(&[u8], &[u8]); 4] = [
            (b"MESSAGE", b"first"),
            (b"bad name\x1b[31m", b"v\xff"),
            (b"MESSAGE", b"second"),
            (b"lower", b"x=y"),
        ];
        assert_eq!(got, want);
        assert_eq!(r.message(), Some(&b"first"[..]));
    }

    /// The other accessors: no `MESSAGE` is `None`; `PRIORITY` as
    /// journalctl reads it; the boot id and cursor from the entry header.
    #[test]
    fn record_accessors() {
        let (mut c, entry) = one_entry(&[b"PRIORITY=3", b"U=x"]);
        let t = c.file();
        let r = tail(&files(&[t.path()]), &Query::field(&name("U"), b"x"), 1)
            .records
            .remove(0);
        assert_eq!(r.message(), None);
        assert_eq!(r.priority(), Some(Priority::Error));
        assert_eq!(r.boot_id().to_string(), "07".repeat(16));
        assert_eq!(r.boot_id().as_bytes(), &[7; 16]);
        assert_eq!(r.cursor().seqnum, 1);
        assert_eq!(r.cursor().realtime, c.get(entry + 24));
        for (value, want) in [
            (&b"0"[..], Some(Priority::Emergency)),
            (b"1", Some(Priority::Alert)),
            (b"2", Some(Priority::Critical)),
            (b"3", Some(Priority::Error)),
            (b"4", Some(Priority::Warning)),
            (b"5", Some(Priority::Notice)),
            (b"6", Some(Priority::Info)),
            (b"7", Some(Priority::Debug)),
            (b"8", None),
            (b"03", None),
            (b"", None),
        ] {
            assert_eq!(Priority::parse(value), want, "{value:?}");
        }
    }

    /// A record's time, and `None` past what jiff can hold.
    #[test]
    fn realtime_is_none_out_of_jiffs_range() {
        let record = |realtime: u64| Record {
            cursor: Cursor {
                realtime,
                ..parse(CURSOR).expect("parses")
            },
            fields: Vec::new(),
        };
        assert_eq!(
            record(1_000_001).realtime(),
            Some(jiff::Timestamp::from_microsecond(1_000_001).expect("in range"))
        );
        // 9999-12-30T00:00:00Z, in jiff's range; 10000-01-01T00:00:00Z, past
        // it (and still an i64).
        let late = record(253_402_128_000_000_000).realtime();
        assert_eq!(late.map(jiff::Timestamp::as_second), Some(253_402_128_000));
        assert_eq!(record(253_402_300_800_000_000).realtime(), None);
        assert_eq!(record(u64::MAX).realtime(), None);
    }

    /// `Directory` reads the directory's files and its machine-id
    /// subdirectories'; a directory that can't be read is an `OpenError`,
    /// shown escaped.
    #[test]
    fn a_directory_source_is_checked_and_read() {
        let dir = tempfile::tempdir().expect("dir");
        let sub = dir.path().join("0123456789abcdef0123456789abcdef");
        std::fs::create_dir(&sub).expect("subdir");
        let (mut c, _l) = crafted::journal(2);
        std::fs::copy(c.file().path(), sub.join("system.journal")).expect("write");
        let j = Journal::open(Source::Directory(dir.path().to_path_buf())).expect("opens");
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 2);

        let missing = dir.path().join("gone\u{1b}[2J");
        let err = Journal::open(Source::Directory(missing.clone())).expect_err("missing");
        let OpenError::Directory { path, source } = &err else {
            panic!("{err:?}");
        };
        assert_eq!((path, source.kind()), (&missing, io::ErrorKind::NotFound));
        assert!(err.to_string().ends_with("gone\\u{1b}[2J"), "{err}");
        assert!(err.to_string().starts_with("read the journal directory "));
    }

    /// The local journal's directories are the roots joined with the
    /// machine id, in search order; an unreadable machine id is an
    /// `OpenError`.
    #[test]
    fn the_local_journal_is_found_by_machine_id() {
        let dir = tempfile::tempdir().expect("dir");
        let id = dir.path().join("machine-id");
        std::fs::write(&id, "0123456789abcdef0123456789abcdef\n").expect("write");
        let (run, var) = (dir.path().join("run"), dir.path().join("var"));
        let dirs = local_dirs(&id, [&run, &var]).expect("reads");
        let mid = "0123456789abcdef0123456789abcdef";
        assert_eq!(dirs, [run.join(mid), var.join(mid)]);

        // Both roots are read, either may be missing.
        std::fs::create_dir_all(&dirs[1]).expect("var");
        let (mut c, _l) = crafted::journal(3);
        std::fs::copy(c.file().path(), dirs[1].join("system.journal")).expect("write");
        let j = Journal {
            roots: Roots::Local(dirs),
        };
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 3);
        assert!(page.errors.is_empty(), "{:?}", page.errors);

        let gone = dir.path().join("nothing");
        let err = local_dirs(&gone, [&run, &var]).expect_err("missing");
        assert!(matches!(&err, OpenError::MachineId { path, .. } if *path == gone));
        assert_eq!(err.to_string(), format!("read {}", gone.display()));
    }

    /// Every page lists and opens the files again: a file replaced between
    /// two pages is read anew by the second.
    #[test]
    fn each_page_reads_the_files_again() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("system.journal");
        let (mut c, _l) = crafted::journal(2);
        std::fs::copy(c.file().path(), &path).expect("write");
        let j = files(&[&path]);
        let q = Query::field(&name("U"), b"x");
        assert_eq!(tail(&j, &q, 10).records.len(), 2);
        let (mut c, _l) = crafted::journal(5);
        std::fs::copy(c.file().path(), dir.path().join("new")).expect("write");
        std::fs::rename(dir.path().join("new"), &path).expect("replace");
        assert_eq!(tail(&j, &q, 10).records.len(), 5);
        std::fs::remove_file(&path).expect("remove");
        let page = tail(&j, &q, 10);
        assert!(page.records.is_empty());
        assert_eq!(page.errors.shown.len(), 1);
    }

    /// A page's errors name the file escaped (L5), whatever the file's
    /// name holds.
    #[test]
    fn a_pages_errors_are_escaped() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("x\u{1b}]52;c;aGk=\u{7}.journal");
        std::fs::write(&path, b"not a journal").expect("write");
        let page = tail(&files(&[&path]), &Query::field(&name("U"), b"x"), 10);
        let shown = page.errors.shown[0].to_string();
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(
            shown.contains("x\\u{1b}]52;c;aGk=\\u{7}.journal: "),
            "{shown}"
        );
    }

    /// No entries asked for, none read.
    #[test]
    fn a_page_of_none_is_empty() {
        let (mut c, _l) = crafted::journal(2);
        let t = c.file();
        let page = tail(&files(&[t.path()]), &Query::field(&name("U"), b"x"), 0);
        assert!(page.records.is_empty() && page.errors.is_empty());
    }

    /// The output limit a caller sets is the page's: an entry over it is
    /// left out and reported (Difference 5).
    #[test]
    fn a_pages_output_limit_is_the_callers() {
        let (mut c, _e) = one_entry(&[b"U=x", &[b'B'; 2000]]);
        let t = c.file();
        let j = files(&[t.path()]);
        let q = Query::field(&name("U"), b"x");
        let page = j.page(&q, &Position::Tail, 10, &Limits::with_output(1000));
        assert!(page.records.is_empty());
        assert!(matches!(
            page.errors.shown[0].kind(),
            ReadErrorKind::TooLarge { limit: 1000, .. }
        ));
        assert_eq!(tail(&j, &q, 10).records.len(), 1);
    }
}
