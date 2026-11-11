//! One journal file, read with `pread` only (no mmap, so a file truncated
//! under us is an I/O error, never SIGBUS). Layouts are systemd v255's
//! src/libsystemd/sd-journal/journal-def.h; the rules are
//! <https://systemd.io/JOURNAL_FILE_FORMAT/>. Every offset and size read from
//! the file is checked before use: a file journald is still writing may be
//! inconsistent for a moment ("Concurrency").
//!
//! Why our own reader (ADR-013, from proto-journal's evidence): sdjournal
//! skips `.journal~` files silently, can lose a follow on a rotation race
//! and carries a `RustSec` advisory through ruzstd; netdata's
//! systemd-journal-sdk maps files with mmap and a SIGBUS handler and links
//! the C zstd library; the libsystemd `systemd` crate can't link into the
//! static musl binary. Fields are read with `from_le_bytes` at checked
//! offsets rather than zerocopy or binrw: neither is in the tree, and
//! their derived casts sit badly with `forbid(unsafe_code)`.
//!
//! Re-exported by `testing` (the feature), for the tools (journal spec,
//! Project Structure); the library's own API is the crate root's.

use crate::open::{Handle, Opener};
use crate::{OverLimit, ReadError, ReadErrorKind, hash};
use std::borrow::Cow;
use std::cell::Cell;
use std::io::Read;
use std::path::{Path, PathBuf};

type Result<T, E = ReadError> = std::result::Result<T, E>;

pub const SIGNATURE: &[u8; 8] = b"LPKSHHRH";

pub const INCOMPAT_XZ: u32 = 1 << 0;
pub const INCOMPAT_LZ4: u32 = 1 << 1;
pub const INCOMPAT_KEYED_HASH: u32 = 1 << 2;
pub const INCOMPAT_ZSTD: u32 = 1 << 3;
pub const INCOMPAT_COMPACT: u32 = 1 << 4;

/// The incompatible flags the supported releases' journald writes
/// (22.04: systemd 249, 24.04: 255, 26.04: 259). An unknown bit is a hard
/// error naming the file, never a silent skip: a new LTS adds its flags
/// here deliberately.
pub const INCOMPAT_KNOWN: u32 =
    INCOMPAT_XZ | INCOMPAT_LZ4 | INCOMPAT_KEYED_HASH | INCOMPAT_ZSTD | INCOMPAT_COMPACT;

/// `STATE_ARCHIVED` (journal-def.h): `Header::archived`.
const STATE_ARCHIVED: u8 = 2;
/// `HEADER_COMPATIBLE_TAIL_ENTRY_BOOT_ID` (journal-def.h): the header's
/// `tail_entry_boot_id` is written only when an entry is appended, so it
/// goes with `tail_entry_monotonic`.
pub(crate) const COMPATIBLE_TAIL_ENTRY_BOOT_ID: u32 = 1 << 1;

pub(crate) const OBJECT_DATA: u8 = 1;
const OBJECT_FIELD: u8 = 2;
pub(crate) const OBJECT_ENTRY: u8 = 3;
pub(crate) const OBJECT_ENTRY_ARRAY: u8 = 6;

pub(crate) const COMPRESSED_XZ: u8 = 1 << 0;
pub(crate) const COMPRESSED_LZ4: u8 = 1 << 1;
pub(crate) const COMPRESSED_ZSTD: u8 = 1 << 2;

/// systemd's own limit on one DATA object (`DATA_SIZE_MAX` in journal-def.h,
/// main branch: 768 MiB, sized for coredumps stored in the journal; the
/// smaller values there are for fuzzing builds). A larger decompressed
/// payload is corruption or a bomb.
pub(crate) const DATA_SIZE_MAX: usize = 768 * 1024 * 1024;

/// systemd's limit on one ENTRY object (`ENTRY_SIZE_MAX`, same header:
/// 770 MiB). An entry holds only item offsets, so this is a loose bound;
/// the arena bound in `object` is the tight one.
const ENTRY_SIZE_MAX: u64 = 770 * 1024 * 1024;

/// Our own bound on xz decoder memory, not a journald value: journald
/// compresses with explicit LZMA2 options and a 1 MiB dictionary
/// (compress.c, `compress_blob_xz`), so 64 MiB is ample and still bounds what
/// a corrupt stream header can claim (journal spec, Difference 6).
const XZ_MEM_LIMIT_KB: u32 = 64 * 1024;

/// The largest zstd window decoded: ruzstd's own default maximum
/// (`MAXIMUM_ALLOWED_WINDOW_SIZE`, 128 MiB), kept when a frame's content
/// would allow more (journal spec, Difference 5). journald's one-shot
/// `ZSTD_compress` frames have windows of a few MiB at most.
const ZSTD_WINDOW_MAX: u64 = 128 << 20;

/// Bytes read at once for an entry or DATA object: one `pread` covers
/// a typical one whole (an entry of 22 regular items is 416 bytes), and
/// a larger one costs a second read for the rest.
const READ_GUESS: u64 = 1024;

pub(crate) const OBJECT_HEADER: u64 = 16;
pub(crate) const ENTRY_HEADER: u64 = 64;
pub(crate) const DATA_HEADER: u64 = 64;
const DATA_HEADER_COMPACT: u64 = 72;
/// An entry array's head: its object header and next link.
const ENTRY_ARRAY_HEADER: u64 = 24;

/// The header fields this reader uses (journal-def.h `struct Header`).
#[derive(Debug, Clone)]
pub struct Header {
    /// `compatible_flags`: only `COMPATIBLE_TAIL_ENTRY_BOOT_ID` is read.
    pub compatible: u32,
    pub incompatible: u32,
    /// `STATE_OFFLINE`, `STATE_ONLINE` or `STATE_ARCHIVED` ("Header").
    pub state: u8,
    pub file_id: [u8; 16],
    /// The machine that wrote the file: the boot table compares boots of
    /// one machine only (`JournalFile::tail`).
    pub machine_id: [u8; 16],
    /// The boot id of the newest entry, kept with `tail_entry_monotonic`
    /// only under `COMPATIBLE_TAIL_ENTRY_BOOT_ID`.
    pub tail_entry_boot_id: [u8; 16],
    pub seqnum_id: [u8; 16],
    /// `header_size`: where the arena starts.
    pub size: u64,
    pub arena_size: u64,
    pub data_hash_table_offset: u64,
    pub data_hash_table_size: u64,
    pub field_hash_table_offset: u64,
    pub field_hash_table_size: u64,
    /// The last object appended.
    pub tail_object_offset: u64,
    pub n_entries: u64,
    pub entry_array_offset: u64,
    pub tail_entry_realtime: u64,
    pub tail_entry_monotonic: u64,
    /// The newest entry, `None` in a header too short to hold the field
    /// (journald before 254: `JOURNAL_HEADER_CONTAINS`, journal-file.h).
    pub tail_entry_offset: Option<u64>,
}

impl Header {
    #[must_use]
    pub fn compact(&self) -> bool {
        self.incompatible & INCOMPAT_COMPACT != 0
    }
    #[must_use]
    pub fn keyed(&self) -> bool {
        self.incompatible & INCOMPAT_KEYED_HASH != 0
    }
    /// Archived files are never written again ("Header": a writer only
    /// appends to a file in `STATE_OFFLINE`, after setting it online).
    #[must_use]
    pub fn archived(&self) -> bool {
        self.state == STATE_ARCHIVED
    }
}

/// A file's newest entry for sd-journal's boot table (`JournalFile::tail`):
/// its boot, times and machine id (the header's, or the entry's own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tail {
    pub(crate) boot_id: [u8; 16],
    pub(crate) monotonic: u64,
    pub(crate) realtime: u64,
    pub(crate) machine_id: [u8; 16],
}

/// Where a tail's times come from (`JournalFile::tail`).
#[derive(Debug)]
enum TailSource<'a> {
    /// An entry, which on 259.9 also names the machine.
    Entry(Cow<'a, Entry>),
    /// An archived file's header.
    Header,
}

/// The field a tail entry names its machine by, and its payload's
/// length: the name, `=` and 32 hex digits.
const MACHINE_ID_FIELD: &[u8] = b"_MACHINE_ID=";
const MACHINE_ID_PAYLOAD: usize = MACHINE_ID_FIELD.len() + 32;

/// What one item of a tail entry says of the machine
/// (`JournalFile::entry_machine_id`).
#[derive(Debug)]
enum MachineItem {
    /// Another field.
    Other,
    /// A `_MACHINE_ID=` field: its id, `None` when it isn't a valid one.
    Named(Option<[u8; 16]>),
}

/// One entry's fixed fields and the offsets of its DATA objects.
#[derive(Debug, Clone)]
pub struct Entry {
    pub seqnum: u64,
    pub realtime: u64,
    pub monotonic: u64,
    pub boot_id: [u8; 16],
    pub xor_hash: u64,
    pub items: Vec<u64>,
}

/// A DATA object's entry list: the first entry in-line, then a chain of
/// entry arrays (journal-def.h `DataObject`).
#[derive(Debug, Clone, Copy)]
pub struct DataRef {
    pub offset: u64,
    pub n_entries: u64,
    pub entry_offset: u64,
    pub entry_array_offset: u64,
}

#[derive(Debug)]
pub struct JournalFile {
    pub path: PathBuf,
    /// The open file's device and inode: a page's end names the files it
    /// read by it, and a follow keeps the file by it across a rename
    /// (rotation) and past its deletion (vacuum).
    pub inode: (u64, u64),
    file: Handle,
    pub header: Header,
    /// Where objects may lie: size + `arena_size`, capped by the
    /// file's length. It grows when a read reaches past it (`within`).
    limit: Cell<u64>,
}

impl JournalFile {
    /// Open the file at `path` and read its header, with an opener of its
    /// own (the tools and tests; a page opens through its journal's).
    ///
    /// # Errors
    ///
    /// As `open_with`.
    #[cfg(any(test, feature = "testing"))]
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, &Opener::default())
    }

    /// Open the file at `path` through `opener` (safely: `crate::open`) and
    /// read its header.
    ///
    /// # Errors
    ///
    /// The file can't be opened (a symlink, not a regular file, or the
    /// system's error) or read, isn't a journal file, has an incompatible
    /// flag this reader doesn't know, or a bad header size.
    pub(crate) fn open_with(path: &Path, opener: &Opener) -> Result<Self> {
        let file = opener.open(path).map_err(|kind| ReadError {
            path: Some(path.to_path_buf()),
            kind,
        })?;
        let mut jf = Self {
            path: path.to_path_buf(),
            inode: file.inode(),
            file,
            header: empty_header(),
            limit: Cell::new(0),
        };
        jf.refresh()?;
        Ok(jf)
    }

    /// Re-read the header: journald grows the file and links in new
    /// entries while we read.
    ///
    /// # Errors
    ///
    /// As `open`, for the header read again.
    pub fn refresh(&mut self) -> Result<()> {
        let mut buf = [0u8; 272];
        let n = read_up_to(&self.file, &mut buf, 0).map_err(|e| self.io("read the header", e))?;
        // The length after the header: journald grows the file before it
        // raises arena_size and links objects into the new space
        // (journal_file_allocate), so a length read first could be older
        // than the header and cut off entries it counts.
        let len = self
            .file
            .metadata()
            .map_err(|e| self.io("metadata", e))?
            .len();
        self.check(n >= 208, || "header too short".into())?;
        self.check(&buf[0..8] == SIGNATURE, || "not a journal file".into())?;
        let incompatible = le32(&buf, 12);
        self.check(incompatible & !INCOMPAT_KNOWN == 0, || {
            format!(
                "unsupported journal format (incompatible flags {incompatible:#x}); this sb release doesn't read it"
            )
        })?;
        let size = le64(&buf, 88);
        let h = Header {
            compatible: le32(&buf, 8),
            incompatible,
            state: buf[16],
            file_id: id(&buf, 24),
            machine_id: id(&buf, 40),
            tail_entry_boot_id: id(&buf, 56),
            seqnum_id: id(&buf, 72),
            size,
            arena_size: le64(&buf, 96),
            data_hash_table_offset: le64(&buf, 104),
            data_hash_table_size: le64(&buf, 112),
            field_hash_table_offset: le64(&buf, 120),
            field_hash_table_size: le64(&buf, 128),
            tail_object_offset: le64(&buf, 136),
            n_entries: le64(&buf, 152),
            entry_array_offset: le64(&buf, 176),
            tail_entry_realtime: le64(&buf, 192),
            tail_entry_monotonic: le64(&buf, 200),
            // `JOURNAL_HEADER_CONTAINS(h, tail_entry_offset)`: the field
            // ends at 272, the header's whole size.
            tail_entry_offset: (size >= 272).then(|| le64(&buf, 264)),
        };
        self.check(h.size >= 208 && h.size <= len, || "bad header size".into())?;
        self.limit.set(h.size.saturating_add(h.arena_size).min(len));
        self.header = h;
        Ok(())
    }

    /// The open file's metadata (its link count tells the follow a
    /// deleted file).
    ///
    /// # Errors
    ///
    /// The system's, from `fstat`.
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// This file's error of `kind`.
    pub(crate) fn error(&self, kind: ReadErrorKind) -> ReadError {
        ReadError {
            path: Some(self.path.clone()),
            kind,
        }
    }

    fn io(&self, context: &str, source: std::io::Error) -> ReadError {
        self.error(ReadErrorKind::Io {
            context: context.to_owned(),
            source,
        })
    }

    /// `Ok` when `ok`, else corruption with the message `what` gives.
    fn check(&self, ok: bool, what: impl FnOnce() -> String) -> Result<()> {
        if ok {
            Ok(())
        } else {
            Err(self.error(ReadErrorKind::Corrupt(what())))
        }
    }

    fn to_usize(&self, v: u64) -> Result<usize> {
        usize_of(v).map_err(|k| self.error(k))
    }

    /// Whether `len` bytes at `offset` lie in the arena. journald goes on
    /// appending while we read, and links read live (an entry list, a
    /// hash chain) can name objects past the arena as it was when the
    /// header was read: before refusing, re-read the arena size and the
    /// file's length once and grow the limit, as sd-journal re-stats in
    /// `journal_file_move_to` before returning -EADDRNOTAVAIL (journal-file.c,
    /// v255).
    fn within(&self, offset: u64, len: u64) -> bool {
        let fits = |limit: u64| {
            offset >= self.header.size && offset.checked_add(len).is_some_and(|end| end <= limit)
        };
        if fits(self.limit.get()) {
            return true;
        }
        // Growth can't help an offset inside the header.
        if offset < self.header.size {
            return false;
        }
        let mut arena = [0u8; 8];
        let grown = self.file.read_exact_at(&mut arena, 96).is_ok()
            && self.file.metadata().is_ok_and(|m| {
                let limit = self
                    .header
                    .size
                    .saturating_add(u64::from_le_bytes(arena))
                    .min(m.len());
                if limit > self.limit.get() {
                    self.limit.set(limit);
                }
                true
            });
        grown && fits(self.limit.get())
    }

    /// Read `len` bytes at `offset`, inside the arena.
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.check(self.within(offset, len), || {
            format!("offset {offset:#x}+{len} outside the arena")
        })?;
        let mut buf = vec![0u8; self.to_usize(len)?];
        self.file
            .read_exact_at(&mut buf, offset)
            .map_err(|e| self.io(&format!("read at {offset:#x}"), e))?;
        Ok(buf)
    }

    /// The start of an object: its flags, its full size and its first
    /// `min(size, guess)` bytes, from one read, checked for alignment,
    /// type and bounds.
    fn object_head(&self, offset: u64, want: u8, guess: u64) -> Result<(u8, u64, Vec<u8>)> {
        self.check(offset.is_multiple_of(8), || {
            format!("unaligned object {offset:#x}")
        })?;
        let room = self.limit.get().saturating_sub(offset);
        let mut b = self.read(offset, guess.min(room).max(OBJECT_HEADER))?;
        let (kind, flags, size) = (b[0], b[1], le64(&b, 8));
        self.check(kind == want, || {
            format!("object {offset:#x} is type {kind}, wanted {want}")
        })?;
        self.check(size >= OBJECT_HEADER && self.within(offset, size), || {
            format!("object {offset:#x} has a bad size")
        })?;
        b.truncate(self.to_usize(size)?);
        Ok((flags, size, b))
    }

    /// The rest of an object whose head `object_head` read.
    fn object_rest(&self, offset: u64, size: u64, b: &mut Vec<u8>) -> Result<()> {
        let have = b.len() as u64;
        if size > have {
            b.extend_from_slice(&self.read(offset + have, size - have)?);
        }
        Ok(())
    }

    /// The entry at `offset`.
    ///
    /// # Errors
    ///
    /// The object isn't a well-formed entry inside the arena, or can't
    /// be read.
    pub fn entry(&self, offset: u64) -> Result<Entry> {
        self.entry_before(offset, u64::MAX)
    }

    /// The file's newest entry as sd-journal's boot table reads it (v255
    /// and v259 sd-journal.c `journal_file_read_tail_timestamp`, the same
    /// steps in both): the entry `tail_entry_offset` names, or in a header
    /// without that field the tail object if it is an entry; failing that,
    /// in an archived file with `COMPATIBLE_TAIL_ENTRY_BOOT_ID`, the
    /// header's `tail_entry_*`; else the main list's newest entry
    /// (`journal_file_next_entry(f, 0, DIRECTION_UP)`), which `last` is,
    /// as the page read it (`walk::lasts`; `None` when the file has none
    /// or it couldn't be read, where systemd's read fails too). A tail
    /// offset of zero, or a monotonic time ahead of the realtime, is no
    /// tail (`-ENODATA`). The machine id is the header's; on 259.9 the
    /// table takes `tail_machine_id`'s where it names one.
    ///
    /// The cost: nothing when the tail is `last`, as journald 254 and
    /// later make it (`journal_file_link_entry` sets `tail_entry_offset`
    /// after linking the entry into the main list); otherwise one entry
    /// read, usually one pread. Corrupt files aside, where this reader's
    /// object checks and systemd's `check_object` don't refuse quite the
    /// same objects (as Difference 19), the result is systemd's.
    pub(crate) fn tail(&self, last: Option<&(u64, Entry)>) -> Option<Tail> {
        let h = &self.header;
        let (boot_id, monotonic, realtime) = match self.tail_source(last)? {
            TailSource::Entry(e) => (e.boot_id, e.monotonic, e.realtime),
            TailSource::Header => (
                h.tail_entry_boot_id,
                h.tail_entry_monotonic,
                h.tail_entry_realtime,
            ),
        };
        (monotonic <= realtime).then_some(Tail {
            boot_id,
            monotonic,
            realtime,
            machine_id: h.machine_id,
        })
    }

    /// Where `tail` takes its times from; `None` for no tail offset.
    fn tail_source<'a>(&self, last: Option<&'a (u64, Entry)>) -> Option<TailSource<'a>> {
        let h = &self.header;
        let offset = h.tail_entry_offset.unwrap_or(h.tail_object_offset);
        if offset == 0 {
            return None;
        }
        let read = match last {
            Some((o, e)) if *o == offset => Some(Cow::Borrowed(e)),
            // A tail object that isn't an entry is refused here, as
            // systemd checks `o->object.type == OBJECT_ENTRY`.
            _ => self.entry(offset).ok().map(Cow::Owned),
        };
        Some(match read {
            Some(e) => TailSource::Entry(e),
            None if h.compatible & COMPATIBLE_TAIL_ENTRY_BOOT_ID != 0 && h.archived() => {
                TailSource::Header
            }
            None => TailSource::Entry(Cow::Borrowed(&last?.1)),
        })
    }

    /// The machine id the entry that gave `tail` its times names, as
    /// systemd v259.9 reads it for the boot table: systemd's v259-stable
    /// commit 2d8b0b6a0aaf ("sd-journal: prefer machine ID from the tail
    /// entry", from upstream ae940730c6f2, issue #34169; first tagged in
    /// v259.9, not in v255), src/libsystemd/sd-journal/sd-journal.c
    /// `journal_file_entry_get_machine_id`, called by
    /// `journal_file_read_tail_timestamp` once the times are taken. `None`,
    /// so the header's id stays, when the times came from an archived
    /// file's header ("header-only fallback"), or the entry names no valid
    /// machine (`entry_machine_id`). For a file `tail` gave a tail; the
    /// boot table asks only for each boot's newest file
    /// (`BootOrder::name_machines`).
    ///
    /// The cost: `entry_machine_id`'s, plus the entry read again when it
    /// isn't `last` (a file whose header names another tail).
    pub(crate) fn tail_machine_id(&self, last: Option<&(u64, Entry)>) -> Option<[u8; 16]> {
        match self.tail_source(last)? {
            TailSource::Entry(e) => self.entry_machine_id(&e),
            TailSource::Header => None,
        }
    }

    /// The machine id entry `e` names, as `journal_file_entry_get_machine_id`
    /// reads it: it walks the entry's items in order and takes the first
    /// DATA object whose payload is `_MACHINE_ID=…`
    /// (`journal_file_data_payload` with the field, decompressed as far as
    /// the name); its value must be 32 characters (else `-EBADMSG`) of an
    /// id that isn't null (`id128_from_string_nonzero`). An item it can't
    /// read as a DATA object (`-EBADMSG`, `-EADDRNOTAVAIL`) is passed over;
    /// any other error ends the walk. `None` when no item names a machine,
    /// the first that does is invalid, or a read failed otherwise: here a
    /// read the system refused (`Io`), and every other error is
    /// corruption, passed over.
    ///
    /// One pread per item read, up to the `_MACHINE_ID=` one (every item
    /// when there is none). journald adds `_MACHINE_ID=` to every entry,
    /// and its DATA object comes early in each file, so on journald's
    /// files that is the first few items (they are in offset order).
    /// journal-remote keeps a source's fields as sent.
    ///
    /// Why not the `_MACHINE_ID` FIELD object's list of values
    /// (`field_values`): a constant three preads where this is usually
    /// fewer, but a list cut by corruption elsewhere in it would lose the
    /// tail's value, and systemd reads the entry's items.
    ///
    /// Two departures, neither on files as journald writes them:
    /// - A compressed object that would decode past the field's 44 bytes
    ///   is passed over undecoded, where systemd stops at it with the
    ///   header's id if its name is `_MACHINE_ID`: it differs only for an
    ///   entry with a malformed compressed `_MACHINE_ID=` before another.
    ///   journald stores a payload compressed only when that makes it
    ///   shorter (`maybe_compress_payload`), so a 44-byte `_MACHINE_ID=`
    ///   is never stored compressed longer, and no valid one is missed.
    /// - A file cut short after it was opened: the read past its new end
    ///   is `Io` here and ends the walk, where systemd's fstat in
    ///   `journal_file_move_to` sees the cut, returns `-EADDRNOTAVAIL` and
    ///   passes the item over. The next page opens the file again.
    fn entry_machine_id(&self, e: &Entry) -> Option<[u8; 16]> {
        for &item in &e.items {
            match self.machine_id_item(item) {
                Ok(MachineItem::Named(id)) => return id,
                Err(err) if err.is_io() => return None,
                // Another field, or corruption: passed over.
                Ok(MachineItem::Other) | Err(_) => {}
            }
        }
        None
    }

    /// What the DATA object at `offset` says of the machine, read from
    /// its first bytes in one pread.
    fn machine_id_item(&self, offset: u64) -> Result<MachineItem> {
        let start = self.data_header();
        let field = MACHINE_ID_FIELD.len();
        let (flags, size, mut b) =
            self.object_head(offset, OBJECT_DATA, start + MACHINE_ID_PAYLOAD as u64)?;
        self.check(size >= start, || {
            format!("data object {offset:#x} has a bad size")
        })?;
        // The payload's first bytes, all of it when it is no longer than a
        // machine id's field. `object_head` reads them in its one pread,
        // except for an object past the arena as last read (the file grew
        // since): only its header then, and the rest is read here.
        self.object_rest(offset, size.min(start + MACHINE_ID_PAYLOAD as u64), &mut b)?;
        let stored = b.split_off(self.to_usize(start)?);
        let stored_len = size - start;
        let (payload, len) = if flags & (COMPRESSED_XZ | COMPRESSED_LZ4 | COMPRESSED_ZSTD) == 0 {
            (stored, stored_len)
        } else if stored_len > MACHINE_ID_PAYLOAD as u64 {
            return Ok(MachineItem::Other);
        } else {
            match decompress_within(flags, stored, MACHINE_ID_PAYLOAD, &mut 0) {
                Ok(p) => {
                    let len = p.len() as u64;
                    (p, len)
                }
                Err(ReadErrorKind::OverLimit { .. }) => return Ok(MachineItem::Other),
                Err(k) => return Err(self.error(in_context(&format!("data {offset:#x}"), k))),
            }
        };
        if !payload.starts_with(MACHINE_ID_FIELD) {
            return Ok(MachineItem::Other);
        }
        let value = (len == MACHINE_ID_PAYLOAD as u64).then_some(&payload[field..]);
        Ok(MachineItem::Named(
            value.and_then(crate::MachineId::from_hex),
        ))
    }

    /// An entry that must end by `end`: the next newer entry's offset,
    /// since journald appends objects and they never overlap. Checked
    /// before the rest of its body is read, so a corrupt size can't make
    /// many entries each read the rest of the file. Most entries fit
    /// `READ_GUESS`, so this is one `pread`, rarely two.
    ///
    /// # Errors
    ///
    /// As `entry`, and an entry running past `end`.
    pub fn entry_before(&self, offset: u64, end: u64) -> Result<Entry> {
        let (_, size, mut b) = self.object_head(offset, OBJECT_ENTRY, READ_GUESS)?;
        let item = if self.header.compact() { 4 } else { 16 };
        // At least one whole item, as sd-journal's check_object requires.
        self.check(
            size > ENTRY_HEADER
                && size <= ENTRY_SIZE_MAX
                && (size - ENTRY_HEADER).is_multiple_of(item),
            || format!("entry {offset:#x} has a bad size"),
        )?;
        self.check(offset + size <= end, || {
            format!("entry {offset:#x} overlaps the next entry")
        })?;
        self.object_rest(offset, size, &mut b)?;
        self.check(le64(&b, 16) != 0, || {
            format!("entry {offset:#x} has no sequence number")
        })?;
        let n = self.to_usize((size - ENTRY_HEADER) / item)?;
        let step = self.to_usize(item)?;
        let mut items = Vec::with_capacity(n);
        for i in 0..n {
            let at = 64 + i * step;
            items.push(if self.header.compact() {
                u64::from(le32(&b, at))
            } else {
                le64(&b, at)
            });
        }
        Ok(Entry {
            seqnum: le64(&b, 16),
            realtime: le64(&b, 24),
            monotonic: le64(&b, 32),
            boot_id: id(&b, 40),
            xor_hash: le64(&b, 56),
            items,
        })
    }

    fn data_header(&self) -> u64 {
        if self.header.compact() {
            DATA_HEADER_COMPACT
        } else {
            DATA_HEADER
        }
    }

    /// A DATA object's payload (`FIELD=value`), decompressed. For the
    /// tools and tests only (`testing`): the library reads payloads
    /// within its output limit.
    ///
    /// # Errors
    ///
    /// The object isn't a well-formed DATA object inside the arena, its
    /// payload doesn't decode, or it holds more than `DATA_SIZE_MAX`.
    #[cfg(any(test, feature = "testing"))]
    pub fn data_payload(&self, offset: u64) -> Result<Vec<u8>> {
        self.payload(offset, DATA_SIZE_MAX, &mut 0)
    }

    /// A DATA object's payload, refused with `OverLimit` when it holds
    /// more than `max` bytes (at most `DATA_SIZE_MAX`).
    ///
    /// # Errors
    ///
    /// As `data_payload`, and `ReadErrorKind::OverLimit` over `max`.
    pub fn data_payload_within(&self, offset: u64, max: usize) -> Result<Vec<u8>> {
        self.payload(offset, max, &mut 0)
    }

    /// A DATA object's payload within `left` bytes, which it spends: the
    /// whole object and what it decoded, whether it succeeds or fails (a
    /// corrupt stream can decode a long way before it fails). The query's
    /// `decode` bounds its work this way.
    ///
    /// # Errors
    ///
    /// As `data_payload`, and `ReadErrorKind::OverLimit` when it spent
    /// more than was left.
    pub fn data_payload_spending(&self, offset: u64, left: &mut usize) -> Result<Vec<u8>> {
        let mut cost = 0;
        let allowed = *left;
        let out = self.payload(offset, allowed, &mut cost);
        let over = cost > allowed;
        *left = allowed.saturating_sub(cost);
        match out {
            // A failed read is reported as what it is, corruption, even
            // when it also spent what was left.
            Err(e) => Err(e),
            // Read, but its header made it spend more than was left: over.
            Ok(_) if over => Err(self.error(ReadErrorKind::OverLimit {
                context: format!("data {offset:#x}"),
                over: OverLimit { limit: allowed },
            })),
            ok => ok,
        }
    }

    fn payload(&self, offset: u64, max: usize, cost: &mut usize) -> Result<Vec<u8>> {
        let max = max.min(DATA_SIZE_MAX);
        let start = self.data_header();
        let (flags, size, mut b) = self.object_head(offset, OBJECT_DATA, READ_GUESS)?;
        self.check(
            size >= start && size - start <= DATA_SIZE_MAX as u64,
            || format!("data object {offset:#x} has a bad size"),
        )?;
        // Refused before reading the rest: stored as written, it is too
        // long; and journald stores a payload compressed only when that
        // makes it shorter (maybe_compress_payload compresses into
        // size - 1 bytes, journal-file.c v255), so compressed it decodes
        // to more still.
        if size - start > max as u64 {
            return Err(self.error(ReadErrorKind::OverLimit {
                context: format!("data {offset:#x}"),
                over: OverLimit { limit: max },
            }));
        }
        // Every attempt spends the whole object, header included, so many
        // empty or failing reads still add up.
        *cost += self.to_usize(size)?;
        self.object_rest(offset, size, &mut b)?;
        b.drain(..self.to_usize(start)?);
        decompress_within(flags, b, max, cost)
            .map_err(|k| self.error(in_context(&format!("data {offset:#x}"), k)))
    }

    /// A DATA object's entry list, stored hash and next link in its hash
    /// chain, checked to lie past the object's end (`forward`).
    fn data_ref(&self, offset: u64) -> Result<(DataRef, u64, u64)> {
        let (_, size, b) = self.object_head(offset, OBJECT_DATA, DATA_HEADER)?;
        self.check(size >= self.data_header(), || "short data object".into())?;
        Ok((
            DataRef {
                offset,
                entry_offset: le64(&b, 40),
                entry_array_offset: le64(&b, 48),
                n_entries: le64(&b, 56),
            },
            le64(&b, 16),                                     // hash
            forward(&self.path, offset, size, le64(&b, 24))?, // next_hash_offset
        ))
    }

    /// The number of hash-table buckets, with the table checked as
    /// sd-journal checks it on open (journal-file.c, `hash_table_is_valid`):
    /// whole items, inside the arena, and no offset without a size or the
    /// reverse. sd-journal refuses the whole file; this refuses lookups
    /// only, so the entries are still read in file order (journal spec,
    /// Difference 9).
    fn buckets(&self) -> Result<u64> {
        self.table_buckets(
            "data",
            self.header.data_hash_table_offset,
            self.header.data_hash_table_size,
        )
    }

    /// The buckets of the `what` hash table at `offset`, `size` bytes,
    /// checked as `buckets` describes; 0 for no table.
    fn table_buckets(&self, what: &str, offset: u64, size: u64) -> Result<u64> {
        if offset == 0 && size == 0 {
            return Ok(0);
        }
        self.check(
            size > 0
                && size.is_multiple_of(16)
                && offset >= self.header.size + OBJECT_HEADER
                && self.within(offset, size),
            || format!("bad {what} hash table ({size} bytes at {offset:#x})"),
        )?;
        Ok(size / 16)
    }

    /// The head of hash-table bucket `b` (< `buckets()`, so in the table).
    fn bucket_head(&self, b: u64) -> Result<u64> {
        let item = self.read(self.header.data_hash_table_offset + b * 16, 16)?;
        Ok(le64(&item, 0))
    }

    /// The hash of `payload` as this file's DATA objects store it.
    #[must_use]
    pub fn hash(&self, payload: &[u8]) -> u64 {
        if self.header.keyed() {
            hash::keyed(&self.header.file_id, payload)
        } else {
            hash::jenkins64(payload)
        }
    }

    /// Find the DATA object for `FIELD=value` through the data hash table
    /// ("Hash Table Objects"): the bucket's chain, comparing hash then
    /// payload. `None` when no entry in this file carries it.
    ///
    /// # Errors
    ///
    /// A bad hash table, a chain that can't be followed, or a candidate
    /// that can't be read.
    pub fn find_data(&self, payload: &[u8]) -> Result<Option<DataRef>> {
        let buckets = self.buckets()?;
        if buckets == 0 {
            return Ok(None);
        }
        let h = self.hash(payload);
        let mut at = self.bucket_head(h % buckets)?;
        while at != 0 {
            let (d, stored, next) = self.data_ref(at)?;
            if stored == h {
                // Decoded only as far as the value looked for: a chain of
                // objects claiming its hash can't each cost a full
                // decompression.
                match self.data_payload_within(at, payload.len()) {
                    Ok(p) if p == payload => return Ok(Some(d)),
                    Err(e) if !e.is_over_limit() => {
                        return Err(e);
                    }
                    _ => {}
                }
            }
            at = next;
        }
        Ok(None)
    }

    /// The DATA objects of `field`'s values (`FIELD=value`, each with its
    /// payload), newest first, as sd-journal enumerates them
    /// (src/libsystemd/sd-journal/sd-journal.c v259,
    /// `sd_journal_enumerate_unique`): the FIELD object found through the
    /// field hash table (journal-file.c
    /// `journal_file_find_field_object_with_hash`: the bucket's chain,
    /// comparing hash, size and name), then its `head_data_offset` and each
    /// DATA object's `next_field_offset`. journald prepends a new value
    /// (`journal_file_link_data`), so each link must point to a lower
    /// offset; one that doesn't would loop, and is corrupt. Empty when the
    /// file has no field table or no such field. A value whose payload is
    /// over `max` bytes is left out: the caller looks for none so long.
    /// One pread for the bucket, one per FIELD object in its chain, one per
    /// value (its payload read with it unless compressed or larger than
    /// the read).
    ///
    /// # Errors
    ///
    /// A bad field table, a link that can't be followed, or an object that
    /// can't be read.
    pub(crate) fn field_values(&self, field: &[u8], max: usize) -> Result<Vec<(DataRef, Vec<u8>)>> {
        let offset = self.header.field_hash_table_offset;
        let buckets = self.table_buckets("field", offset, self.header.field_hash_table_size)?;
        if buckets == 0 {
            return Ok(Vec::new());
        }
        let h = self.hash(field);
        let bucket = self.read(offset + h % buckets * 16, 16)?;
        let mut link = le64(&bucket, 0);
        let mut head = 0;
        while link != 0 {
            let (_, osize, mut b) = self.object_head(link, OBJECT_FIELD, READ_GUESS)?;
            self.check(osize >= 40, || format!("field object {link:#x} is short"))?;
            if le64(&b, 16) == h && osize == 40 + field.len() as u64 {
                self.object_rest(link, osize, &mut b)?;
                if &b[40..] == field {
                    head = le64(&b, 32);
                    break;
                }
            }
            link = forward(&self.path, link, osize, le64(&b, 24))?;
        }
        let mut out = Vec::new();
        let start = self.data_header();
        while head != 0 {
            let (flags, osize, b) = self.object_head(head, OBJECT_DATA, READ_GUESS)?;
            self.check(osize >= start, || "short data object".into())?;
            let d = DataRef {
                offset: head,
                entry_offset: le64(&b, 40),
                entry_array_offset: le64(&b, 48),
                n_entries: le64(&b, 56),
            };
            let payload = if flags == 0 && b.len() as u64 == osize {
                Some(b[self.to_usize(start)?..].to_vec()).filter(|p| p.len() <= max)
            } else {
                match self.data_payload_within(head, max) {
                    Ok(p) => Some(p),
                    Err(e) if e.is_over_limit() => None,
                    Err(e) => return Err(e),
                }
            };
            if let Some(p) = payload {
                out.push((d, p));
            }
            let next = le64(&b, 32);
            self.check(next < head, || {
                format!("field list link {head:#x} -> {next:#x} doesn't point back")
            })?;
            head = next;
        }
        Ok(out)
    }

    /// Every DATA object reachable from the hash table, with its stored
    /// hash: for checking our hash functions against journald's (the
    /// hash-check tool and tests only, under `testing`). Each
    /// object must sit in its hash's bucket, as journald links it
    /// (`journal_file_link_data`: `hash % buckets`); that also means no two
    /// buckets share a chain, so the walk reads each object once.
    ///
    /// # Errors
    ///
    /// As `find_data`, and an object in another bucket than its hash's.
    #[cfg(any(test, feature = "testing"))]
    pub fn all_data(&self) -> Result<Vec<(u64, u64)>> {
        let buckets = self.buckets()?;
        let mut out = Vec::new();
        for b in 0..buckets {
            let mut at = self.bucket_head(b)?;
            while at != 0 {
                let (_, stored, next) = self.data_ref(at)?;
                self.check(stored % buckets == b, || {
                    format!("data object {at:#x} is in hash bucket {b}, not its own")
                })?;
                out.push((at, stored));
                at = next;
            }
        }
        Ok(out)
    }
}

