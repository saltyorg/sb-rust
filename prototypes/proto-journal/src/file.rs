//! One journal file, read with `pread` only (no mmap, so a file truncated
//! under us is an I/O error, never SIGBUS). Layouts are systemd v255's
//! src/libsystemd/sd-journal/journal-def.h; the rules are
//! https://systemd.io/JOURNAL_FILE_FORMAT/. Every offset and size read from
//! the file is checked before use: a file journald is still writing may be
//! inconsistent for a moment ("Concurrency").
//!
//! Why our own reader (FINDINGS.md has the evidence): sdjournal skips
//! `.journal~` files silently, can lose a follow on a rotation race and
//! carries a RustSec advisory through ruzstd; netdata's
//! systemd-journal-sdk maps files with mmap and a SIGBUS handler and links
//! the C zstd library; the libsystemd `systemd` crate can't link into the
//! static musl binary. Fields are read with `from_le_bytes` at checked
//! offsets rather than zerocopy or binrw: neither is in the tree, and
//! their derived casts sit badly with `forbid(unsafe_code)`.

use crate::hash;
use anyhow::{Context, Result, bail, ensure};
use std::cell::Cell;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

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

const STATE_ARCHIVED: u8 = 2;

const OBJECT_DATA: u8 = 1;
const OBJECT_ENTRY: u8 = 3;
const OBJECT_ENTRY_ARRAY: u8 = 6;

const COMPRESSED_XZ: u8 = 1 << 0;
const COMPRESSED_LZ4: u8 = 1 << 1;
const COMPRESSED_ZSTD: u8 = 1 << 2;

/// systemd's own limit on one DATA object (DATA_SIZE_MAX in journal-def.h,
/// main branch: 768 MiB, sized for coredumps stored in the journal; the
/// smaller values there are for fuzzing builds). A larger decompressed
/// payload is corruption or a bomb.
const DATA_SIZE_MAX: usize = 768 * 1024 * 1024;

/// systemd's limit on one ENTRY object (ENTRY_SIZE_MAX, same header:
/// 770 MiB). An entry holds only item offsets, so this is a loose bound;
/// the arena bound in `object` is the tight one.
const ENTRY_SIZE_MAX: u64 = 770 * 1024 * 1024;

/// Our own bound on xz decoder memory, not a journald value: journald
/// compresses with explicit LZMA2 options and a 1 MiB dictionary
/// (compress.c, compress_blob_xz), so 64 MiB is ample and still bounds what
/// a corrupt stream header can claim.
const XZ_MEM_LIMIT_KB: u32 = 64 * 1024;

/// Bytes read at once for an entry or DATA object: one `pread` covers
/// a typical one whole (an entry of 22 regular items is 416 bytes), and
/// a larger one costs a second read for the rest.
const READ_GUESS: u64 = 1024;

const OBJECT_HEADER: u64 = 16;
const ENTRY_HEADER: u64 = 64;
const DATA_HEADER: u64 = 64;
const DATA_HEADER_COMPACT: u64 = 72;
const ENTRY_ARRAY_HEADER: u64 = 24;
/// The header fields this reader uses (journal-def.h `struct Header`).
#[derive(Debug, Clone)]
pub struct Header {
    pub incompatible: u32,
    /// STATE_OFFLINE, STATE_ONLINE or STATE_ARCHIVED ("Header").
    pub state: u8,
    pub file_id: [u8; 16],
    pub seqnum_id: [u8; 16],
    pub header_size: u64,
    pub arena_size: u64,
    pub data_hash_table_offset: u64,
    pub data_hash_table_size: u64,
    pub n_entries: u64,
    pub entry_array_offset: u64,
}

impl Header {
    pub fn compact(&self) -> bool {
        self.incompatible & INCOMPAT_COMPACT != 0
    }
    pub fn keyed(&self) -> bool {
        self.incompatible & INCOMPAT_KEYED_HASH != 0
    }
    /// Archived files are never written again ("Header": a writer only
    /// appends to a file in STATE_OFFLINE, after setting it online).
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

/// A payload larger than the caller allowed. Not corruption by itself:
/// `find_data` reads a candidate only as far as the value it looks for,
/// and a query stops once its output limit is spent (`query::Journal`'s
/// `decode`).
#[derive(Debug)]
pub struct OverLimit {
    pub limit: usize,
}

impl std::fmt::Display for OverLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "payload over its limit of {} bytes", self.limit)
    }
}

impl std::error::Error for OverLimit {}

pub struct JournalFile {
    pub path: PathBuf,
    /// The open file's device and inode: a follow keeps the file by it
    /// across a rename (rotation) and past its deletion (vacuum).
    pub inode: (u64, u64),
    file: File,
    pub header: Header,
    /// Where objects may lie: header_size + arena_size, capped by the
    /// file's length. It grows when a read reaches past it (`within`).
    limit: Cell<u64>,
}

