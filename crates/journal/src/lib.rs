//! sb's own read-only reader of systemd's journal files: pread only, pure
//! Rust, exact against each supported release's journalctl except the
//! differences the journal spec lists (docs/specs/journal.md). Why sb owns
//! the reader rather than using a crate or a `journalctl` child:
//! docs/decisions/ADR-013-own-journal-reader.md.
//!
//! A [`Journal`] names a [`Source`]; [`Journal::page`] reads the newest
//! entries matching a [`Query`], or the page before or after a [`Cursor`],
//! as [`Record`]s holding raw bytes; [`Journal::follow`] goes on from a
//! page's [`PageEnd`], giving a [`Follower`] and what was written since as
//! its first [`Batch`]. [`Follower::drain`] then reads what came next,
//! without blocking, whenever [`Follower::wake_fd`] is readable.

use std::collections::HashSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// One declaration each, not a `pub` and a private one under opposite
// `cfg`s: cargo-mutants walks every `mod` without evaluating `cfg`, so two
// declarations made it test each of file.rs's mutants twice.
mod boots;
mod file;
mod follow;
mod forward;
mod hash;
mod open;
mod rules;
#[cfg(feature = "testing")]
pub mod testing;
mod walk;

use boots::BootOrder;
use file::{Entry, JournalFile};

// A new public item here or below is also named in sb-host's re-export
// (crates/host/src/journal.rs), sb's only way to reach this crate.
pub use follow::Follower;

pub use rules::{
    BootRule, FieldName, QUERY_GROUPS_MAX, Query, QueryError, RELEASES, Release, ReleaseError, Uid,
    UnitName, UnitRule,
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
/// reports the held limit. An entry listing more items than the limit
/// could show (each field shown spends some of it) is refused before its
/// body is read, so one entry's size can't cost more memory than the
/// limit allows (Difference 5).
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
    /// `OutputLimit` for a forward page (`Position::After`), which reads
    /// its entries oldest first: the newer entries are left out, with no gap
    /// after the cursor.
    #[error("output limit of {limit} bytes reached; {left_out} newer entries left out")]
    OutputLimitNewer {
        /// The output limit.
        limit: usize,
        /// The entries chosen but not read.
        left_out: usize,
    },
    /// `HeldLimit` for a forward page: it held fewer entries than asked
    /// for, the oldest after the cursor, and the newer ones are left out.
    #[error("limit of held entries reached; newer entries left out")]
    HeldLimitNewer,
    /// A journal file that is a symlink, which isn't followed (security
    /// audit L2; sd-journal follows it: Difference 4). Its directory may be
    /// one: directories are followed.
    #[error("a symlink, not followed (only directories are)")]
    Symlink,
    /// A journal file's name on something other than a regular file (a
    /// FIFO, a device), refused so no read can block.
    #[error("not a regular file")]
    NotRegular,
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
/// `Display` writes the path escaped (`escaped`), so control
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

    /// What went wrong, for the details a kind carries (an `Io` error's
    /// context, an `OverLimit`'s limit). To branch on the kind, use the
    /// `is_*` helpers: `ReadErrorKind` is `#[non_exhaustive]` and may gain
    /// kinds a caller can't handle.
    #[must_use]
    pub fn kind(&self) -> &ReadErrorKind {
        &self.kind
    }

    // The `is_*` helpers follow M-ERRORS-CANONICAL-STRUCTS
    // (https://microsoft.github.io/rust-guidelines/guidelines/libs/ux/#M-ERRORS-CANONICAL-STRUCTS):
    // callers branch on what they can handle without matching every kind
    // (maintainer, 2026-11-11, journal Checkpoint A). `kind()` stays public
    // for the fields.

    /// A payload over the read's limit (`ReadErrorKind::OverLimit`): the
    /// object may be fine, only larger than this read allows. Not the
    /// limits that end a page (`OutputLimit`, `HeldLimit` and their
    /// `Newer` forms) nor `TooLarge`.
    #[must_use]
    pub fn is_over_limit(&self) -> bool {
        matches!(self.kind, ReadErrorKind::OverLimit { .. })
    }

    /// An entry listing a DATA object this query already failed to read
    /// (`ReadErrorKind::KnownBad`): definitive, not one to wait for.
    #[must_use]
    pub fn is_known_bad(&self) -> bool {
        matches!(self.kind, ReadErrorKind::KnownBad(_))
    }

    /// The system refused a read (`ReadErrorKind::Io`); the system's error
    /// is the `source()`.
    #[must_use]
    pub fn is_io(&self) -> bool {
        matches!(self.kind, ReadErrorKind::Io { .. })
    }

    /// An entry larger than the whole output limit, left out
    /// (`ReadErrorKind::TooLarge`, Difference 5).
    #[must_use]
    pub fn is_too_large(&self) -> bool {
        matches!(self.kind, ReadErrorKind::TooLarge { .. })
    }

    /// A journal file that is a symlink, not followed
    /// (`ReadErrorKind::Symlink`, Difference 4).
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, ReadErrorKind::Symlink)
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            Some(path) => write!(f, "{}: {}", escaped(path), self.kind),
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
    keys: HashSet<String>,
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
            self.keys.insert(key);
            self.shown.push(e);
        } else {
            self.omitted += 1;
        }
    }

    /// `other`'s read errors, as if each had been added here.
    pub(crate) fn absorb(&mut self, other: ReadErrors) {
        for e in other.shown {
            self.add(e);
        }
        self.omitted += other.omitted;
    }

    /// An error opening or listing a file: shown, outside the cap.
    fn add_open(&mut self, e: ReadError) {
        // A set, so many files failing to open (one each) cost a lookup
        // apiece, not a scan of every error before them.
        if self.keys.insert(key(&e)) {
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
    /// The entries after this one, which isn't shown.
    After(Cursor),
}

/// A page of entries, oldest first, and what went wrong reading it.
/// Built by `Journal::page` only: more fields can come.
///
/// `more_older` and `more_newer` are exact for the files as the page read
/// them: whether a *matching* entry lies beyond the entries it chose and
/// reached (`oldest_chosen` to `newest_chosen`), older or newer, even when
/// the next entry in the file doesn't match. One the output limit left out
/// counts, with its error in `errors`: a page from `oldest_chosen` or
/// `newest_chosen` meets it again. One the page reached but couldn't show
/// (unreadable, or alone over the limit) is reported in `errors` and
/// doesn't count: a page from `oldest_chosen` or `newest_chosen` steps
/// past it, so a page of entries that can't be shown can still be paged
/// past, and a tail whose newest entry can't be shown isn't `more_newer`. With nothing chosen, they say
/// whether a matching entry lies older or newer than the position (for
/// `Tail`, older is whether anything matched at all). Entries written after
/// the page are not known to it: a follow from `end` reads them.
#[derive(Debug)]
#[non_exhaustive]
pub struct Page {
    /// The entries, oldest first.
    pub records: Vec<Record>,
    /// A matching entry is older than `oldest_chosen` (see above).
    pub more_older: bool,
    /// A matching entry is newer than `newest_chosen` (see above).
    pub more_newer: bool,
    /// The oldest entry the page chose and reached, shown or not.
    ///
    /// An unreadable one, or one alone over the limit, is reported in
    /// `errors`; `None` when the page chose none. `Position::Before` from
    /// it is the page before this one, past the entries it couldn't show.
    pub oldest_chosen: Option<Cursor>,
    /// The newest entry the page chose and reached, shown or not.
    ///
    /// `None` when the page chose none. `Position::After` from it is the
    /// page after this one, past the entries it couldn't show.
    pub newest_chosen: Option<Cursor>,
    /// Where the page ended, for a follow to start from (journal spec,
    /// "Starting a follow"); it exists even when the page matched nothing.
    pub end: PageEnd,
    /// What couldn't be read, and what the limits left out.
    pub errors: ReadErrors,
}

/// Where a page ended: each file it read, by device and inode (and the
/// header's file id), with the entry count it read up to, and the newest
/// entry in those files, matching or not, with that entry's directory.
/// Opaque: a follow starts from it. Two ends are equal when the page read
/// the same files at the same counts and the same newest entry, and showed
/// the same entries past it.
///
/// Those entries, which a filtered page showed past the newest entry its
/// first header reads gave (written during the page), are kept by what a
/// flush copy keeps of them: boot id, monotonic and realtime stamps and XOR
/// hash. The follow drops one copy of each from a file the page didn't
/// read, so a flush to a new /var file between the page and the follow
/// doesn't show them again (journal spec decision 13).
///
/// The count is the header's when the page was unfiltered, or the file is
/// sealed; a filtered page reads the header again after its lookups and
/// holds that count, having examined every entry below it (journal spec
/// decision 13). So a page plus the follow from its end gives every entry
/// once.
///
/// It is the journal's end, not the page's: a follow from it starts after
/// the newest entries the files held, wherever the page was. A follow from
/// a `Before` page, or from a page its limits stopped, skips the entries
/// between the page's last record and the end.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageEnd {
    files: Vec<FileEnd>,
    newest: Option<(Cursor, Option<PathBuf>)>,
    /// The entries the page showed past `newest`, oldest first.
    shown: Vec<Copied>,
}

/// What a flush copy keeps of an entry (journal-file.c
/// `journal_file_copy_entry`: the boot id and both times, the XOR hash
/// computed again from the same payloads; docs/platform-notes/
/// systemd-journal.md, "Flush"), and the directory of the file the page
/// showed it from: how the follow knows a copy of an entry the page
/// showed. The seqnum isn't kept: 255 numbers the copies anew. A copy is
/// in another directory (a flush goes from /run to /var); an entry with the
/// same identity in the same directory is 249's rotation repeat, which
/// journalctl shows too.
///
/// The identity matches the copy unless the original held a duplicate
/// `FIELD=value` (the copy drops duplicate items on 255 and later and on
/// 22.04's 249.11, so its XOR hash differs) or a DATA object the copy
/// skips as bad. Such a copy isn't matched, and is shown again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Copied {
    boot_id: [u8; 16],
    monotonic: u64,
    realtime: u64,
    xor_hash: u64,
    dir: Option<PathBuf>,
}

impl Copied {
    pub(crate) fn of(c: &Cursor, dir: Option<&Path>) -> Self {
        Self {
            boot_id: c.boot_id,
            monotonic: c.monotonic,
            realtime: c.realtime,
            xor_hash: c.xor_hash,
            dir: dir.map(Path::to_path_buf),
        }
    }

    /// Whether `l`, an entry of a file in `dir`, is a copy of this one.
    pub(crate) fn copied_as(&self, l: &Cursor, dir: Option<&Path>) -> bool {
        self.boot_id == l.boot_id
            && self.monotonic == l.monotonic
            && self.realtime == l.realtime
            && self.xor_hash == l.xor_hash
            && self.dir.as_deref() != dir
    }
}

/// One file of a `PageEnd`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileEnd {
    inode: (u64, u64),
    /// The header's file id: with the inode, what tells the file from a
    /// new one journald made at the same inode once this one was freed.
    file_id: [u8; 16],
    n_entries: u64,
}