// The entry lists: the file's own (every entry, in file order) and each
// DATA object's ("Entry Array Objects").
impl JournalFile {
    /// Entry offsets of the whole file, in file order; an error if any
    /// part of the chain can't be read. For the tools and tests only
    /// (`testing`): the library walks newest first.
    ///
    /// # Errors
    ///
    /// The first error the walk met (see `EntriesRev`).
    #[cfg(any(test, feature = "testing"))]
    pub fn all_entries(&self) -> Result<Vec<u64>> {
        let mut out = self.all_entries_rev().collect::<Result<Vec<u64>>>()?;
        out.reverse();
        Ok(out)
    }

    /// Entry offsets of the whole file, newest first, read lazily.
    #[must_use]
    pub fn all_entries_rev(&self) -> EntriesRev<'_> {
        EntriesRev::new(
            self,
            self.header.entry_array_offset,
            self.header.n_entries,
            None,
        )
    }

    /// Entry offsets referencing a DATA object, newest first, read lazily:
    /// the chain of entry arrays, then the in-line first entry.
    #[must_use]
    pub fn entries_of_rev(&self, d: &DataRef) -> EntriesRev<'_> {
        // journald sets the in-line entry before it counts it
        // (link_entry_into_array_plus_one): an offset with no count yet is
        // a live file caught mid-link, nothing to read; a count with no
        // offset is corruption (sd-journal's check_object).
        if d.n_entries == 0 {
            return EntriesRev::new(self, 0, 0, None);
        }
        if d.entry_offset == 0 {
            let mut empty = EntriesRev::new(self, 0, 0, None);
            empty.first.push(self.error(ReadErrorKind::Corrupt(format!(
                "data {:#x} has a bad entry count",
                d.offset
            ))));
            return empty;
        }
        EntriesRev::new(
            self,
            d.entry_array_offset,
            d.n_entries - 1,
            Some(d.entry_offset),
        )
    }

    fn array_item_size(&self) -> u64 {
        if self.header.compact() { 4 } else { 8 }
    }

    /// Whether an entry-array item `off` can be an entry's offset:
    /// aligned, past the header, inside the arena (so never 0).
    #[must_use]
    pub fn plausible_entry(&self, off: u64) -> bool {
        // `within` re-stats: an entry appended since the open is in the
        // arena.
        off.is_multiple_of(8) && self.within(off, ENTRY_HEADER)
    }

    /// An entry array's size and next link, from one read of its first 24
    /// bytes.
    fn array_head(&self, at: u64) -> Result<(u64, u64)> {
        let (_, size, b) = self.object_head(at, OBJECT_ENTRY_ARRAY, ENTRY_ARRAY_HEADER)?;
        self.check(size >= ENTRY_ARRAY_HEADER, || "short entry array".into())?;
        Ok((size, le64(&b, 16)))
    }
}

/// An entry array's items, as read: 4 bytes each in a compact file, 8
/// otherwise (journal-def.h `EntryArrayObject`).
fn array_items(b: &[u8], compact: bool) -> impl Iterator<Item = u64> + '_ {
    let (width, read): (usize, fn(&[u8]) -> u64) = if compact {
        (4, |c| u64::from(le32(c, 0)))
    } else {
        (8, |c| le64(c, 0))
    };
    b.chunks_exact(width).map(read)
}

/// Items read per `pread` when walking a chain backwards: 4 KiB of regular
/// items. The arrays double in size, so the newest few hundred entries sit
/// in the last one or two arrays and one read usually covers a page.
const CHUNK_ITEMS: u64 = 512;

/// An entry-array chain ("Entry Array Objects") walked from its end,
/// newest first, reading only what is asked for.
///
/// Building it reads each array's 24-byte head to learn the chain's
/// shape: an array's capacity, and the `n` links (from the file; capped by
/// the most entries the arena could hold) spread over them. Items are then
/// read in chunks from the last array backwards. journald only appends, so
/// arrays link forward, past their own end (`forward`, which rules out a
/// cycle), and their items never decrease; a decrease is corruption.
///
/// Corruption costs only what it hides, as sd-journal's `generic_array_get`
/// has it: a chain that can't be followed ends there, and the arrays
/// before the break are still walked; an array whose items go out of
/// order, or can't be read, is left at that point and the walk goes on
/// with the older arrays (going up, sd-journal "moves back to the previous
/// entry array"). Each error is yielded where it was met: a chain cut
/// short first (the entries past the break are the newest), an array left
/// at its gap, so a caller that stops after N entries still sees it. An
/// item that can't be an entry's offset is refused and skipped.
///
/// - A repeat is skipped, as sd-journal skips it: systemd 249 (22.04)
///   links an entry once per item, and an entry carrying the same
///   FIELD=value twice lists its offset twice in a row
///   (`journal_file_append_entry` sorts items but keeps duplicates). It
///   still counts against `n`, as systemd's `n_entries` rose once per link;
///   reading past `n` would reach slots written after it was read.
/// - A zero item (a slot never written, as after an unclean shutdown) is
///   skipped (journal spec, Difference 8): our choice, not parity.
///   sd-journal depends on the path: its unfiltered walk skips a bad item
///   (`generic_array_get`), while its match path bisects
///   (`generic_array_bisect`) and cuts the array short at a zero, or with
///   v249's chain cache drops the file. UNVERIFIED against a real file
///   with one.
/// - A stale item naming a real older entry looks out of order: its array
///   is left there, reported (Difference 10). The stale item itself comes
///   out before the break is seen (it is lower than the item after it), so
///   the walk's own check of each entry (`entry_before`, Task 3) is what
///   keeps it from being shown wrong.
#[derive(Debug)]
pub struct EntriesRev<'a> {
    f: &'a JournalFile,
    /// (array offset, items not yet read) per array, oldest first; the
    /// last one is being read.
    arrays: Vec<(u64, u64)>,
    /// Items read but not yet returned, in file order (popped from the end).
    buf: Vec<u64>,
    /// A DATA object's in-line first entry, returned last.
    inline: Option<u64>,
    last: Option<u64>,
    /// Errors met building the chain: they stand for the arrays past a
    /// break, newer than every entry reachable, so they come first.
    first: Vec<ReadError>,
}

impl<'a> EntriesRev<'a> {
    fn new(f: &'a JournalFile, mut at: u64, n: u64, inline: Option<u64>) -> Self {
        let item = f.array_item_size();
        let n = n.min((f.limit.get() - f.header.size) / ENTRY_HEADER);
        let mut arrays = Vec::new();
        let mut first = Vec::new();
        let mut links = 0u64;
        while at != 0 && links < n {
            let (size, next) = match f.array_head(at) {
                Ok(head) => head,
                Err(e) => {
                    first.push(e);
                    break;
                }
            };
            let count = ((size - ENTRY_ARRAY_HEADER) / item).min(n - links);
            arrays.push((at, count));
            links += count;
            // A bad link ends the chain here; this array is still read.
            match forward(&f.path, at, size, next) {
                Ok(next) => at = next,
                Err(e) => {
                    first.push(e);
                    break;
                }
            }
        }
        Self {
            f,
            arrays,
            buf: Vec::new(),
            inline,
            last: None,
            first,
        }
    }

    fn corrupt(&self, what: String) -> ReadError {
        self.f.error(ReadErrorKind::Corrupt(what))
    }

    /// The next older offset, an error where one was met, or `None` at
    /// the end.
    fn advance(&mut self) -> Option<Result<u64>> {
        if !self.first.is_empty() {
            return Some(Err(self.first.remove(0)));
        }
        let item = self.f.array_item_size();
        loop {
            if let Some(off) = self.buf.pop() {
                if off == 0 {
                    continue;
                }
                // `plausible_entry`: one that can't be an entry is refused
                // before it becomes the order check's reference, so a
                // bogus low item can't make every older item look out of
                // order.
                if !self.f.plausible_entry(off) {
                    let at = self.arrays.last().map_or(0, |a| a.0);
                    return Some(Err(self.corrupt(format!(
                        "entry array {at:#x} lists {off:#x}, not an entry offset"
                    ))));
                }
                match self.last {
                    Some(last) if off == last => continue,
                    Some(last) if off > last => {
                        // Out of order: leave this array here, and start
                        // the older one afresh (the walk's own checks,
                        // `entry_before`, catch an entry out of place).
                        let at = self.arrays.last().map_or(0, |a| a.0);
                        self.buf.clear();
                        self.arrays.pop();
                        self.last = None;
                        return Some(Err(
                            self.corrupt(format!("entry array {at:#x} is out of order"))
                        ));
                    }
                    _ => {}
                }
                self.last = Some(off);
                return Some(Ok(off));
            }
            let Some((at, left)) = self.arrays.last_mut() else {
                // The in-line entry may repeat as the chain's first item on
                // systemd 249 (see above).
                // It is the oldest: one newer than the chain is corruption.
                let inline = self.inline.take()?;
                if let Some(last) = self.last
                    && inline > last
                {
                    return Some(Err(self.corrupt(format!(
                        "in-line entry {inline:#x} is newer than its entry array chain"
                    ))));
                }
                if Some(inline) == self.last {
                    return None;
                }
                return Some(Ok(inline));
            };
            if *left == 0 {
                self.arrays.pop();
                continue;
            }
            let k = (*left).min(CHUNK_ITEMS);
            let lo = *left - k;
            let at = *at;
            *left = lo;
            match self.f.read(at + ENTRY_ARRAY_HEADER + lo * item, k * item) {
                Ok(b) => {
                    self.buf.clear();
                    self.buf.extend(array_items(&b, self.f.header.compact()));
                }
                Err(e) => {
                    self.arrays.pop();
                    return Some(Err(e));
                }
            }
        }
    }
}