impl JournalFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let meta = file.metadata()?;
        let mut jf = Self {
            path: path.to_path_buf(),
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
    pub fn refresh(&mut self) -> Result<()> {
        let mut buf = [0u8; 272];
        let n = read_up_to(&self.file, &mut buf, 0)?;
        // The length after the header: journald grows the file before it
        // raises arena_size and links objects into the new space
        // (journal_file_allocate), so a length read first could be older
        // than the header and cut off entries it counts.
        let len = self.file.metadata()?.len();
        ensure!(n >= 208, "{}: header too short", self.path.display());
        ensure!(
            &buf[0..8] == SIGNATURE,
            "{}: not a journal file",
            self.path.display()
        );
        let incompatible = le32(&buf, 12);
        if incompatible & !INCOMPAT_KNOWN != 0 {
            bail!(
                "{}: unsupported journal format (incompatible flags {incompatible:#x}); this sb release doesn't read it",
                self.path.display()
            );
        }
        let h = Header {
            incompatible,
            state: buf[16],
            file_id: id(&buf, 24),
            seqnum_id: id(&buf, 72),
            header_size: le64(&buf, 88),
            arena_size: le64(&buf, 96),
            data_hash_table_offset: le64(&buf, 104),
            data_hash_table_size: le64(&buf, 112),
            n_entries: le64(&buf, 152),
            entry_array_offset: le64(&buf, 176),
        };
        ensure!(
            h.header_size >= 208 && h.header_size <= len,
            "{}: bad header size",
            self.path.display()
        );
        self.limit
            .set(h.header_size.saturating_add(h.arena_size).min(len));
        self.header = h;
        Ok(())
    }

    /// The open file's metadata (its link count tells a deleted file).
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// Whether `len` bytes at `offset` lie in the arena. journald goes on
    /// appending while we read, and links read live (an entry list, a
    /// hash chain) can name objects past the arena as it was when the
    /// header was read: before refusing, re-read the arena size and the
    /// file's length once and grow the limit, as sd-journal re-stats in
    /// journal_file_move_to before returning -EADDRNOTAVAIL (journal-file.c,
    /// v255).
    fn within(&self, offset: u64, len: u64) -> bool {
        let fits = |limit: u64| {
            offset >= self.header.header_size
                && offset.checked_add(len).is_some_and(|end| end <= limit)
        };
        if fits(self.limit.get()) {
            return true;
        }
        // Growth can't help an offset inside the header.
        if offset < self.header.header_size {
            return false;
        }
        let mut arena = [0u8; 8];
        let grown = self.file.read_exact_at(&mut arena, 96).is_ok()
            && self.file.metadata().is_ok_and(|m| {
                let limit = self
                    .header
                    .header_size
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
        ensure!(
            self.within(offset, len),
            "{}: offset {offset:#x}+{len} outside the arena",
            self.path.display()
        );
        let mut buf = vec![0u8; usize::try_from(len)?];
        self.file
            .read_exact_at(&mut buf, offset)
            .with_context(|| format!("{}: read at {offset:#x}", self.path.display()))?;
        Ok(buf)
    }

    /// The start of an object: its flags, its full size and its first
    /// `min(size, guess)` bytes, from one read, checked for alignment,
    /// type and bounds.
    fn object_head(&self, offset: u64, want: u8, guess: u64) -> Result<(u8, u64, Vec<u8>)> {
        ensure!(
            offset.is_multiple_of(8),
            "{}: unaligned object {offset:#x}",
            self.path.display()
        );
        let room = self.limit.get().saturating_sub(offset);
        let mut b = self.read(offset, guess.min(room).max(OBJECT_HEADER))?;
        let (kind, flags, size) = (b[0], b[1], le64(&b, 8));
        ensure!(
            kind == want,
            "{}: object {offset:#x} is type {kind}, wanted {want}",
            self.path.display()
        );
        ensure!(
            size >= OBJECT_HEADER && self.within(offset, size),
            "{}: object {offset:#x} has a bad size",
            self.path.display()
        );
        b.truncate(usize::try_from(size)?);
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

    pub fn entry(&self, offset: u64) -> Result<Entry> {
        self.entry_before(offset, u64::MAX)
    }

    /// An entry that must end by `end`: the next newer entry's offset,
    /// since journald appends objects and they never overlap. Checked
    /// before the rest of its body is read, so a corrupt size can't make
    /// many entries each read the rest of the file. Most entries fit
    /// `READ_GUESS`, so this is one `pread`, rarely two.
    pub fn entry_before(&self, offset: u64, end: u64) -> Result<Entry> {
        let (_, size, mut b) = self.object_head(offset, OBJECT_ENTRY, READ_GUESS)?;
        let item = if self.header.compact() { 4 } else { 16 };
        // At least one whole item, as sd-journal's check_object requires.
        ensure!(
            size > ENTRY_HEADER
                && size <= ENTRY_SIZE_MAX
                && (size - ENTRY_HEADER).is_multiple_of(item),
            "{}: entry {offset:#x} has a bad size",
            self.path.display()
        );
        ensure!(
            offset + size <= end,
            "{}: entry {offset:#x} overlaps the next entry",
            self.path.display()
        );
        self.object_rest(offset, size, &mut b)?;
        ensure!(
            le64(&b, 16) != 0,
            "{}: entry {offset:#x} has no sequence number",
            self.path.display()
        );
        let n = usize::try_from((size - ENTRY_HEADER) / item)?;
        let mut items = Vec::with_capacity(n);
        for i in 0..n {
            let at = 64 + i * usize::try_from(item)?;
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

    /// A DATA object's payload (`FIELD=value`), decompressed.
    pub fn data_payload(&self, offset: u64) -> Result<Vec<u8>> {
        self.payload(offset, DATA_SIZE_MAX, &mut 0)
    }

    /// A DATA object's payload, refused with `OverLimit` when it holds
    /// more than `max` bytes (at most DATA_SIZE_MAX).
    pub fn data_payload_within(&self, offset: u64, max: usize) -> Result<Vec<u8>> {
        self.payload(offset, max, &mut 0)
    }

    /// A DATA object's payload within `left` bytes, which it spends: the
    /// whole object and what it decoded, whether it succeeds or fails (a
    /// corrupt stream can decode a long way before it fails). The query's
    /// `decode` bounds its work this way.
    pub fn data_payload_spending(&self, offset: u64, left: &mut usize) -> Result<Vec<u8>> {
        let mut cost = 0;
        let out = self.payload(offset, *left, &mut cost);
        let over = cost > *left;
        *left = left.saturating_sub(cost);
        match out {
            // A failed read is reported as what it is, corruption, even
            // when it also spent what was left.
            Err(e) => Err(e),
            // Read, but its header made it spend more than was left: over.
            Ok(_) if over => Err(OverLimit { limit: cost })
                .with_context(|| format!("{}: data {offset:#x}", self.path.display())),
            ok => ok,
        }
    }

    fn payload(&self, offset: u64, max: usize, cost: &mut usize) -> Result<Vec<u8>> {
        let max = max.min(DATA_SIZE_MAX);
        let start = self.data_header();
        let (flags, size, mut b) = self.object_head(offset, OBJECT_DATA, READ_GUESS)?;
        ensure!(
            size >= start && size - start <= DATA_SIZE_MAX as u64,
            "{}: data object {offset:#x} has a bad size",
            self.path.display()
        );
        // Refused before reading the rest: stored as written, it is too
        // long; and journald stores a payload compressed only when that
        // makes it shorter (maybe_compress_payload compresses into
        // size - 1 bytes, journal-file.c v255), so compressed it decodes
        // to more still.
        if size - start > max as u64 {
            return Err(OverLimit { limit: max })
                .with_context(|| format!("{}: data {offset:#x}", self.path.display()));
        }
        // Every attempt spends the whole object, header included, so many
        // empty or failing reads still add up.
        *cost += usize::try_from(size)?;
        self.object_rest(offset, size, &mut b)?;
        b.drain(..usize::try_from(start)?);
        decompress_within(flags, b, max, cost)
            .with_context(|| format!("{}: data {offset:#x}", self.path.display()))
    }

    /// A DATA object's entry list, stored hash and next link in its hash
    /// chain, checked to lie past the object's end (`forward`).
    fn data_ref(&self, offset: u64) -> Result<(DataRef, u64, u64)> {
        let (_, size, b) = self.object_head(offset, OBJECT_DATA, DATA_HEADER)?;
        ensure!(
            size >= self.data_header(),
            "{}: short data object",
            self.path.display()
        );
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
    /// sd-journal checks it on open (journal-file.c, hash_table_is_valid):
    /// whole items, inside the arena, and no offset without a size or the
    /// reverse. sd-journal refuses the whole file; this refuses lookups
    /// only, so the entries are still read in file order.
    fn buckets(&self) -> Result<u64> {
        let (offset, size) = (
            self.header.data_hash_table_offset,
            self.header.data_hash_table_size,
        );
        if offset == 0 && size == 0 {
            return Ok(0);
        }
        ensure!(
            size > 0
                && size.is_multiple_of(16)
                && offset >= self.header.header_size + OBJECT_HEADER
                && self.within(offset, size),
            "{}: bad data hash table ({size} bytes at {offset:#x})",
            self.path.display()
        );
        Ok(size / 16)
    }

    /// The head of hash-table bucket `b` (< `buckets()`, so in the table).
    fn bucket_head(&self, b: u64) -> Result<u64> {
        let item = self.read(self.header.data_hash_table_offset + b * 16, 16)?;
        Ok(le64(&item, 0))
    }

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
                    Ok(_) => {}
                    Err(e) if e.is::<OverLimit>() => {}
                    Err(e) => return Err(e),
                }
            }
            at = next;
        }
        Ok(None)
    }

    /// Every DATA object reachable from the hash table, with its stored
    /// hash: for checking our hash functions against journald's. Each
    /// object must sit in its hash's bucket, as journald links it
    /// (journal_file_link_data: `hash % buckets`); that also means no two
    /// buckets share a chain, so the walk reads each object once.
    pub fn all_data(&self) -> Result<Vec<(u64, u64)>> {
        let buckets = self.buckets()?;
        let mut out = Vec::new();
        for b in 0..buckets {
            let mut at = self.bucket_head(b)?;
            while at != 0 {
                let (_, stored, next) = self.data_ref(at)?;
                ensure!(
                    stored % buckets == b,
                    "{}: data object {at:#x} is in hash bucket {b}, not its own",
                    self.path.display()
                );
                out.push((at, stored));
                at = next;
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    /// Entry offsets of the whole file, in file order; an error if any
    /// part of the chain can't be read.
    pub fn all_entries(&self) -> Result<Vec<u64>> {
        let mut out = self.all_entries_rev().collect::<Result<Vec<u64>>>()?;
        out.reverse();
        Ok(out)
    }

    /// Entry offsets of the whole file, newest first, read lazily.
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
            empty.first.push(anyhow::anyhow!(
                "{}: data {:#x} has a bad entry count",
                self.path.display(),
                d.offset
            ));
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

    /// An entry array's size and next link, from one read of its first 24
    /// bytes.
    fn array_head(&self, at: u64) -> Result<(u64, u64)> {
        let (_, size, b) = self.object_head(at, OBJECT_ENTRY_ARRAY, ENTRY_ARRAY_HEADER)?;
        ensure!(
            size >= ENTRY_ARRAY_HEADER,
            "{}: short entry array",
            self.path.display()
        );
        Ok((size, le64(&b, 16)))
    }
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
/// Corruption costs only what it hides, as sd-journal's generic_array_get
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
///   (journal_file_append_entry sorts items but keeps duplicates). It
///   still counts against `n`, as systemd's n_entries rose once per link;
///   reading past `n` would reach slots written after it was read.
/// - A zero item (a slot never written, as after an unclean shutdown) is
///   skipped: our choice, not parity. sd-journal depends on the path: its
///   unfiltered walk skips a bad item (generic_array_get), while its match
///   path bisects (generic_array_bisect) and cuts the array short at a
///   zero, or with v249's chain cache drops the file. UNVERIFIED against a
///   real file with one.
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
    first: Vec<anyhow::Error>,
}

impl<'a> EntriesRev<'a> {
    fn new(f: &'a JournalFile, mut at: u64, n: u64, inline: Option<u64>) -> Self {
        let item = f.array_item_size();
        let n = n.min((f.limit.get() - f.header.header_size) / ENTRY_HEADER);
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

    /// Whether `off` can be an entry's offset: aligned, past the header,
    /// inside the arena. One that can't is refused before it becomes the
    /// order check's reference, so a bogus low item can't make every
    /// older item look out of order.
    fn plausible(&self, off: u64) -> bool {
        // `within` re-stats: an entry appended since the open is in the
        // arena.
        off.is_multiple_of(8) && self.f.within(off, ENTRY_HEADER)
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
                if !self.plausible(off) {
                    return Some(Err(anyhow::anyhow!(
                        "{}: entry array {:#x} lists {off:#x}, not an entry offset",
                        self.f.path.display(),
                        self.arrays.last().map_or(0, |a| a.0)
                    )));
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
                        return Some(Err(anyhow::anyhow!(
                            "{}: entry array {at:#x} is out of order",
                            self.f.path.display()
                        )));
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
                    return Some(Err(anyhow::anyhow!(
                        "{}: in-line entry {inline:#x} is newer than its entry array chain",
                        self.f.path.display()
                    )));
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
                    self.buf.extend(b.chunks_exact(item as usize).map(|c| {
                        if item == 4 {
                            u64::from(le32(c, 0))
                        } else {
                            le64(c, 0)
                        }
                    }));
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
#[derive(Debug, Clone, Copy)]
pub struct ArrayCursor {
    at: u64,
    base: u64,
}

impl JournalFile {
    /// The offsets of the entries numbered `from` up to the header's
    /// `n_entries` in the file's entry array chain (the list of every
    /// entry, in file order), reading only those items. `cursor` is where
    /// the last call stopped, and is updated. The header must be fresh
    /// (`refresh`): journald writes the item before it raises n_entries
    /// (journal_file_link_entry), so every item below it is there.
    ///
    /// A zero item (a slot never written, as after an unclean shutdown) is
    /// returned as 0 and the read stops after it: the caller decides
    /// whether to wait for it or skip it (`EntriesRev` skips them).
    ///
    /// Why not `EntriesRev`: it learns the whole chain's shape and walks
    /// back from the end, for the newest N; a follow reads forward from a
    /// position it already holds, a few items per wake.
    pub fn entries_since(&self, cursor: &mut Option<ArrayCursor>, from: u64) -> Result<Vec<u64>> {
        let item = self.array_item_size();
        let n = self
            .header
            .n_entries
            .min((self.limit.get() - self.header.header_size) / ENTRY_HEADER);
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
                for chunk in b.chunks_exact(usize::try_from(item)?) {
                    let off = if item == 4 {
                        u64::from(le32(chunk, 0))
                    } else {
                        le64(chunk, 0)
                    };
                    if off == 0 {
                        out.push(0);
                        *cursor = Some(c);
                        return Ok(out);
                    }
                    ensure!(
                        out.last().is_none_or(|&last| off > last),
                        "{}: entry array {:#x} is out of order",
                        self.path.display(),
                        c.at
                    );
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
    ensure!(
        next == 0 || at.checked_add(size).is_some_and(|end| next >= end),
        "{}: chain link {at:#x} -> {next:#x} doesn't point past the object",
        path.display()
    );
    Ok(next)
}

/// Decompress a DATA payload the way systemd's compress.c writes it:
/// xz as an .xz stream, LZ4 as a little-endian u64 size then one LZ4 block,
/// zstd as a frame.
#[cfg(test)]
fn decompress(flags: u8, raw: Vec<u8>) -> Result<Vec<u8>> {
    decompress_within(flags, raw, DATA_SIZE_MAX, &mut 0)
}

/// As `decompress`, refusing output over `max` bytes (at most
/// DATA_SIZE_MAX), and adding to `cost` the bytes it allocated for output
/// or decoded, on success or failure.
fn decompress_within(flags: u8, raw: Vec<u8>, max: usize, cost: &mut usize) -> Result<Vec<u8>> {
    match flags & (COMPRESSED_XZ | COMPRESSED_LZ4 | COMPRESSED_ZSTD) {
        0 => {
            // `payload` checks this before reading; this is for the
            // other callers.
            if raw.len() > max {
                return Err(OverLimit { limit: max }.into());
            }
            Ok(raw)
        }
        COMPRESSED_XZ => {
            // The memory limit fails before the dictionary is allocated, and
            // `take` caps the output: a corrupt stream can't exhaust memory.
            // https://docs.rs/lzma-rust2/0.25.0/lzma_rust2/struct.XzReader.html
            let mut dec =
                lzma_rust2::XzReader::new_mem_limit(raw.as_slice(), false, XZ_MEM_LIMIT_KB);
            let out = read_counted(&mut dec, max, cost).context("xz")?;
            if out.len() > max {
                return Err(OverLimit { limit: max }).context("xz");
            }
            Ok(out)
        }
        COMPRESSED_LZ4 => {
            ensure!(raw.len() >= 8, "short lz4 payload");
            let claim = le64(&raw, 0);
            // The block is allocated at the claimed size before decoding.
            // An LZ4 block can't expand more than 255 times: past the
            // token, every byte of output beyond a literal costs at least
            // one length byte worth at most 255
            // (https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md).
            let most = (raw.len() as u64 - 8).saturating_mul(255);
            ensure!(
                claim <= most && claim <= DATA_SIZE_MAX as u64,
                "lz4 payload claims {claim} bytes from {}",
                raw.len() - 8
            );
            if claim > max as u64 {
                return Err(OverLimit { limit: max }).context("lz4");
            }
            let size = usize::try_from(claim)?;
            // Allocated whole, before decoding.
            *cost += size;
            let out = lz4_flex::block::decompress(&raw[8..], size).context("lz4")?;
            ensure!(out.len() == size, "lz4 size mismatch");
            Ok(out)
        }
        COMPRESSED_ZSTD => {
            // As sd-journal's decompress_blob_zstd (compress.c, v255): a
            // frame must declare its content size, and libzstd refuses
            // output of another length. ruzstd 0.9 doesn't check it, so
            // it is checked here; and a size over `max` is refused before
            // decoding, so a lookup or a spent budget decodes nothing.
            let content = zstd_header(&raw)
                .and_then(|h| h.content)
                .context("zstd: no content size in the frame")?;
            // A 4-byte RLE block yields at most 128 KiB (RFC 8878,
            // Block_Maximum_Size): a larger claim than the stored bytes
            // allow is corruption, not a large payload.
            let most = (raw.len() as u64 / 4 + 1).saturating_mul(128 * 1024);
            ensure!(
                content <= most,
                "zstd: the frame declares {content} bytes from {}",
                raw.len()
            );
            if content > max as u64 {
                return Err(OverLimit { limit: max }).context("zstd");
            }
            let content_len = usize::try_from(content)?;
            // ruzstd holds back up to a window of output until the frame
            // ends, so the window bounds the decoding no read returns.
            // journald's frames (one-shot ZSTD_compress) have a window
            // fitted to the content: allow twice the content, at least 1
            // KiB, and refuse a larger window as corruption.
            // https://docs.rs/ruzstd/0.9.1/ruzstd/decoding/struct.StreamingDecoder.html#method.new_with_max_window_size
            let window = content.saturating_mul(2).max(1024);
            let mut dec = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                raw.as_slice(),
                window,
            )
            .map_err(|e| anyhow::anyhow!("zstd: {e}"))?;
            // Decoded only to the declared size (plus one byte, to see a
            // longer stream): ruzstd would go on to the last block.
            let out = match read_counted(&mut dec, content_len, cost) {
                Ok(out) => out,
                Err(e) => {
                    *cost += usize::try_from(window).unwrap_or(max).min(max);
                    return Err(e).context("zstd");
                }
            };
            ensure!(
                out.len() as u64 == content,
                "zstd: {} bytes decoded, the frame declares {content}",
                out.len()
            );
            Ok(out)
        }
        other => bail!("more than one compression flag ({other:#x})"),
    }
}

/// A zstd frame header's sizes (RFC 8878, Frame_Header).
#[derive(Debug, PartialEq, Eq)]
struct ZstdHeader {
    /// The window: the content size for a single-segment frame.
    window: u64,
    /// The declared content size, when the frame has one.
    content: Option<u64>,
}

/// `None` when the header is cut short.
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

fn read_up_to(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
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
        state: 0,
        file_id: [0; 16],
        seqnum_id: [0; 16],
        header_size: 0,
        arena_size: 0,
        data_hash_table_offset: 0,
        data_hash_table_size: 0,
        n_entries: 0,
        entry_array_offset: 0,
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("4 bytes"))
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

fn id(b: &[u8], at: usize) -> [u8; 16] {
    b[at..at + 16].try_into().expect("16 bytes")
}

/// A 128-bit id as systemd prints it: 32 lowercase hex digits.
pub fn hex_id(id: &[u8; 16]) -> String {
    hex::encode(id)
}

/// Crafted journal files for the corruption tests: a regular (not
/// compact), unkeyed file laid out object by object, as journald appends
/// them (journal-def.h layouts). Shared with query.rs's tests.
#[cfg(test)]
pub mod crafted {
    use super::*;
    use std::io::Write;

    const OBJECT_DATA_HASH_TABLE: u8 = 4;
    /// Header fields (journal-def.h `struct Header`).
    pub const H_ARENA_SIZE: u64 = 96;
    pub const H_HASH_TABLE_OFFSET: u64 = 104;
    pub const H_HASH_TABLE_SIZE: u64 = 112;
    pub const H_N_ENTRIES: u64 = 152;
    pub const H_ENTRY_ARRAY_OFFSET: u64 = 176;
    /// Object fields, from the object's start.
    pub const O_SIZE: u64 = 8;
    pub const D_HASH: u64 = 16;
    pub const D_NEXT_HASH: u64 = 24;
    pub const D_ENTRY_OFFSET: u64 = 40;
    pub const D_ENTRY_ARRAY: u64 = 48;
    pub const D_N_ENTRIES: u64 = 56;
    pub const E_ITEMS: u64 = 64;
    pub const A_NEXT: u64 = 16;
    pub const A_ITEMS: u64 = 24;

    pub struct Crafted {
        pub b: Vec<u8>,
    }

    fn at(v: u64) -> usize {
        usize::try_from(v).expect("small")
    }

    impl Crafted {
        pub fn new() -> Self {
            let mut b = vec![0u8; 272];
            b[0..8].copy_from_slice(SIGNATURE);
            let mut c = Self { b };
            c.set(88, 272); // header_size
            c
        }

        pub fn set(&mut self, offset: u64, v: u64) {
            self.b[at(offset)..at(offset) + 8].copy_from_slice(&v.to_le_bytes());
        }

        pub fn get(&self, offset: u64) -> u64 {
            le64(&self.b, at(offset))
        }

        /// Append an object, 8-byte aligned as journald places them; returns
        /// its offset.
        pub fn object(&mut self, kind: u8, flags: u8, body: &[u8]) -> u64 {
            let offset = self.b.len() as u64;
            let size = OBJECT_HEADER + body.len() as u64;
            self.b.extend_from_slice(&[kind, flags, 0, 0, 0, 0, 0, 0]);
            self.b.extend_from_slice(&size.to_le_bytes());
            self.b.extend_from_slice(body);
            self.b.resize(self.b.len().next_multiple_of(8), 0);
            offset
        }

        /// The data hash table, with `buckets` empty buckets.
        pub fn hash_table(&mut self, buckets: u64) {
            let o = self.object(OBJECT_DATA_HASH_TABLE, 0, &vec![0; at(buckets * 16)]);
            self.set(H_HASH_TABLE_OFFSET, o + OBJECT_HEADER);
            self.set(H_HASH_TABLE_SIZE, buckets * 16);
        }

        /// A DATA object storing `stored` (compressed per `flags`) under
        /// `hash`, linked into its bucket's chain.
        pub fn data_raw(&mut self, flags: u8, stored: &[u8], hash: u64) -> u64 {
            let mut body = vec![0u8; 48];
            body[0..8].copy_from_slice(&hash.to_le_bytes());
            body.extend_from_slice(stored);
            let d = self.object(OBJECT_DATA, flags, &body);
            let buckets = self.get(H_HASH_TABLE_SIZE) / 16;
            let bucket = self.get(H_HASH_TABLE_OFFSET) + hash % buckets * 16;
            match self.get(bucket + 8) {
                0 => self.set(bucket, d),
                tail => self.set(tail + D_NEXT_HASH, d),
            }
            self.set(bucket + 8, d);
            d
        }

        pub fn data(&mut self, payload: &[u8]) -> u64 {
            self.data_raw(0, payload, hash::jenkins64(payload))
        }

        /// An ENTRY object; items are DATA offsets, their hashes zero.
        pub fn entry(&mut self, seqnum: u64, items: &[u64]) -> u64 {
            let mut body = vec![0u8; 48];
            body[0..8].copy_from_slice(&seqnum.to_le_bytes());
            body[8..16].copy_from_slice(&(1_000_000 + seqnum).to_le_bytes()); // realtime
            body[16..24].copy_from_slice(&seqnum.to_le_bytes()); // monotonic
            body[24..40].copy_from_slice(&[7; 16]); // boot id
            for i in items {
                body.extend_from_slice(&i.to_le_bytes());
                body.extend_from_slice(&[0; 8]);
            }
            self.object(OBJECT_ENTRY, 0, &body)
        }

        /// An ENTRY_ARRAY object with room for `capacity` items, filled
        /// with `items`.
        pub fn array(&mut self, items: &[u64], capacity: usize, next: u64) -> u64 {
            let mut body = next.to_le_bytes().to_vec();
            for i in items {
                body.extend_from_slice(&i.to_le_bytes());
            }
            body.resize(8 + 8 * capacity.max(items.len()), 0);
            self.object(OBJECT_ENTRY_ARRAY, 0, &body)
        }

        /// Write the file with arena_size covering every object, and open it.
        pub fn write(&mut self) -> (tempfile::NamedTempFile, JournalFile) {
            self.set(H_ARENA_SIZE, self.b.len() as u64 - 272);
            let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
            tmp.write_all(&self.b).expect("write");
            let jf = JournalFile::open(tmp.path()).expect("opens");
            (tmp, jf)
        }
    }

    /// The offsets of `journal`'s objects.
    pub struct Layout {
        pub main_array: u64,
        pub shared: u64,
        pub entries: Vec<u64>,
        pub messages: Vec<u64>,
    }

    /// A valid journal of `n` entries, each carrying `MESSAGE=m<i>` and the
    /// shared `U=x`. The arrays are allocated before the entries, with room
    /// for all of them, so a file cut short keeps its arrays.
    pub fn journal(n: u64) -> (Crafted, Layout) {
        let mut c = Crafted::new();
        c.hash_table(7);
        let cap = at(n);
        let main_array = c.array(&[], cap, 0);
        let shared = c.data(b"U=x");
        let shared_array = c.array(&[], cap, 0);
        let (mut entries, mut messages) = (Vec::new(), Vec::new());
        for i in 0..n {
            let m = c.data(format!("MESSAGE=m{i}").as_bytes());
            c.set(m + D_ENTRY_OFFSET, c.b.len() as u64);
            c.set(m + D_N_ENTRIES, 1);
            let e = c.entry(i + 1, &[shared, m]);
            c.set(main_array + A_ITEMS + 8 * i, e);
            if i == 0 {
                c.set(shared + D_ENTRY_OFFSET, e);
            } else {
                c.set(shared_array + A_ITEMS + 8 * (i - 1), e);
            }
            entries.push(e);
            messages.push(m);
        }
        c.set(shared + D_ENTRY_ARRAY, shared_array);
        c.set(shared + D_N_ENTRIES, n);
        c.set(H_N_ENTRIES, n);
        c.set(H_ENTRY_ARRAY_OFFSET, main_array);
        let layout = Layout {
            main_array,
            shared,
            entries,
            messages,
        };
        (c, layout)
    }
}

#[cfg(test)]
mod tests {
    use super::crafted::*;
    use super::*;
    use std::io::Write;

    /// A regular (not compact) file: a 272-byte header, then one entry
    /// array per element of `arrays`, each linked to the next. The items
    /// are bare offsets: the chain walk doesn't read the entries they
    /// point to.
    fn file_with_chain(arrays: &[&[u64]]) -> (tempfile::NamedTempFile, JournalFile) {
        let mut body = Vec::new();
        let mut at = 272u64;
        for (k, items) in arrays.iter().enumerate() {
            let size = ENTRY_ARRAY_HEADER + 8 * items.len() as u64;
            let next = if k + 1 < arrays.len() { at + size } else { 0 };
            let mut arr = vec![0u8; 24];
            arr[0] = OBJECT_ENTRY_ARRAY;
            arr[8..16].copy_from_slice(&size.to_le_bytes());
            arr[16..24].copy_from_slice(&next.to_le_bytes());
            for i in *items {
                arr.extend_from_slice(&i.to_le_bytes());
            }
            body.extend_from_slice(&arr);
            at += size;
        }
        // Room in the arena for the entries the offsets name, so the cap on
        // `n` (entries that fit the arena) doesn't bite.
        let pad = 128 * 1024u64;
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(SIGNATURE);
        b[88..96].copy_from_slice(&272u64.to_le_bytes()); // header_size
        b[96..104].copy_from_slice(&(body.len() as u64 + pad).to_le_bytes()); // arena_size
        b.extend_from_slice(&body);
        b.resize(b.len() + usize::try_from(pad).expect("small"), 0);
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(&b).expect("write");
        let jf = JournalFile::open(tmp.path()).expect("opens");
        (tmp, jf)
    }

    fn file_with_array(items: &[u64]) -> (tempfile::NamedTempFile, JournalFile) {
        file_with_chain(&[items])
    }

    /// The chain at 272 with `n` links, newest first.
    fn walk(f: &JournalFile, n: u64) -> Result<Vec<u64>> {
        EntriesRev::new(f, 272, n, None).collect()
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

    /// `n` counts links, repeats included (systemd 249 raises n_entries
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

    /// A regular file whose arena is exactly `object`, at 272.
    fn file_with_object(object: &[u8]) -> (tempfile::NamedTempFile, JournalFile) {
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(SIGNATURE);
        b[88..96].copy_from_slice(&272u64.to_le_bytes());
        b[96..104].copy_from_slice(&(object.len() as u64).to_le_bytes());
        b.extend_from_slice(object);
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(&b).expect("write");
        let jf = JournalFile::open(tmp.path()).expect("opens");
        (tmp, jf)
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

    /// An entry array too short for its next link is refused, not read
    /// past.
    #[test]
    fn a_short_entry_array_is_refused() {
        let (_t, f) = file_with_object(&object_bytes(OBJECT_ENTRY_ARRAY, 20, 24));
        let err = f.array_head(272).expect_err("refused");
        assert!(err.to_string().contains("short entry array"));
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

    #[test]
    fn entries_since_reads_across_arrays_up_to_n_entries() {
        let (t, mut f) = file_with_chain(&[&[1000, 1100], &[1200, 1300, 0, 0]]);
        set_list(&t, &mut f, 3, 272);
        let mut c = None;
        assert_eq!(
            f.entries_since(&mut c, 0).expect("reads"),
            vec![1000, 1100, 1200]
        );
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 1).expect("reads"), vec![1100, 1200]);
    }

    /// The cursor resumes where the last read stopped, also when that was
    /// exactly the end of an array.
    #[test]
    fn entries_since_resumes_from_its_cursor() {
        let (t, mut f) = file_with_chain(&[&[1000, 1100], &[1200, 1300, 1400, 1500]]);
        set_list(&t, &mut f, 2, 272);
        let mut c = None;
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000, 1100]);
        set_list(&t, &mut f, 4, 272);
        assert_eq!(f.entries_since(&mut c, 2).expect("reads"), vec![1200, 1300]);
        assert!(f.entries_since(&mut c, 4).expect("reads").is_empty());
        set_list(&t, &mut f, 6, 272);
        assert_eq!(f.entries_since(&mut c, 4).expect("reads"), vec![1400, 1500]);
        // The cursor is on the second array, not back at the head.
        assert_eq!(c.map(|c| c.base), Some(2));
    }

    /// A slot not written below n_entries comes back as 0, last.
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
        let (t, mut f) = file_with_chain(&[&[1000, 1100]]);
        set_list(&t, &mut f, 0, 0);
        let mut c = None;
        assert!(f.entries_since(&mut c, 0).expect("reads").is_empty());
        set_list(&t, &mut f, 2, 272);
        assert_eq!(f.entries_since(&mut c, 0).expect("reads"), vec![1000, 1100]);
    }

    #[test]
    fn entries_since_refuses_a_decreasing_offset() {
        let (t, mut f) = file_with_chain(&[&[2000], &[1000]]);
        set_list(&t, &mut f, 2, 272);
        assert!(f.entries_since(&mut None, 0).is_err());
    }

    #[test]
    fn all_entries_is_in_file_order() {
        let (_t, mut f) = file_with_array(&[1000, 2000, 3000]);
        f.header.entry_array_offset = 272;
        f.header.n_entries = 3;
        assert_eq!(f.all_entries().expect("reads"), vec![1000, 2000, 3000]);
    }

    #[test]
    fn an_unknown_incompatible_flag_names_the_file() {
        let (t, _f) = file_with_array(&[]);
        let mut b = std::fs::read(t.path()).expect("read");
        b[12..16].copy_from_slice(&(1u32 << 5).to_le_bytes());
        std::fs::write(t.path(), &b).expect("write");
        let err = JournalFile::open(t.path()).err().expect("refused");
        assert!(err.to_string().contains("unsupported journal format"));
    }

    /// A walk's readable offsets, in file order, and its first error.
    struct Split {
        offsets: Vec<u64>,
        error: Option<anyhow::Error>,
    }

    impl Split {
        /// The offsets of a walk that met no error.
        fn ok(self) -> Vec<u64> {
            if let Some(e) = self.error {
                panic!("walk failed: {e:#}");
            }
            self.offsets
        }
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

    /// A chain cut short hides its newest entries: the error comes first,
    /// so a caller taking only the newest few still sees it.
    #[test]
    fn a_chain_cut_short_reports_before_its_older_entries() {
        let (_t, f) = file_with_chain(&[&[1000, 2000], &[3000, 4000]]);
        let mut b = std::fs::read(_t.path()).expect("read");
        // The second array's type byte.
        b[272 + 24 + 16] = 0;
        std::fs::write(_t.path(), &b).expect("write");
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
        let d = f.find_data(b"U=x").expect("lookup").expect("found");
        assert_eq!(split(f.entries_of_rev(&d)).ok(), l.entries);
        let e = f.entry(l.entries[3]).expect("entry");
        assert_eq!(e.items, vec![l.shared, l.messages[3]]);
        let payload = f.data_payload(l.messages[3]).expect("payload");
        assert_eq!(payload, b"MESSAGE=m3");
        let all = f.all_data().expect("hash table");
        assert_eq!(all.len(), 6);
        for (off, stored) in all {
            assert_eq!(f.hash(&f.data_payload(off).expect("payload")), stored);
        }
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

    #[test]
    fn an_entry_array_running_past_the_arena_ends_the_walk() {
        let (mut c, l) = journal(3);
        c.set(l.main_array + O_SIZE, 1 << 40);
        let (_t, f) = c.write();
        let walk = split(f.all_entries_rev());
        assert!(walk.error.is_some());
        assert!(walk.offsets.is_empty());
    }

    // Links pointing backwards, into an object, or outside the arena.

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
        assert!(format!("{err:#}").contains("doesn't point past"), "{err:#}");
        assert_eq!(walk.offsets, l.entries);
    }

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
        let e = f.entry(l.entries[1]).expect("the entry itself is fine");
        assert!(f.data_payload(e.items[0]).is_err());
        let m = f.find_data(b"MESSAGE=m2").expect("lookup").expect("found");
        let walk = split(f.entries_of_rev(&m));
        assert!(walk.error.is_some());
        assert_eq!(walk.offsets, vec![l.entries[2]]);
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

    // A truncated file.

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
        for &e in &l.entries[..4] {
            let e = f.entry(e).expect("before the cut");
            assert!(f.data_payload(e.items[1]).is_ok());
        }
        assert!(f.entry(l.entries[4]).is_err());
        assert!(f.entry(l.entries[5]).is_err());
    }

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

    /// sd-journal refuses the whole file over these (journal-file.c,
    /// hash_table_is_valid). This reader still reads its entries, but no
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
        assert!(f.all_data().expect("valid").is_empty());
    }

    /// journald puts a DATA object in bucket `hash % buckets`; one found in
    /// another bucket is corruption. Checking it also stops many buckets
    /// sharing one long chain, which would make all_data quadratic.
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
            assert!(format!("{err:#}").contains("lz4 payload claims"), "{err:#}");
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
        assert!(format!("{err:#}").contains("xz"), "{err:#}");
    }

    /// `MESSAGE=compress me, compress me, compress me ` 20 times, as
    /// libzstd writes it (`zstd --no-check`, as journald's ZSTD_compress
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
    /// through the context the reader adds, for every encoding.
    #[test]
    fn a_payload_over_the_limit_is_told_apart_from_corruption() {
        let text = b"MESSAGE=over the limit".to_vec();
        let mut lz4 = (text.len() as u64).to_le_bytes().to_vec();
        lz4.extend_from_slice(&lz4_flex::block::compress(&text));
        let zstd = zstd_sample();
        let mut c = Crafted::new();
        c.hash_table(3);
        let plain = c.data(&text);
        let l = c.data_raw(COMPRESSED_LZ4, &lz4, hash::jenkins64(&text));
        let z = c.data_raw(COMPRESSED_ZSTD, &zstd, hash::jenkins64(&text));
        let x = c.data_raw(
            COMPRESSED_XZ,
            &xz_sample(),
            hash::jenkins64(b"MESSAGE=hello"),
        );
        let (_t, f) = c.write();
        for d in [plain, l, z, x] {
            let err = f.data_payload_within(d, 5).expect_err("over");
            assert!(err.is::<OverLimit>(), "{err:#}");
            assert!(f.data_payload_within(d, 1000).is_ok());
        }
        let mut bad = lz4.clone();
        bad[8] ^= 0xff;
        assert!(
            !decompress_within(COMPRESSED_LZ4, bad, 100, &mut 0)
                .expect_err("bad")
                .is::<OverLimit>()
        );
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
        assert!(!err.is::<OverLimit>(), "{err:#}");
        assert!(cost < 1 << 20, "spent {cost}");
    }

    /// A declared size the stored bytes can't hold is corruption.
    #[test]
    fn a_zstd_size_claim_beyond_the_stored_bytes_is_corruption() {
        let mut raw = vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0];
        raw.extend_from_slice(&(100u32 << 20).to_le_bytes());
        raw.extend_from_slice(&[0x01, 0x00, 0x00]);
        let err = decompress_within(COMPRESSED_ZSTD, raw, 256 << 20, &mut 0).expect_err("claim");
        assert!(!err.is::<OverLimit>(), "{err:#}");
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

    #[test]
    fn two_compression_flags_are_refused() {
        let both = COMPRESSED_LZ4 | COMPRESSED_ZSTD;
        assert!(decompress(both, b"x".to_vec()).is_err());
    }
}