/// A journal that can't be read at all. Files that fail to open on a later
/// page are errors of that page, not an `OpenError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The machine id couldn't be read (`Source::Local`).
    #[error("read {}", escaped(.path))]
    MachineId {
        /// The machine-id file.
        path: PathBuf,
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// The machine id isn't one: not 32 hex digits, or all zeros (security
    /// audit L1). A journal isn't looked for under it.
    #[error("{}not a machine id (32 hex digits, not all zero)", file_prefix(.path.as_deref()))]
    InvalidMachineId {
        /// The machine-id file it was read from; `None` from
        /// `MachineId::parse`.
        path: Option<PathBuf>,
    },
    /// The directory couldn't be read (`Source::Directory`).
    #[error("read the journal directory {}", escaped(.path))]
    Directory {
        /// The directory.
        path: PathBuf,
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// A follow couldn't get its inotify instance (`Journal::follow`).
    #[error("create the follow's inotify instance")]
    Inotify {
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// A follow couldn't watch one of the journal's directories
    /// (`Journal::follow`): it would wait on it for ever.
    #[error("watch the journal directory {}", escaped(.path))]
    Watch {
        /// The directory.
        path: PathBuf,
        /// The system's error.
        #[source]
        source: io::Error,
    },
    /// A follow found no directory to watch (`Journal::follow`): neither
    /// local journal root exists, or no file given has a directory. No
    /// event could ever wake it, so its wake descriptor would never become
    /// readable.
    #[error("no journal directory to watch: a follow would never wake")]
    NothingToWatch,
}

/// A machine's id, as `/etc/machine-id` holds it: 32 hex digits, not all
/// zero. sd-journal looks for the local journal under it (security audit
/// L1: an id read from the file is checked before it is joined into a
/// path). `Display` writes the digits in lower case, the name journald gives
/// the journal's directory.
///
/// systemd v255 reads the file with `id128_read_fd`
/// (src/libsystemd/sd-id128/id128-util.c, `ID128_FORMAT_PLAIN |
/// ID128_REFUSE_NULL` from `sd_id128_get_machine` in sd-id128.c): 32
/// characters, optionally one newline, parsed by `sd_id128_from_string`,
/// whose `unhexchar` (src/basic/hexdecoct.c) takes `a-f` and `A-F`; a null
/// id is `-ENOMEDIUM`. journald names the directory `SERVER_MACHINE_ID`
/// (src/journal/journald-server.h), the id `server_cache_machine_id`
/// (journald-server.c) wrote with `sd_id128_to_string`, whose `hexchar`
/// writes lower case. machine-id(5) asks for lower case, but systemd reads
/// either, and so does sb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MachineId([u8; 16]);

impl MachineId {
    /// The id in `text`: exactly 32 hex digits in either case, not all zero
    /// (systemd refuses a null machine id). No newline: the file's one is
    /// taken off by the reader. A GUID's dashes, which
    /// `sd_id128_from_string` also takes, are refused, as `id128_read_fd`
    /// refuses them in a plain-format file (36 characters).
    ///
    /// # Errors
    ///
    /// `OpenError::InvalidMachineId`, with no path.
    pub fn parse(text: &str) -> Result<Self, OpenError> {
        Self::from_hex(text.as_bytes())
            .map(Self)
            .ok_or(OpenError::InvalidMachineId { path: None })
    }

    /// The id `digits` spell: exactly 32 hex digits in either case, not
    /// all zero, as `parse` and as sd-journal's `id128_from_string_nonzero`
    /// read them (a tail entry's `_MACHINE_ID=` value, file.rs).
    fn from_hex(digits: &[u8]) -> Option<[u8; 16]> {
        let mut id = [0u8; 16];
        // <https://docs.rs/hex/0.4.3/hex/fn.decode_to_slice.html>: exactly
        // 32 digits, of either case as `unhexchar` takes them, fill the 16
        // bytes.
        (hex::decode_to_slice(digits, &mut id).is_ok() && id != [0; 16]).then_some(id)
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&file::hex_id(&self.0))
    }
}

/// `path` on one line, escaped character by character as
/// `char::escape_debug` does
/// (<https://doc.rust-lang.org/std/primitive.char.html#method.escape_debug>),
/// so control bytes in a file name can't reach a terminal (security audit
/// L5), except that a combining mark stays with its base character: one
/// right after a character shown as itself goes out as itself, as
/// sb-cli-core's escaper keeps it (plan decision 40). A mark at the start
/// or after an escaped character is escaped, and so is an invisible one
/// (a variation selector, a joiner). sb-journal depends on no sb crate, so
/// it holds its own copy of the rule.
///
/// Why not std's `str::escape_debug`: Rust 1.100 escapes every grapheme
/// extender (<https://github.com/rust-lang/rust/pull/158303>), which
/// would show an NFD name as `Ame\u{301}lie`. Unicode's `Grapheme_Extend`
/// and `Default_Ignorable_Code_Point` come from ICU4X, as std's own test is
/// private; its data can know marks newer than std's tables
/// (<https://docs.rs/icu_properties/2.4.0/icu_properties/props/struct.GraphemeExtend.html>).
fn escaped(path: &Path) -> String {
    use icu_properties::CodePointSetData;
    use icu_properties::props::{DefaultIgnorableCodePoint, GraphemeExtend};
    let path = path.to_string_lossy();
    let mut out = String::with_capacity(path.len());
    // Whether the last character went out as itself, a base a mark keeps.
    let mut base = false;
    for c in path.chars() {
        if base
            && CodePointSetData::new::<GraphemeExtend>().contains(c)
            && !CodePointSetData::new::<DefaultIgnorableCodePoint>().contains(c)
        {
            out.push(c);
        } else {
            let shown = c.escape_debug();
            base = shown.len() == 1;
            out.extend(shown);
        }
    }
    out
}

/// `PATH: `, escaped, or nothing without a path.
fn file_prefix(path: Option<&Path>) -> String {
    path.map_or_default(|p| format!("{}: ", escaped(p)))
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
    opener: open::Opener,
    /// The release whose journalctl the pages match: its order between
    /// boots (`Release::boot_rule`).
    release: Release,
}

impl Journal {
    /// Checks the source: the machine id for `Source::Local`, the directory
    /// for `Source::Directory`. A machine id that isn't 32 hex digits, or is
    /// all zeros, is refused (`OpenError::InvalidMachineId`): no journal is
    /// looked for under it.
    ///
    /// The pages match `release`'s journalctl where releases differ in
    /// reading, not matching: its order between entries of different boots
    /// (`Release::boot_rule`, journal spec decision 11). For every source,
    /// `Directory` and `Files` too, it is the release the caller passes:
    /// sb-host's, the host's own. Pass the same release to `Query::unit`:
    /// nothing checks that the two agree.
    ///
    /// # Errors
    ///
    /// `OpenError` for a source that can't be read at all.
    pub fn open(source: Source, release: &Release) -> Result<Self, OpenError> {
        Self::open_on(source, release, &open::Host::default())
    }

    /// `open`, with `Source::Local`'s machine-id file and roots from `host`
    /// (`testing::open_on` injects them).
    fn open_on(source: Source, release: &Release, host: &open::Host) -> Result<Self, OpenError> {
        let roots = match source {
            Source::Local => Roots::Local(local_dirs(host)?),
            Source::Directory(path) => match std::fs::read_dir(&path) {
                Ok(_) => Roots::Directory(path),
                Err(source) => return Err(OpenError::Directory { path, source }),
            },
            Source::Files(paths) => Roots::Files(paths),
        };
        Ok(Self {
            roots,
            opener: open::Opener::default(),
            release: *release,
        })
    }

    /// The `n` matching entries at `at`, oldest first: the newest ones
    /// (`Position::Tail`), as `journalctl -n` finds them, or those just
    /// before or just after a cursor.
    ///
    /// Every call lists the files, opens them (safely: a symlinked journal
    /// file or one that isn't a regular file is an error of the page, one
    /// per file) and lists them again, and closes them before it returns.
    /// So a rotation or vacuum between pages is seen by the next one, and
    /// an `Io` error in `errors` from a file cut short while the page read
    /// it is gone from the next page, which reads the file's header again.
    ///
    /// On a journal being written, a filtered page reads each unsealed
    /// file's header a second time after its lookups and also shows the
    /// matching entries that read counts which their lists hold by then,
    /// so its `end` covers every entry it could have shown and the follow
    /// from it repeats and loses none (journal spec decision 13). A newest
    /// entry not linked yet is left to the follow. The page stops at that
    /// count: entries written after it come with the follow.
    ///
    /// Entries of different boots that no sequence number orders are
    /// ordered as the release's journalctl orders them (decision 11): on
    /// 22.04 by realtime; on 24.04 and 26.04 first by each boot's newest
    /// file's tail realtime (sd-journal's `compare_boot_ids`), a table built
    /// from every file the page opened.
    ///
    /// The cost: `Tail` reads the newest entries; `Before` also reads every
    /// entry newer than the cursor that the query's rarest term lists
    /// (journal spec, success criterion 5). `After` reads forward from where
    /// journalctl 259's seek lands (decision 10): a bisect per value of the
    /// query, O(log n) preads, at any depth. It reads its entries oldest
    /// first, so its limits leave out the newer end (`OutputLimitNewer`,
    /// `HeldLimitNewer`), and the next `After` from the page's
    /// `newest_chosen` goes on from there. On 24.04 and 26.04 a file of more than one boot,
    /// or of one the boot table orders against the cursor's, is sought per
    /// boot, so a forward page is the entries after the cursor in the
    /// release's order, each once, even where two boots' clocks overlap;
    /// journalctl's seek, by one key per file, can pass an entry or show
    /// the cursor's own one there (decision 11, Difference 20). The order
    /// isn't transitive, though: where it cycles (a boot in no file's tail
    /// whose realtime falls between two boots the table orders), paging
    /// forward one page after another cycles too, and a caller paging on
    /// should stop at a cursor it has shown. The entry after a vanished or
    /// non-matching cursor isn't dropped (Difference 18). Every
    /// page also reads each file's newest entry, for its `end`; on 24.04
    /// and 26.04 the boot table reuses it, and reads one more entry only
    /// for a file whose header names another tail (journald before 254, a
    /// corrupt file, or one written to between the page's reads). On 26.04,
    /// with two boots or more in the table, it also reads each boot's
    /// newest file's tail entry's items, one pread each, up to its
    /// `_MACHINE_ID=` (all of them without one). A filtered page's second
    /// header read costs, per unsealed file, the header, one pread per array
    /// of the main list up to the newest counted entry (about log2 of the
    /// count), that entry's item and the entry, then each value looked up
    /// again and, for an entry that carries it, its list bisected (O(log n)
    /// preads): 18 on `crafted::journal(200)`, more when entries were
    /// written during the page.
    #[must_use]
    pub fn page(&self, q: &Query, at: &Position, n: usize, limits: &Limits) -> Page {
        let mut errors = ReadErrors::default();
        let files = walk::open_listed(|| self.list(), &self.opener, limits.output(), &mut errors);
        let bounds = walk::Bounds {
            payload: limits.output(),
            held: limits.output(),
        };
        let lasts = walk::lasts(&files, &mut errors);
        let boots = boot_order(self.release.boot_rule(), &files, &lasts);
        let paged = walk::page(&files, q.groups(), n, at, bounds, &boots, &mut errors);
        // The counts a filtered page read again after its lookups (journal
        // spec decision 13); the newest entry from the first header reads.
        let mut end = walk::end(&files, &lasts, &paged.counts, &boots);
        // The records shown past the end's newest entry: written during the
        // page, so a flush may copy them to a file the follow finds new.
        end.shown = paged
            .records
            .iter()
            .zip(&paged.files)
            .map(|(r, &file)| (r.cursor(), files[file].path.parent()))
            .filter(|(c, _)| {
                end.newest
                    .as_ref()
                    .is_none_or(|(n, _)| c.order(n, &boots).is_gt())
            })
            .map(|(c, dir)| Copied::of(&c, dir))
            .collect();
        Page {
            records: paged.records,
            more_older: paged.more_older,
            more_newer: paged.more_newer,
            oldest_chosen: paged.oldest_chosen,
            newest_chosen: paged.newest_chosen,
            end,
            errors,
        }
    }

    fn list(&self) -> walk::Listing {
        self.roots.list()
    }

    /// Follows the entries matching `q` from a page's end, `from`.
    ///
    /// What was written since the page comes as the first [`Batch`]
    /// (journal spec, "Starting a follow").
    ///
    /// The watches go on first, so nothing written once the files are
    /// open waits for a later event. Then the files are opened, and each
    /// is read from the entry count `from` recorded for it; a file the
    /// page didn't read (new since) is walked back from its newest entry
    /// to the newest one at or below `from`'s newest entry, by sd-journal's
    /// order within that entry's directory and by the flush-copy rule
    /// across directories. What that reads is merged as a drain merges
    /// it, so an entry written to /run after the page and flushed to /var
    /// before the follow shows once. Nothing written between the page and
    /// the follow is lost or shown twice, even after a page that matched
    /// nothing. A follow from `PageEnd::default()` reads every entry.
    ///
    /// The first batch holds at most `limits`' output limit per file, as a
    /// drain does; `Batch::more` says the limit stopped it, and the next
    /// [`Follower::drain`] goes on from there. Each drain then gives what
    /// was written since; [`Follower::wake_fd`] is what to wait on between
    /// drains.
    ///
    /// The follow keeps its files open, by inode, so a rotation's rename
    /// or a vacuum doesn't lose one, and orders entries of different boots
    /// as the page does (decision 11), its boot table read again from each
    /// file whose header changed.
    ///
    /// # Errors
    ///
    /// `OpenError::Inotify` or `OpenError::Watch` when the follow can't
    /// watch the journal's directories, and `OpenError::NothingToWatch`
    /// when none of them exists. Files that can't be opened are errors of
    /// the first batch.
    pub fn follow(
        &self,
        q: Query,
        from: &PageEnd,
        limits: &Limits,
    ) -> Result<(Follower, Batch), OpenError> {
        Follower::start(self, q, from, *limits)
    }
}

impl Roots {
    /// The journal files now, in sd-journal's search order.
    fn list(&self) -> walk::Listing {
        let mut out = walk::Listing::default();
        match self {
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

/// The order between boots `rule` gives for `files`, each with its newest
/// readable entry in `lasts` (`walk::lasts`): none on 22.04; on 24.04 and
/// 26.04 the table of the files' tails (`JournalFile::tail`), on 26.04
/// with each boot's newest file's tail-entry machine id when two boots or
/// more are in it.
fn boot_order(rule: BootRule, files: &[JournalFile], lasts: &[Option<(u64, Entry)>]) -> BootOrder {
    let table = || {
        BootOrder::of(
            files
                .iter()
                .zip(lasts)
                .map(|(f, last)| f.tail(last.as_ref())),
        )
    };
    match rule {
        BootRule::Realtime => BootOrder::none(),
        BootRule::NewestTail => table(),
        BootRule::NewestTailEntryMachine => {
            let mut boots = table();
            boots.name_machines(|i| files[i].tail_machine_id(lasts[i].as_ref()));
            boots
        }
    }
}

/// The local journal's directories: each of `host`'s roots joined with the
/// machine id read from its machine-id file, in lower case as journald
/// names the directories. The file holds the id and at most one newline,
/// as systemd reads it (`MachineId`); anything else is refused, and the
/// path is built from the parsed id, not the file's text (security audit
/// L1).
fn local_dirs(host: &open::Host) -> Result<[PathBuf; 2], OpenError> {
    let path = &host.machine_id;
    let bytes = open::read_machine_id(path).map_err(|source| OpenError::MachineId {
        path: path.clone(),
        source,
    })?;
    let text = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let id = std::str::from_utf8(text)
        .ok()
        .and_then(|t| MachineId::parse(t).ok())
        .ok_or_else(|| OpenError::InvalidMachineId {
            path: Some(path.clone()),
        })?;
    let id = id.to_string();
    Ok(host.roots.clone().map(|r| r.join(&id)))
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
    /// sd-journal's order between two entries, the same between two heads
    /// (`compare_locations`) and between a head and a location
    /// (`compare_with_location`): the same sequence-number space orders by
    /// seqnum; the same boot by monotonic time; different boots by `boots`;
    /// otherwise realtime; the XOR hash breaks ties.
    ///
    /// `boots` is the release's (decision 11): systemd 249 has no boot step
    /// (journal-file.c v249, `journal_file_compare_locations`),
    /// `BootOrder::none`; 255 and 259 put `compare_boot_ids` between the
    /// monotonic and the realtime step (sd-journal.c v255 and v259
    /// `compare_locations` and `compare_with_location`), which orders boots
    /// of one machine by their newest file's tail realtime and leaves
    /// others equal (`BootOrder`). The order isn't transitive across
    /// seqnum ids and boots, and the boot step adds cycles of its own: a
    /// boot missing from the table compares equal to every other, so
    /// realtime orders its pairs while the table orders the rest. It is
    /// never handed to a sort: the merges keep journalctl's arg-min and
    /// arg-max in file order (`real_journal_next`), so the result is
    /// decided by the files' order and the comparisons alone, as
    /// journalctl's is.
    fn order(&self, other: &Self, boots: &BootOrder) -> std::cmp::Ordering {
        if self.seqnum_id == other.seqnum_id {
            let o = self.seqnum.cmp(&other.seqnum);
            if o.is_ne() {
                return o;
            }
        }
        let o = if self.boot_id == other.boot_id {
            self.monotonic.cmp(&other.monotonic)
        } else {
            boots.compare(&self.boot_id, &other.boot_id)
        };
        if o.is_ne() {
            return o;
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
    /// Where the entry is: `Position::Before` or `Position::After` from it
    /// is a page next to this entry. The page next to a whole page starts
    /// from `Page::oldest_chosen` or `Page::newest_chosen` instead: those
    /// step past the entries the page chose but couldn't show.
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

/// What a follow read: the matching entries written since the last drain
/// (or, for `Journal::follow`'s first batch, since the page), oldest
/// first, and what went wrong. Built by `Journal::follow` and
/// `Follower::drain` only: more fields can come.
#[derive(Debug)]
#[non_exhaustive]
pub struct Batch {
    /// The new entries, oldest first.
    pub records: Vec<Record>,
    /// The output limit stopped a file in this batch: the entries from
    /// there come with the next drain, none lost. Drain again before
    /// waiting on the wake descriptor, which may show nothing new.
    pub more: bool,
    /// What couldn't be read.
    pub errors: ReadErrors,
}

// What a caller moves into `spawn_blocking` or shares across threads
// (journal spec, assumption 3).
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    const fn send_static<T: Send + 'static>() {}
    send_sync::<Journal>();
    send_static::<Page>();
    send_static::<Batch>();
    send_static::<Follower>();
    // `Follower::wake_fd`'s descriptor, for the caller's readiness wait.
    send_static::<std::os::fd::OwnedFd>();
    send_static::<PageEnd>();
    send_static::<MachineId>();
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

    /// One error of every kind, named by its variant.
    fn every_kind() -> Vec<(&'static str, ReadError)> {
        vec![
            (
                "OverLimit",
                error(
                    "/f",
                    ReadErrorKind::OverLimit {
                        context: String::new(),
                        over: OverLimit { limit: 5 },
                    },
                ),
            ),
            ("KnownBad", error("/f", KnownBad { item: 0x2a0 }.into())),
            ("Corrupt", error("/f", ReadErrorKind::Corrupt("x".into()))),
            (
                "Io",
                error(
                    "/f",
                    ReadErrorKind::Io {
                        context: "read".into(),
                        source: io::Error::other("gone"),
                    },
                ),
            ),
            (
                "TooLarge",
                error(
                    "/f",
                    ReadErrorKind::TooLarge {
                        context: String::new(),
                        limit: 5,
                    },
                ),
            ),
            (
                "OutputLimit",
                ReadError::of_page(ReadErrorKind::OutputLimit {
                    limit: 5,
                    left_out: 1,
                }),
            ),
            ("HeldLimit", ReadError::of_page(ReadErrorKind::HeldLimit)),
            (
                "OutputLimitNewer",
                ReadError::of_page(ReadErrorKind::OutputLimitNewer {
                    limit: 5,
                    left_out: 1,
                }),
            ),
            (
                "HeldLimitNewer",
                ReadError::of_page(ReadErrorKind::HeldLimitNewer),
            ),
            ("Symlink", error("/f", ReadErrorKind::Symlink)),
            ("NotRegular", error("/f", ReadErrorKind::NotRegular)),
            (
                "ListChanging",
                ReadError::of_page(ReadErrorKind::ListChanging),
            ),
        ]
    }

    /// `every_kind` holds one error of each kind: a match with no
    /// wildcard (`#[non_exhaustive]` binds only other crates), so a new
    /// kind fails to build until it is listed there and its helpers are
    /// decided.
    #[test]
    fn every_kind_lists_each_kind_once() {
        let name = |k: &ReadErrorKind| match k {
            ReadErrorKind::OverLimit { .. } => "OverLimit",
            ReadErrorKind::KnownBad(_) => "KnownBad",
            ReadErrorKind::Corrupt(_) => "Corrupt",
            ReadErrorKind::Io { .. } => "Io",
            ReadErrorKind::TooLarge { .. } => "TooLarge",
            ReadErrorKind::OutputLimit { .. } => "OutputLimit",
            ReadErrorKind::HeldLimit => "HeldLimit",
            ReadErrorKind::OutputLimitNewer { .. } => "OutputLimitNewer",
            ReadErrorKind::HeldLimitNewer => "HeldLimitNewer",
            ReadErrorKind::Symlink => "Symlink",
            ReadErrorKind::NotRegular => "NotRegular",
            ReadErrorKind::ListChanging => "ListChanging",
        };
        let kinds = every_kind();
        for (listed, e) in &kinds {
            assert_eq!(*listed, name(e.kind()));
        }
        let distinct: std::collections::BTreeSet<&str> = kinds.iter().map(|(n, _)| *n).collect();
        assert_eq!((kinds.len(), distinct.len()), (12, 12));
    }

    /// The kinds of `every_kind` for which `is` holds.
    fn kinds_where(is: impl Fn(&ReadError) -> bool) -> Vec<&'static str> {
        every_kind()
            .into_iter()
            .filter(|(_, e)| is(e))
            .map(|(name, _)| name)
            .collect()
    }

    /// `is_over_limit` holds for `OverLimit` only, not for the limits
    /// that end a page.
    #[test]
    fn is_over_limit_is_an_over_limit_payload_only() {
        assert_eq!(kinds_where(ReadError::is_over_limit), ["OverLimit"]);
    }

    #[test]
    fn is_known_bad_is_a_known_bad_object_only() {
        assert_eq!(kinds_where(ReadError::is_known_bad), ["KnownBad"]);
    }

    #[test]
    fn is_io_is_a_refused_read_only() {
        assert_eq!(kinds_where(ReadError::is_io), ["Io"]);
    }

    #[test]
    fn is_too_large_is_an_entry_over_the_whole_limit_only() {
        assert_eq!(kinds_where(ReadError::is_too_large), ["TooLarge"]);
    }

    #[test]
    fn is_symlink_is_a_symlinked_file_only() {
        assert_eq!(kinds_where(ReadError::is_symlink), ["Symlink"]);
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

    /// Errors gathered apart (a forward seek's) are absorbed as if added:
    /// distinct ones shown up to the cap, the rest counted, with the counts
    /// both had.
    #[test]
    fn absorbed_errors_are_added_and_counted() {
        let mut errors = ReadErrors::default();
        errors.add(page_error(ReadErrorKind::Corrupt("e0".into())));
        errors.omitted = 3;
        let mut met = ReadErrors::default();
        for i in 0..12 {
            met.add(page_error(ReadErrorKind::Corrupt(format!("e{i}"))));
        }
        assert_eq!((met.shown.len(), met.omitted), (10, 2));
        errors.absorb(met);
        // e0 was there; e1 to e9 join it; the two `met` counted are added.
        assert_eq!((errors.shown.len(), errors.omitted), (10, 5));
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
        self, A_ITEMS, Crafted, D_ENTRY_ARRAY, D_ENTRY_OFFSET, D_N_ENTRIES, Format, H_ARENA_SIZE,
        H_ENTRY_ARRAY_OFFSET, H_N_ENTRIES, O_SIZE, each_format,
    };
    use std::os::unix::fs::FileExt;

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
        files_on(&release("26.04"), paths)
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
        let j = Journal::open(
            Source::Directory(dir.path().to_path_buf()),
            &release("26.04"),
        )
        .expect("opens");
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 2);

        let missing = dir.path().join("gone\u{1b}[2J");
        let err = Journal::open(Source::Directory(missing.clone()), &release("26.04"))
            .expect_err("missing");
        let OpenError::Directory { path, source } = &err else {
            panic!("{err:?}");
        };
        assert_eq!((path, source.kind()), (&missing, io::ErrorKind::NotFound));
        assert!(err.to_string().ends_with("gone\\u{1b}[2J"), "{err}");
        assert!(err.to_string().starts_with("read the journal directory "));
    }

    /// A host whose files live under `dir`: its machine-id file holding
    /// `id`, and two roots.
    fn host_in(dir: &Path, id: &[u8]) -> crate::testing::Host {
        let machine_id = dir.join("machine-id");
        std::fs::write(&machine_id, id).expect("write");
        crate::testing::Host {
            machine_id,
            roots: [dir.join("run"), dir.join("var")],
        }
    }

    const MID: &str = "0123456789abcdef0123456789abcdef";

    /// The local journal's directories are the roots joined with the
    /// machine id, in search order; an unreadable machine id is an
    /// `OpenError`.
    #[test]
    fn the_local_journal_is_found_by_machine_id() {
        let dir = tempfile::tempdir().expect("dir");
        let mid = MID;
        let host = host_in(dir.path(), format!("{mid}\n").as_bytes());
        let [run, var] = host.roots.clone();
        let dirs = local_dirs(&host).expect("reads");
        assert_eq!(dirs, [run.join(mid), var.join(mid)]);

        // Both roots are read, either may be missing.
        std::fs::create_dir_all(&dirs[1]).expect("var");
        let (mut c, _l) = crafted::journal(3);
        std::fs::copy(c.file().path(), dirs[1].join("system.journal")).expect("write");
        let j = crate::testing::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 3);
        assert!(page.errors.is_empty(), "{:?}", page.errors);

        let gone = dir.path().join("nothing");
        let missing = crate::testing::Host {
            machine_id: gone.clone(),
            ..host
        };
        let err = crate::testing::open_on(Source::Local, &release("26.04"), &missing)
            .expect_err("missing");
        assert!(matches!(&err, OpenError::MachineId { path, .. } if *path == gone));
        assert_eq!(err.to_string(), format!("read {}", gone.display()));
    }

    /// The machine id in the file is the id and one newline at most:
    /// anything else is refused, so nothing else is joined into a path
    /// (L1). The error names the file, escaped.
    #[test]
    fn an_invalid_machine_id_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        assert!(local_dirs(&host_in(dir.path(), MID.as_bytes())).is_ok());
        for bad in [
            format!("{MID}\n\n"),
            format!(" {MID}\n"),
            format!("{MID}\r\n"),
            "0".repeat(32),
            "../../../../etc/ssh/whatever".to_owned(),
            String::new(),
        ] {
            let host = host_in(dir.path(), bad.as_bytes());
            let err = local_dirs(&host).expect_err("refused");
            assert!(
                matches!(&err, OpenError::InvalidMachineId { path: Some(p) } if *p == host.machine_id),
                "{bad:?}: {err:?}"
            );
        }
        let host = host_in(dir.path(), b"\xff0123456789abcdef0123456789abcde");
        assert!(matches!(
            local_dirs(&host),
            Err(OpenError::InvalidMachineId { path: Some(_) })
        ));
        let odd = dir.path().join("m\u{1b}]0;x\u{7}");
        std::fs::write(&odd, b"").expect("write");
        let err = crate::testing::open_on(
            Source::Local,
            &release("26.04"),
            &crate::testing::Host {
                machine_id: odd,
                ..host
            },
        )
        .expect_err("refused");
        assert!(
            err.to_string()
                .ends_with("m\\u{1b}]0;x\\u{7}: not a machine id (32 hex digits, not all zero)"),
            "{err}"
        );
    }

    /// An upper-case machine id, which systemd reads, finds the journal in
    /// the lower-case directory journald names after it.
    #[test]
    fn an_upper_case_machine_id_finds_the_lower_case_directory() {
        let dir = tempfile::tempdir().expect("dir");
        let host = host_in(dir.path(), format!("{}\n", MID.to_uppercase()).as_bytes());
        let [run, var] = host.roots.clone();
        assert_eq!(
            local_dirs(&host).expect("reads"),
            [run.join(MID), var.join(MID)]
        );
        std::fs::create_dir_all(var.join(MID)).expect("var");
        let (mut c, _l) = crafted::journal(2);
        std::fs::copy(c.file().path(), var.join(MID).join("system.journal")).expect("write");
        let j = crate::testing::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 2);
        assert!(page.errors.is_empty(), "{:?}", page.errors);
        let mixed = MachineId::parse("0123456789ABCDEF0123456789abcdef").expect("parses");
        assert_eq!(mixed.to_string(), MID);
        assert_eq!(mixed, MachineId::parse(MID).expect("parses"));
    }

    /// `MachineId::parse`: exactly 32 hex digits, either case, not all
    /// zero; `Display` writes them back in lower case.
    #[test]
    fn machine_id_parses_32_hex_digits() {
        let id = MachineId::parse(MID).expect("parses");
        assert_eq!(id.to_string(), MID);
        let one = format!("{}1", "0".repeat(31));
        assert_eq!(
            MachineId::parse(&one).expect("not all zero").to_string(),
            one
        );
        for bad in [
            "",
            &MID[..31],
            &format!("{MID}0"),
            &format!("{MID}\n"),
            "0123456789abcdeg0123456789abcdef",
            &"0".repeat(32),
            "+123456789abcdef0123456789abcdef",
            // A GUID, which a plain-format file can't hold.
            "01234567-89ab-cdef-0123-456789abcdef",
        ] {
            assert!(
                matches!(
                    MachineId::parse(bad),
                    Err(OpenError::InvalidMachineId { path: None })
                ),
                "{bad:?}"
            );
        }
        assert_eq!(
            OpenError::InvalidMachineId { path: None }.to_string(),
            "not a machine id (32 hex digits, not all zero)"
        );
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

    /// Difference 11, through a directory's listing: a file replaced, then
    /// one deleted, between two pages is seen by the second, and every file
    /// a page opened is closed when it returns (the opener's counts).
    #[test]
    fn each_page_lists_again_and_closes_what_it_opened() {
        let dir = tempfile::tempdir().expect("dir");
        let put = |n: u64, as_name: &str| {
            let (mut c, _l) = crafted::journal(n);
            std::fs::copy(c.file().path(), dir.path().join("new")).expect("write");
            std::fs::rename(dir.path().join("new"), dir.path().join(as_name)).expect("rename");
        };
        put(2, "a.journal");
        put(3, "b.journal");
        let host = crate::testing::Host::default();
        let j = crate::testing::open_on(
            Source::Directory(dir.path().to_path_buf()),
            &release("26.04"),
            &host,
        )
        .expect("opens");
        let q = Query::field(&name("U"), b"x");
        let closed = |want_opens: u64| {
            let c = crate::testing::counts(&j);
            assert_eq!((c.opens, c.closes), (want_opens, want_opens), "{c:?}");
        };
        // Two copies of each other's entries: the flushed-copy rule shows
        // three (crafted journals share their locations).
        assert_eq!(tail(&j, &q, 10).records.len(), 3);
        closed(2);
        put(5, "a.journal");
        assert_eq!(tail(&j, &q, 10).records.len(), 5);
        closed(4);
        std::fs::remove_file(dir.path().join("a.journal")).expect("remove");
        let page = tail(&j, &q, 10);
        assert_eq!(page.records.len(), 3);
        assert!(page.errors.is_empty(), "{:?}", page.errors);
        closed(5);
        // A forward and a backward page close theirs too.
        let at = page.records[1].cursor();
        let _ = j.page(&q, &Position::After(at), 10, &Limits::default());
        let _ = j.page(&q, &Position::Before(at), 10, &Limits::default());
        closed(7);
    }

    /// Opening through a page (L2): a FIFO is refused without blocking, a
    /// symlinked journal file is refused (Difference 4), each an error of
    /// its own naming the file, and the other files still read. The FIFO
    /// is named by `Source::Files`: a directory's listing doesn't take one,
    /// as sd-journal's doesn't (walk.rs, `scan`).
    #[test]
    fn a_page_refuses_fifos_and_symlinked_files() {
        let dir = tempfile::tempdir().expect("dir");
        let fifo = dir.path().join("fifo.journal");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).expect("mkfifo");
        let (mut c, _l) = crafted::journal(2);
        let real = dir.path().join("real.journal");
        std::fs::copy(c.file().path(), &real).expect("write");
        let link = dir.path().join("link.journal");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let j = files(&[&fifo, &link, &real]);
        let page = tail(&j, &Query::field(&name("U"), b"x"), 10);
        assert_eq!(page.records.len(), 2);
        let got: Vec<(Option<&Path>, String)> = page
            .errors
            .shown
            .iter()
            .map(|e| (e.path(), e.kind().to_string()))
            .collect();
        assert_eq!(
            got,
            [
                (Some(fifo.as_path()), "not a regular file".to_owned()),
                (
                    Some(link.as_path()),
                    "a symlink, not followed (only directories are)".to_owned()
                ),
            ]
        );
    }

    /// A symlinked journal directory is read, as sd-journal reads it
    /// (Difference 4: only files aren't followed), as a `Directory` source
    /// and as a local root.
    #[test]
    fn a_symlinked_directory_is_read() {
        let dir = tempfile::tempdir().expect("dir");
        let real = dir.path().join("elsewhere");
        std::fs::create_dir_all(real.join(MID)).expect("dirs");
        let (mut c, _l) = crafted::journal(2);
        std::fs::copy(c.file().path(), real.join(MID).join("system.journal")).expect("write");
        let linked = dir.path().join("var");
        std::os::unix::fs::symlink(&real, &linked).expect("link");
        let q = Query::field(&name("U"), b"x");
        let j = Journal::open(Source::Directory(linked), &release("26.04")).expect("opens");
        assert_eq!(tail(&j, &q, 10).records.len(), 2);
        let host = host_in(dir.path(), format!("{MID}\n").as_bytes());
        let j = crate::testing::open_on(Source::Local, &release("26.04"), &host).expect("opens");
        let page = tail(&j, &q, 10);
        assert_eq!(page.records.len(), 2);
        assert!(page.errors.is_empty(), "{:?}", page.errors);
    }

    /// Open errors are one per file and outside the ten read errors' cap,
    /// on a page whose read errors went past it.
    #[test]
    fn a_pages_open_errors_are_outside_the_cap() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut c, l) = crafted::journal(30);
        for (i, &e) in l.entries.iter().enumerate() {
            if i % 2 == 1 {
                c.set(e + crafted::O_SIZE, 0);
            }
        }
        let broken = c.file();
        let mut paths = vec![broken.path().to_path_buf()];
        for i in 0..12 {
            let link = dir.path().join(format!("{i}.journal"));
            std::os::unix::fs::symlink(broken.path(), &link).expect("link");
            paths.push(link.clone());
            paths.push(link);
        }
        let j = Journal::open(Source::Files(paths), &release("26.04")).expect("opens");
        let page = j.page(
            &Query::field(&name("U"), b"x"),
            &Position::Tail,
            100,
            &Limits::default(),
        );
        assert_eq!(page.records.len(), 15);
        let symlinks = page.errors.shown.iter().filter(|e| e.is_symlink()).count();
        assert_eq!(symlinks, 12);
        assert_eq!(page.errors.shown.len(), 12 + ReadErrors::SHOWN);
        assert_eq!(page.errors.omitted, 5);
    }

    /// An empty page has its end too: the files read, at their counts,
    /// and the newest entry, though none matched. Two pages of an unchanged
    /// journal end the same; an entry appended changes the end.
    #[test]
    fn an_empty_page_has_an_end() {
        each_format(an_empty_page_has_an_end_in);
    }

    fn an_empty_page_has_an_end_in(format: Format) {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("system.journal");
        let (mut c, _l) = crafted::journal_in(format, 3);
        std::fs::copy(c.file().path(), &path).expect("write");
        let j = files(&[&path]);
        let none = Query::field(&name("U"), b"nothing");
        let page = tail(&j, &none, 10);
        assert!(page.records.is_empty() && !page.more_older && !page.more_newer);
        let meta = std::fs::metadata(&path).expect("stat");
        assert_eq!(
            page.end.files,
            [FileEnd {
                inode: (
                    std::os::unix::fs::MetadataExt::dev(&meta),
                    std::os::unix::fs::MetadataExt::ino(&meta)
                ),
                file_id: file::JournalFile::open(&path)
                    .expect("opens")
                    .header
                    .file_id,
                n_entries: 3
            }]
        );
        let (newest, in_dir) = page.end.newest.clone().expect("the newest entry");
        assert_eq!((newest.seqnum, in_dir.as_deref()), (3, Some(dir.path())));
        assert_eq!(tail(&j, &none, 10).end, page.end);
        let (mut c, _l) = crafted::journal_in(format, 4);
        std::fs::write(&path, &c.b).expect("rewrite in place");
        let _ = c.file();
        let later = tail(&j, &none, 10).end;
        assert_ne!(later, page.end);
        assert_eq!(later.files[0].n_entries, 4);
        // No files: an empty end.
        let nothing = files(&[]);
        assert_eq!(tail(&nothing, &none, 10).end, PageEnd::default());
    }

    /// Success criterion 5 on a crafted fixture: a tail of ten, the page
    /// before it and the page after that page's first entry take the
    /// preads stated here, the bound tasks/journal/plan.md records. The
    /// fixture is `crafted::journal(200)`, matched on `U=x` (every entry), pages of
    /// ten. Each page reads the header and the end's newest entry, then
    /// one pread per entry walked (the in-line arrays are read in chunks)
    /// and one per DATA object shown; `Before` also walks the newer entries
    /// again. `After` bisects instead (an item and an entry per probe), so
    /// near the end it costs more than the walk it replaced (50) and stays
    /// the same at any depth (`a_deep_forward_page_costs_a_bisect`). Being
    /// filtered, each page also reads the header again after its lookup,
    /// then the newest counted entry's item (the one array's head and the
    /// item) and the entry, looks `U=x` up again and bisects its list for
    /// that entry: 18 preads (journal spec decision 13; 42, 52 and 88
    /// before it).
    #[test]
    fn the_preads_of_a_tail_a_page_back_and_a_page_forward_are_bounded() {
        const TAIL: u64 = 60;
        const BACK: u64 = 70;
        const FORWARD: u64 = 106;
        let (mut c, _l) = crafted::journal(200);
        let t = c.file();
        let j = files(&[t.path()]);
        let q = Query::field(&name("U"), b"x");
        let limits = Limits::default();
        let preads = || crate::testing::counts(&j).preads;
        let tail = j.page(&q, &Position::Tail, 10, &limits);
        let after_tail = preads();
        let back = j.page(&q, &Position::Before(tail.records[0].cursor()), 10, &limits);
        let after_back = preads();
        let forward = j.page(&q, &Position::After(back.records[0].cursor()), 10, &limits);
        let after_forward = preads();
        assert_eq!(
            messages_of(&tail),
            (190..200).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        assert_eq!(
            messages_of(&back),
            (180..190).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        assert_eq!(
            messages_of(&forward),
            (181..191).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        let costs = (
            after_tail,
            after_back - after_tail,
            after_forward - after_back,
        );
        // Exactly the figures stated: a counter that counted nothing, or a
        // page that read more, fails.
        assert_eq!(costs, (TAIL, BACK, FORWARD));
        // Each page opened the file once and closed it.
        let counts = crate::testing::counts(&j);
        assert_eq!((counts.opens, counts.closes), (3, 3));
    }

    /// A forward page costs a bisect, not a walk back to its cursor: from
    /// near the start of `crafted::journal(2000)`, a page of ten after the
    /// tenth entry takes the preads stated here, what one near the end
    /// takes (O(log n) per bisect). A cursor whose entry has vanished costs
    /// a little more: the landing is shown, and each file's first match is
    /// read to tell whether older entries lie before it.
    #[test]
    fn a_deep_forward_page_costs_a_bisect() {
        const DEEP: u64 = 122;
        const NEAR_END: u64 = 122;
        const VANISHED: u64 = 125;
        let (mut c, _l) = crafted::journal(2000);
        let t = c.file();
        let j = files(&[t.path()]);
        let q = Query::field(&name("U"), b"x");
        let limits = Limits::default();
        let all = j.page(&q, &Position::Tail, 2000, &limits);
        let preads = || crate::testing::counts(&j).preads;
        let before = preads();
        let deep = j.page(&q, &Position::After(all.records[9].cursor()), 10, &limits);
        let deep_cost = preads() - before;
        let before = preads();
        let near = j.page(
            &q,
            &Position::After(all.records[1980].cursor()),
            10,
            &limits,
        );
        let near_cost = preads() - before;
        assert_eq!(
            messages_of(&deep),
            (10..20).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        assert_eq!(
            messages_of(&near),
            (1981..1991).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        let mut vanished = all.records[9].cursor();
        vanished.xor_hash ^= 1;
        let before = preads();
        let gone = j.page(&q, &Position::After(vanished), 10, &limits);
        let gone_cost = preads() - before;
        assert_eq!(
            messages_of(&gone),
            (9..19).map(|i| format!("m{i}")).collect::<Vec<_>>()
        );
        assert!(gone.more_older && gone.more_newer);
        assert_eq!(
            (deep_cost, near_cost, gone_cost),
            (DEEP, NEAR_END, VANISHED)
        );
    }

    fn messages_of(page: &Page) -> Vec<String> {
        page.records
            .iter()
            .map(|r| String::from_utf8_lossy(r.message().expect("a message")).into_owned())
            .collect()
    }

    /// The flags and the end come through `Journal::page` (the walk's
    /// tests cover each position).
    #[test]
    fn a_pages_flags_and_end_are_the_walks() {
        each_format(a_pages_flags_and_end_are_the_walks_in);
    }

    fn a_pages_flags_and_end_are_the_walks_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 4);
        let t = c.file();
        let j = files(&[t.path()]);
        let q = Query::field(&name("U"), b"x");
        let page = tail(&j, &q, 2);
        assert_eq!((page.more_older, page.more_newer), (true, false));
        assert_eq!(page.end.files.len(), 1);
        let fwd = j.page(
            &q,
            &Position::After(page.records[0].cursor()),
            1,
            &Limits::default(),
        );
        assert_eq!(messages_of(&fwd), ["m3"]);
        assert_eq!((fwd.more_older, fwd.more_newer), (true, false));
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

    /// A combining mark in a file name stays with its base character (an
    /// NFD name), as cli-core shows it; one after an escaped character is
    /// escaped (plan decision 40).
    #[test]
    fn a_paths_combining_marks_stay_with_their_base() {
        let err = OpenError::Directory {
            path: PathBuf::from("/var/log/Ame\u{301}lie/\"\u{301}"),
            source: io::Error::from(io::ErrorKind::NotFound),
        };
        assert_eq!(
            err.to_string(),
            "read the journal directory /var/log/Ame\u{301}lie/\\\"\\u{301}"
        );
    }

    /// A path's mark with no base is escaped, and so are invisible
    /// extenders and direction marks after a base character (audit L5),
    /// as cli-core's escaper does.
    #[test]
    fn a_paths_marks_without_a_base_and_invisibles_are_escaped() {
        let shown = |path: &str| {
            OpenError::Directory {
                path: PathBuf::from(path),
                source: io::Error::from(io::ErrorKind::NotFound),
            }
            .to_string()
        };
        assert_eq!(shown("\u{301}x"), "read the journal directory \\u{301}x");
        assert_eq!(
            shown("a\u{fe0f}b\u{200d}c\u{200c}d\u{34f}e\u{202e}f\n\u{301}"),
            "read the journal directory a\\u{fe0f}b\\u{200d}c\\u{200c}d\\u{34f}e\\u{202e}f\\n\\u{301}"
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

    /// Security audit (Task 11, M2): a page bounds each entry it reads by
    /// its output limit, so an entry listing more items than the page
    /// could show is refused before its body is read (here, a sparse
    /// file's 16 MiB entry under a 1 MiB limit), reported, and the page
    /// goes on to the older entry.
    #[test]
    fn a_page_refuses_an_entry_with_more_items_than_its_limit_shows() {
        let mut c = Crafted::new();
        c.hash_table(7);
        let old = c.data(b"MESSAGE=old");
        let e1 = c.entry(1, &[old]);
        let list = c.array(&[0], 1, 0);
        let main = c.array(&[e1, 0], 2, 0);
        // Last, so its claimed size runs to the file's (sparse) end.
        let big = c.entry(2, &[old]);
        let size: u64 = 64 + 16 * (1 << 20);
        c.set(big + O_SIZE, size);
        c.set(list + A_ITEMS, big);
        c.set(main + A_ITEMS + 8, big);
        c.set(old + D_ENTRY_OFFSET, e1);
        c.set(old + D_N_ENTRIES, 2);
        c.set(old + D_ENTRY_ARRAY, list);
        c.set(H_N_ENTRIES, 2);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let t = c.file();
        let disk = std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open");
        disk.set_len(big + size).expect("sparse growth");
        disk.write_all_at(&(big + size - 272).to_le_bytes(), H_ARENA_SIZE)
            .expect("arena size");
        let j = files(&[t.path()]);
        let query = Query::field(&name("MESSAGE"), b"old");
        let page = j.page(&query, &Position::Tail, 10, &Limits::with_output(1 << 20));
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.records[0].message(), Some(b"old".as_slice()));
        assert!(
            page.errors
                .shown
                .iter()
                .any(|e| e.to_string().contains("lists 1048576 items")),
            "{:?}",
            page.errors
        );
    }

    /// The decision-11 fixture's rows: per entry its name, boot,
    /// monotonic and realtime.
    type Rows<'a> = &'a [(&'a str, u8, u64, u64)];

    /// `_BOOT_ID=` and the boot's id, as journald writes the field.
    fn boot_field(boot: u8) -> Vec<u8> {
        format!("_BOOT_ID={}", file::hex_id(&[boot; 16])).into_bytes()
    }

    /// A file of `rows`, each entry carrying `WHO=<name>`, its
    /// `_BOOT_ID=` and, when `matching`, `U=x`; its seqnum id and machine
    /// id all `seqnum_id` and `machine` bytes; its header's
    /// `tail_entry_offset` naming its last entry, as journald 254 and
    /// later write it; and a field table with the `_BOOT_ID` FIELD object
    /// listing the boots' DATA objects.
    fn boot_file(
        seqnum_id: u8,
        machine: u8,
        rows: Rows<'_>,
        matching: bool,
    ) -> tempfile::NamedTempFile {
        boot_file_in(Format::Regular, seqnum_id, machine, rows, matching)
    }

    /// `boot_file`, in `format`.
    fn boot_file_in(
        format: Format,
        seqnum_id: u8,
        machine: u8,
        rows: Rows<'_>,
        matching: bool,
    ) -> tempfile::NamedTempFile {
        boot_file_naming_in(format, seqnum_id, machine, rows, matching, &[])
    }

    /// `boot_file`, row `i`'s entry carrying `named[i]` last when it is
    /// given (a `_MACHINE_ID=` field, as journal-remote keeps the source's).
    fn boot_file_naming(
        seqnum_id: u8,
        machine: u8,
        rows: Rows<'_>,
        matching: bool,
        named: &[Option<&[u8]>],
    ) -> tempfile::NamedTempFile {
        boot_file_naming_in(Format::Regular, seqnum_id, machine, rows, matching, named)
    }

    /// `boot_file_naming`, in `format`.
    fn boot_file_naming_in(
        format: Format,
        seqnum_id: u8,
        machine: u8,
        rows: Rows<'_>,
        matching: bool,
        named: &[Option<&[u8]>],
    ) -> tempfile::NamedTempFile {
        let payloads: Vec<Vec<Vec<u8>>> = rows
            .iter()
            .enumerate()
            .map(|(i, &(who, boot, ..))| {
                let mut p = vec![format!("WHO={who}").into_bytes(), boot_field(boot)];
                if matching {
                    p.push(b"U=x".to_vec());
                }
                if let Some(Some(field)) = named.get(i) {
                    p.push(field.to_vec());
                }
                p
            })
            .collect();
        let refs: Vec<Vec<&[u8]>> = payloads
            .iter()
            .map(|p| p.iter().map(Vec::as_slice).collect())
            .collect();
        let entries: Vec<&[&[u8]]> = refs.iter().map(Vec::as_slice).collect();
        let (mut c, offsets) = crafted::journal_of_in(format, &entries);
        for (&e, &(_, boot, mono, real)) in offsets.iter().zip(rows) {
            c.set(e + 24, real);
            c.set(e + 32, mono);
            let e = usize::try_from(e).expect("small");
            c.b[e + 40..e + 56].copy_from_slice(&[boot; 16]);
        }
        c.b[40..56].copy_from_slice(&[machine; 16]);
        c.b[72..88].copy_from_slice(&[seqnum_id; 16]);
        c.set(264, *offsets.last().expect("an entry"));
        // The boots' DATA objects, oldest first, found in the file so far.
        let t = c.file();
        let f = file::JournalFile::open(t.path()).expect("opens");
        let mut boots: Vec<u64> = Vec::new();
        for &(_, boot, ..) in rows {
            let d = f.find_data(&boot_field(boot)).expect("lookup");
            let d = d.expect("found").offset;
            if !boots.contains(&d) {
                boots.push(d);
            }
        }
        c.field_table(3);
        c.field(b"_BOOT_ID", &boots);
        c.file()
    }

    /// Two boots of one machine whose realtimes overlap, in two files
    /// (seqnum ids 1 and 2, so no sequence number orders them): boot 0xa's
    /// a1 and a2 at realtimes 100 and 300, boot 0xb's b1 and b2 at 200 and
    /// 400. Boot 0xa's newest tail is at 300, 0xb's at 400.
    const BOOT_A: Rows<'static> = &[("a1", 0xa, 10, 100), ("a2", 0xa, 20, 300)];
    const BOOT_B: Rows<'static> = &[("b1", 0xb, 5, 200), ("b2", 0xb, 15, 400)];

    fn release(v: &str) -> Release {
        Release::parse(v).expect("a supported release")
    }

    fn files_on(release: &Release, paths: &[&Path]) -> Journal {
        let paths = paths.iter().map(|p| p.to_path_buf()).collect();
        Journal::open(Source::Files(paths), release).expect("opens")
    }

    fn who(page: &Page) -> Vec<String> {
        page.records
            .iter()
            .map(|r| String::from_utf8_lossy(r.field(&name("WHO")).expect("WHO")).into_owned())
            .collect()
    }

    /// The pages decision 11 changes, by `who`: the tail of four and of
    /// two, the pages of two before b2 and a2, and after a1, b1 and a2.
    /// Each comparison site differs between the orders in one of them:
    /// the tail's merge, `Before`'s cursor test (before a2), the forward
    /// page's first step (after b1, whose landings are a2 and b1) and its
    /// later steps (after a1).
    fn pages(j: &Journal) -> [Vec<String>; 7] {
        let q = Query::field(&name("U"), b"x");
        let limits = Limits::default();
        let all = tail(j, &q, 4);
        let page = |at: Position| who(&j.page(&q, &at, 2, &limits));
        let at = |name: &str| {
            let i = who(&all).iter().position(|w| w == name).expect("shown");
            all.records[i].cursor()
        };
        [
            who(&all),
            who(&tail(j, &q, 2)),
            page(Position::Before(at("b2"))),
            page(Position::Before(at("a2"))),
            page(Position::After(at("a1"))),
            page(Position::After(at("b1"))),
            page(Position::After(at("a2"))),
        ]
    }

    const BY_REALTIME: [&[&str]; 7] = [
        &["a1", "b1", "a2", "b2"],
        &["a2", "b2"],
        &["b1", "a2"],
        &["a1", "b1"],
        &["b1", "a2"],
        &["a2", "b2"],
        &["b2"],
    ];
    /// A forward page is the entries after the cursor in this order
    /// (decision 11): after b1 only b2, after a2 b1 and b2. On the
    /// matrix's same fixture journalctl 259.9 gives [b2] after b1 and
    /// [b2] after a2, 255.4 [a2, b1, b2] and [b2] (Difference 20).
    const BY_BOOT: [&[&str]; 7] = [
        &["a1", "a2", "b1", "b2"],
        &["b1", "b2"],
        &["a2", "b1"],
        &["a1"],
        &["a2", "b1"],
        &["b2"],
        &["b1", "b2"],
    ];

    /// Decision 11: entries of two boots of one machine order as each
    /// release's journalctl orders them. On 22.04 (systemd 249,
    /// `journal_file_compare_locations`) by realtime; on 24.04 and 26.04
    /// (255 and 259, `compare_boot_ids`) the boot whose newest tail is
    /// older comes first, whole: the tail, a page back and a page forward
    /// all differ.
    #[test]
    fn boots_order_as_each_releases_journalctl() {
        let (a, b) = (boot_file(1, 9, BOOT_A, true), boot_file(2, 9, BOOT_B, true));
        let paths = [a.path(), b.path()];
        assert_eq!(pages(&files_on(&release("22.04"), &paths)), BY_REALTIME);
        for v in ["24.04", "26.04"] {
            assert_eq!(pages(&files_on(&release(v), &paths)), BY_BOOT, "{v}");
        }
    }

    /// A boot not in the table compares equal to any other, so realtime
    /// decides, as on 22.04: here boot 0xa's file has no tail
    /// (`tail_entry_offset` zero).
    #[test]
    fn a_boot_missing_from_the_table_orders_by_realtime() {
        let (a, b) = (boot_file(1, 9, BOOT_A, true), boot_file(2, 9, BOOT_B, true));
        let mut bytes = std::fs::read(a.path()).expect("read");
        bytes[264..272].fill(0);
        std::fs::write(a.path(), &bytes).expect("write");
        let j = files_on(&release("26.04"), &[a.path(), b.path()]);
        assert_eq!(pages(&j), BY_REALTIME);
    }

    /// Boots whose newest files name two machines aren't ordered by the
    /// table ("timestamps of the boot ids might be as off as the
    /// timestamps on the entries"): realtime decides.
    #[test]
    fn boots_of_two_machines_order_by_realtime() {
        let (a, b) = (boot_file(1, 9, BOOT_A, true), boot_file(2, 8, BOOT_B, true));
        let j = files_on(&release("24.04"), &[a.path(), b.path()]);
        assert_eq!(pages(&j), BY_REALTIME);
    }

    /// `_MACHINE_ID=` of machine `m` (its 16 bytes all `m`).
    fn machine_field(m: u8) -> Vec<u8> {
        format!("_MACHINE_ID={}", file::hex_id(&[m; 16])).into_bytes()
    }

    /// 26.04's journalctl (systemd 259.9; v259-stable commit 2d8b0b6a0aaf,
    /// issue #34169) takes each boot's machine from its newest file's tail
    /// entry's `_MACHINE_ID=`, where 24.04's (255.4) takes the file
    /// header's. Files journal-remote writes carry the receiving machine
    /// in their header and the source's in each entry: with one header
    /// machine and two sources 26.04 orders by realtime and 24.04 by boot,
    /// and with two header machines and one source the other way round.
    /// 22.04 has no boot step.
    #[test]
    fn on_26_04_the_tail_entries_name_the_machines() {
        let (m1, m2) = (machine_field(1), machine_field(2));
        let a = boot_file_naming(1, 9, BOOT_A, true, &[Some(&m1), Some(&m1)]);
        let b = boot_file_naming(2, 9, BOOT_B, true, &[Some(&m2), Some(&m2)]);
        let paths = [a.path(), b.path()];
        let by = |v: &str| pages(&files_on(&release(v), &paths));
        assert_eq!(by("22.04"), BY_REALTIME);
        assert_eq!(by("24.04"), BY_BOOT);
        assert_eq!(by("26.04"), BY_REALTIME);
        let b = boot_file_naming(2, 8, BOOT_B, true, &[Some(&m1), Some(&m1)]);
        let paths = [a.path(), b.path()];
        let by = |v: &str| pages(&files_on(&release(v), &paths));
        assert_eq!(by("22.04"), BY_REALTIME);
        assert_eq!(by("24.04"), BY_REALTIME);
        assert_eq!(by("26.04"), BY_BOOT);
    }

    /// Only the tail entry counts: where it has no `_MACHINE_ID=`, the
    /// header's machine stays, though older entries name others.
    #[test]
    fn a_tail_entry_without_a_machine_id_keeps_the_headers_machine() {
        let (m1, m2) = (machine_field(1), machine_field(2));
        let a = boot_file_naming(1, 9, BOOT_A, true, &[Some(&m1), None]);
        let b = boot_file_naming(2, 9, BOOT_B, true, &[Some(&m2), None]);
        let j = files_on(&release("26.04"), &[a.path(), b.path()]);
        assert_eq!(pages(&j), BY_BOOT);
    }

    /// A tail entry whose `_MACHINE_ID=` item can't be read keeps the
    /// header's machine. Boot 0xb's newest tail is in a third file that
    /// doesn't match (so its entries aren't shown), at realtime 450; it
    /// names machine 2 where boot 0xa's names the headers' 9, so realtime
    /// decides, until its item points at no DATA object.
    #[test]
    fn an_unreadable_machine_id_keeps_the_headers_machine() {
        let (m9, m2) = (machine_field(9), machine_field(2));
        let a = boot_file_naming(1, 9, BOOT_A, true, &[Some(&m9), Some(&m9)]);
        let b = boot_file(2, 9, BOOT_B, true);
        let other = boot_file_naming(3, 9, &[("b3", 0xb, 30, 450)], false, &[Some(&m2)]);
        let paths = [a.path(), b.path(), other.path()];
        assert_eq!(pages(&files_on(&release("26.04"), &paths)), BY_REALTIME);
        assert_eq!(pages(&files_on(&release("24.04"), &paths)), BY_BOOT);
        // MESSAGE, WHO, _BOOT_ID, then _MACHINE_ID: point the fourth item
        // at the entry itself.
        let mut bytes = std::fs::read(other.path()).expect("read");
        let e = file::le64(&bytes, 264);
        let item = usize::try_from(e + 64 + 3 * 16).expect("small");
        bytes[item..item + 8].copy_from_slice(&e.to_le_bytes());
        std::fs::write(other.path(), &bytes).expect("write");
        assert_eq!(pages(&files_on(&release("26.04"), &paths)), BY_BOOT);
    }

    /// The cost on 26.04: one pread per page and file for each item of
    /// the file's tail entry read, up to its `_MACHINE_ID=`, or all of
    /// them without one. Here each tail entry lists MESSAGE, WHO,
    /// `_BOOT_ID`, `U` and, when named, `_MACHINE_ID`, last: five reads
    /// in each of two files, or four without it. The rest of the page
    /// costs what it does on 24.04.
    #[test]
    fn the_tail_entrys_machine_id_costs_a_pread_per_item_read() {
        let m1 = machine_field(1);
        let cost = |v: &str, named: &[Option<&[u8]>]| {
            let a = boot_file_naming(1, 9, BOOT_A, true, named);
            let b = boot_file_naming(2, 9, BOOT_B, true, named);
            let j = files_on(&release(v), &[a.path(), b.path()]);
            let q = Query::field(&name("U"), b"x");
            assert_eq!(tail(&j, &q, 4).records.len(), 4);
            crate::testing::counts(&j).preads
        };
        let named: &[Option<&[u8]>] = &[Some(&m1), Some(&m1)];
        assert_eq!(cost("26.04", named), cost("24.04", named) + 2 * 5);
        assert_eq!(cost("26.04", &[]), cost("24.04", &[]) + 2 * 4);
        assert_eq!(cost("22.04", named), cost("24.04", named));
    }

    /// The table holds every file the page opened, matching or not
    /// (sd-journal reads each added file's tail): a third file of boot
    /// 0xb, with no `U=x`, has its newest tail (the highest monotonic time,
    /// 30) at realtime 250, now older than boot 0xa's 300, so all of 0xb
    /// comes first. The page's end, the newest entry across the files in
    /// file order, follows the same order: a2 on 26.04; on 22.04, b2 over
    /// a2 by realtime, then b3 over b2 by monotonic time (the order isn't
    /// transitive).
    #[test]
    fn every_file_counts_in_the_table_matching_or_not() {
        let (a, b) = (boot_file(1, 9, BOOT_A, true), boot_file(2, 9, BOOT_B, true));
        let other = boot_file(3, 9, &[("b3", 0xb, 30, 250)], false);
        let paths = [a.path(), b.path(), other.path()];
        let q = Query::field(&name("U"), b"x");
        let newest = |page: &Page| page.end.newest.as_ref().map(|(c, _)| c.realtime);
        let page = tail(&files_on(&release("26.04"), &paths), &q, 4);
        assert_eq!(who(&page), ["b1", "b2", "a1", "a2"]);
        assert_eq!(newest(&page), Some(300));
        let page = tail(&files_on(&release("22.04"), &paths), &q, 4);
        assert_eq!(who(&page), ["a1", "b1", "a2", "b2"]);
        assert_eq!(newest(&page), Some(250));
    }

    /// Paging forward one entry at a time shows every entry once, in the
    /// release's order. On 24.04 and 26.04 journalctl's own seek (a
    /// realtime bisect in boot 0xb's file, landing on b2 from a2) passes
    /// b1 (Difference 20; the matrix's `boots overlap` row shows a2, b2);
    /// sb lands each boot the table orders after the cursor's at its first
    /// entry, so b1 is shown. On 22.04 the pages are journalctl's.
    #[test]
    fn forward_pages_show_every_entry_of_another_boot() {
        each_format(forward_pages_show_every_entry_of_another_boot_in);
    }

    fn forward_pages_show_every_entry_of_another_boot_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let paths = [a.path(), b.path()];
        let q = Query::field(&name("U"), b"x");
        let forward = |j: &Journal| {
            let step = |c: Cursor| j.page(&q, &Position::After(c), 1, &Limits::default());
            let mut shown = Vec::new();
            let mut page = step(tail(j, &q, 4).records[0].cursor());
            // At most one page per entry and one more: a regression that
            // pages for ever fails here instead of hanging.
            for _ in 0..5 {
                let Some(r) = page.records.first() else { break };
                shown.extend(who(&page));
                page = step(r.cursor());
            }
            shown
        };
        let j = files_on(&release("22.04"), &paths);
        assert_eq!(forward(&j), ["b1", "a2", "b2"]);
        for v in ["24.04", "26.04"] {
            let j = files_on(&release(v), &paths);
            assert_eq!(forward(&j), ["a2", "b1", "b2"], "{v}");
        }
    }

    /// A file sought per boot (24.04's table, another seqnum space) whose
    /// newest entry, b2, journald hasn't linked into `U=x`'s list nor its
    /// `_BOOT_ID` list at the page's lookups. Linked by the page's second
    /// header read, it is taken (journal spec decision 13): by the cursor's
    /// order, as boot 0xb's start, from a list that didn't hold it yet,
    /// can't place it; b1 by that start. Never linked, it isn't shown, as
    /// journalctl's view doesn't hold it.
    #[test]
    fn a_per_boot_forward_page_takes_the_newest_entry_once_listed() {
        each_format(a_per_boot_forward_page_takes_the_newest_entry_once_listed_in);
    }

    fn a_per_boot_forward_page_takes_the_newest_entry_once_listed_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let linked = std::fs::read(b.path()).expect("read");
        let mut early = linked.clone();
        let n_entries = usize::try_from(D_N_ENTRIES).expect("small");
        for payload in [b"U=x".to_vec(), boot_field(0xb)] {
            let at = early.windows(payload.len()).position(|w| w == payload);
            let data =
                at.expect("the payload") - usize::try_from(format.payload_at()).expect("small");
            early[data + n_entries..data + n_entries + 8].copy_from_slice(&1u64.to_le_bytes());
        }
        let j = files_on(&release("24.04"), &[a.path(), b.path()]);
        let q = Query::field(&name("U"), b"x");
        let all = tail(&j, &q, 4);
        let a2 = all
            .records
            .iter()
            .find(|r| r.field(&name("WHO")) == Some(b"a2"));
        let at = Position::After(a2.expect("a2").cursor());
        for (links, want) in [(true, vec!["b1", "b2"]), (false, vec!["b1"])] {
            std::fs::write(b.path(), &early).expect("write");
            let (path, bytes) = (b.path().to_path_buf(), linked.clone());
            let fwd = crate::testing::seam::between_header_reads(
                move |p| {
                    if links && p == path {
                        std::fs::write(p, &bytes).expect("write");
                    }
                },
                || j.page(&q, &at, 10, &Limits::default()),
            );
            assert_eq!(who(&fwd), want, "linked by the second read: {links}");
            assert!(fwd.errors.is_empty(), "{:?}", fwd.errors);
        }
    }

    /// One file holding three boots against a cursor in boot 0xa (whose
    /// newest tail is at 300): b1 and b2 of boot 0xb, ordered after it;
    /// c1 and c2 of boot 0xc, in no file's tail, so left equal and ordered
    /// by realtime; and d0 of boot 0xd, whose tail (in a file of its own,
    /// at 50) orders it before. Laid out b1, c1, d0, c2, b2 (tail b2). In
    /// `format`.
    fn mixed_boots_in(format: Format) -> [tempfile::NamedTempFile; 3] {
        let mixed: Rows<'static> = &[
            ("b1", 0xb, 5, 200),
            ("c1", 0xc, 1, 150),
            ("d0", 0xd, 2, 60),
            ("c2", 0xc, 2, 350),
            ("b2", 0xb, 15, 400),
        ];
        [
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, mixed, true),
            boot_file_in(format, 3, 9, &[("d1", 0xd, 3, 50)], false),
        ]
    }

    /// After a2 (realtime 300), the mixed file's boots start on their own:
    /// 0xb at its first entry, 0xc where its realtime reaches 300 (c2), 0xd
    /// nowhere. Walking from b1, c1 (before 0xc's start) and d0 are passed
    /// over. (`more_older` is set by the cursor's own entry, skipped, and
    /// a1 before it in its file.)
    #[test]
    fn each_boot_of_a_file_starts_on_its_own() {
        each_format(each_boot_of_a_file_starts_on_its_own_in);
    }

    fn each_boot_of_a_file_starts_on_its_own_in(format: Format) {
        let files = mixed_boots_in(format);
        let paths: Vec<&Path> = files.iter().map(tempfile::NamedTempFile::path).collect();
        let j = files_on(&release("26.04"), &paths);
        let q = Query::field(&name("U"), b"x");
        let all = tail(&j, &q, 10);
        let a2 = all
            .records
            .iter()
            .find(|r| r.field(&name("WHO")) == Some(b"a2"));
        let fwd = j.page(
            &q,
            &Position::After(a2.expect("a2").cursor()),
            10,
            &Limits::default(),
        );
        assert_eq!(who(&fwd), ["b1", "c2", "b2"]);
        assert!(fwd.more_older && !fwd.more_newer);
        assert!(fwd.errors.is_empty(), "{:?}", fwd.errors);
    }

    /// A file whose `_BOOT_ID` list can't be read is sought as journalctl
    /// 259 seeks it, and the error is reported.
    #[test]
    fn an_unreadable_boot_list_falls_back_to_journalctls_seek() {
        each_format(an_unreadable_boot_list_falls_back_to_journalctls_seek_in);
    }

    fn an_unreadable_boot_list_falls_back_to_journalctls_seek_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let mut bytes = std::fs::read(b.path()).expect("read");
        let table = usize::try_from(u64::from_le_bytes(bytes[120..128].try_into().expect("8")))
            .expect("small");
        bytes[table..table + 48].fill(0xff);
        std::fs::write(b.path(), &bytes).expect("write");
        let j = files_on(&release("26.04"), &[a.path(), b.path()]);
        let q = Query::field(&name("U"), b"x");
        let all = tail(&j, &q, 4);
        let a2 = all
            .records
            .iter()
            .find(|r| r.field(&name("WHO")) == Some(b"a2"));
        let fwd = j.page(
            &q,
            &Position::After(a2.expect("a2").cursor()),
            2,
            &Limits::default(),
        );
        assert_eq!(who(&fwd), ["b2"], "259's realtime landing");
        assert_eq!(fwd.errors.shown.len(), 1, "{:?}", fwd.errors);
    }

    /// The cost of sb's forward seek against journalctl 259's, both stated
    /// exactly: on the decision-11 fixture, a page after a2 costs 68 preads
    /// on 22.04 (259's seek, showing b2) and 72 on 24.04, which reads boot
    /// 0xb's file's `_BOOT_ID` list (the field table's bucket, the FIELD
    /// object and its one value: 3 preads, as
    /// `a_fields_values_are_its_data_objects_newest_first` counts them),
    /// needs no bisect there (0xb starts at its first entry), and shows b1
    /// as well. Boot 0xa's file shares the cursor's seqnum id, so it reads
    /// no `_BOOT_ID` list. 26.04 costs 8 more: the boot table reads each
    /// file's tail entry's four items for a `_MACHINE_ID=` it doesn't have
    /// (`the_tail_entrys_machine_id_costs_a_pread_per_item_read`). Each
    /// file's second header read, its newest counted entry, `U=x` looked up
    /// again and its list bisected for that entry add 11 per file, 22 on every row
    /// (journal spec decision 13).
    #[test]
    fn the_boot_seek_reads_the_files_boots() {
        each_format(the_boot_seek_reads_the_files_boots_in);
    }

    fn the_boot_seek_reads_the_files_boots_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let paths = [a.path(), b.path()];
        let q = Query::field(&name("U"), b"x");
        let cost = |v: &str| {
            let j = files_on(&release(v), &paths);
            let all = tail(&j, &q, 4);
            let a2 = all
                .records
                .iter()
                .find(|r| r.field(&name("WHO")) == Some(b"a2"));
            let at = Position::After(a2.expect("a2").cursor());
            let before = crate::testing::counts(&j).preads;
            let page = j.page(&q, &at, 2, &Limits::default());
            (who(&page), crate::testing::counts(&j).preads - before)
        };
        let (old, old_cost) = cost("22.04");
        let (new, new_cost) = cost("24.04");
        let (named, named_cost) = cost("26.04");
        assert_eq!(
            (old, new),
            (
                vec!["b2".to_owned()],
                vec!["b1".to_owned(), "b2".to_owned()]
            )
        );
        assert_eq!(named, ["b1", "b2"]);
        assert_eq!((old_cost, new_cost, named_cost), (68, 72, 80));
    }

    /// On every row, a page of one after an entry of the tail is the next
    /// entry of the tail and a page of one before it the previous one: the
    /// tail, forward and back pages agree, and paging either way from any
    /// record always moves on.
    ///
    /// Two layouts: the decision-11 fixture, and boot 0xa's file with a
    /// second file holding c1 (boot 0xc, in no file's tail, at realtime
    /// 400) then a3 (boot 0xa, monotonic 30, realtime 350). There, after
    /// a2, journalctl 259's monotonic seek in the cursor's boot would land
    /// on a3 and pass c1; sb seeks that file per boot on 24.04 and 26.04
    /// because it holds two. 22.04 keeps journalctl's seek, unchanged, and
    /// does land on a3 there.
    #[test]
    fn forward_back_and_tail_pages_agree() {
        each_format(forward_back_and_tail_pages_agree_in);
    }

    fn forward_back_and_tail_pages_agree_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let mixed = boot_file_in(
            format,
            2,
            9,
            &[("c1", 0xc, 1, 400), ("a3", 0xa, 30, 350)],
            true,
        );
        let q = Query::field(&name("U"), b"x");
        let all = ["22.04", "24.04", "26.04"];
        let layouts = [
            ([a.path(), b.path()], &all[..]),
            ([a.path(), mixed.path()], &all[1..]),
        ];
        let on_22 = files_on(&release("22.04"), &[a.path(), mixed.path()]);
        let tail_22 = tail(&on_22, &q, 4);
        let a2 = tail_22
            .records
            .iter()
            .find(|r| r.field(&name("WHO")) == Some(b"a2"));
        let at_a2 = a2.expect("a2").cursor();
        let landed = on_22.page(&q, &Position::After(at_a2), 1, &Limits::default());
        assert_eq!(who(&landed), ["a3"], "22.04: journalctl's landing");
        for (paths, v) in layouts
            .iter()
            .flat_map(|(p, rows)| rows.iter().map(move |v| (p, *v)))
        {
            let j = files_on(&release(v), paths);
            let all = tail(&j, &q, 4);
            let order = who(&all);
            for (i, r) in all.records.iter().enumerate() {
                let one = |at: Position| who(&j.page(&q, &at, 1, &Limits::default()));
                let next: Vec<String> = order.get(i + 1).cloned().into_iter().collect();
                let prev: Vec<String> = i
                    .checked_sub(1)
                    .map(|p| order[p].clone())
                    .into_iter()
                    .collect();
                assert_eq!(
                    one(Position::After(r.cursor())),
                    next,
                    "{v} after {}",
                    order[i]
                );
                assert_eq!(
                    one(Position::Before(r.cursor())),
                    prev,
                    "{v} before {}",
                    order[i]
                );
            }
        }
    }

    /// The release's order can cycle, and a forward page then does too:
    /// a (boot 0xa, realtime 300) comes before b (boot 0xb, 100) by the
    /// table (0xa's newest tail, a non-matching entry with the higher
    /// monotonic time, is at 50; 0xb's at 100); b before c (boot 0xc, in no
    /// table, 200) by realtime; and c before a by realtime. Paging forward
    /// one entry at a time from b goes c, a, b, c, ... for ever: a cursor
    /// alone can't break it. (journalctl's own order cycles the same way.)
    #[test]
    fn forward_paging_cycles_where_the_order_does() {
        each_format(forward_paging_cycles_where_the_order_does_in);
    }

    fn forward_paging_cycles_where_the_order_does_in(format: Format) {
        let file_a = boot_file_in(format, 1, 9, &[("a", 0xa, 10, 300)], true);
        let a_tail = boot_file_in(format, 2, 9, &[("a0", 0xa, 20, 50)], false);
        let file_b = boot_file_in(format, 3, 9, &[("b", 0xb, 1, 100)], true);
        let file_c = boot_file_in(format, 4, 9, &[("c", 0xc, 1, 200)], true);
        let mut bytes = std::fs::read(file_c.path()).expect("read");
        bytes[264..272].fill(0);
        std::fs::write(file_c.path(), &bytes).expect("write");
        let j = files_on(
            &release("26.04"),
            &[file_a.path(), a_tail.path(), file_b.path(), file_c.path()],
        );
        let q = Query::field(&name("U"), b"x");
        let all = tail(&j, &q, 4);
        let at_b = all
            .records
            .iter()
            .find(|r| r.field(&name("WHO")) == Some(b"b"));
        let mut page = j.page(
            &q,
            &Position::After(at_b.expect("b").cursor()),
            1,
            &Limits::default(),
        );
        let mut shown = Vec::new();
        for _ in 0..6 {
            let Some(r) = page.records.first() else { break };
            shown.extend(who(&page));
            page = j.page(&q, &Position::After(r.cursor()), 1, &Limits::default());
        }
        assert_eq!(shown, ["c", "a", "b", "c", "a", "b"]);
    }

    /// One boot, 0xc, in two files of two seqnum spaces, its realtime
    /// clock set back between them (journald restarted on a 249 host, or
    /// journal-remote): b1 and b2 at monotonic 1 and 2 µs, realtimes 300
    /// and 400 µs; then x1 to x4 at monotonic 3 to 6, realtimes 10 to 40.
    /// The matrix's `own boot` row has the same shape (the hardening
    /// plan's decision 33).
    const OWN_BOOT_B: Rows<'static> = &[("b1", 0xc, 1, 300), ("b2", 0xc, 2, 400)];
    const OWN_BOOT_X: Rows<'static> = &[
        ("x1", 0xc, 3, 10),
        ("x2", 0xc, 4, 20),
        ("x3", 0xc, 5, 30),
        ("x4", 0xc, 6, 40),
    ];

    /// fwd-M1, decision 33: after an entry of `OWN_BOOT_X`, file b's
    /// matching entries all lie before the cursor by monotonic time, and
    /// the file is sought per boot, so b1 and b2 aren't shown, nor the
    /// cursor's own entry; paging forward one at a time from b1 ends at
    /// the last. As journalctl 249 and 259.9 show it; 255 shows b1 and
    /// b2, then the cursor's entry (Difference 21). On every row, in both
    /// file orders, with the query and without; and with file b also
    /// holding b3, at monotonic 10 and realtime 500, which doesn't match
    /// `U=x`: the query's seek then finds b3's boot position but no
    /// matching entry from it, and falls back to realtime as well.
    #[test]
    fn a_forward_page_on_the_cursors_own_boot_shows_only_later_entries() {
        each_format(a_forward_page_on_the_cursors_own_boot_shows_only_later_entries_in);
    }

    fn a_forward_page_on_the_cursors_own_boot_shows_only_later_entries_in(format: Format) {
        let u = Some(b"U=x".as_slice());
        let with_b3: Rows<'static> = &[
            ("b1", 0xc, 1, 300),
            ("b2", 0xc, 2, 400),
            ("b3", 0xc, 10, 500),
        ];
        let x = boot_file_in(format, 1, 9, OWN_BOOT_X, true);
        let bs = [
            (boot_file_in(format, 2, 9, OWN_BOOT_B, true), false),
            (
                boot_file_naming_in(format, 2, 9, with_b3, false, &[u, u, None]),
                true,
            ),
        ];
        let matching = Query::field(&name("U"), b"x");
        let all = crate::testing::tools::every_entry();
        for (b, b3) in &bs {
            let queries = [
                (&matching, vec!["b1", "b2", "x1", "x2", "x3", "x4"]),
                (
                    &all,
                    if *b3 {
                        vec!["b1", "b2", "x1", "x2", "x3", "x4", "b3"]
                    } else {
                        vec!["b1", "b2", "x1", "x2", "x3", "x4"]
                    },
                ),
            ];
            let layouts = [[b.path(), x.path()], [x.path(), b.path()]];
            for v in ["22.04", "24.04", "26.04"] {
                for (paths, (q, order)) in layouts
                    .iter()
                    .flat_map(|p| queries.iter().map(move |q| (p, q)))
                {
                    let at = format!("{v}, b3 {b3}, {} entries", order.len());
                    let j = files_on(&release(v), paths);
                    let whole = tail(&j, q, 10);
                    assert_eq!(who(&whole), *order, "{at}");
                    for (i, r) in whole.records.iter().enumerate() {
                        let fwd = j.page(q, &Position::After(r.cursor()), 10, &Limits::default());
                        assert_eq!(who(&fwd), order[i + 1..], "{at}, after {}", order[i]);
                    }
                    let mut shown = Vec::new();
                    let mut cursor = whole.records[0].cursor();
                    // One page per entry and one more: a loop fails here.
                    for _ in 0..order.len() {
                        let page = j.page(q, &Position::After(cursor), 1, &Limits::default());
                        let Some(r) = page.records.first() else { break };
                        shown.extend(who(&page));
                        cursor = r.cursor();
                    }
                    assert_eq!(shown, order[1..], "{at}, one at a time");
                }
            }
        }
    }

    /// The property probe's sixth mode (fwd-M1): one boot written to
    /// several files of their own seqnum spaces, each file's monotonic
    /// times rising and its realtimes anywhere (clocks set back and
    /// forward), each entry matching `U=x` or not. One boot orders by
    /// monotonic time alone, so on every row a page after any matching
    /// entry is exactly the matching entries with a later monotonic time,
    /// in that order, and a page before it the earlier ones. 300 seeded
    /// cases per format.
    #[test]
    fn pages_over_one_boot_in_several_seqnum_spaces_follow_its_monotonic_order() {
        each_format(pages_over_one_boot_in_several_seqnum_spaces_follow_its_monotonic_order_in);
    }

    fn pages_over_one_boot_in_several_seqnum_spaces_follow_its_monotonic_order_in(format: Format) {
        let q = Query::field(&name("U"), b"x");
        let u = b"U=x".as_slice();
        for seed in 0..300 {
            let mut rng = fastrand::Rng::with_seed(seed);
            let n_files = rng.usize(2..=3);
            let n = rng.u64(u64::try_from(n_files).expect("small")..=8);
            // Distinct monotonic times 1..=n, dealt to the files (each gets
            // one at least), each file's in rising order, as journald writes
            // them; two entries in three match.
            let mut monotonic: Vec<u64> = (1..=n).collect();
            rng.shuffle(&mut monotonic);
            let mut dealt: Vec<Vec<u64>> = vec![Vec::new(); n_files];
            for (k, &m) in monotonic.iter().enumerate() {
                let to = if k < n_files { k } else { rng.usize(..n_files) };
                dealt[to].push(m);
            }
            let matches: Vec<bool> = (0..=n).map(|_| rng.u8(..3) < 2).collect();
            let names: Vec<String> = (0..=n).map(|m| format!("m{m}")).collect();
            let files: Vec<tempfile::NamedTempFile> = dealt
                .iter_mut()
                .zip(1u8..)
                .map(|(ms, seqnum_id)| {
                    ms.sort_unstable();
                    let rows: Vec<(&str, u8, u64, u64)> = ms
                        .iter()
                        .map(|&m| {
                            (
                                names[usize::try_from(m).expect("small")].as_str(),
                                0xc,
                                m,
                                rng.u64(1..1000),
                            )
                        })
                        .collect();
                    let fields: Vec<Option<&[u8]>> = ms
                        .iter()
                        .map(|&m| matches[usize::try_from(m).expect("small")].then_some(u))
                        .collect();
                    boot_file_naming_in(format, seqnum_id, 9, &rows, false, &fields)
                })
                .collect();
            let paths: Vec<&Path> = files.iter().map(tempfile::NamedTempFile::path).collect();
            let order: Vec<String> = (1..=n)
                .filter(|&m| matches[usize::try_from(m).expect("small")])
                .map(|m| format!("m{m}"))
                .collect();
            for v in ["22.04", "24.04", "26.04"] {
                let j = files_on(&release(v), &paths);
                let whole = tail(&j, &q, 10);
                assert_eq!(who(&whole), order, "seed {seed}, {v}");
                for (i, r) in whole.records.iter().enumerate() {
                    let page = |at: Position| who(&j.page(&q, &at, 10, &Limits::default()));
                    assert_eq!(
                        page(Position::After(r.cursor())),
                        order[i + 1..],
                        "seed {seed}, {v}, after {}",
                        order[i]
                    );
                    assert_eq!(
                        page(Position::Before(r.cursor())),
                        order[..i],
                        "seed {seed}, {v}, before {}",
                        order[i]
                    );
                }
            }
        }
    }

    /// A per-boot page with nothing older: from a cursor of another seqnum
    /// space at the very start of boot 0xa (monotonic and realtime 0), the
    /// page is every entry, and `more_older` is false.
    #[test]
    fn a_per_boot_page_from_before_everything_has_nothing_older() {
        each_format(a_per_boot_page_from_before_everything_has_nothing_older_in);
    }

    fn a_per_boot_page_from_before_everything_has_nothing_older_in(format: Format) {
        let (a, b) = (
            boot_file_in(format, 1, 9, BOOT_A, true),
            boot_file_in(format, 2, 9, BOOT_B, true),
        );
        let j = files_on(&release("26.04"), &[a.path(), b.path()]);
        let q = Query::field(&name("U"), b"x");
        let start = Cursor {
            seqnum_id: [9; 16],
            seqnum: 1,
            boot_id: [0xa; 16],
            monotonic: 0,
            realtime: 0,
            xor_hash: 0,
        };
        let page = j.page(&q, &Position::After(start), 10, &Limits::default());
        assert_eq!(who(&page), ["a1", "a2", "b1", "b2"]);
        assert_eq!((page.more_older, page.more_newer), (false, false));
    }

    /// The boot table's cost, on `crafted::journal(200)` as
    /// `the_preads_of_a_tail_a_page_back_and_a_page_forward_are_bounded`
    /// pages it: nothing on any row while the header's tail is the newest
    /// entry, which the page reads anyway for its end (`walk::lasts`); one
    /// pread per page and file on 24.04 and 26.04 when it names another
    /// entry; none on 22.04, which has no table.
    #[test]
    fn the_boot_table_reads_at_most_one_entry_per_file() {
        let costs = |release: &Release, tail_entry: usize| {
            let (mut crafted, layout) = crafted::journal(200);
            crafted.set(264, layout.entries[tail_entry]);
            let file = crafted.file();
            let j = files_on(release, &[file.path()]);
            let q = Query::field(&name("U"), b"x");
            let limits = Limits::default();
            let preads = || crate::testing::counts(&j).preads;
            let first = tail(&j, &q, 10);
            let after_tail = preads();
            let back = j.page(
                &q,
                &Position::Before(first.records[0].cursor()),
                10,
                &limits,
            );
            let after_back = preads();
            let fwd = j.page(&q, &Position::After(back.records[0].cursor()), 10, &limits);
            assert_eq!(fwd.records.len(), 10);
            (after_tail, after_back - after_tail, preads() - after_back)
        };
        for v in ["22.04", "24.04", "26.04"] {
            assert_eq!(costs(&release(v), 199), (60, 70, 106), "{v}");
        }
        assert_eq!(costs(&release("22.04"), 5), (60, 70, 106));
        for v in ["24.04", "26.04"] {
            assert_eq!(costs(&release(v), 5), (61, 71, 107), "{v}");
        }
    }
}