impl Iterator for EntriesRev<'_> {
    type Item = Result<u64>;

    /// The next older entry offset, or an error in the place it was met:
    /// first for a chain cut short (its newest arrays are missing), at the
    /// gap for an array left. The walk goes on after an error.
    fn next(&mut self) -> Option<Result<u64>> {
        self.advance()
    }
}

/// One entry list as sd-journal bisects it: a DATA object's (its in-line
/// first entry, the "extra", then its chain of entry arrays) or the file's
/// main entry array chain (no extra). Indexes count the extra as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntryList {
    extra: Option<u64>,
    /// The chain's first entry array; 0 for none.
    first: u64,
    /// The items in the chain (the extra not counted).
    n: u64,
}

impl EntryList {
    /// A DATA object's entry list. Its `n_entries` counts the extra
    /// (`generic_array_bisect_for_data`).
    pub(crate) fn of_data(d: &DataRef) -> Self {
        if d.n_entries == 0 {
            return Self {
                extra: None,
                first: 0,
                n: 0,
            };
        }
        Self {
            extra: Some(d.entry_offset),
            first: d.entry_array_offset,
            n: d.n_entries - 1,
        }
    }
}

/// What a bisect compares an entry by: journal-file.c's `test_object_offset`,
/// `_seqnum`, `_monotonic` and `_realtime` (systemd v259).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Offset,
    Seqnum,
    Monotonic,
    Realtime,
}

/// journal-file.c's chain cache (v259 `ChainCacheItem`, `chain_cache_put`):
/// per chain, where its last bisect ended, so the next bisect of that chain
/// can start there. A page starts with none, as journalctl's run does for
/// a file whose header names its tail entry (systemd 254 and later write
/// `tail_entry_offset`); for an older file, journalctl's first read of the
/// tail (`journal_file_read_tail_timestamp`) can fill the main chain's
/// entry, which only a query matching every entry bisects (none of sb's
/// does). At most `CHAIN_CACHE_MAX` (20) chains, the first put given up
/// first, and a lookup doesn't reorder (an `OrderedHashmap` and
/// `ordered_hashmap_steal_first`). Why not the `lru` crate, in the tree: it
/// promotes an entry on use, which would give up other chains than
/// systemd does.
#[derive(Debug, Default)]
pub(crate) struct ChainCache {
    items: Vec<ChainItem>,
}

#[derive(Debug, Clone, Copy)]
struct ChainItem {
    /// The chain's first array: the key.
    first: u64,
    /// The array the bisect ended in.
    array: u64,
    /// That array's first item.
    begin: u64,
    /// The items in the arrays before it.
    total: u64,
    /// The index the bisect ended at, in that array.
    last_index: u64,
}

/// `CHAIN_CACHE_MAX` (journal-file.c).
const CHAIN_CACHE_MAX: usize = 20;

impl ChainCache {
    fn get(&self, first: u64) -> Option<ChainItem> {
        self.items.iter().find(|c| c.first == first).copied()
    }

    /// `chain_cache_put`: a chain whose bisect ended in its first array
    /// isn't worth caching, unless it already is.
    fn put(&mut self, item: ChainItem) {
        if let Some(c) = self.items.iter_mut().find(|c| c.first == item.first) {
            *c = item;
            return;
        }
        if item.array == item.first {
            return;
        }
        if self.items.len() >= CHAIN_CACHE_MAX {
            self.items.remove(0);
        }
        self.items.push(item);
    }
}

/// What a bisect met on its way: the entries it couldn't read (systemd
/// only logs them), and whether it cut an array short at a bad item, a zero
/// slot included (then "nothing found" may only mean "nothing before it").
#[derive(Debug, Default)]
pub(crate) struct Met {
    pub(crate) errors: Vec<ReadError>,
    pub(crate) cut: bool,
}

/// A bisect's result: the entry's offset, and its index in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) offset: u64,
    pub(crate) index: u64,
}

/// A bisect step's test of one item (`TEST_LEFT`, `TEST_RIGHT`, `TEST_FOUND`),
/// or an item that isn't a readable entry (`-EBADMSG`, `-EADDRNOTAVAIL`).
enum Test {
    Left,
    Right,
    Found,
    Invalid(ReadError),
}

/// `generic_array_bisect_step`'s outcome going down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stepped {
    Left,
    Right,
    /// `TEST_GOTO_PREVIOUS`: going down, nothing is found.
    Previous,
    /// `TEST_GOTO_NEXT`: on to the next array.
    Next,
}

/// What a bisect looks for: the first entry whose `key` is at or after
/// `needle` (`DIRECTION_DOWN`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Probe {
    pub(crate) key: Key,
    pub(crate) needle: u64,
}

/// The state of one array's bisect: the array, and `m`, `left` and `right`
/// in `generic_array_bisect`.
struct Bounds {
    array: u64,
    m: u64,
    left: u64,
    right: u64,
}

impl JournalFile {
    /// The file's main entry array chain, as a bisect reads it.
    pub(crate) fn main_list(&self) -> EntryList {
        EntryList {
            extra: None,
            first: self.header.entry_array_offset,
            n: self.header.n_entries,
        }
    }

    /// The item `i` of the entry array at `array`.
    fn array_item(&self, array: u64, i: u64) -> Result<u64> {
        let width = self.array_item_size();
        let b = self.read(array + ENTRY_ARRAY_HEADER + i * width, width)?;
        Ok(if self.header.compact() {
            u64::from(le32(&b, 0))
        } else {
            le64(&b, 0)
        })
    }

    /// `test_object_*`: the entry at `p` against the probe. An offset is
    /// compared without reading the entry. The system's error stops the
    /// bisect; an entry that can't be read is `Invalid`.
    fn test_object(&self, p: u64, probe: Probe) -> Result<Test> {
        let value = if probe.key == Key::Offset {
            p
        } else {
            match self.entry(p) {
                Ok(e) => match probe.key {
                    Key::Seqnum => e.seqnum,
                    Key::Monotonic => e.monotonic,
                    _ => e.realtime,
                },
                Err(e) if e.is_io() => return Err(e),
                Err(e) => return Ok(Test::Invalid(e)),
            }
        };
        Ok(match value.cmp(&probe.needle) {
            std::cmp::Ordering::Less => Test::Left,
            std::cmp::Ordering::Equal => Test::Found,
            std::cmp::Ordering::Greater => Test::Right,
        })
    }

    /// `generic_array_bisect_step`, going down: tests item `i` and moves
    /// the bounds. An item that isn't a readable entry cuts the array short
    /// there (its error goes to `met`), or, at the left bound, ends the
    /// bisect with nothing found. A zero item does the same, unreported: it
    /// is a slot never written, which the walks skip without a word
    /// (Difference 8).
    fn bisect_step(&self, i: u64, probe: Probe, b: &mut Bounds, met: &mut Met) -> Result<Stepped> {
        let p = self.array_item(b.array, i)?;
        let test = if p == 0 {
            None
        } else {
            Some(self.test_object(p, probe)?)
        };
        Ok(match test {
            None | Some(Test::Invalid(_)) => {
                met.cut = true;
                if let Some(Test::Invalid(e)) = test {
                    met.errors.push(e);
                }
                if i == b.left {
                    return Ok(Stepped::Previous);
                }
                b.m = i;
                b.right = i - 1;
                Stepped::Right
            }
            Some(Test::Found | Test::Right) => {
                b.right = i;
                Stepped::Right
            }
            Some(Test::Left) => {
                if i == b.m - 1 {
                    return Ok(Stepped::Next);
                }
                b.left = i + 1;
                Stepped::Left
            }
        })
    }
}
impl JournalFile {
    /// systemd v259's `generic_array_bisect` (journal-file.c) going down
    /// (`DIRECTION_DOWN`): the first item of the chain at `first` (`n`
    /// items) that the probe finds at or after its needle, as its offset
    /// and its index in the chain. Followed step for step, because on a
    /// chain that isn't in order (a 249 file whose clock stepped back) the
    /// result is wherever systemd's probes land, and journalctl's forward
    /// page starts there:
    /// - a cached chain (`cache`) starts at the cached array when that
    ///   array's first item tests left of the needle;
    /// - each array's last item is tested first: left of the needle moves
    ///   on to the next array;
    /// - in a cached array, the items beside the last index are tested
    ///   next;
    /// - then halving, `(left + right) / 2`;
    /// - an item that isn't a readable entry cuts the array short there,
    ///   or, at the left bound, ends with nothing found (going down, systemd
    ///   doesn't return to the previous array). Its error goes to `met`;
    ///   systemd only logs it.
    ///
    /// Why not std's `partition_point` or `binary_search_by`: their probe
    /// order is unspecified, and on input out of order the probe order is
    /// the result; they also know nothing of chained arrays, the cached
    /// array and its last index, or cutting an array short.
    ///
    /// # Errors
    ///
    /// An entry array or item that can't be read, or a first item of 0
    /// where the result is taken (systemd's `-EBADMSG`).
    pub(crate) fn bisect(
        &self,
        first: u64,
        items: u64,
        probe: Probe,
        cache: &mut ChainCache,
        met: &mut Met,
    ) -> Result<Option<Found>> {
        if items == 0 {
            return Ok(None);
        }
        // systemd's `a`, `n` and `t`: the array, the items from it on, and
        // the items before it.
        let (mut array, mut left_items, mut before) = (first, items, 0u64);
        let mut last_index = u64::MAX;
        let cached = cache.get(first);
        if let Some(ci) = cached
            && left_items > ci.total
            && ci.begin != 0
        {
            match self.test_object(ci.begin, probe)? {
                Test::Left => {
                    array = ci.array;
                    left_items -= ci.total;
                    before = ci.total;
                    last_index = ci.last_index;
                }
                Test::Invalid(e) => met.errors.push(e),
                Test::Right | Test::Found => {}
            }
        }
        let width = self.array_item_size();
        while array != 0 {
            let (size, next) = self.array_head(array)?;
            let capacity = (size - ENTRY_ARRAY_HEADER) / width;
            let count = capacity.min(left_items);
            if count == 0 {
                return Ok(None);
            }
            let mut b = Bounds {
                array,
                m: count,
                left: 0,
                right: count - 1,
            };
            let r = self.bisect_step(b.right, probe, &mut b, met)?;
            if r == Stepped::Previous {
                return Ok(None);
            }
            if r == Stepped::Right {
                // The neighbours of the cached last index first.
                if last_index > 0 && b.left < last_index - 1 && last_index - 1 < b.right {
                    let r = self.bisect_step(last_index - 1, probe, &mut b, met)?;
                    if r == Stepped::Previous {
                        return Ok(None);
                    }
                }
                if last_index < u64::MAX && b.left < last_index + 1 && last_index + 1 < b.right {
                    let r = self.bisect_step(last_index + 1, probe, &mut b, met)?;
                    if r == Stepped::Previous {
                        return Ok(None);
                    }
                }
                loop {
                    if b.left == b.right {
                        // An array cut short: its last item left hasn't
                        // been tested yet.
                        if b.m != count {
                            let r = self.bisect_step(b.left, probe, &mut b, met)?;
                            if matches!(r, Stepped::Previous | Stepped::Next) {
                                return Ok(None);
                            }
                        }
                        return self
                            .bisect_found(first, array, before, b.left, cache)
                            .map(Some);
                    }
                    let mid = u64::midpoint(b.left, b.right);
                    let r = self.bisect_step(mid, probe, &mut b, met)?;
                    if matches!(r, Stepped::Previous | Stepped::Next) {
                        return Ok(None);
                    }
                }
            }
            // `Next`: every item of this array is left of the needle.
            if capacity >= left_items {
                return Ok(None);
            }
            left_items -= capacity;
            before += capacity;
            last_index = u64::MAX;
            array = forward(&self.path, array, size, next)?;
        }
        Ok(None)
    }

    /// `generic_array_bisect`'s `found:` label: caches where the bisect
    /// ended and returns the item.
    fn bisect_found(
        &self,
        first: u64,
        array: u64,
        total: u64,
        i: u64,
        cache: &mut ChainCache,
    ) -> Result<Found> {
        let zero = || {
            self.error(ReadErrorKind::Corrupt(format!(
                "entry array {array:#x} lists 0"
            )))
        };
        let begin = self.array_item(array, 0)?;
        if begin == 0 {
            return Err(zero());
        }
        cache.put(ChainItem {
            first,
            array,
            begin,
            total,
            last_index: i,
        });
        let p = self.array_item(array, i)?;
        if p == 0 {
            return Err(zero());
        }
        Ok(Found {
            offset: p,
            index: total + i,
        })
    }

    /// systemd v259's `generic_array_bisect_for_data` going down, or
    /// `generic_array_bisect` for the main list: the first entry of `list`
    /// at or after the probe's needle, as its offset and its index in the
    /// list (the extra is 0). A DATA object's extra is tested first: at or
    /// after the needle it is the result, and the chain isn't read.
    ///
    /// # Errors
    ///
    /// As `bisect`, and an extra that isn't a readable entry (systemd
    /// returns its error).
    pub(crate) fn bisect_list(
        &self,
        list: &EntryList,
        probe: Probe,
        cache: &mut ChainCache,
        met: &mut Met,
    ) -> Result<Option<Found>> {
        let Some(extra) = list.extra else {
            return self.bisect(list.first, list.n, probe, cache, met);
        };
        match self.test_object(extra, probe)? {
            Test::Found | Test::Right => {
                return Ok(Some(Found {
                    offset: extra,
                    index: 0,
                }));
            }
            Test::Invalid(e) => return Err(e),
            Test::Left => {}
        }
        Ok(self
            .bisect(list.first, list.n, probe, cache, met)?
            .map(|f| Found {
                index: f.index + 1,
                ..f
            }))
    }

    /// The entries of `list` from its index `from` on, in file order.
    pub(crate) fn entries_from(&self, list: &EntryList, from: u64) -> EntriesFwd<'_> {
        EntriesFwd::new(self, list, from)
    }
}

/// An entry list read forward from an index, oldest first: the extra (a
/// DATA object's in-line entry), then the chain's arrays in link order,
/// read in chunks. The rules are `EntriesRev`'s, the other way: a zero item
/// is skipped (Difference 8), an item that can't be an entry offset is
/// refused, a repeat is skipped, and an item lower than the one before
/// leaves the rest of its array (reported), the walk going on with the next
/// array. Why not `entries_since`: that reads the main array only, and
/// stops at the first item it can't use, for the follow to wait on.
#[derive(Debug)]
pub(crate) struct EntriesFwd<'a> {
    f: &'a JournalFile,
    extra: Option<u64>,
    /// The array being read, its size and link; 0 when the chain is done.
    at: u64,
    size: u64,
    link: u64,
    /// The chain index of the array's first item, and of the next to read.
    base: u64,
    next: u64,
    /// The chain's items (capped by what the arena could hold).
    n: u64,
    buf: std::collections::VecDeque<u64>,
    last: Option<u64>,
    /// An error met finding the start, yielded first.
    failed: Option<ReadError>,
}

impl<'a> EntriesFwd<'a> {
    fn new(f: &'a JournalFile, list: &EntryList, from: u64) -> Self {
        let n = list.n.min((f.limit.get() - f.header.size) / ENTRY_HEADER);
        let mut it = Self {
            f,
            extra: None,
            at: 0,
            size: 0,
            link: 0,
            base: 0,
            next: 0,
            n,
            buf: std::collections::VecDeque::new(),
            last: None,
            failed: None,
        };
        // The chain index to start at: the extra is index 0 of a DATA list.
        let chain_from = match list.extra {
            Some(extra) if from == 0 => {
                it.extra = Some(extra);
                0
            }
            Some(_) => from - 1,
            None => from,
        };
        it.next = chain_from;
        let (mut a, mut base) = (list.first, 0u64);
        let item = f.array_item_size();
        while a != 0 && base < n {
            let (size, link) = match f.array_head(a) {
                Ok(head) => head,
                Err(e) => {
                    it.failed = Some(e);
                    return it;
                }
            };
            let cap = (size - ENTRY_ARRAY_HEADER) / item;
            if chain_from < base + cap {
                (it.at, it.size, it.link, it.base) = (a, size, link, base);
                return it;
            }
            base += cap;
            match forward(&f.path, a, size, link) {
                Ok(next) => a = next,
                Err(e) => {
                    it.failed = Some(e);
                    return it;
                }
            }
        }
        it
    }

    /// The next array of the chain, or done.
    fn next_array(&mut self) -> Option<Result<()>> {
        let end = self.base + (self.size - ENTRY_ARRAY_HEADER) / self.f.array_item_size();
        let a = match forward(&self.f.path, self.at, self.size, self.link) {
            Ok(a) => a,
            Err(e) => {
                self.at = 0;
                return Some(Err(e));
            }
        };
        self.at = 0;
        if a == 0 || end >= self.n {
            return None;
        }
        match self.f.array_head(a) {
            Ok((size, link)) => {
                (self.at, self.size, self.link, self.base) = (a, size, link, end);
                self.next = self.next.max(end);
                Some(Ok(()))
            }
            Err(e) => Some(Err(e)),
        }
    }
}

