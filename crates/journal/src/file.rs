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

use crate::{OverLimit, ReadError, ReadErrorKind, hash};
use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
#[cfg(any(test, feature = "testing"))]
use std::os::unix::fs::MetadataExt;
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

/// Follow-only, as `Header::archived` is.
#[cfg(any(test, feature = "testing"))]
const STATE_ARCHIVED: u8 = 2;

pub(crate) const OBJECT_DATA: u8 = 1;
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
    pub incompatible: u32,
    /// `STATE_OFFLINE`, `STATE_ONLINE` or `STATE_ARCHIVED` ("Header").
    /// Read by the follow only (`archived`).
    #[cfg(any(test, feature = "testing"))]
    pub state: u8,
    pub file_id: [u8; 16],
    pub seqnum_id: [u8; 16],
    /// `header_size`: where the arena starts.
    pub size: u64,
    pub arena_size: u64,
    pub data_hash_table_offset: u64,
    pub data_hash_table_size: u64,
    pub n_entries: u64,
    pub entry_array_offset: u64,
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
    /// The follow's (plan Task 6): until it lands, the tools' and tests'.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn archived(&self) -> bool {
        self.state == STATE_ARCHIVED
    }
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
    /// The open file's device and inode: a follow keeps the file by it
    /// across a rename (rotation) and past its deletion (vacuum). The
    /// follow's (plan Task 6): until it lands, the tools' and tests'.
    #[cfg(any(test, feature = "testing"))]
    pub inode: (u64, u64),
    file: File,
    pub header: Header,
    /// Where objects may lie: size + `arena_size`, capped by the
    /// file's length. It grows when a read reaches past it (`within`).
    limit: Cell<u64>,
}

impl JournalFile {
    /// Open the file at `path` and read its header.
    ///
    /// # Errors
    ///
    /// The file can't be opened or read, isn't a journal file, has an
    /// incompatible flag this reader doesn't know, or a bad header size.
    pub fn open(path: &Path) -> Result<Self> {
        let io = |context: &str| {
            let context = context.to_owned();
            move |source| ReadError {
                path: Some(path.to_path_buf()),
                kind: ReadErrorKind::Io { context, source },
            }
        };
        let file = File::open(path).map_err(io("open"))?;
        #[cfg(any(test, feature = "testing"))]
        let meta = file.metadata().map_err(io("metadata"))?;
        let mut jf = Self {
            path: path.to_path_buf(),
            #[cfg(any(test, feature = "testing"))]
            inode: (meta.dev(), meta.ino()),
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
        let h = Header {
            incompatible,
            #[cfg(any(test, feature = "testing"))]
            state: buf[16],
            file_id: id(&buf, 24),
            seqnum_id: id(&buf, 72),
            size: le64(&buf, 88),
            arena_size: le64(&buf, 96),
            data_hash_table_offset: le64(&buf, 104),
            data_hash_table_size: le64(&buf, 112),
            n_entries: le64(&buf, 152),
            entry_array_offset: le64(&buf, 176),
        };
        self.check(h.size >= 208 && h.size <= len, || "bad header size".into())?;
        self.limit.set(h.size.saturating_add(h.arena_size).min(len));
        self.header = h;
        Ok(())
    }

    /// The open file's metadata (its link count tells a deleted file).
    /// The follow's (plan Task 6): until it lands, the tools' and tests'.
    ///
    /// # Errors
    ///
    /// The system's, from `fstat`.
    #[cfg(any(test, feature = "testing"))]
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
        let (offset, size) = (
            self.header.data_hash_table_offset,
            self.header.data_hash_table_size,
        );
        if offset == 0 && size == 0 {
            return Ok(0);
        }
        self.check(
            size > 0
                && size.is_multiple_of(16)
                && offset >= self.header.size + OBJECT_HEADER
                && self.within(offset, size),
            || format!("bad data hash table ({size} bytes at {offset:#x})"),
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
                    Err(e) if !matches!(e.kind(), ReadErrorKind::OverLimit { .. }) => {
                        return Err(e);
                    }
                    _ => {}
                }
            }
            at = next;
        }
        Ok(None)
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

/// Where a forward read of the file's entry array chain stopped (for
/// following): the array object at `at`, whose first item is the chain's
/// item number `base`. Arrays are only appended and linked forward, so a
/// later read resumes there instead of walking the chain from its head.
/// The follow's (plan Task 6), as `entries_since` is.
#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Clone, Copy)]
pub struct ArrayCursor {
    at: u64,
    base: u64,
}

/// The follow's reads (plan Task 6): until it lands, the tools' and tests'.
#[cfg(any(test, feature = "testing"))]
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
fn read_up_to(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
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
        incompatible: 0,
        #[cfg(any(test, feature = "testing"))]
        state: 0,
        file_id: [0; 16],
        seqnum_id: [0; 16],
        size: 0,
        arena_size: 0,
        data_hash_table_offset: 0,
        data_hash_table_size: 0,
        n_entries: 0,
        entry_array_offset: 0,
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
            assert!(is_over(err.kind()), "{err}");
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
        assert!(!is_over(err.kind()), "{err}");
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
        assert!(!is_over(err.kind()), "{err}");
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
        assert!(is_over(err.kind()), "{err}");
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