impl Iterator for EntriesFwd<'_> {
    type Item = Result<u64>;

    fn next(&mut self) -> Option<Result<u64>> {
        if let Some(e) = self.failed.take() {
            return Some(Err(e));
        }
        if let Some(extra) = self.extra.take() {
            if !self.f.plausible_entry(extra) {
                return Some(Err(self.f.error(ReadErrorKind::Corrupt(format!(
                    "in-line entry {extra:#x} is not an entry offset"
                )))));
            }
            self.last = Some(extra);
            return Some(Ok(extra));
        }
        let item = self.f.array_item_size();
        loop {
            if let Some(off) = self.buf.pop_front() {
                if off == 0 {
                    continue;
                }
                if !self.f.plausible_entry(off) {
                    return Some(Err(self.f.error(ReadErrorKind::Corrupt(format!(
                        "entry array {:#x} lists {off:#x}, not an entry offset",
                        self.at
                    )))));
                }
                match self.last {
                    Some(last) if off == last => continue,
                    Some(last) if off < last => {
                        // Out of order: leave the rest of this array.
                        self.buf.clear();
                        self.next = self.base + (self.size - ENTRY_ARRAY_HEADER) / item;
                        return Some(Err(self.f.error(ReadErrorKind::Corrupt(format!(
                            "entry array {:#x} is out of order",
                            self.at
                        )))));
                    }
                    _ => {}
                }
                self.last = Some(off);
                return Some(Ok(off));
            }
            if self.at == 0 || self.next >= self.n {
                return None;
            }
            let end = self.base + (self.size - ENTRY_ARRAY_HEADER) / item;
            if self.next >= end {
                match self.next_array()? {
                    Ok(()) => continue,
                    Err(e) => return Some(Err(e)),
                }
            }
            let k = (end.min(self.n) - self.next).min(CHUNK_ITEMS);
            match self.f.read(
                self.at + ENTRY_ARRAY_HEADER + (self.next - self.base) * item,
                k * item,
            ) {
                Ok(b) => {
                    self.buf.extend(array_items(&b, self.f.header.compact()));
                    self.next += k;
                }
                Err(e) => {
                    // This array can't be read: on with the next.
                    self.next = end;
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Where a forward read of the file's entry array chain stopped (for
/// following): the array object at `at`, whose first item is the chain's
/// item number `base`. Arrays are only appended and linked forward, so a
/// later read resumes there instead of walking the chain from its head.
#[derive(Debug, Clone, Copy)]
pub struct ArrayCursor {
    at: u64,
    base: u64,
}

/// The follow's reads.
impl JournalFile {
    /// The offsets of the entries numbered `from` up to the header's
    /// `n_entries` in the file's entry array chain (the list of every
    /// entry, in file order), reading only those items. `cursor` is where
    /// the last call stopped, and is updated. The header must be fresh
    /// (`refresh`): journald writes the item before it raises `n_entries`
    /// (`journal_file_link_entry`), so every item below it is there.
    ///
    /// An item that can't be an entry offset (`plausible_entry`: a zero
    /// slot never written, as after an unclean shutdown, or a bogus value)
    /// is returned as it is and the read stops after it: the caller
    /// decides whether to wait for it or skip it (`EntriesRev` skips a
    /// zero and reports a bogus one). Stopping there also keeps a bogus
    /// high item from making the good items after it look out of order.
    ///
    /// Why not `EntriesRev`: it learns the whole chain's shape and walks
    /// back from the end, for the newest N; a follow reads forward from a
    /// position it already holds, a few items per wake.
    ///
    /// # Errors
    ///
    /// An entry array that can't be read, a link that doesn't point
    /// forward, or items out of order.
    pub fn entries_since(&self, cursor: &mut Option<ArrayCursor>, from: u64) -> Result<Vec<u64>> {
        let item = self.array_item_size();
        let n = self
            .header
            .n_entries
            .min((self.limit.get() - self.header.size) / ENTRY_HEADER);
        // A cursor at offset 0 is a read made before the file had an entry
        // array: start from the head, which journald has linked since.
        let mut c = match *cursor {
            Some(c) if c.base <= from && c.at != 0 => c,
            _ => ArrayCursor {
                at: self.header.entry_array_offset,
                base: 0,
            },
        };
        let mut out: Vec<u64> = Vec::new();
        let mut index = from;
        while index < n && c.at != 0 {
            let (size, next) = self.array_head(c.at)?;
            let end = c.base.saturating_add((size - ENTRY_ARRAY_HEADER) / item);
            if index < end {
                let count = n.min(end) - index;
                let b = self.read(
                    c.at + ENTRY_ARRAY_HEADER + (index - c.base) * item,
                    count * item,
                )?;
                for off in array_items(&b, self.header.compact()) {
                    if !self.plausible_entry(off) {
                        out.push(off);
                        *cursor = Some(c);
                        return Ok(out);
                    }
                    self.check(out.last().is_none_or(|&last| off > last), || {
                        format!("entry array {:#x} is out of order", c.at)
                    })?;
                    out.push(off);
                    index += 1;
                }
                // Stopped at n inside this array: resume here next time.
                if index < end {
                    break;
                }
            }
            let next = forward(&self.path, c.at, size, next)?;
            if next == 0 {
                break;
            }
            c = ArrayCursor {
                at: next,
                base: end,
            };
        }
        *cursor = Some(c);
        Ok(out)
    }
}

/// A link from the object at `at` (`size` bytes) to the next in its
/// chain. journald appends objects and links a new one from an older one,
/// so the next object starts past this one's end: a link backwards or into
/// the object is corruption. Refusing it ends any cycle, and keeps a walk
/// reading each byte of the file at most once.
fn forward(path: &Path, at: u64, size: u64, next: u64) -> Result<u64> {
    if next == 0 || at.checked_add(size).is_some_and(|end| next >= end) {
        Ok(next)
    } else {
        Err(ReadError {
            path: Some(path.to_path_buf()),
            kind: ReadErrorKind::Corrupt(format!(
                "chain link {at:#x} -> {next:#x} doesn't point past the object"
            )),
        })
    }
}

/// `kind` met at `context` (`data 0x1f8`): the message's prefix, as the
/// prototype's anyhow context gave it.
pub(crate) fn in_context(context: &str, kind: ReadErrorKind) -> ReadErrorKind {
    match kind {
        ReadErrorKind::Corrupt(m) => ReadErrorKind::Corrupt(format!("{context}: {m}")),
        ReadErrorKind::OverLimit { context: c, over } if c.is_empty() => ReadErrorKind::OverLimit {
            context: context.to_owned(),
            over,
        },
        ReadErrorKind::OverLimit { context: c, over } => ReadErrorKind::OverLimit {
            context: format!("{context}: {c}"),
            over,
        },
        ReadErrorKind::Io { context: c, source } => ReadErrorKind::Io {
            context: format!("{context}: {c}"),
            source,
        },
        other => other,
    }
}

fn corrupt(what: String) -> ReadErrorKind {
    ReadErrorKind::Corrupt(what)
}

fn over(context: &str, limit: usize) -> ReadErrorKind {
    ReadErrorKind::OverLimit {
        context: context.to_owned(),
        over: OverLimit { limit },
    }
}

/// `v` as a `usize`: always on the 64-bit targets sb builds for.
fn usize_of(v: u64) -> Result<usize, ReadErrorKind> {
    usize::try_from(v).map_err(|_| corrupt(format!("size {v} doesn't fit in memory")))
}

/// Decompress a DATA payload the way systemd's compress.c writes it:
/// xz as an .xz stream, LZ4 as a little-endian u64 size then one LZ4 block,
/// zstd as a frame.
#[cfg(test)]
fn decompress(flags: u8, raw: Vec<u8>) -> Result<Vec<u8>, ReadErrorKind> {
    decompress_within(flags, raw, DATA_SIZE_MAX, &mut 0)
}

/// As `decompress`, refusing output over `max` bytes (at most
/// `DATA_SIZE_MAX`), and adding to `cost` the bytes it allocated for output
/// or decoded, on success or failure.
fn decompress_within(
    flags: u8,
    raw: Vec<u8>,
    max: usize,
    cost: &mut usize,
) -> Result<Vec<u8>, ReadErrorKind> {
    match flags & (COMPRESSED_XZ | COMPRESSED_LZ4 | COMPRESSED_ZSTD) {
        0 => {
            // `payload` checks this before reading; this is for the
            // other callers.
            if raw.len() > max {
                return Err(over("", max));
            }
            Ok(raw)
        }
        COMPRESSED_XZ => {
            // The memory limit fails before the dictionary is allocated, and
            // `read_counted` caps the output: a corrupt stream can't exhaust
            // memory.
            // https://docs.rs/lzma-rust2/0.25.0/lzma_rust2/struct.XzReader.html
            let mut dec =
                lzma_rust2::XzReader::new_mem_limit(raw.as_slice(), false, XZ_MEM_LIMIT_KB);
            let out = read_counted(&mut dec, max, cost).map_err(|e| corrupt(format!("xz: {e}")))?;
            if out.len() > max {
                return Err(over("xz", max));
            }
            Ok(out)
        }
        COMPRESSED_LZ4 => {
            // Why not `lz4_flex::block::decompress_size_prepended`: it reads
            // a u32 size prefix, and journald writes a u64.
            if raw.len() < 8 {
                return Err(corrupt("short lz4 payload".into()));
            }
            let claim = le64(&raw, 0);
            // The block is allocated at the claimed size before decoding.
            // An LZ4 block can't expand more than 255 times: past the
            // token, every byte of output beyond a literal costs at least
            // one length byte worth at most 255
            // (https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md).
            let most = (raw.len() as u64 - 8).saturating_mul(255);
            if claim > most || claim > DATA_SIZE_MAX as u64 {
                return Err(corrupt(format!(
                    "lz4 payload claims {claim} bytes from {}",
                    raw.len() - 8
                )));
            }
            if claim > max as u64 {
                return Err(over("lz4", max));
            }
            let size = usize_of(claim)?;
            // Allocated whole, before decoding.
            *cost += size;
            let out = lz4_flex::block::decompress(&raw[8..], size)
                .map_err(|e| corrupt(format!("lz4: {e}")))?;
            if out.len() != size {
                return Err(corrupt("lz4 size mismatch".into()));
            }
            Ok(out)
        }
        COMPRESSED_ZSTD => zstd(&raw, max, cost),
        other => Err(corrupt(format!(
            "more than one compression flag ({other:#x})"
        ))),
    }
}

/// A zstd frame, decoded as sd-journal's `decompress_blob_zstd` (compress.c,
/// v255): it must declare its content size, and libzstd refuses output of
/// another length. ruzstd 0.9 doesn't check it, so it is checked here; and
/// a size over `max` is refused before decoding, so a lookup or a spent
/// budget decodes nothing.
fn zstd(raw: &[u8], max: usize, cost: &mut usize) -> Result<Vec<u8>, ReadErrorKind> {
    let content = zstd_header(raw)
        .and_then(|h| h.content)
        .ok_or_else(|| corrupt("zstd: no content size in the frame".into()))?;
    // A 4-byte RLE block yields at most 128 KiB (RFC 8878,
    // Block_Maximum_Size): a larger claim than the stored bytes
    // allow is corruption, not a large payload.
    let most = (raw.len() as u64 / 4 + 1).saturating_mul(128 * 1024);
    if content > most {
        return Err(corrupt(format!(
            "zstd: the frame declares {content} bytes from {}",
            raw.len()
        )));
    }
    if content > max as u64 {
        return Err(over("zstd", max));
    }
    let content_len = usize_of(content)?;
    // ruzstd holds back up to a window of output until the frame
    // ends, so the window bounds the decoding no read returns.
    // journald's frames (one-shot ZSTD_compress) have a window
    // fitted to the content: allow twice the content, at least 1
    // KiB and at most ZSTD_WINDOW_MAX, and refuse a larger window as
    // corruption.
    // https://docs.rs/ruzstd/0.9.1/ruzstd/decoding/struct.StreamingDecoder.html#method.new_with_max_window_size
    let window = content.saturating_mul(2).clamp(1024, ZSTD_WINDOW_MAX);
    let mut dec = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(raw, window)
        .map_err(|e| corrupt(format!("zstd: {e}")))?;
    // Decoded only to the declared size (plus one byte, to see a
    // longer stream): ruzstd would go on to the last block.
    let out = match read_counted(&mut dec, content_len, cost) {
        Ok(out) => out,
        Err(e) => {
            *cost += usize_of(window).unwrap_or(max).min(max);
            return Err(corrupt(format!("zstd: {e}")));
        }
    };
    if out.len() as u64 != content {
        return Err(corrupt(format!(
            "zstd: {} bytes decoded, the frame declares {content}",
            out.len()
        )));
    }
    Ok(out)
}

/// A zstd frame header's sizes (RFC 8878, `Frame_Header`).
#[derive(Debug, PartialEq, Eq)]
struct ZstdHeader {
    /// The window: the content size for a single-segment frame.
    window: u64,
    /// The declared content size, when the frame has one.
    content: Option<u64>,
}

/// `None` when the header is cut short.
///
/// Why not ruzstd's parser: `read_frame_header` is in its `pub(crate)`
/// `frame` module, and the public `FrameDecoder::content_size()` gives 0
/// both for no declared size and for a declared 0, which sd-journal tells
/// apart (it refuses a frame without one). The size is also needed before
/// the decoder is built, to fit its maximum window to it.
fn zstd_header(raw: &[u8]) -> Option<ZstdHeader> {
    let fhd = *raw.get(4)?;
    let single = fhd & 0x20 != 0;
    let (window, mut at) = if single {
        (None, 5)
    } else {
        let wd = *raw.get(5)?;
        let base = 1u64 << (10 + (wd >> 3));
        (Some(base + base / 8 * u64::from(wd & 7)), 6)
    };
    at += [0, 1, 2, 4][usize::from(fhd & 3)];
    let fcs = |n: usize| -> Option<u64> {
        let mut b = [0u8; 8];
        b[..n].copy_from_slice(raw.get(at..at + n)?);
        Some(u64::from_le_bytes(b))
    };
    let content = match (fhd >> 6, single) {
        (0, false) => None,
        (0, true) => Some(fcs(1)?),
        (1, _) => Some(fcs(2)? + 256),
        (2, _) => Some(fcs(4)?),
        _ => Some(fcs(8)?),
    };
    Some(ZstdHeader {
        window: window.or(content)?,
        content,
    })
}

/// Read a decoder's output up to `max + 1` bytes, adding each chunk to
/// `cost` as it arrives, so a stream that fails part way still spends
/// what it decoded. Why not `take(..).read_to_end`: it asks for ever
/// larger reads, and a streaming decoder decodes as much as a read asks
/// for into its own buffer before it can fail, so the bytes decoded
/// before a failure would never reach `cost`. Fixed 64 KiB reads keep
/// that to about one block.
fn read_counted(dec: &mut impl Read, max: usize, cost: &mut usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    while out.len() <= max {
        let want = chunk.len().min(max.saturating_add(1) - out.len());
        let n = dec.read(&mut chunk[..want])?;
        if n == 0 {
            break;
        }
        *cost += n;
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

/// `pread` into `buf` until it is full or the file ends; the bytes read.
/// Why not `FileExt::read_exact_at`: it fails on a short read, and a
/// header read asks for 272 bytes where 208 are a valid header.
fn read_up_to(file: &Handle, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read_at(&mut buf[n..], offset + n as u64)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn empty_header() -> Header {
    Header {
        compatible: 0,
        incompatible: 0,
        state: 0,
        file_id: [0; 16],
        machine_id: [0; 16],
        tail_entry_boot_id: [0; 16],
        seqnum_id: [0; 16],
        size: 0,
        arena_size: 0,
        data_hash_table_offset: 0,
        data_hash_table_size: 0,
        field_hash_table_offset: 0,
        field_hash_table_size: 0,
        tail_object_offset: 0,
        n_entries: 0,
        entry_array_offset: 0,
        tail_entry_realtime: 0,
        tail_entry_monotonic: 0,
        tail_entry_offset: None,
    }
}

pub(crate) fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

pub(crate) fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn id(b: &[u8], at: usize) -> [u8; 16] {
    b[at..at + 16].try_into().expect("16 bytes")
}

/// A 128-bit id as systemd prints it: 32 lowercase hex digits.
#[must_use]
pub fn hex_id(id: &[u8; 16]) -> String {
    hex::encode(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted::*;
    use std::os::unix::fs::FileExt;

    /// For `decompress_within`, which returns a bare kind.
    fn is_over(kind: &ReadErrorKind) -> bool {
        matches!(kind, ReadErrorKind::OverLimit { .. })
    }

    /// A regular (not compact) file: a 272-byte header, then one entry
    /// array per element of `arrays`, each linked to the next. The items
    /// are bare offsets: the chain walk doesn't read the entries they
    /// point to. Built with the crafted-file builder; the prototype wrote
    /// the same bytes by hand.
    fn file_with_chain(arrays: &[&[u64]]) -> (tempfile::NamedTempFile, JournalFile) {
        let mut c = Crafted::new();
        let mut at = 272u64;
        for (k, items) in arrays.iter().enumerate() {
            let size = ENTRY_ARRAY_HEADER + 8 * items.len() as u64;
            let next = if k + 1 < arrays.len() { at + size } else { 0 };
            assert_eq!(c.array(items, items.len(), next), at);
            at += size;
        }
        // Room in the arena for the entries the offsets name, so the cap on
        // `n` (entries that fit the arena) doesn't bite.
        c.b.resize(c.b.len() + 128 * 1024, 0);
        c.write()
    }

    /// A file of entries with these realtimes (seqnums 1 on), linked in a
    /// main chain of arrays with these capacities; and the entries.
    fn bisect_file(
        reals: &[u64],
        caps: &[usize],
    ) -> (tempfile::NamedTempFile, JournalFile, Vec<u64>) {
        let mut c = Crafted::new();
        let entries: Vec<u64> = reals
            .iter()
            .enumerate()
            .map(|(i, &real)| {
                // One item: an entry lists at least one DATA object.
                let e = c.entry(i as u64 + 1, &[8]);
                c.set(e + 24, real);
                e
            })
            .collect();
        let head = c.b.len() as u64;
        let mut at = head;
        let mut rest = &entries[..];
        for (k, &cap) in caps.iter().enumerate() {
            let size = ENTRY_ARRAY_HEADER + 8 * cap as u64;
            let next = if k + 1 < caps.len() { at + size } else { 0 };
            let (items, more) = rest.split_at(cap.min(rest.len()));
            assert_eq!(c.array(items, cap, next), at);
            rest = more;
            at += size;
        }
        c.set(H_N_ENTRIES, entries.len() as u64);
        c.set(H_ENTRY_ARRAY_OFFSET, head);
        let (t, f) = c.write();
        (t, f, entries)
    }

    fn realtime(needle: u64) -> Probe {
        Probe {
            key: Key::Realtime,
            needle,
        }
    }

    /// The first index at or after the needle, by systemd's bisect, with a
    /// fresh cache.
    fn landing(f: &JournalFile, needle: u64) -> Option<u64> {
        let mut met = Met::default();
        let got = f
            .bisect_list(
                &f.main_list(),
                realtime(needle),
                &mut ChainCache::default(),
                &mut met,
            )
            .expect("bisects");
        assert!(met.errors.is_empty(), "{met:?}");
        got.map(|f| f.index)
    }

    /// In order: the first entry at or after the needle, the first of
    /// equal ones, none past the end; across chained arrays too.
    #[test]
    fn the_bisect_finds_the_first_entry_at_or_after() {
        let (_t, f, e) = bisect_file(&[10, 30, 30, 30, 50], &[5]);
        assert_eq!(f.main_list().first, f.header.entry_array_offset);
        for (needle, want) in [
            (5, Some(0)),
            (10, Some(0)),
            (30, Some(1)),
            (35, Some(4)),
            (50, Some(4)),
            (51, None),
        ] {
            assert_eq!(landing(&f, needle), want, "{needle}");
        }
        let mut met = Met::default();
        let got = f.bisect_list(
            &f.main_list(),
            realtime(35),
            &mut ChainCache::default(),
            &mut met,
        );
        assert_eq!(
            got.expect("bisects"),
            Some(Found {
                offset: e[4],
                index: 4
            })
        );
        let (_t, f, _e) = bisect_file(&[10, 20, 30, 40, 50, 60], &[2, 4]);
        for (needle, want) in [
            (15, Some(1)),
            (20, Some(1)),
            (25, Some(2)),
            (45, Some(4)),
            (60, Some(5)),
            (61, None),
        ] {
            assert_eq!(landing(&f, needle), want, "{needle}");
        }
        let (_t, f, _e) = bisect_file(&[], &[1]);
        assert_eq!(landing(&f, 1), None);
    }

    /// Out of order, the result is where systemd's probes land: realtimes
    /// 10, 50, 5, 45 and the needle 40 test 45, then 50, then 10, and land
    /// on 50; 10, 50, 20, 30, 45 land on 45.
    #[test]
    fn out_of_order_the_bisect_lands_where_systemds_does() {
        let (_t, f, _e) = bisect_file(&[10, 50, 5, 45], &[4]);
        assert_eq!(landing(&f, 40), Some(1));
        let (_t, f, _e) = bisect_file(&[10, 50, 20, 30, 45], &[5]);
        assert_eq!(landing(&f, 40), Some(4));
    }

    /// The chain cache: a bisect that ended in a later array starts the
    /// next bisect of that chain there when that array's first item is
    /// left of the needle, which out of order changes the result. Arrays
    /// [10, 60] and [20, 30, 70, 80]: 75 lands on 80 and caches the second
    /// array; then 25 starts there and lands on 30 (index 3), where a fresh
    /// bisect lands on 60 (index 1). A bisect ending in the first array
    /// isn't cached.
    #[test]
    fn the_chain_cache_starts_at_the_last_array() {
        let (_t, f, _e) = bisect_file(&[10, 60, 20, 30, 70, 80], &[2, 4]);
        let mut cache = ChainCache::default();
        let mut met = Met::default();
        let mut at = |needle, cache: &mut ChainCache| {
            f.bisect_list(&f.main_list(), realtime(needle), cache, &mut met)
                .expect("bisects")
                .map(|f| f.index)
        };
        assert_eq!(at(75, &mut cache), Some(5));
        assert_eq!(cache.items.len(), 1);
        assert_eq!(at(25, &mut cache), Some(3));
        assert_eq!(at(25, &mut ChainCache::default()), Some(1));
        // The cached array's first item right of the needle: from the head.
        assert_eq!(at(15, &mut cache), Some(1));
        let mut first = ChainCache::default();
        assert_eq!(at(5, &mut first), Some(0));
        assert!(first.items.is_empty());
    }

    /// The cached array's last index: its neighbours are tested before
    /// halving, which out of order changes the result (traced against v259
    /// and a model of it). Arrays [10, 15] and [20, 40, 30, 50, 70, 60]: 55
    /// lands on index 6 and caches index 4; then 35 tests index 3 (the one
    /// before it, 50) and lands on 3, where without that test, or with a
    /// fresh cache, it lands on 5. Arrays [94, 32] and [36, 76, 35, 88]: 71
    /// lands on 3 and caches index 1; then 75 tests index 2 (the one after
    /// it) and lands on 5, where without that test it lands on 3.
    #[test]
    fn the_cached_last_index_is_tested_first() {
        let at = |f: &JournalFile, needle, cache: &mut ChainCache| {
            let mut met = Met::default();
            f.bisect_list(&f.main_list(), realtime(needle), cache, &mut met)
                .expect("bisects")
                .map(|found| found.index)
        };
        let (_t, f, _e) = bisect_file(&[10, 15, 20, 40, 30, 50, 70, 60], &[2, 6]);
        let mut cache = ChainCache::default();
        assert_eq!(at(&f, 55, &mut cache), Some(6));
        assert_eq!(cache.items[0].last_index, 4);
        assert_eq!(at(&f, 35, &mut cache), Some(3));
        assert_eq!(at(&f, 35, &mut ChainCache::default()), Some(5));
        let (_t, f, _e) = bisect_file(&[94, 32, 36, 76, 35, 88], &[2, 4]);
        let mut cache = ChainCache::default();
        assert_eq!(at(&f, 71, &mut cache), Some(3));
        assert_eq!(cache.items[0].last_index, 1);
        assert_eq!(at(&f, 75, &mut cache), Some(5));
    }

    /// A cached array whose first entry can't be read isn't jumped to: the
    /// bisect starts from the chain's head, and the entry is reported
    /// (systemd logs "Cached entry is corrupted, ignoring").
    #[test]
    fn an_unreadable_cached_start_is_not_jumped_to() {
        let (t, f, e) = bisect_file(&[10, 60, 20, 30, 70, 80], &[2, 4]);
        let mut cache = ChainCache::default();
        let mut met = Met::default();
        let got = f.bisect_list(&f.main_list(), realtime(75), &mut cache, &mut met);
        assert_eq!(got.expect("bisects").map(|found| found.index), Some(5));
        let w = std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open");
        FileExt::write_all_at(&w, &0u64.to_le_bytes(), e[2] + O_SIZE).expect("corrupt");
        let f = JournalFile::open(t.path()).expect("opens");
        let got = f.bisect_list(&f.main_list(), realtime(25), &mut cache, &mut met);
        assert_eq!(got.expect("bisects").map(|found| found.index), Some(1));
        assert_eq!(met.errors.len(), 1, "{met:?}");
    }

    /// The system's error reading a probed entry (here the file cut short
    /// after it was opened) stops the bisect, where an entry that isn't
    /// well-formed only cuts its array short.
    #[test]
    fn the_systems_error_stops_the_bisect() {
        let (t, f, e) = bisect_file(&[10, 20, 30, 40], &[4]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(e[2])
            .expect("truncate");
        let mut met = Met::default();
        let got = f.bisect_list(
            &f.main_list(),
            realtime(35),
            &mut ChainCache::default(),
            &mut met,
        );
        let err = got.expect_err("stopped");
        assert!(err.is_io(), "{err:?}");
    }

    /// The same with the array intact and the probed entry cut off: the
    /// system's error reading the entry the bisect tests stops it, where an
    /// entry that isn't well-formed would only cut the array short there.
    #[test]
    fn the_systems_error_reading_a_probed_entry_stops_the_bisect() {
        let (mut c, l) = journal(4);
        let (t, f) = c.write();
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(l.entries[2] + 8)
            .expect("truncate");
        let mut met = Met::default();
        let got = f.bisect_list(
            &f.main_list(),
            realtime(1_000_002),
            &mut ChainCache::default(),
            &mut met,
        );
        let err = got.expect_err("stopped");
        assert!(err.is_io(), "{err:?}");
    }

    /// The cache holds at most 20 chains, the oldest given up first; an
    /// entry for a chain is updated in place.
    #[test]
    fn the_chain_cache_is_bounded() {
        let item = |first| ChainItem {
            first,
            array: first + 8,
            begin: 1,
            total: 1,
            last_index: 0,
        };
        let mut cache = ChainCache::default();
        for first in 1..=21 {
            cache.put(item(first));
        }
        assert_eq!(cache.items.len(), CHAIN_CACHE_MAX);
        assert!(cache.get(1).is_none() && cache.get(21).is_some());
        cache.put(ChainItem {
            total: 9,
            ..item(21)
        });
        assert_eq!(cache.get(21).map(|c| c.total), Some(9));
        assert_eq!(cache.items.len(), CHAIN_CACHE_MAX);
    }

    /// An item of 0 cuts its array short there, unreported: a needle left
    /// of everything before it lands on the first; one right of it finds
    /// nothing. A zero at the left bound ends the bisect.
    #[test]
    fn a_zero_item_cuts_the_array_short() {
        let (t, _f, e) = bisect_file(&[10, 20, 30, 40], &[4]);
        let at = JournalFile::open(t.path())
            .expect("opens")
            .header
            .entry_array_offset
            + ENTRY_ARRAY_HEADER;
        let zero = |items: &[usize]| {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(t.path())
                .expect("open");
            for &i in items {
                FileExt::write_all_at(&f, &[0; 8], at + 8 * i as u64).expect("zero");
            }
            JournalFile::open(t.path()).expect("opens")
        };
        let f = zero(&[1]);
        let mut met = Met::default();
        let mut cache = ChainCache::default();
        assert_eq!(
            f.bisect_list(&f.main_list(), realtime(5), &mut cache, &mut met)
                .expect("ok"),
            Some(Found {
                offset: e[0],
                index: 0
            })
        );
        assert_eq!(
            f.bisect_list(&f.main_list(), realtime(15), &mut cache, &mut met)
                .expect("ok"),
            None
        );
        // A zero slot is cut at without a word (Difference 8).
        assert!(met.errors.is_empty(), "{met:?}");
        let f = zero(&[0]);
        assert_eq!(
            f.bisect_list(&f.main_list(), realtime(5), &mut cache, &mut met)
                .expect("ok"),
            None
        );
    }

    /// A DATA object's list: its in-line entry tested first, the result
    /// when at or after the needle (index 0); else the chain, indexes after
    /// it. One that can't be read is the bisect's error. An empty list
    /// finds nothing.
    #[test]
    fn a_data_lists_in_line_entry_comes_first() {
        let (_t, f, e) = bisect_file(&[10, 20, 30, 40], &[4]);
        let chain = f.header.entry_array_offset;
        let d = DataRef {
            offset: 0,
            n_entries: 4,
            entry_offset: e[0],
            entry_array_offset: chain,
        };
        let list = EntryList::of_data(&d);
        let mut met = Met::default();
        let mut at = |needle| {
            f.bisect_list(
                &list,
                realtime(needle),
                &mut ChainCache::default(),
                &mut met,
            )
            .map(|r| r.map(|f| f.index))
        };
        assert_eq!(at(10).expect("ok"), Some(0));
        assert_eq!(at(5).expect("ok"), Some(0));
        // The chain holds n - 1 items: 10, 20, 30.
        assert_eq!(at(25).expect("ok"), Some(3));
        assert_eq!(at(35).expect("ok"), None);
        let bad = EntryList::of_data(&DataRef {
            entry_offset: 8,
            ..d
        });
        assert!(
            f.bisect_list(&bad, realtime(5), &mut ChainCache::default(), &mut met)
                .is_err()
        );
        let empty = EntryList::of_data(&DataRef { n_entries: 0, ..d });
        assert_eq!(
            f.bisect_list(&empty, realtime(5), &mut ChainCache::default(), &mut met)
                .expect("ok"),
            None
        );
    }

    /// The forward reader: from an index, the extra first, then the chain
    /// across arrays; a zero and a repeat are skipped; an item lower than
    /// the one before leaves the rest of its array, reported, and the next
    /// array is read.
    #[test]
    fn entries_are_read_forward_from_an_index() {
        let (_t, f, e) = bisect_file(&[10, 20, 30, 40, 50, 60], &[2, 4]);
        let fwd = |list: &EntryList, from| -> Vec<std::result::Result<u64, String>> {
            f.entries_from(list, from)
                .map(|r| r.map_err(|e| e.to_string()))
                .collect()
        };
        assert_eq!(
            fwd(&f.main_list(), 1),
            (1..6).map(|i| Ok(e[i])).collect::<Vec<_>>()
        );
        assert_eq!(fwd(&f.main_list(), 6), []);
        let chain = f.header.entry_array_offset;
        let d = DataRef {
            offset: 0,
            n_entries: 4,
            entry_offset: e[0],
            entry_array_offset: chain,
        };
        let list = EntryList::of_data(&d);
        // The test reuses the main chain, whose first item is the extra:
        // a repeat, read once.
        assert_eq!(fwd(&list, 0), [Ok(e[0]), Ok(e[1]), Ok(e[2])]);
        assert_eq!(fwd(&list, 2), [Ok(e[1]), Ok(e[2])]);
        // An in-line entry that can't be an entry's offset is refused.
        let bad = EntryList::of_data(&DataRef {
            entry_offset: 8,
            ..d
        });
        let got = fwd(&bad, 0);
        assert!(
            got[0]
                .as_ref()
                .is_err_and(|m| m.contains("not an entry offset")),
            "{got:?}"
        );
        // A zero, a repeat and an item out of order.
        let (_t, g) = file_with_chain(&[&[e[0], 0], &[e[1], e[1], e[0], e[3]], &[e[4]]]);
        let chain = EntryList {
            extra: None,
            first: 272,
            n: 7,
        };
        let got = g
            .entries_from(&chain, 0)
            .map(|r| r.map_err(|e| e.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(got.len(), 4, "{got:?}");
        assert_eq!(got[..2], [Ok(e[0]), Ok(e[1])]);
        assert!(
            got[2]
                .as_ref()
                .is_err_and(|m| m.contains("is out of order")),
            "{got:?}"
        );
        assert_eq!(got[3], Ok(e[4]));
    }

    fn file_with_array(items: &[u64]) -> (tempfile::NamedTempFile, JournalFile) {
        file_with_chain(&[items])
    }

    /// A regular file whose arena is exactly `object`, at 272.
    fn file_with_object(object: &[u8]) -> (tempfile::NamedTempFile, JournalFile) {
        let mut c = Crafted::new();
        c.b.extend_from_slice(object);
        c.write()
    }

    /// An object header: type and size.
    fn object_bytes(kind: u8, size: u64, len: usize) -> Vec<u8> {
        let mut o = vec![0u8; len];
        o[0] = kind;
        o[8..16].copy_from_slice(&size.to_le_bytes());
        o
    }

    /// An entry smaller than the read guess, ending exactly at the arena's
    /// end, reads whole.
    #[test]
    fn an_object_ending_at_the_arena_end_reads() {
        let mut o = object_bytes(OBJECT_ENTRY, 80, 80);
        o[16..24].copy_from_slice(&7u64.to_le_bytes()); // seqnum
        o[64..72].copy_from_slice(&4242u64.to_le_bytes()); // item
        let (_t, f) = file_with_object(&o);
        let e = f.entry(272).expect("reads");
        assert_eq!((e.seqnum, e.items), (7, vec![4242]));
    }

    #[test]
    fn an_object_claiming_a_size_past_the_arena_is_refused() {
        let (_t, f) = file_with_object(&object_bytes(OBJECT_ENTRY, 4096, 80));
        let err = f.entry(272).expect_err("refused");
        assert!(err.to_string().contains("bad size"));
    }

    #[test]
    fn an_object_with_no_room_for_its_header_is_refused() {
        let (_t, f) = file_with_object(&[0u8; 8]);
        let err = f.entry(272).expect_err("refused");
        assert!(err.to_string().contains("outside the arena"));
    }

    #[test]
    fn an_unknown_incompatible_flag_names_the_file() {
        let (t, _f) = file_with_array(&[]);
        let mut b = std::fs::read(t.path()).expect("read");
        b[12..16].copy_from_slice(&(1u32 << 5).to_le_bytes());
        std::fs::write(t.path(), &b).expect("write");
        let err = JournalFile::open(t.path()).expect_err("refused");
        assert!(err.to_string().contains("unsupported journal format"));
        // Named by the typed error, and first in its message.
        assert_eq!(err.path(), Some(t.path()));
        assert!(
            err.to_string()
                .starts_with(&format!("{}: ", t.path().display()))
        );
    }

    // Bad object sizes.

    #[test]
    fn an_object_of_size_zero_is_refused() {
        let (mut c, l) = journal(3);
        c.set(l.entries[1] + O_SIZE, 0);
        let (_t, f) = c.write();
        assert!(f.entry(l.entries[1]).is_err());
    }

    #[test]
    fn an_object_size_that_overflows_is_refused() {
        let (mut c, l) = journal(3);
        c.set(l.entries[1] + O_SIZE, u64::MAX);
        c.set(l.messages[1] + O_SIZE, u64::MAX - 7);
        let (_t, f) = c.write();
        assert!(f.entry(l.entries[1]).is_err());
        assert!(f.data_payload(l.messages[1]).is_err());
    }

    #[test]
    fn an_entry_shorter_than_its_header_is_refused() {
        let (mut c, l) = journal(3);
        c.set(l.entries[1] + O_SIZE, ENTRY_HEADER - 8);
        let (_t, f) = c.write();
        assert!(f.entry(l.entries[1]).is_err());
    }

    #[test]
    fn a_data_object_shorter_than_its_header_is_refused() {
        let (mut c, l) = journal(3);
        c.set(l.messages[1] + O_SIZE, DATA_HEADER - 8);
        let (_t, f) = c.write();
        assert!(f.data_payload(l.messages[1]).is_err());
    }

    // Links pointing backwards, into an object, or outside the arena.

    #[test]
    fn a_hash_chain_linking_backwards_or_into_its_object_is_refused() {
        // Every DATA object chained in one bucket.
        let mut c = Crafted::new();
        c.hash_table(1);
        let a = c.data(b"A=1");
        let b = c.data(b"B=2");
        for next in [a, b, b + 16] {
            let mut c2 = Crafted { b: c.b.clone() };
            c2.set(b + D_NEXT_HASH, next);
            let (_t, f) = c2.write();
            assert!(f.find_data(b"C=3").is_err(), "next {next:#x}");
            assert!(f.all_data().is_err(), "next {next:#x}");
        }
    }

    #[test]
    fn a_hash_chain_leaving_the_arena_is_refused() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let a = c.data(b"A=1");
        let end = c.b.len() as u64;
        c.set(a + D_NEXT_HASH, end + 64);
        let (_t, f) = c.write();
        assert!(f.find_data(b"C=3").is_err());
    }

    // Offsets that overflow.

    #[test]
    fn offsets_near_the_top_of_u64_are_refused() {
        let (mut c, _l) = journal(3);
        let top = !7u64;
        let (_t, f) = c.write();
        for at in [top, top - 8, top - 16] {
            assert!(f.entry(at).is_err());
            assert!(f.data_payload(at).is_err());
        }
    }

    #[test]
    fn a_hash_table_offset_that_overflows_is_refused() {
        let top = !15u64;
        for (offset, size) in [(top, 16), (top, top), (400, top)] {
            let (mut c, _l) = journal(3);
            c.set(H_HASH_TABLE_OFFSET, offset);
            c.set(H_HASH_TABLE_SIZE, size);
            let (_t, f) = c.write();
            assert!(f.find_data(b"U=x").is_err(), "{offset:#x} {size:#x}");
            assert!(f.all_data().is_err(), "{offset:#x} {size:#x}");
        }
    }

    // A growing or truncated file.

    /// journald appends while we read: an object past the arena as it
    /// was at open, named by a link read later, is read once the file and
    /// its arena have grown, as sd-journal re-stats.
    #[test]
    fn an_object_appended_after_open_is_read() {
        let (mut c, _l) = journal(2);
        let (t, f) = c.write();
        let late = c.data(b"LATE=1");
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        std::fs::write(t.path(), &c.b).expect("grow");
        assert_eq!(f.data_payload(late).expect("read after growth"), b"LATE=1");
    }

    /// New in the crate: a file cut short after it was opened (a vacuum,
    /// or a copy cut off) is an I/O error from pread, never a panic or a
    /// SIGBUS; once the header is read again, what lies before the cut
    /// reads.
    #[test]
    fn a_file_truncated_after_open_is_an_io_error() {
        let (mut c, l) = journal(3);
        let (t, f) = c.write();
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(l.messages[2] + 8)
            .expect("truncate");
        let err = f.data_payload(l.messages[2]).expect_err("cut");
        let ReadErrorKind::Io { context, source } = err.kind() else {
            panic!("not an I/O error: {err}");
        };
        assert_eq!(context, &format!("read at {:#x}", l.messages[2]));
        assert_eq!(source.kind(), std::io::ErrorKind::UnexpectedEof);
        // A read near the cut can reach past it until the header is read
        // again, which caps the arena at the file's new length.
        let mut f = f;
        f.refresh().expect("refresh");
        assert_eq!(
            f.data_payload(l.messages[1]).expect("before the cut"),
            b"MESSAGE=m1"
        );
        let err = f.data_payload(l.messages[2]).expect_err("past the cut");
        assert!(err.to_string().contains("outside the arena"), "{err}");
    }

    #[test]
    fn a_file_cut_inside_its_header_is_refused() {
        let (mut c, _l) = journal(1);
        let (t, _f) = c.write();
        for len in [0, 8, 207] {
            std::fs::OpenOptions::new()
                .write(true)
                .open(t.path())
                .expect("open")
                .set_len(len)
                .expect("truncate");
            assert!(JournalFile::open(t.path()).is_err(), "{len}");
        }
    }

    // Bad hash-table sizes.

    #[test]
    fn a_hash_table_inside_the_header_is_refused() {
        let (mut c, _l) = journal(3);
        c.set(H_HASH_TABLE_OFFSET, 16);
        let (_t, f) = c.write();
        assert!(f.find_data(b"U=x").is_err());
    }

    /// No table at all (offset and size zero) is valid: nothing is found.
    #[test]
    fn no_hash_table_finds_nothing() {
        let (mut c, _l) = journal(3);
        c.set(H_HASH_TABLE_OFFSET, 0);
        c.set(H_HASH_TABLE_SIZE, 0);
        let (_t, f) = c.write();
        assert!(f.find_data(b"U=x").expect("valid").is_none());
        assert_eq!(f.all_data().expect("valid"), [] as [(u64, u64); 0]);
    }

    /// journald puts a DATA object in bucket `hash % buckets`; one found in
    /// another bucket is corruption. Checking it also stops many buckets
    /// sharing one long chain, which would make `all_data` quadratic.
    #[test]
    fn a_data_object_in_the_wrong_bucket_is_corruption() {
        let (mut c, l) = journal(3);
        let table = c.get(H_HASH_TABLE_OFFSET);
        let right = c.get(l.shared + D_HASH) % 7;
        let wrong = (right + 1) % 7;
        c.set(table + wrong * 16, l.shared);
        let (_t, f) = c.write();
        assert!(f.all_data().is_err());
    }

    // Compressed payloads.

    /// An LZ4 payload's first 8 bytes claim its decompressed size. LZ4
    /// expands at most 255 times (each length byte adds at most 255), so
    /// a claim past that is refused before anything is allocated.
    #[test]
    fn an_lz4_size_claim_beyond_what_the_block_can_hold_is_refused() {
        for claim in [700u64 << 20, (DATA_SIZE_MAX as u64) + 1, u64::MAX] {
            let mut raw = claim.to_le_bytes().to_vec();
            raw.extend_from_slice(&[0x10, b'x', 0, 0, 0]);
            let err = decompress(COMPRESSED_LZ4, raw).expect_err("refused");
            assert!(err.to_string().contains("lz4 payload claims"), "{err}");
        }
    }

    #[test]
    fn a_zstd_frame_claiming_a_huge_window_is_refused() {
        // Magic, a descriptor with an 8-byte content size and no single
        // segment, a window descriptor of 2^41 bytes, the content size,
        // then an empty raw last block.
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0xc0, 0xf8];
        raw.extend_from_slice(&(1u64 << 41).to_le_bytes());
        raw.extend_from_slice(&[0x01, 0x00, 0x00]);
        assert!(decompress(COMPRESSED_ZSTD, raw).is_err());
    }

    /// Difference 5: a frame whose window is over 128 MiB is refused even
    /// when its declared content would allow twice as much. Window
    /// descriptor 0x8a: 2^27 plus 2/8 of it, 160 MiB; a 4-byte content
    /// size of 100 MiB, which 800 RLE blocks of 128 KiB can hold.
    #[test]
    fn a_zstd_window_over_128_mib_is_refused() {
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x8a];
        raw.extend_from_slice(&(100u32 << 20).to_le_bytes());
        for i in 0..800u32 {
            let last = u32::from(i == 799);
            raw.extend_from_slice(&(((128 << 10) << 3) | (1 << 1) | last).to_le_bytes()[..3]);
            raw.push(b'x');
        }
        assert_eq!(
            zstd_header(&raw).map(|h| h.window),
            Some(160 << 20),
            "the frame as described"
        );
        let mut cost = 0;
        // Not `expect_err`: a decoded payload is 100 MiB of Debug output.
        let Err(err) = decompress_within(COMPRESSED_ZSTD, raw, 256 << 20, &mut cost) else {
            panic!("a frame with a 160 MiB window was decoded");
        };
        assert!(!is_over(&err), "{err}");
        assert!(err.to_string().starts_with("zstd: "), "{err}");
        assert_eq!(cost, 0, "nothing decoded");
    }

    /// A short message as `xz --format=xz --check=none` writes it.
    fn xz_sample() -> Vec<u8> {
        let hex = concat!(
            // Stream header, block header.
            "fd377a585a000000ff12d9410200210116000000742fe5a3",
            // One LZMA2 chunk, its end and the block padding.
            "01000c4d4553534147453d68656c6c6f00000000",
            // Index, stream footer.
            "00011d0d8aa55ba106729e7a010000000000595a",
        );
        hex::decode(hex).expect("hex")
    }

    #[test]
    fn the_xz_sample_decodes() {
        let out = decompress(COMPRESSED_XZ, xz_sample()).expect("xz");
        assert_eq!(out, b"MESSAGE=hello");
    }

    /// The block header's LZMA2 dictionary size raised to 3 GiB (property
    /// 39), its CRC re-sealed: the decoder's memory limit refuses it
    /// before the dictionary is allocated.
    #[test]
    fn an_xz_dictionary_beyond_the_memory_limit_is_refused() {
        let mut raw = xz_sample();
        raw[16] = 39;
        let crc = crc32fast::hash(&raw[12..20]);
        raw[20..24].copy_from_slice(&crc.to_le_bytes());
        let err = decompress(COMPRESSED_XZ, raw).expect_err("refused");
        assert!(err.to_string().contains("xz"), "{err}");
        // New in the crate: refused by the memory limit itself, in
        // lzma-rust2 0.25.0's words (src/xz/reader.rs).
        assert!(
            err.to_string()
                .contains("needed memory too big for mem_limit_kb"),
            "{err}"
        );
    }

    /// `MESSAGE=compress me, compress me, compress me ` 20 times, as
    /// libzstd writes it (`zstd --no-check`, as journald's `ZSTD_compress`
    /// does: the content size declared, no checksum). ruzstd's own encoder
    /// leaves the content size out, which journald never does.
    fn zstd_sample() -> Vec<u8> {
        let hex = concat!(
            "28b52ffd609802050100b04d4553534147453d636f6d",
            "7072657373206d652c2020020067a3aa44002713",
        );
        hex::decode(hex).expect("hex")
    }

    /// Every single-byte corruption and every truncation of a compressed
    /// payload decodes to an error or to some bounded output; none panics.
    #[test]
    fn corrupted_compressed_payloads_fail_cleanly() {
        let text = b"MESSAGE=compress me, compress me, compress me ".repeat(20);
        let mut lz4 = (text.len() as u64).to_le_bytes().to_vec();
        lz4.extend_from_slice(&lz4_flex::block::compress(&text));
        let zstd = zstd_sample();
        assert_eq!(decompress(COMPRESSED_LZ4, lz4.clone()).expect("lz4"), text);
        assert_eq!(
            decompress(COMPRESSED_ZSTD, zstd.clone()).expect("zstd"),
            text
        );
        let samples = [
            (COMPRESSED_LZ4, lz4),
            (COMPRESSED_ZSTD, zstd),
            (COMPRESSED_XZ, xz_sample()),
        ];
        for (flag, good) in samples {
            for i in 0..good.len() {
                for x in [0x01, 0x80, 0xff] {
                    let mut bad = good.clone();
                    bad[i] ^= x;
                    if let Ok(out) = decompress(flag, bad) {
                        assert!(out.len() <= 64 * 1024, "flag {flag} byte {i}");
                    }
                }
                if let Ok(out) = decompress(flag, good[..i].to_vec()) {
                    assert!(out.len() <= 64 * 1024, "flag {flag} cut at {i}");
                }
            }
        }
    }

    /// A payload over the caller's limit is told apart from corruption,
    /// through the error's kind, for every encoding.
    #[test]
    fn a_payload_over_the_limit_is_told_apart_from_corruption() {
        let text = b"MESSAGE=over the limit".to_vec();
        let mut lz4 = (text.len() as u64).to_le_bytes().to_vec();
        lz4.extend_from_slice(&lz4_flex::block::compress(&text));
        let zstd = zstd_sample();
        let mut c = Crafted::new();
        c.hash_table(3);
        let plain = c.data(&text);
        let lz4_data = c.data_raw(COMPRESSED_LZ4, &lz4, hash::jenkins64(&text));
        let zstd_data = c.data_raw(COMPRESSED_ZSTD, &zstd, hash::jenkins64(&text));
        let xz_data = c.data_raw(
            COMPRESSED_XZ,
            &xz_sample(),
            hash::jenkins64(b"MESSAGE=hello"),
        );
        let (_t, f) = c.write();
        for d in [plain, lz4_data, zstd_data, xz_data] {
            let err = f.data_payload_within(d, 5).expect_err("over");
            assert!(err.is_over_limit(), "{err}");
            assert!(f.data_payload_within(d, 1000).is_ok());
        }
        let mut bad = lz4.clone();
        bad[8] ^= 0xff;
        assert!(!is_over(
            &decompress_within(COMPRESSED_LZ4, bad, 100, &mut 0).expect_err("bad")
        ));
    }

    /// New in the crate: a candidate of the same length, read whole, is
    /// compared by payload, not taken on its hash.
    #[test]
    fn a_lookup_passes_over_another_value_with_the_same_hash() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let want = b"U=x";
        c.data_raw(0, b"U=y", hash::jenkins64(want));
        let (_t, f) = c.write();
        assert!(f.find_data(want).expect("lookup").is_none());
    }

    /// A lookup reads each candidate with the looked-for hash only as far
    /// as the value: a longer one, or a bomb, isn't decoded whole, and
    /// isn't an error.
    #[test]
    fn a_lookup_passes_over_a_longer_object_with_the_same_hash() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let want = b"U=x";
        c.data_raw(0, b"U=xxxxxxxxxxxx", hash::jenkins64(want));
        let d = c.data(want);
        let (_t, f) = c.write();
        let found = f.find_data(want).expect("lookup").expect("found");
        assert_eq!(found.offset, d);
    }

    /// The sizes, read as RFC 8878 lays the frame header out.
    #[test]
    fn zstd_sizes_are_read_from_the_frame_header() {
        let magic = [0x28, 0xb5, 0x2f, 0xfd];
        let frame = |rest: &[u8]| [&magic[..], rest].concat();
        let h = |window, content| Some(ZstdHeader { window, content });
        // Window descriptor (exponent 7, mantissa 2: 128 KiB + 2/8 of
        // it), no content size.
        assert_eq!(zstd_header(&frame(&[0x00, 0x3a])), h(160 * 1024, None));
        // Window descriptor, then a 4-byte content size.
        assert_eq!(
            zstd_header(&frame(&[0x80, 0x00, 5, 0, 0, 0])),
            h(1024, Some(5))
        );
        // Single segment: 1-byte, 2-byte (plus 256) content sizes.
        assert_eq!(zstd_header(&frame(&[0x20, 200])), h(200, Some(200)));
        assert_eq!(zstd_header(&frame(&[0x60, 0x00, 0x01])), h(512, Some(512)));
        // Single segment, a 1-byte dictionary id, then 4 bytes.
        assert_eq!(zstd_header(&frame(&[0xa1, 9, 1, 0, 0, 0])), h(1, Some(1)));
        assert_eq!(zstd_header(&frame(&[0xe0, 1, 2])), None);
        assert_eq!(zstd_header(&zstd_sample()), h(920, Some(920)));
    }

    /// A frame declaring 64 KiB and then decoding RLE blocks on and on is
    /// corrupt: decoded only to its declared size, reported as an error,
    /// not as a spent output limit, and spending little.
    #[test]
    fn a_zstd_frame_decoding_past_its_declared_size_is_corrupt() {
        // Window 128 KiB, a 4-byte content size of 64 KiB, then 100 RLE
        // blocks of 128 KiB each (type 1, the byte repeated).
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x38];
        raw.extend_from_slice(&(64u32 << 10).to_le_bytes());
        for i in 0..100u32 {
            let last = u32::from(i == 99);
            raw.extend_from_slice(&(((128 << 10) << 3) | (1 << 1) | last).to_le_bytes()[..3]);
            raw.push(b'x');
        }
        let mut cost = 0;
        let err = decompress_within(COMPRESSED_ZSTD, raw, 256 << 20, &mut cost).expect_err("long");
        assert!(!is_over(&err), "{err}");
        assert!(cost < 1 << 20, "spent {cost}");
    }

    /// A declared size the stored bytes can't hold is corruption.
    #[test]
    fn a_zstd_size_claim_beyond_the_stored_bytes_is_corruption() {
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0];
        raw.extend_from_slice(&(100u32 << 20).to_le_bytes());
        raw.extend_from_slice(&[0x01, 0x00, 0x00]);
        let err = decompress_within(COMPRESSED_ZSTD, raw, 256 << 20, &mut 0).expect_err("claim");
        assert!(!is_over(&err), "{err}");
    }

    /// An entry with no items, or no sequence number, is corruption, as
    /// sd-journal has it.
    #[test]
    fn an_entry_without_items_or_seqnum_is_refused() {
        let (mut c, l) = journal(2);
        let empty = c.entry(7, &[]);
        c.set(l.entries[1] + 16, 0);
        let (_t, f) = c.write();
        assert!(f.entry(empty).is_err());
        assert!(f.entry(l.entries[1]).is_err());
        assert!(f.entry(l.entries[0]).is_ok());
    }

    /// A frame decoding to another length than it declares is corrupt,
    /// as libzstd and sd-journal have it; so is one declaring none.
    #[test]
    fn a_zstd_frame_must_decode_to_its_declared_size() {
        // Window 1 KiB, 4-byte content size, one raw last block of 3.
        let frame = |declared: u8| {
            let mut f = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x00, declared, 0, 0, 0];
            f.extend_from_slice(&((3u32 << 3) | 1).to_le_bytes()[..3]);
            f.extend_from_slice(b"A=1");
            f
        };
        assert_eq!(decompress(COMPRESSED_ZSTD, frame(3)).expect("good"), b"A=1");
        assert!(decompress(COMPRESSED_ZSTD, frame(2)).is_err());
        assert!(decompress(COMPRESSED_ZSTD, frame(9)).is_err());
        let mut none = frame(3);
        none[4] = 0x00;
        none.drain(6..10);
        assert!(decompress(COMPRESSED_ZSTD, none).is_err());
    }

    /// A trailing part item, or an entry running into the next one, is
    /// corruption; the overlap is refused before the body is read.
    #[test]
    fn entries_with_part_items_or_overlapping_the_next_are_refused() {
        let (mut c, l) = journal(3);
        c.set(l.entries[0] + O_SIZE, c.get(l.entries[0] + O_SIZE) + 8);
        let (_t, f) = c.write();
        assert!(f.entry(l.entries[0]).is_err());
        assert!(f.entry_before(l.entries[1], l.entries[1] + 70).is_err());
        assert!(f.entry_before(l.entries[1], l.entries[2]).is_ok());
    }

    #[test]
    fn two_compression_flags_are_refused() {
        let both = COMPRESSED_LZ4 | COMPRESSED_ZSTD;
        assert!(decompress(both, b"x".to_vec()).is_err());
    }

    // New in the crate: paths the prototype's file tests left to the
    // container matrix or the query tests.

    fn set_incompatible(c: &mut Crafted, flags: u32) {
        c.b[12..16].copy_from_slice(&flags.to_le_bytes());
    }

    /// New in the crate: a compact file's entry arrays hold 4-byte items,
    /// walked back and read forward alike.
    #[test]
    fn a_compact_files_entry_arrays_hold_four_byte_items() {
        let mut c = Crafted::new();
        set_incompatible(&mut c, INCOMPAT_COMPACT);
        let items = [0x1000u32, 0x1008, 0x1010];
        let second_body: Vec<u8> =
            [0u64.to_le_bytes().as_slice(), &0x1018u32.to_le_bytes()].concat();
        // The first array: its object header, next link and three 4-byte
        // items, padded to 8.
        let second_at: u64 = 272 + (OBJECT_HEADER + 8 + 3 * 4).next_multiple_of(8);
        let mut body = second_at.to_le_bytes().to_vec();
        for i in items {
            body.extend_from_slice(&i.to_le_bytes());
        }
        let first = c.object(OBJECT_ENTRY_ARRAY, 0, &body);
        let second = c.object(OBJECT_ENTRY_ARRAY, 0, &second_body);
        assert_eq!((first, second), (272, second_at));
        c.b.resize(0x2000, 0);
        c.set(H_N_ENTRIES, 4);
        c.set(H_ENTRY_ARRAY_OFFSET, first);
        let (_t, f) = c.write();
        let walked: Vec<u64> = f.all_entries_rev().collect::<Result<_>>().expect("walk");
        assert_eq!(walked, vec![0x1018, 0x1010, 0x1008, 0x1000]);
        let mut cursor = None;
        assert_eq!(
            f.entries_since(&mut cursor, 1).expect("forward"),
            vec![0x1008, 0x1010, 0x1018]
        );
    }

    /// A compact file's entries list 4-byte items, and its DATA payloads
    /// start after a 72-byte header.
    #[test]
    fn a_compact_file_reads_its_entries_and_payloads() {
        let mut c = Crafted::new();
        set_incompatible(&mut c, INCOMPAT_COMPACT);
        c.hash_table(1);
        let payload = b"MESSAGE=compact";
        let mut body = hash::jenkins64(payload).to_le_bytes().to_vec();
        body.resize(
            usize::try_from(DATA_HEADER_COMPACT - OBJECT_HEADER).expect("small"),
            0,
        );
        body.extend_from_slice(payload);
        let d = c.object(OBJECT_DATA, 0, &body);
        let mut entry = vec![0u8; 48];
        entry[0..8].copy_from_slice(&9u64.to_le_bytes()); // seqnum
        for item in [d, 0x1234_5678] {
            entry.extend_from_slice(&u32::try_from(item).expect("small").to_le_bytes());
        }
        let e = c.object(OBJECT_ENTRY, 0, &entry);
        let (_t, f) = c.write();
        assert!(f.header.compact());
        let got = f.entry(e).expect("entry");
        assert_eq!((got.seqnum, got.items), (9, vec![d, 0x1234_5678]));
        assert_eq!(f.data_payload(d).expect("payload"), payload);
        // A compact entry's size is checked in 4-byte items: one more
        // byte is a part item.
        let mut bad = Crafted { b: c.b.clone() };
        bad.set(e + O_SIZE, bad.get(e + O_SIZE) + 1);
        let (_t, f) = bad.write();
        assert!(f.entry(e).is_err());
    }

    /// A keyed file hashes with siphash24 keyed by its file id, and its
    /// lookups find what is stored under that hash.
    #[test]
    fn a_keyed_file_hashes_with_its_file_id() {
        let mut c = Crafted::new();
        set_incompatible(&mut c, INCOMPAT_KEYED_HASH);
        let file_id: [u8; 16] = std::array::from_fn(|i| u8::try_from(i).expect("small"));
        c.b[24..40].copy_from_slice(&file_id);
        c.hash_table(5);
        let payload = b"MESSAGE=keyed";
        let d = c.data_raw(0, payload, hash::keyed(&file_id, payload));
        let (_t, f) = c.write();
        assert!(f.header.keyed());
        assert_eq!(f.hash(payload), hash::keyed(&file_id, payload));
        assert_ne!(f.hash(payload), hash::jenkins64(payload));
        assert_eq!(
            f.find_data(payload).expect("lookup").map(|r| r.offset),
            Some(d)
        );
    }

    /// The header fields read as journal-def.h lays them out.
    #[test]
    fn the_header_fields_are_read() {
        let (mut c, l) = journal(2);
        c.b[16] = STATE_ARCHIVED;
        c.b[24..40].copy_from_slice(&[0xab; 16]);
        c.b[72..88].copy_from_slice(&[0xcd; 16]);
        let (_t, f) = c.write();
        let header = &f.header;
        assert!(header.archived() && !header.compact() && !header.keyed());
        assert_eq!(hex_id(&header.file_id), "ab".repeat(16));
        assert_eq!(header.seqnum_id, [0xcd; 16]);
        assert_eq!(header.size, 272);
        assert_eq!(header.n_entries, 2);
        assert_eq!(header.entry_array_offset, l.main_array);
        let entry = f.entry(l.entries[1]).expect("entry");
        assert_eq!(
            (
                entry.realtime,
                entry.monotonic,
                entry.boot_id,
                entry.xor_hash
            ),
            (1_000_002, 2, [7; 16], 0)
        );
        assert_eq!(f.metadata().expect("fstat").len(), c.b.len() as u64);
        let (_t, f) = journal(1).0.write();
        assert!(!f.header.archived());
    }

    /// The header fields the boot table reads (journal-def.h `struct
    /// Header`): `tail_entry_offset` only from a header that holds it
    /// (`JOURNAL_HEADER_CONTAINS`, journal-file.h v255: `header_size` at
    /// least its end, 272).
    #[test]
    fn the_tail_header_fields_are_read() {
        let (mut c, _l) = journal(1);
        c.b[8..12].copy_from_slice(&COMPATIBLE_TAIL_ENTRY_BOOT_ID.to_le_bytes());
        c.b[40..56].copy_from_slice(&[0x4d; 16]);
        c.b[56..72].copy_from_slice(&[0xb0; 16]);
        c.set(136, 0x1111);
        c.set(192, 0x2222);
        c.set(200, 0x3333);
        c.set(264, 0x4444);
        let (_t, f) = c.write();
        let h = &f.header;
        assert_eq!(h.compatible, COMPATIBLE_TAIL_ENTRY_BOOT_ID);
        assert_eq!(
            (h.machine_id, h.tail_entry_boot_id),
            ([0x4d; 16], [0xb0; 16])
        );
        assert_eq!(
            (
                h.tail_object_offset,
                h.tail_entry_realtime,
                h.tail_entry_monotonic
            ),
            (0x1111, 0x2222, 0x3333)
        );
        assert_eq!(h.tail_entry_offset, Some(0x4444));
        c.set(88, 271);
        let (_t, f) = c.write();
        assert_eq!(f.header.tail_entry_offset, None);
    }

    /// One crafted file for the tail rule: entries of boots 0xa then 0xb
    /// (monotonic 10 and 20, realtime 100 and 200), the header's
    /// `tail_entry_*` fields naming a third, made-up tail (boot 0xc,
    /// monotonic 30, realtime 300), machine 0x4d. Returns the file's bytes
    /// and its entries.
    fn tail_file() -> (Crafted, Vec<u64>) {
        let (mut c, entries) = journal_of(&[&[], &[]]);
        for (&e, (boot, mono, real)) in entries.iter().zip([(0xa, 10, 100), (0xb, 20, 200)]) {
            c.set(e + 24, real);
            c.set(e + 32, mono);
            c.b[at(e + 40)..at(e + 56)].copy_from_slice(&[boot; 16]);
        }
        c.b[40..56].copy_from_slice(&[0x4d; 16]);
        c.b[56..72].copy_from_slice(&[0xc; 16]);
        c.set(192, 300);
        c.set(200, 30);
        (c, entries)
    }

    fn at(v: u64) -> usize {
        usize::try_from(v).expect("small")
    }

    /// The main list's newest entry, as the page reads it (`walk::lasts`).
    fn last_of(f: &JournalFile) -> Option<(u64, Entry)> {
        let off = f.all_entries_rev().next()?.expect("reads");
        Some((off, f.entry(off).expect("entry")))
    }

    fn tail(boot: u8, monotonic: u64, realtime: u64) -> Tail {
        Tail {
            boot_id: [boot; 16],
            monotonic,
            realtime,
            machine_id: [0x4d; 16],
        }
    }

    /// sd-journal.c v255/v259 `journal_file_read_tail_timestamp`: the
    /// entry `tail_entry_offset` names, with the header's machine id.
    /// Read again when it isn't the main list's newest; reused, with no
    /// pread, when it is.
    #[test]
    fn the_tail_is_the_entry_the_header_names() {
        let (mut c, e) = tail_file();
        c.set(264, e[1]);
        let t = c.file();
        let opener = Opener::default();
        let f = JournalFile::open_with(t.path(), &opener).expect("opens");
        let last = last_of(&f);
        let before = opener.counts().preads;
        assert_eq!(f.tail(last.as_ref()), Some(tail(0xb, 20, 200)));
        assert_eq!(opener.counts().preads, before, "reused, not read again");
        c.set(264, e[0]);
        let t = c.file();
        let f = JournalFile::open_with(t.path(), &opener).expect("opens");
        let last = last_of(&f);
        let before = opener.counts().preads;
        assert_eq!(f.tail(last.as_ref()), Some(tail(0xa, 10, 100)));
        assert_eq!(opener.counts().preads, before + 1, "one entry read");
    }

    /// No tail entry (a header field of zero, as an empty file has, or
    /// as every crafted file has unless set): the file isn't in the table
    /// (`-ENODATA`).
    #[test]
    fn a_zero_tail_offset_has_no_tail() {
        let (c, _e) = tail_file();
        let (_t, f) = c.clone().write();
        assert_eq!(f.tail(last_of(&f).as_ref()), None);
        let mut old = c;
        old.set(88, 256);
        let (_t, f) = old.write();
        assert_eq!(f.tail(last_of(&f).as_ref()), None);
    }

    /// A header without `tail_entry_offset` (journald before 254) names
    /// the tail object instead (`tail_object_offset`), used when it is an
    /// entry.
    #[test]
    fn an_old_header_reads_the_tail_object() {
        let (mut c, e) = tail_file();
        c.set(88, 256);
        c.set(264, e[1]); // not in a 256-byte header: ignored
        c.set(136, e[0]);
        let (_t, f) = c.write();
        assert_eq!(f.tail(last_of(&f).as_ref()), Some(tail(0xa, 10, 100)));
    }

    /// A tail that isn't an entry: an archived file whose header has
    /// `HEADER_COMPATIBLE_TAIL_ENTRY_BOOT_ID` gives the header's
    /// `tail_entry_*`; any other file, the main list's newest entry
    /// (`journal_file_next_entry(f, 0, DIRECTION_UP)`).
    #[test]
    fn a_tail_that_is_not_an_entry_falls_back() {
        let (mut c, e) = tail_file();
        let main = c.get(H_ENTRY_ARRAY_OFFSET);
        c.set(264, main);
        let (_t, f) = c.clone().write();
        assert_eq!(f.tail(last_of(&f).as_ref()), Some(tail(0xb, 20, 200)));
        c.b[8..12].copy_from_slice(&COMPATIBLE_TAIL_ENTRY_BOOT_ID.to_le_bytes());
        let (_t, f) = c.clone().write();
        assert_eq!(
            f.tail(last_of(&f).as_ref()),
            Some(tail(0xb, 20, 200)),
            "online"
        );
        c.b[16] = STATE_ARCHIVED;
        let (_t, f) = c.clone().write();
        assert_eq!(f.tail(last_of(&f).as_ref()), Some(tail(0xc, 30, 300)));
        c.b[8..12].copy_from_slice(&0u32.to_le_bytes());
        let (_t, f) = c.clone().write();
        assert_eq!(
            f.tail(last_of(&f).as_ref()),
            Some(tail(0xb, 20, 200)),
            "no flag"
        );
        // The main list's newest unreadable: no tail (the walk's error).
        assert_eq!(f.tail(None), None);
        c.set(264, e[1] + 8);
        let (_t, f) = c.write();
        assert_eq!(
            f.tail(last_of(&f).as_ref()),
            Some(tail(0xb, 20, 200)),
            "not an object"
        );
    }

    /// A tail whose monotonic time is ahead of its realtime is refused
    /// (`mo > rt`, "that's weird"); equal ones are used.
    #[test]
    fn a_tail_ahead_of_its_realtime_is_refused() {
        let (mut c, e) = tail_file();
        c.set(264, e[1]);
        c.set(e[1] + 32, 201);
        let (_t, f) = c.clone().write();
        assert_eq!(f.tail(last_of(&f).as_ref()), None);
        c.set(e[1] + 32, 200);
        let (_t, f) = c.write();
        assert_eq!(f.tail(last_of(&f).as_ref()), Some(tail(0xb, 200, 200)));
    }

    /// `_MACHINE_ID=` and `id`, as journald writes the field.
    fn machine_field(id: &str) -> Vec<u8> {
        format!("_MACHINE_ID={id}").into_bytes()
    }

    /// `tail_file` whose header names as its tail an entry outside the
    /// main list (seqnum 3), listing the DATA objects `items` appends.
    fn named_tail(items: impl FnOnce(&mut Crafted) -> Vec<u64>) -> Crafted {
        let (mut c, _e) = tail_file();
        let items = items(&mut c);
        let e = c.entry(3, &items);
        c.set(264, e);
        c
    }

    /// The machine id `c`'s tail names, read as 26.04 reads it; `None`
    /// keeps the header's.
    fn entry_machine(c: &mut Crafted) -> Option<[u8; 16]> {
        let (_t, f) = c.write();
        f.tail_machine_id(last_of(&f).as_ref())
    }

    /// systemd v259.9 (v259-stable commit 2d8b0b6a0aaf, from upstream
    /// ae940730c6f2, issue #34169): the tail entry's `_MACHINE_ID=` names
    /// the machine, its digits in either case, read item by item, one
    /// pread each, up to it; the entry is read again when it isn't `last`.
    /// `tail` keeps the header's (24.04's 255.4).
    #[test]
    fn the_tail_entrys_machine_id_names_the_machine() {
        // A long payload is read only as far as a machine id's field: one
        // pread, as for a short one.
        let long = [&b"MESSAGE="[..], &[b'm'; 500]].concat();
        let mut c = named_tail(|c| {
            vec![
                c.data(&long),
                c.data(b"X=1"),
                c.data(&machine_field(&"aB".repeat(16))),
                c.data(b"Y=1"),
            ]
        });
        let t = c.file();
        let opener = Opener::default();
        let f = JournalFile::open_with(t.path(), &opener).expect("opens");
        let last = last_of(&f);
        let before = opener.counts().preads;
        assert_eq!(
            f.tail(last.as_ref()).map(|t| t.machine_id),
            Some([0x4d; 16])
        );
        assert_eq!(opener.counts().preads, before + 1, "the entry");
        assert_eq!(f.tail_machine_id(last.as_ref()), Some([0xab; 16]));
        assert_eq!(
            opener.counts().preads,
            before + 2 + 3,
            "the entry again and three items"
        );
        // The tail as the page read it: no entry read.
        let named = c.get(264);
        let tail = (named, f.entry(named).expect("entry"));
        let before = opener.counts().preads;
        assert_eq!(f.tail_machine_id(Some(&tail)), Some([0xab; 16]));
        assert_eq!(opener.counts().preads, before + 3, "three items");
    }

    /// With no `_MACHINE_ID=` item the header's machine id stays ("the
    /// entry lacks the `_MACHINE_ID=` field (older journals), fall back to
    /// the header's `machine_id`"), after a read of every item.
    #[test]
    fn a_tail_entry_without_a_machine_id_keeps_the_headers() {
        let mut c = named_tail(|c| vec![c.data(b"MESSAGE=m"), c.data(b"X=1")]);
        let t = c.file();
        let opener = Opener::default();
        let f = JournalFile::open_with(t.path(), &opener).expect("opens");
        let last = last_of(&f);
        let before = opener.counts().preads;
        assert_eq!(f.tail_machine_id(last.as_ref()), None);
        assert_eq!(opener.counts().preads, before + 3, "the entry, two items");
        let mut none = named_tail(|_| vec![]);
        assert_eq!(entry_machine(&mut none), None);
    }

    /// The first `_MACHINE_ID=` item decides: one whose value isn't 32
    /// characters (`-EBADMSG`), isn't hex or is all zero
    /// (`id128_from_string_nonzero`) leaves the header's machine id, and a
    /// later valid one isn't read. A field whose name only starts so isn't
    /// one.
    #[test]
    fn the_first_machine_id_item_decides() {
        let valid = machine_field(&"11".repeat(16));
        for (first, want) in [
            (machine_field(&"22".repeat(15)), None),
            (machine_field(&"22".repeat(17)), None),
            (machine_field(""), None),
            (machine_field(&"2g".repeat(16)), None),
            (machine_field(&"00".repeat(16)), None),
            (b"_MACHINE_IDX=1".to_vec(), Some([0x11; 16])),
            (b"_MACHINE_I=1".to_vec(), Some([0x11; 16])),
            (machine_field(&"22".repeat(16)), Some([0x22; 16])),
        ] {
            let mut c = named_tail(|c| vec![c.data(&first), c.data(&valid)]);
            assert_eq!(
                entry_machine(&mut c),
                want,
                "{}",
                String::from_utf8_lossy(&first)
            );
        }
    }

    /// An item systemd can't read as a DATA object (`-EBADMSG`,
    /// `-EADDRNOTAVAIL`: another object's type, an unaligned or a zero
    /// offset, a bad size, a corrupt payload) is passed over
    /// ("data object is bad, skipping over it").
    #[test]
    fn an_unreadable_item_is_passed_over() {
        let valid = machine_field(&"11".repeat(16));
        let mut c = named_tail(|c| {
            let bad_size = c.data(&machine_field(&"22".repeat(16)));
            c.set(bad_size + O_SIZE, 8);
            // An object, but shorter than a DATA object's header.
            let short = c.data(&machine_field(&"22".repeat(16)));
            c.set(short + O_SIZE, 40);
            let mut lz4 = 1000u64.to_le_bytes().to_vec();
            lz4.extend_from_slice(&[0xff; 8]);
            let bad_lz4 = c.data_raw(COMPRESSED_LZ4, &lz4, 1);
            // Too short for lz4's size prefix: corrupt, not over a limit.
            let cut_lz4 = c.data_raw(COMPRESSED_LZ4, &[1, 2, 3, 4], 2);
            let array = c.get(H_ENTRY_ARRAY_OFFSET);
            vec![
                array,
                0,
                0x111,
                bad_size,
                short,
                bad_lz4,
                cut_lz4,
                c.data(&valid),
            ]
        });
        assert_eq!(entry_machine(&mut c), Some([0x11; 16]));
        // The tail's only `_MACHINE_ID=` unreadable: the header's.
        let mut c = named_tail(|c| {
            let bad = c.data(&machine_field(&"22".repeat(16)));
            c.b[at(bad)] = OBJECT_ENTRY;
            vec![bad]
        });
        assert_eq!(entry_machine(&mut c), None);
    }

    /// A read the system refuses (`Io`) ends the search: the header's
    /// machine id, though a later item names another. A file cut short
    /// after it was opened stands in for a system error here; systemd
    /// itself sees that cut (`-EADDRNOTAVAIL`) and passes the item over,
    /// a departure `entry_machine_id` lists.
    #[test]
    fn a_refused_read_ends_the_search() {
        let mut c = named_tail(|c| {
            let valid = c.data(&machine_field(&"11".repeat(16)));
            vec![valid, valid]
        });
        // The first item, after the entry: cut off below.
        let cut = c.data(&machine_field(&"22".repeat(16)));
        c.set(c.get(264) + E_ITEMS, cut);
        let (t, f) = c.write();
        // Read before the cut, as the page reads its newest entry first.
        let named = c.get(264);
        let last = Some((named, f.entry(named).expect("entry")));
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(cut + 8)
            .expect("truncate");
        assert_eq!(f.tail_machine_id(last.as_ref()), None);
    }

    /// An item past the arena as the file was opened, whose DATA object
    /// was appended since (a corrupt forward item in a growing file): the
    /// first read gets only the object's header, and the rest of what the
    /// field needs is read before the payload is looked at, not sliced
    /// out of bytes that were never read.
    #[test]
    fn an_item_appended_after_open_is_read_whole() {
        let mut c = named_tail(|_| vec![0]);
        let later = c.b.len() as u64;
        c.set(c.get(264) + E_ITEMS, later);
        let t = c.file();
        let f = JournalFile::open(t.path()).expect("opens");
        let last = last_of(&f);
        c.data(&machine_field(&"11".repeat(16)));
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        std::fs::write(t.path(), &c.b).expect("grow");
        assert_eq!(f.tail_machine_id(last.as_ref()), Some([0x11; 16]));
    }

    /// A compressed `_MACHINE_ID=` is decoded (systemd's
    /// `decompress_startswith`). One that would decode past the field's
    /// 44 bytes is passed over: stored longer than that, unread (journald
    /// stores a payload compressed only when that makes it shorter);
    /// stored shorter, by the bounded decode. systemd would stop at such
    /// a malformed `_MACHINE_ID=` with the header's id, a departure
    /// `entry_machine_id` lists.
    #[test]
    fn a_compressed_machine_id_is_decoded() {
        let lz4_of = |payload: &[u8]| {
            let mut stored = (payload.len() as u64).to_le_bytes().to_vec();
            stored.extend_from_slice(&lz4_flex::block::compress(payload));
            stored
        };
        let lz4 = lz4_of(&machine_field(&"ab".repeat(16)));
        assert!(lz4.len() <= 44, "{}", lz4.len());
        let mut c = named_tail(|c| vec![c.data_raw(COMPRESSED_LZ4, &lz4, 1)]);
        assert_eq!(entry_machine(&mut c), Some([0xab; 16]));
        // Decoded but short: the first `_MACHINE_ID=`, invalid.
        let short = lz4_of(&machine_field(&"ab".repeat(15)));
        let mut c = named_tail(|c| {
            vec![
                c.data_raw(COMPRESSED_LZ4, &short, 1),
                c.data_raw(COMPRESSED_LZ4, &lz4, 2),
            ]
        });
        assert_eq!(entry_machine(&mut c), None);
        // Stored in exactly 44 bytes: decoded. An id whose first `k` digits
        // vary compresses less the larger `k`; one of them stores in 44.
        let exact = (0..=32)
            .map(|k: usize| {
                let id: String = "3f8c1e9a5b07d264e1c3a9f05b7d82e6"[..k]
                    .chars()
                    .chain(std::iter::repeat_n('a', 32 - k))
                    .collect();
                (id.clone(), lz4_of(&machine_field(&id)))
            })
            .find(|(_, stored)| stored.len() == 44);
        let (id, stored) = exact.expect("an id stored in 44 bytes");
        let mut c = named_tail(|c| vec![c.data_raw(COMPRESSED_LZ4, &stored, 4)]);
        let mut want = [0u8; 16];
        hex::decode_to_slice(&id, &mut want).expect("hex");
        assert_eq!(entry_machine(&mut c), Some(want));
        // Stored longer than 44 bytes: not read further.
        let noise: Vec<u8> = (0u8..60).map(|i| b'0' + i.wrapping_mul(37) % 75).collect();
        let big = lz4_of(&[&machine_field(&"22".repeat(16))[..], &noise].concat());
        assert!(big.len() > 44, "{}", big.len());
        // Stored in 44 bytes or fewer, decoding to more.
        let long = lz4_of(&[&machine_field(&"22".repeat(16))[..], &[b'2'; 100]].concat());
        assert!(long.len() <= 44, "{}", long.len());
        let mut c = named_tail(|c| {
            vec![
                c.data_raw(COMPRESSED_LZ4, &big, 1),
                c.data_raw(COMPRESSED_LZ4, &long, 2),
                c.data_raw(COMPRESSED_LZ4, &lz4, 3),
            ]
        });
        assert_eq!(entry_machine(&mut c), Some([0xab; 16]));
    }

    /// The machine id comes from whichever entry gave the tail's times:
    /// the main list's newest when the header's tail isn't an entry; none
    /// (the header's) when an archived file's header gave them ("header-only
    /// fallback for archived files"), or there is no tail.
    #[test]
    fn the_machine_id_comes_from_the_entry_that_gave_the_times() {
        let named = machine_field(&"11".repeat(16));
        let (mut c, _e) = journal_of(&[&[], &[&named]]);
        c.b[40..56].copy_from_slice(&[0x4d; 16]);
        c.b[56..72].copy_from_slice(&[0xc; 16]);
        c.set(192, 3_000_000);
        c.set(200, 30);
        assert_eq!(entry_machine(&mut c), None, "no tail");
        let main = c.get(H_ENTRY_ARRAY_OFFSET);
        c.set(264, main);
        assert_eq!(entry_machine(&mut c), Some([0x11; 16]), "the newest");
        c.b[8..12].copy_from_slice(&COMPATIBLE_TAIL_ENTRY_BOOT_ID.to_le_bytes());
        c.b[16] = STATE_ARCHIVED;
        assert_eq!(entry_machine(&mut c), None, "the header's");
    }

    /// In a compact file (journald 252 and later: 4-byte items, 72-byte
    /// DATA headers) the field is found the same way.
    #[test]
    fn a_compact_files_tail_names_its_machine() {
        let mut c = Crafted::new();
        set_incompatible(&mut c, INCOMPAT_COMPACT);
        let data = |c: &mut Crafted, payload: &[u8]| {
            let mut body =
                vec![0u8; usize::try_from(DATA_HEADER_COMPACT - OBJECT_HEADER).expect("small")];
            body.extend_from_slice(payload);
            c.object(OBJECT_DATA, 0, &body)
        };
        let other = data(&mut c, b"X=1");
        let named = data(&mut c, &machine_field(&"11".repeat(16)));
        let mut entry = vec![0u8; 48];
        entry[0..8].copy_from_slice(&9u64.to_le_bytes()); // seqnum
        for item in [other, named] {
            entry.extend_from_slice(&u32::try_from(item).expect("small").to_le_bytes());
        }
        let e = c.object(OBJECT_ENTRY, 0, &entry);
        c.set(264, e);
        let (_t, f) = c.write();
        assert!(f.header.compact());
        assert_eq!(f.tail_machine_id(None), Some([0x11; 16]));
    }

    /// A journal of three entries, the first of boot 0xa, the others of
    /// boot 0xb, with `_BOOT_ID=` fields; a field table of `buckets` and
    /// the `_BOOT_ID` FIELD object listing both values. Returns the bytes,
    /// the two DATA objects (oldest first) and the FIELD object.
    fn boot_field_file(buckets: u64) -> (Crafted, [u64; 2], u64) {
        let boot_a = format!("_BOOT_ID={}", "0a".repeat(16));
        let boot_b = format!("_BOOT_ID={}", "0b".repeat(16));
        let (a, b) = (boot_a.as_bytes(), boot_b.as_bytes());
        let (mut crafted, _e) = journal_of(&[&[a], &[b], &[b]]);
        let written = crafted.file();
        let file = JournalFile::open(written.path()).expect("opens");
        let find = |p: &[u8]| file.find_data(p).expect("lookup").expect("found");
        let data = [find(a).offset, find(b).offset];
        crafted.field_table(buckets);
        let field = crafted.field(b"_BOOT_ID", &data);
        (crafted, data, field)
    }

    /// `_BOOT_ID`'s values as sd-journal enumerates them
    /// (`sd_journal_enumerate_unique`): the field table's bucket, the
    /// FIELD object, then its DATA objects by `next_field_offset`, newest
    /// first. One pread each for the bucket, the FIELD object and each
    /// DATA object (its payload in the same read).
    #[test]
    fn a_fields_values_are_its_data_objects_newest_first() {
        let (mut c, data, _field) = boot_field_file(3);
        let t = c.file();
        let opener = Opener::default();
        let f = JournalFile::open_with(t.path(), &opener).expect("opens");
        let before = opener.counts().preads;
        let values = f.field_values(b"_BOOT_ID", 64).expect("reads");
        assert_eq!(opener.counts().preads - before, 4);
        let got: Vec<(u64, String)> = values
            .iter()
            .map(|(d, p)| (d.offset, String::from_utf8_lossy(p).into_owned()))
            .collect();
        let want = |d: u64, b: &str| (d, format!("_BOOT_ID={}", b.repeat(16)));
        assert_eq!(got, [want(data[1], "0b"), want(data[0], "0a")]);
        assert_eq!(values[0].0.n_entries, 2);
        // Values over the bound are left out.
        assert!(f.field_values(b"_BOOT_ID", 40).expect("reads").is_empty());
        assert!(f.field_values(b"_PID", 64).expect("reads").is_empty());
        let (_t, plain) = journal(1).0.write();
        assert!(
            plain
                .field_values(b"_BOOT_ID", 64)
                .expect("reads")
                .is_empty()
        );
    }

    /// Another FIELD object in the same bucket is passed over by its hash
    /// and name (`journal_file_find_field_object_with_hash`).
    #[test]
    fn a_field_is_found_past_another_in_its_bucket() {
        let (mut c, data, _field) = boot_field_file(1);
        c.field(b"_COMM", &[]);
        let (_t, f) = c.write();
        assert_eq!(f.field_values(b"_BOOT_ID", 64).expect("reads").len(), 2);
        assert!(f.field_values(b"_COMM", 64).expect("reads").is_empty());
        let (mut c, _d, _f) = boot_field_file(1);
        let other = c.field(b"_BOOT_IX", &[data[0]]);
        let (_t, f) = c.write();
        assert_eq!(f.field_values(b"_BOOT_IX", 64).expect("reads").len(), 1);
        c.set(other + 16, 1); // its stored hash
        let (_t, f) = c.write();
        assert!(f.field_values(b"_BOOT_IX", 64).expect("reads").is_empty());
    }

    /// The field is found through its own bucket whatever the table's
    /// size (`hash % buckets`, each bucket 16 bytes).
    #[test]
    fn a_field_is_found_in_its_bucket_at_any_table_size() {
        for buckets in 1..=8 {
            let (mut c, _data, _field) = boot_field_file(buckets);
            let (_t, f) = c.write();
            let values = f.field_values(b"_BOOT_ID", 64).expect("reads");
            assert_eq!(values.len(), 2, "{buckets} buckets");
        }
    }

    /// A value longer than one read (`READ_GUESS`) is read whole after its
    /// head; one over `max` is left out.
    #[test]
    fn a_long_value_is_read_whole_or_left_out() {
        let mut long = b"BIG=".to_vec();
        long.extend_from_slice(&[b'v'; 2000]);
        let (mut c, _e) = journal_of(&[&[&long]]);
        let t = c.file();
        let d = JournalFile::open(t.path())
            .expect("opens")
            .find_data(&long)
            .expect("lookup")
            .expect("found")
            .offset;
        c.field_table(3);
        c.field(b"BIG", &[d]);
        let (_t, f) = c.write();
        let values = f.field_values(b"BIG", 4096).expect("reads");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].1, long);
        assert!(f.field_values(b"BIG", 100).expect("reads").is_empty());
        // A value that fails to decode for another reason than its length
        // is an error, not left out: marked xz, its bytes aren't xz.
        c.b[usize::try_from(d).expect("small") + 1] = COMPRESSED_XZ;
        let (_t, f) = c.write();
        assert!(f.field_values(b"BIG", 4096).is_err());
    }

    /// A list whose next DATA object isn't older (lower) than the last, as
    /// journald's prepending makes it, is corrupt: it would loop.
    #[test]
    fn a_field_list_that_does_not_go_back_is_corrupt() {
        let (mut c, data, _field) = boot_field_file(3);
        c.set(data[0] + D_NEXT_FIELD, data[0]);
        let (_t, f) = c.clone().write();
        let err = f.field_values(b"_BOOT_ID", 64).expect_err("a loop");
        assert!(err.to_string().contains("field list"), "{err}");
        c.set(data[0] + D_NEXT_FIELD, data[1]);
        let (_t, f) = c.write();
        let err = f.field_values(b"_BOOT_ID", 64).expect_err("a loop");
        assert!(err.to_string().contains("field list"), "{err}");
        let (mut c, _data, field) = boot_field_file(3);
        c.set(field + F_HEAD_DATA, field);
        let (_t, f) = c.write();
        assert!(
            f.field_values(b"_BOOT_ID", 64).is_err(),
            "not a DATA object"
        );
    }

    /// A field table sd-journal would refuse (not whole items, outside the
    /// arena) refuses the lookup.
    #[test]
    fn a_bad_field_table_refuses_the_lookup() {
        let (mut c, _data, _field) = boot_field_file(3);
        c.set(H_FIELD_TABLE_SIZE, 47);
        let (_t, f) = c.write();
        let err = f.field_values(b"_BOOT_ID", 64).expect_err("refused");
        assert!(err.to_string().contains("bad field hash table"), "{err}");
    }

    /// The header is read again on `refresh`: journald links entries in
    /// while the file is open.
    #[test]
    fn refresh_reads_the_header_again() {
        let (mut c, _l) = journal(2);
        let (t, mut f) = c.write();
        c.set(H_N_ENTRIES, 1);
        std::fs::write(t.path(), &c.b).expect("rewrite");
        assert_eq!(f.header.n_entries, 2);
        f.refresh().expect("refresh");
        assert_eq!(f.header.n_entries, 1);
    }

    /// A file that isn't there is an I/O error naming it, with the
    /// system's error as the source.
    #[test]
    fn a_missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("system.journal");
        let err = JournalFile::open(&path).expect_err("missing");
        assert_eq!(err.path(), Some(path.as_path()));
        let ReadErrorKind::Io { context, source } = err.kind() else {
            panic!("not an I/O error: {err}");
        };
        assert_eq!(context, "open");
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_file_without_the_signature_is_refused() {
        let mut c = Crafted::new();
        c.b[0] = b'X';
        let tmp = c.file();
        let err = JournalFile::open(tmp.path()).expect_err("refused");
        assert!(err.to_string().ends_with(": not a journal file"), "{err}");
    }

    #[test]
    fn a_header_size_past_the_file_or_too_small_is_refused() {
        for size in [207, 4096] {
            let mut c = Crafted::new();
            c.set(88, size);
            let tmp = c.file();
            let err = JournalFile::open(tmp.path()).expect_err("refused");
            assert!(err.to_string().ends_with(": bad header size"), "{err}");
        }
    }

    /// An object of another type, or not 8-byte aligned, is refused.
    #[test]
    fn an_object_of_the_wrong_type_or_unaligned_is_refused() {
        let (mut c, l) = journal(2);
        let (_t, f) = c.write();
        let err = f.data_payload(l.entries[0]).expect_err("an entry");
        assert!(err.to_string().contains("is type 3, wanted 1"), "{err}");
        let err = f.entry(l.entries[0] + 4).expect_err("unaligned");
        assert!(err.to_string().contains("unaligned object"), "{err}");
    }

    /// Reading spends the whole object and what it decoded; a read whose
    /// object is larger than what is left is over the limit, and spends
    /// all of it.
    #[test]
    fn a_payload_read_spends_its_object() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let d = c.data(b"MESSAGE=hello");
        let (_t, f) = c.write();
        let size = 64 + 13;
        let mut left = 1000;
        assert_eq!(
            f.data_payload_spending(d, &mut left).expect("read"),
            b"MESSAGE=hello"
        );
        assert_eq!(left, 1000 - size);
        let mut left = size;
        assert!(f.data_payload_spending(d, &mut left).is_ok());
        assert_eq!(left, 0);
        let mut left = 20;
        let err = f.data_payload_spending(d, &mut left).expect_err("over");
        let ReadErrorKind::OverLimit { context, over } = err.kind() else {
            panic!("not over: {err}");
        };
        assert_eq!(
            (context.as_str(), over.limit),
            (format!("data {d:#x}").as_str(), 20)
        );
        assert_eq!(left, 0);
        // A read that fails is corruption, and spends what it read.
        let mut bad = Crafted { b: c.b.clone() };
        bad.b[usize::try_from(d).expect("small") + 1] = COMPRESSED_XZ;
        let (_t, f) = bad.write();
        let mut left = 1000;
        let err = f.data_payload_spending(d, &mut left).expect_err("bad");
        assert!(!err.is_over_limit(), "{err}");
        assert_eq!(left, 1000 - size);
    }

    /// A payload's errors say where they were met: the object, then the
    /// codec.
    #[test]
    fn a_payload_error_names_its_object_and_codec() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let lz4 = c.data_raw(COMPRESSED_LZ4, b"short", 0);
        let mut over = (100u64).to_le_bytes().to_vec();
        over.extend_from_slice(&lz4_flex::block::compress(&[b'x'; 100]));
        let big = c.data_raw(COMPRESSED_LZ4, &over, 0);
        let (_t, f) = c.write();
        let err = f.data_payload(lz4).expect_err("short");
        assert!(
            err.to_string()
                .ends_with(&format!(": data {lz4:#x}: short lz4 payload")),
            "{err}"
        );
        let err = f.data_payload_within(big, 50).expect_err("over");
        assert!(
            err.to_string().ends_with(&format!(
                ": data {big:#x}: lz4: payload over its limit of 50 bytes"
            )),
            "{err}"
        );
    }

    /// A stored payload over the limit is refused by the codec too, for
    /// callers that don't check first.
    #[test]
    fn a_stored_payload_over_the_limit_is_refused() {
        let err = decompress_within(0, b"abcdef".to_vec(), 5, &mut 0).expect_err("over");
        assert_eq!(err.to_string(), "payload over its limit of 5 bytes");
        assert_eq!(
            decompress_within(0, b"abcde".to_vec(), 5, &mut 0).expect("fits"),
            b"abcde"
        );
    }

    /// A decoder's output is counted as it arrives, and read no further
    /// than one byte past the limit.
    #[test]
    fn read_counted_counts_and_stops_past_the_limit() {
        let data = vec![7u8; 200 * 1024];
        let mut cost = 3;
        let out = read_counted(&mut data.as_slice(), 100 * 1024, &mut cost).expect("read");
        assert_eq!(out.len(), 100 * 1024 + 1);
        assert_eq!(cost, 3 + 100 * 1024 + 1);
        let mut cost = 0;
        let out = read_counted(&mut &data[..10], 100, &mut cost).expect("read");
        assert_eq!((out.len(), cost), (10, 10));
    }

    // Boundaries and constants (new in the crate; found by cargo-mutants).

    /// The format's values, as journal-def.h has them.
    #[test]
    fn the_format_constants_are_journal_defs() {
        assert_eq!(
            [
                INCOMPAT_XZ,
                INCOMPAT_LZ4,
                INCOMPAT_KEYED_HASH,
                INCOMPAT_ZSTD,
                INCOMPAT_COMPACT
            ],
            [1, 2, 4, 8, 16]
        );
        assert_eq!(INCOMPAT_KNOWN, 0x1f);
        assert_eq!([COMPRESSED_XZ, COMPRESSED_LZ4, COMPRESSED_ZSTD], [1, 2, 4]);
        assert_eq!(DATA_SIZE_MAX, 768 << 20);
        assert_eq!(ENTRY_SIZE_MAX, 770 << 20);
    }

    /// A file with every known incompatible flag opens.
    #[test]
    fn every_known_incompatible_flag_opens() {
        for flags in [0, 1, 2, 4, 8, 16, 0x1f] {
            let mut c = Crafted::new();
            set_incompatible(&mut c, flags);
            let (_t, f) = c.write();
            assert_eq!(f.header.incompatible, flags);
        }
    }

    /// A data object larger than `DATA_SIZE_MAX` is corruption, refused by
    /// its size before anything is read, even inside a file large enough
    /// to hold it (a sparse one here).
    #[test]
    fn a_data_object_over_data_size_max_is_corruption() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let d = c.data(b"A=1");
        let size = DATA_HEADER + DATA_SIZE_MAX as u64 + 8;
        c.set(d + O_SIZE, size);
        let tmp = c.file();
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .expect("open");
        f.set_len(d + size).expect("sparse growth");
        f.write_all_at(&(d + size - 272).to_le_bytes(), H_ARENA_SIZE)
            .expect("arena size");
        let jf = JournalFile::open(tmp.path()).expect("opens");
        let err = jf.data_payload(d).expect_err("refused");
        assert!(!err.is_over_limit(), "{err}");
        assert!(err.to_string().ends_with("has a bad size"), "{err}");
    }

    /// A payload refused by its stored size before it is read spends
    /// nothing.
    #[test]
    fn a_payload_refused_before_reading_spends_nothing() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let d = c.data(b"MESSAGE=hello");
        let (_t, f) = c.write();
        let mut left = 12;
        let err = f.data_payload_spending(d, &mut left).expect_err("over");
        assert!(err.is_over_limit(), "{err}");
        assert_eq!(left, 12);
    }

    /// A hash table must start past the header and its own object
    /// header.
    #[test]
    fn a_hash_table_without_room_for_its_object_header_is_refused() {
        let (mut c, _l) = journal(3);
        c.set(H_HASH_TABLE_OFFSET, 272);
        let (_t, f) = c.write();
        let err = f.find_data(b"U=x").expect_err("refused");
        assert!(err.to_string().contains("bad data hash table"), "{err}");
    }

    /// Every value of a table with several buckets is found at its own
    /// object.
    #[test]
    fn every_value_is_found_in_its_bucket() {
        let (mut c, l) = journal(9);
        let (_t, f) = c.write();
        for (i, &m) in l.messages.iter().enumerate() {
            let found = f.find_data(format!("MESSAGE=m{i}").as_bytes());
            assert_eq!(found.expect("lookup").map(|d| d.offset), Some(m), "m{i}");
        }
        assert!(f.find_data(b"MESSAGE=m9").expect("lookup").is_none());
    }

    /// A candidate with the looked-for hash that can't be read fails the
    /// lookup: it isn't passed over as another value.
    #[test]
    fn a_lookup_fails_on_a_corrupt_candidate_with_the_same_hash() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let want = b"U=x";
        c.data_raw(COMPRESSED_XZ, b"U=y", hash::jenkins64(want));
        c.data(want);
        let (_t, f) = c.write();
        let err = f.find_data(want).expect_err("corrupt candidate");
        assert!(err.to_string().contains(": xz: "), "{err}");
    }

    /// An I/O error's context is prefixed too; its source stays its own.
    #[test]
    fn a_context_prefixes_an_io_error() {
        let io = ReadErrorKind::Io {
            context: "read at 0x20".into(),
            source: std::io::Error::other("gone"),
        };
        let kind = in_context("entry 3's fields", io);
        assert_eq!(kind.to_string(), "entry 3's fields: read at 0x20");
        let source = std::error::Error::source(&kind).expect("a source");
        assert_eq!(source.to_string(), "gone");
    }

    #[test]
    fn a_context_prefixes_an_over_limit_without_one() {
        assert_eq!(
            in_context("data 0x10", over("", 5)).to_string(),
            "data 0x10: payload over its limit of 5 bytes"
        );
        assert_eq!(
            in_context("data 0x10", over("xz", 5)).to_string(),
            "data 0x10: xz: payload over its limit of 5 bytes"
        );
    }

    /// xz output of exactly the limit is read; one byte over is over.
    #[test]
    fn xz_output_at_the_limit_reads() {
        assert_eq!(
            decompress_within(COMPRESSED_XZ, xz_sample(), 13, &mut 0).expect("fits"),
            b"MESSAGE=hello"
        );
        let err = decompress_within(COMPRESSED_XZ, xz_sample(), 12, &mut 0).expect_err("over");
        assert!(is_over(&err), "{err}");
    }

    /// An LZ4 payload of just its size field isn't "short".
    #[test]
    fn an_lz4_payload_of_just_its_size_is_not_short() {
        let r = decompress(COMPRESSED_LZ4, 0u64.to_le_bytes().to_vec());
        assert!(
            r.as_ref()
                .map_or_else(|e| !e.to_string().contains("short"), Vec::is_empty),
            "{r:?}"
        );
    }

    /// LZ4's claim bounds, at their edges: 255 times the block, and
    /// `DATA_SIZE_MAX`; a claim at the caller's limit decodes.
    #[test]
    fn lz4_claims_at_their_bounds() {
        let lz4 = |claim: u64, block: &[u8]| {
            let mut raw = claim.to_le_bytes().to_vec();
            raw.extend_from_slice(block);
            raw
        };
        let five = [0x10, b'x', 0, 0, 0];
        // At 255 times the five block bytes: past the claim check, and
        // the block itself fails.
        let err = decompress(COMPRESSED_LZ4, lz4(5 * 255, &five)).expect_err("bad block");
        assert!(!err.to_string().contains("claims"), "{err}");
        let err = decompress(COMPRESSED_LZ4, lz4(5 * 255 + 1, &five)).expect_err("claim");
        assert!(err.to_string().contains("claims"), "{err}");
        // At DATA_SIZE_MAX, with a block that could hold it: over a
        // small limit, not corrupt; one more is corrupt.
        let big = vec![0u8; DATA_SIZE_MAX / 255 + 1];
        let max = DATA_SIZE_MAX as u64;
        let err = decompress_within(COMPRESSED_LZ4, lz4(max, &big), 100, &mut 0).expect_err("over");
        assert!(is_over(&err), "{err}");
        let err =
            decompress_within(COMPRESSED_LZ4, lz4(max + 1, &big), 100, &mut 0).expect_err("claim");
        assert!(err.to_string().contains("claims"), "{err}");
        // A claim of exactly the limit decodes, and spends its size.
        let text = [b'x'; 100];
        let mut raw = 100u64.to_le_bytes().to_vec();
        raw.extend_from_slice(&lz4_flex::block::compress(&text));
        let mut cost = 0;
        let out = decompress_within(COMPRESSED_LZ4, raw, 100, &mut cost).expect("fits");
        assert_eq!((out.as_slice(), cost), (&text[..], 100));
    }

    /// zstd's claim bound at its edge: 128 KiB per 4 stored bytes, plus
    /// one block. A 12-byte frame may declare 512 KiB, not one byte more.
    #[test]
    fn a_zstd_claim_at_its_bound() {
        let frame = |content: u32| {
            let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x38];
            raw.extend_from_slice(&content.to_le_bytes());
            raw.extend_from_slice(&[0, 0]);
            raw
        };
        let err =
            decompress_within(COMPRESSED_ZSTD, frame(512 << 10), 100, &mut 0).expect_err("over");
        assert!(is_over(&err), "{err}");
        let err = decompress_within(COMPRESSED_ZSTD, frame((512 << 10) + 1), 100, &mut 0)
            .expect_err("claim");
        assert!(err.to_string().contains("declares"), "{err}");
    }

    /// zstd content of exactly the limit decodes; one byte less is over.
    #[test]
    fn zstd_content_at_the_limit_decodes() {
        assert_eq!(
            decompress_within(COMPRESSED_ZSTD, zstd_sample(), 920, &mut 0)
                .expect("fits")
                .len(),
            920
        );
        let err = decompress_within(COMPRESSED_ZSTD, zstd_sample(), 919, &mut 0).expect_err("over");
        assert!(is_over(&err), "{err}");
    }

    /// A zstd stream failing part way spends its window (here 1 KiB), so
    /// many small failures still add up.
    #[test]
    fn a_failing_zstd_stream_spends_its_window() {
        // Window 1 KiB, content 3, then a block of the reserved type 3.
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x00, 3, 0, 0, 0];
        raw.extend_from_slice(&((3u32 << 3) | (3 << 1) | 1).to_le_bytes()[..3]);
        raw.extend_from_slice(b"A=1");
        let mut cost = 0;
        let err = decompress_within(COMPRESSED_ZSTD, raw, 4096, &mut cost).expect_err("bad block");
        assert!(err.to_string().starts_with("zstd: "), "{err}");
        assert_eq!(cost, 1024);
    }

    // Entry arrays and chains (ported in Task 2).

    /// The chain at 272 with `n` links, newest first.
    fn walk(f: &JournalFile, n: u64) -> Result<Vec<u64>> {
        EntriesRev::new(f, 272, n, None).collect()
    }

    /// Write the header's entry list (count and first array) to the file
    /// and read it back, as a follow sees journald's updates.
    fn set_list(t: &tempfile::NamedTempFile, f: &mut JournalFile, n: u64, array: u64) {
        let mut b = std::fs::read(t.path()).expect("read");
        b[152..160].copy_from_slice(&n.to_le_bytes());
        b[176..184].copy_from_slice(&array.to_le_bytes());
        std::fs::write(t.path(), &b).expect("write");
        f.refresh().expect("refresh");
    }

    fn split(walk: EntriesRev<'_>) -> Split {
        let mut s = Split {
            offsets: Vec::new(),
            error: None,
        };
        for r in walk {
            match r {
                Ok(off) => s.offsets.push(off),
                Err(e) => {
                    s.error.get_or_insert(e);
                }
            }
        }
        s.offsets.reverse();
        s
    }

    /// A walk's readable offsets, in file order, and its first error.
    struct Split {
        offsets: Vec<u64>,
        error: Option<ReadError>,
    }

    impl Split {
        /// The offsets of a walk that met no error.
        fn ok(self) -> Vec<u64> {
            if let Some(e) = self.error {
                panic!("walk failed: {e}");
            }
            self.offsets
        }
    }

    #[test]
    fn a_repeated_offset_is_skipped() {
        let (_t, f) = file_with_array(&[1000, 1000, 2000]);
        assert_eq!(walk(&f, 3).expect("reads"), vec![2000, 1000]);
    }

    #[test]
    fn a_decreasing_offset_is_corruption() {
        let (_t, f) = file_with_array(&[2000, 1000]);
        assert!(walk(&f, 2).is_err());
    }

    /// `n` counts links, repeats included (systemd 249 raises `n_entries`
    /// once per item): an item past the n-th link was written after the
    /// header was read and isn't returned.
    #[test]
    fn n_counts_raw_links_including_repeats() {
        let (_t, f) = file_with_array(&[1000, 1000, 2000, 3000]);
        assert_eq!(walk(&f, 3).expect("reads"), vec![2000, 1000]);
    }

    #[test]
    fn the_inline_entry_repeated_by_the_chain_appears_once() {
        let (_t, f) = file_with_array(&[1000, 2000]);
        let d = DataRef {
            offset: 0,
            n_entries: 3,
            entry_offset: 1000,
            entry_array_offset: 272,
        };
        let got: Vec<u64> = f.entries_of_rev(&d).collect::<Result<_>>().expect("reads");
        assert_eq!(got, vec![2000, 1000]);
    }

    /// New in the crate: a DATA object's count covers the in-line entry
    /// plus its array's links, so array items written after the count
    /// was read aren't returned.
    #[test]
    fn a_data_objects_count_bounds_its_array() {
        let (_t, f) = file_with_array(&[2000, 3000, 4000]);
        let d = DataRef {
            offset: 0,
            n_entries: 3,
            entry_offset: 1000,
            entry_array_offset: 272,
        };
        let got: Vec<u64> = f.entries_of_rev(&d).collect::<Result<_>>().expect("reads");
        assert_eq!(got, vec![3000, 2000, 1000]);
    }

    /// A slot never written (an unclean shutdown) is skipped, as
    /// sd-journal skips a bad item, and the walk goes on past it.
    #[test]
    fn a_zero_item_is_skipped() {
        let (_t, f) = file_with_array(&[1000, 0, 3000]);
        assert_eq!(walk(&f, 3).expect("reads"), vec![3000, 1000]);
    }

    /// Several arrays, one larger than a read chunk, with `n` ending inside
    /// the last array: every item once, newest first, and none past `n`.
    #[test]
    fn a_chain_reads_back_across_arrays_and_chunks() {
        let first: Vec<u64> = (1..=4).map(|i| 4096 + i * 8).collect();
        let second: Vec<u64> = (5..=1204).map(|i| 4096 + i * 8).collect();
        let third: Vec<u64> = (1205..=1300).map(|i| 4096 + i * 8).collect();
        let (_t, f) = file_with_chain(&[&first, &second, &third]);
        let n = 1250;
        let want: Vec<u64> = (1..=n).rev().map(|i| 4096 + i * 8).collect();
        assert_eq!(walk(&f, n).expect("reads"), want);
        // Only what is asked for is read: the first few come from the end,
        // from one chunk of the last array, and the others aren't touched.
        let mut w = EntriesRev::new(&f, 272, n, None);
        let head: Vec<u64> = w.by_ref().take(3).collect::<Result<_>>().expect("reads");
        assert_eq!(
            head,
            vec![4096 + n * 8, 4096 + (n - 1) * 8, 4096 + (n - 2) * 8]
        );
        let left: Vec<u64> = w.arrays.iter().map(|a| a.1).collect();
        assert_eq!(left, vec![4, 1200, 0]);
        assert_eq!(w.buf.len(), 46 - 3);
    }

    /// The in-line entry is the oldest: one newer than the chain is
    /// corruption, not a match out of order.
    #[test]
    fn an_inline_entry_newer_than_the_chain_is_corruption() {
        let (_t, f) = file_with_array(&[1000, 2000]);
        let d = DataRef {
            offset: 0,
            n_entries: 3,
            entry_offset: 3000,
            entry_array_offset: 272,
        };
        let got: Result<Vec<u64>> = f.entries_of_rev(&d).collect();
        assert!(got.is_err());
    }

    /// An entry array too short for its next link is refused, not read
    /// past.
    #[test]
    fn a_short_entry_array_is_refused() {
        let (_t, f) = file_with_object(&object_bytes(OBJECT_ENTRY_ARRAY, 20, 24));
        let err = f.array_head(272).expect_err("refused");
        assert!(err.to_string().contains("short entry array"));
    }

    #[test]
    fn entries_since_reads_across_arrays_up_to_n_entries() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104], &[1200, 1304, 0, 0]]);
        set_list(&t, &mut f, 3, 272);
        let mut c = None;
        assert_eq!(
            f.entries_since(&mut c, 0).expect("reads"),
            vec![1000, 1104, 1200]
        );
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 1).expect("reads"), vec![1104, 1200]);
    }

    /// The cursor resumes where the last read stopped, also when that was
    /// exactly the end of an array.
    #[test]
    fn entries_since_resumes_from_its_cursor() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104], &[1200, 1304, 1400, 1504]]);
        set_list(&t, &mut f, 2, 272);
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000, 1104]);
        set_list(&t, &mut f, 4, 272);
        assert_eq!(f.entries_since(&mut c, 2).expect("reads"), vec![1200, 1304]);
        assert_eq!(f.entries_since(&mut c, 4).expect("reads"), [] as [u64; 0]);
        set_list(&t, &mut f, 6, 272);
        assert_eq!(f.entries_since(&mut c, 4).expect("reads"), vec![1400, 1504]);
        // The cursor is on the second array, not back at the head.
        assert_eq!(c.map(|c| c.base), Some(2));
    }

    /// An item that can't be an entry offset comes back as it is, last,
    /// so a bogus high one doesn't make the good items after it look out
    /// of order.
    #[test]
    fn entries_since_returns_a_bogus_item_and_stops() {
        let (t, mut f) = file_with_chain(&[&[1000, 0xFFFF_FFF8, 1200, 1304]]);
        set_list(&t, &mut f, 4, 272);
        assert_eq!(
            f.entries_since(&mut None, 0).expect("reads"),
            vec![1000, 0xFFFF_FFF8]
        );
        assert_eq!(
            f.entries_since(&mut None, 2).expect("reads"),
            vec![1200, 1304]
        );
    }

    /// A slot not written below `n_entries` comes back as 0, last.
    #[test]
    fn entries_since_returns_a_zero_item_and_stops() {
        let (t, mut f) = file_with_chain(&[&[1000, 0, 1200]]);
        set_list(&t, &mut f, 3, 272);
        assert_eq!(f.entries_since(&mut None, 0).expect("reads"), vec![1000, 0]);
        assert_eq!(f.entries_since(&mut None, 2).expect("reads"), vec![1200]);
    }

    /// A file read while it had no entries (no entry array yet) is read
    /// from its head once it has them: the empty read leaves no cursor.
    #[test]
    fn entries_since_after_an_empty_read() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104]]);
        set_list(&t, &mut f, 0, 0);
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), [] as [u64; 0]);
        set_list(&t, &mut f, 2, 272);
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000, 1104]);
    }

    #[test]
    fn entries_since_refuses_a_decreasing_offset() {
        let (t, mut f) = file_with_chain(&[&[2000], &[1000]]);
        set_list(&t, &mut f, 2, 272);
        assert!(f.entries_since(&mut None, 0).is_err());
    }

    // New in the crate: `entries_since`'s edges (found by cargo-mutants).

    /// The file's own list names each entry once, so a repeat there is
    /// out of order too (only DATA objects' lists repeat, on 249).
    #[test]
    fn entries_since_refuses_a_repeated_offset() {
        let (t, mut f) = file_with_chain(&[&[1000, 1000]]);
        set_list(&t, &mut f, 2, 272);
        let err = f.entries_since(&mut None, 0).expect_err("repeat");
        assert!(err.to_string().contains("is out of order"), "{err}");
    }

    /// A read that stops at `n_entries` inside an array keeps its cursor
    /// on that array, to resume there.
    #[test]
    fn entries_since_stopping_inside_an_array_stays_on_it() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104], &[1200, 1304]]);
        set_list(&t, &mut f, 1, 272);
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000]);
        assert_eq!(c.map(|c| (c.at, c.base)), Some((272, 0)));
    }

    /// A header counting entries with no entry array is corrupt, and is
    /// read as empty without a report. This pins the prototype's
    /// behaviour (the whole-file walk does the same); the follow task
    /// decides whether to report it, as `entries_of_rev` reports the same
    /// shape in a DATA object.
    #[test]
    fn entries_since_with_a_count_and_no_array_reads_nothing_unreported() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104]]);
        set_list(&t, &mut f, 2, 0);
        assert_eq!(
            f.entries_since(&mut None, 0).expect("reads"),
            [] as [u64; 0]
        );
    }

    /// A read from before the cursor's array (a rewind) starts again from
    /// the chain's head rather than reading back from the cursor.
    #[test]
    fn entries_since_from_before_its_cursor_starts_from_the_head() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104], &[1200, 1304, 1400, 1504]]);
        set_list(&t, &mut f, 4, 272);
        let mut c = None;
        assert_eq!(
            f.entries_since(&mut c, 0).expect("reads"),
            vec![1000, 1104, 1200, 1304]
        );
        assert_eq!(c.map(|c| c.base), Some(2));
        assert_eq!(
            f.entries_since(&mut c, 1).expect("reads"),
            vec![1104, 1200, 1304]
        );
    }

    /// An idle wake (nothing past `n_entries`) reads no entry array: one
    /// past `n`, here a broken one, isn't touched.
    #[test]
    fn entries_since_at_n_reads_no_array() {
        let (t, mut f) = file_with_chain(&[&[1000, 1104], &[1200, 1304]]);
        set_list(&t, &mut f, 2, 272);
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000, 1104]);
        assert_eq!(c.map(|c| (c.at, c.base)), Some((312, 2)));
        let mut b = std::fs::read(t.path()).expect("read");
        b[312] = 0; // the second array's type byte
        std::fs::write(t.path(), &b).expect("write");
        assert_eq!(f.entries_since(&mut c, 2).expect("idle"), [] as [u64; 0]);
    }

    /// No more entries are read than the arena could hold, whatever the
    /// header counts: 64 bytes each past the header. The items name
    /// offsets inside the array itself, which is fine here because
    /// `entries_since` never reads the entries.
    #[test]
    fn entries_since_reads_no_more_than_the_arena_holds() {
        let mut c = Crafted::new();
        let items: Vec<u64> = (0..70).map(|k| 272 + 8 * k).collect();
        let array = c.array(&items, items.len(), 0);
        c.set(H_N_ENTRIES, 70);
        c.set(H_ENTRY_ARRAY_OFFSET, array);
        let (_t, f) = c.write();
        let room = (c.b.len() as u64 - 272) / ENTRY_HEADER;
        assert_eq!(room, 9);
        let got = f.entries_since(&mut None, 0).expect("reads");
        assert_eq!(got, items[..9]);
    }

    #[test]
    fn all_entries_is_in_file_order() {
        let (_t, mut f) = file_with_array(&[1000, 2000, 3000]);
        f.header.entry_array_offset = 272;
        f.header.n_entries = 3;
        assert_eq!(f.all_entries().expect("reads"), vec![1000, 2000, 3000]);
    }

    /// An array whose items go out of order is left at that point, and
    /// the older arrays are still walked, as sd-journal moves back to the
    /// previous array going up; the error comes at the gap.
    #[test]
    fn an_out_of_order_array_is_left_and_the_older_ones_walked() {
        let (_t, f) = file_with_chain(&[&[1000, 2000], &[3000, 9000, 4000, 5000]]);
        let got: Vec<std::result::Result<u64, ()>> = EntriesRev::new(&f, 272, 6, None)
            .map(|r| r.map_err(|_| ()))
            .collect();
        // The error at the gap, where the array was left.
        assert_eq!(got, vec![Ok(5000), Ok(4000), Err(()), Ok(2000), Ok(1000)]);
    }

    /// An item that can't be an entry offset (a bogus low value) is
    /// refused before it becomes the order check's reference: it doesn't
    /// make every older item look out of order.
    #[test]
    fn a_bogus_low_item_hides_nothing_else() {
        let (_t, f) = file_with_chain(&[&[1000, 2000], &[3000, 0x10, 5000]]);
        let s = split(EntriesRev::new(&f, 272, 5, None));
        assert!(s.error.is_some());
        assert_eq!(s.offsets, vec![1000, 2000, 3000, 5000]);
    }

    /// New in the crate: an array whose items can't be read any more (the
    /// file cut short after the walk learnt the chain's shape) is left
    /// with an error, and the older arrays are still walked.
    #[test]
    fn an_array_cut_off_after_the_walk_starts_is_left() {
        let (t, f) = file_with_chain(&[&[1000, 2000], &[3000, 4000]]);
        let mut walk = EntriesRev::new(&f, 272, 4, None);
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(272 + 40 + 24)
            .expect("truncate");
        let got: Vec<std::result::Result<u64, ReadErrorKind>> =
            walk.by_ref().map(|r| r.map_err(|e| e.kind)).collect();
        assert!(
            matches!(got.first(), Some(Err(ReadErrorKind::Io { .. }))),
            "{got:?}"
        );
        assert_eq!(
            got[1..]
                .iter()
                .map(|r| r.as_ref().ok().copied())
                .collect::<Vec<_>>(),
            vec![Some(2000), Some(1000)]
        );
    }

    /// A chain cut short hides its newest entries: the error comes first,
    /// so a caller taking only the newest few still sees it.
    #[test]
    fn a_chain_cut_short_reports_before_its_older_entries() {
        let (t, f) = file_with_chain(&[&[1000, 2000], &[3000, 4000]]);
        let mut b = std::fs::read(t.path()).expect("read");
        // The second array's type byte.
        b[272 + 24 + 16] = 0;
        std::fs::write(t.path(), &b).expect("write");
        let mut walk = EntriesRev::new(&f, 272, 4, None);
        assert!(walk.next().expect("an item").is_err());
        assert_eq!(walk.next().expect("an item").expect("readable"), 2000);
    }

    /// The crafted journal itself is valid, so each corruption test below
    /// changes one thing.
    #[test]
    fn the_crafted_journal_reads_back() {
        let (mut c, l) = journal(5);
        let (_t, f) = c.write();
        assert_eq!(split(f.all_entries_rev()).ok(), l.entries);
        let shared = f.find_data(b"U=x").expect("lookup").expect("found");
        assert_eq!(split(f.entries_of_rev(&shared)).ok(), l.entries);
        let entry = f.entry(l.entries[3]).expect("entry");
        assert_eq!(entry.items, vec![l.shared, l.messages[3]]);
        let payload = f.data_payload(l.messages[3]).expect("payload");
        assert_eq!(payload, b"MESSAGE=m3");
        let all = f.all_data().expect("hash table");
        assert_eq!(all.len(), 6);
        for (off, stored) in all {
            assert_eq!(f.hash(&f.data_payload(off).expect("payload")), stored);
        }
    }

    #[test]
    fn an_entry_array_running_past_the_arena_ends_the_walk() {
        let (mut c, l) = journal(3);
        c.set(l.main_array + O_SIZE, 1 << 40);
        let (_t, f) = c.write();
        let walk = split(f.all_entries_rev());
        assert!(walk.error.is_some());
        assert_eq!(walk.offsets, [] as [u64; 0]);
    }

    #[test]
    fn an_array_linking_to_itself_or_backwards_ends_the_walk() {
        for back in [0, 8, 64] {
            let (mut c, l) = journal(3);
            c.set(l.main_array + A_NEXT, l.main_array - back);
            c.set(H_N_ENTRIES, 10);
            let (_t, f) = c.write();
            let walk = split(f.all_entries_rev());
            assert!(walk.error.is_some(), "back {back}");
            assert_eq!(walk.offsets, l.entries);
        }
    }

    /// Objects never overlap: journald appends each after the last. A link
    /// into the current object's own bytes, which a crafted file can make
    /// look like an array, is corruption; refusing it also keeps a walk
    /// linear in the file's size.
    #[test]
    fn an_array_linking_into_its_own_body_ends_the_walk() {
        let (mut c, l) = journal(3);
        c.set(l.main_array + A_NEXT, l.main_array + A_ITEMS);
        c.set(H_N_ENTRIES, 10);
        let (_t, f) = c.write();
        let walk = split(f.all_entries_rev());
        let err = walk.error.expect("refused");
        assert!(err.to_string().contains("doesn't point past"), "{err}");
        assert_eq!(walk.offsets, l.entries);
    }

    #[test]
    fn links_outside_the_arena_are_refused() {
        let (mut c, l) = journal(3);
        let end = c.b.len() as u64;
        c.set(l.main_array + A_NEXT, end + 4096);
        c.set(H_N_ENTRIES, 10);
        c.set(l.messages[2] + D_ENTRY_ARRAY, end + 8);
        c.set(l.messages[2] + D_N_ENTRIES, 3);
        c.set(l.entries[1] + E_ITEMS, end + 16);
        let (_t, f) = c.write();
        let walk = split(f.all_entries_rev());
        assert!(walk.error.is_some());
        assert_eq!(walk.offsets, l.entries);
        let entry = f.entry(l.entries[1]).expect("the entry itself is fine");
        assert!(f.data_payload(entry.items[0]).is_err());
        let message = f.find_data(b"MESSAGE=m2").expect("lookup").expect("found");
        let walk = split(f.entries_of_rev(&message));
        assert!(walk.error.is_some());
        assert_eq!(walk.offsets, vec![l.entries[2]]);
    }

    /// A file cut short (a copy cut off, or a full disk): what lies before
    /// the cut is still read, the rest is an error.
    #[test]
    fn a_truncated_file_still_reads_what_lies_before_the_cut() {
        let (mut c, l) = journal(6);
        let (t, _f) = c.write();
        let cut = l.entries[4] + 8;
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(cut)
            .expect("truncate");
        let f = JournalFile::open(t.path()).expect("opens");
        let walk = split(f.all_entries_rev());
        assert!(walk.error.is_some());
        assert_eq!(walk.offsets, l.entries[..4]);
        for &offset in &l.entries[..4] {
            let entry = f.entry(offset).expect("before the cut");
            assert!(f.data_payload(entry.items[1]).is_ok());
        }
        assert!(f.entry(l.entries[4]).is_err());
        assert!(f.entry(l.entries[5]).is_err());
    }

    /// sd-journal refuses the whole file over these (journal-file.c,
    /// `hash_table_is_valid`). This reader still reads its entries, but no
    /// lookup goes through the table, so a bad size can't silently find
    /// nothing.
    #[test]
    fn a_bad_hash_table_size_fails_lookups_not_the_file() {
        for size in [0, 8, 7 * 16 + 8, 1 << 40] {
            let (mut c, l) = journal(3);
            c.set(H_HASH_TABLE_SIZE, size);
            let (_t, f) = c.write();
            assert!(f.find_data(b"U=x").is_err(), "size {size}");
            assert!(f.all_data().is_err(), "size {size}");
            let walk = split(f.all_entries_rev()).ok();
            assert_eq!(walk, l.entries);
        }
    }

    #[test]
    fn a_data_object_with_one_of_entry_offset_and_count_is_corruption() {
        let (_t, f) = file_with_array(&[]);
        let d = |n_entries, entry_offset| DataRef {
            offset: 0,
            n_entries,
            entry_offset,
            entry_array_offset: 0,
        };
        // A count with no in-line entry: corruption.
        assert!(split(f.entries_of_rev(&d(2, 0))).error.is_some());
        // An in-line entry not counted yet: journald mid-link, nothing to
        // read and nothing wrong.
        let s = split(f.entries_of_rev(&d(0, 1000)));
        assert!(s.error.is_none() && s.offsets.is_empty());
    }
}
