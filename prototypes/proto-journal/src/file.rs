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

pub struct JournalFile {
    pub path: PathBuf,
    /// The open file's device and inode: a follow keeps the file by it
    /// across a rename (rotation) and past its deletion (vacuum).
    pub inode: (u64, u64),
    file: File,
    pub header: Header,
    /// Where objects may lie: header_size + arena_size, capped by the
    /// file's length when the header was read.
    limit: u64,
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
            limit: 0,
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
        self.limit = h.header_size.saturating_add(h.arena_size).min(len);
        self.header = h;
        Ok(())
    }

    /// The open file's metadata (its link count tells a deleted file).
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// Read `len` bytes at `offset`, inside the arena.
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        ensure!(
            offset >= self.header.header_size
                && offset.checked_add(len).is_some_and(|end| end <= self.limit),
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
        let room = self.limit.saturating_sub(offset);
        let mut b = self.read(offset, guess.min(room).max(OBJECT_HEADER))?;
        let (kind, flags, size) = (b[0], b[1], le64(&b, 8));
        ensure!(
            kind == want,
            "{}: object {offset:#x} is type {kind}, wanted {want}",
            self.path.display()
        );
        ensure!(
            size >= OBJECT_HEADER && offset.checked_add(size).is_some_and(|e| e <= self.limit),
            "{}: object {offset:#x} has a bad size",
            self.path.display()
        );
        b.truncate(usize::try_from(size)?);
        Ok((flags, size, b))
    }

    /// A whole object of at most `max` bytes: its flags and bytes. Most
    /// objects fit `READ_GUESS`, so this is one `pread`, rarely two.
    fn object(&self, offset: u64, want: u8, max: u64) -> Result<(u8, Vec<u8>)> {
        let (flags, size, mut b) = self.object_head(offset, want, READ_GUESS)?;
        ensure!(
            size <= max,
            "{}: object {offset:#x} has a bad size",
            self.path.display()
        );
        let have = b.len() as u64;
        if size > have {
            b.extend_from_slice(&self.read(offset + have, size - have)?);
        }
        Ok((flags, b))
    }

    pub fn entry(&self, offset: u64) -> Result<Entry> {
        let (_, b) = self.object(offset, OBJECT_ENTRY, ENTRY_SIZE_MAX)?;
        let size = b.len() as u64;
        ensure!(
            size >= ENTRY_HEADER,
            "{}: entry {offset:#x} has a bad size",
            self.path.display()
        );
        let item = if self.header.compact() { 4 } else { 16 };
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
        let start = self.data_header();
        let (flags, mut b) = self.object(offset, OBJECT_DATA, start + DATA_SIZE_MAX as u64)?;
        ensure!(
            b.len() as u64 >= start,
            "{}: data object {offset:#x} has a bad size",
            self.path.display()
        );
        b.drain(..usize::try_from(start)?);
        decompress(flags, b).with_context(|| format!("{}: data {offset:#x}", self.path.display()))
    }

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
            le64(&b, 16), // hash
            le64(&b, 24), // next_hash_offset
        ))
    }

    /// The offset of hash-table bucket `b`, checked: the table's offset
    /// and size come from the file.
    fn bucket(&self, b: u64) -> Result<u64> {
        b.checked_mul(16)
            .and_then(|o| o.checked_add(self.header.data_hash_table_offset))
            .with_context(|| format!("{}: bad hash table offset", self.path.display()))
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
        let buckets = self.header.data_hash_table_size / 16;
        if buckets == 0 {
            return Ok(None);
        }
        let h = self.hash(payload);
        let item = self.read(self.bucket(h % buckets)?, 16)?;
        let mut at = le64(&item, 0);
        while at != 0 {
            let (d, stored, next) = self.data_ref(at)?;
            if stored == h && self.data_payload(at)? == payload {
                return Ok(Some(d));
            }
            at = forward(&self.path, at, next)?;
        }
        Ok(None)
    }

    /// Every DATA object reachable from the hash table, with its stored
    /// hash: for checking our hash functions against journald's.
    pub fn all_data(&self) -> Result<Vec<(u64, u64)>> {
        let buckets = self.header.data_hash_table_size / 16;
        let mut out = Vec::new();
        for b in 0..buckets {
            let item = self.read(self.bucket(b)?, 16)?;
            let mut at = le64(&item, 0);
            while at != 0 {
                let (_, stored, next) = self.data_ref(at)?;
                out.push((at, stored));
                at = forward(&self.path, at, next)?;
            }
        }
        Ok(out)
    }

    /// Entry offsets of the whole file, in file order.
    pub fn all_entries(&self) -> Result<Vec<u64>> {
        let mut out = self.all_entries_rev()?.collect::<Result<Vec<u64>>>()?;
        out.reverse();
        Ok(out)
    }

    /// Entry offsets of the whole file, newest first, read lazily.
    pub fn all_entries_rev(&self) -> Result<EntriesRev<'_>> {
        EntriesRev::new(
            self,
            self.header.entry_array_offset,
            self.header.n_entries,
            None,
        )
    }

    /// Entry offsets referencing a DATA object, newest first, read lazily:
    /// the chain of entry arrays, then the in-line first entry.
    pub fn entries_of_rev(&self, d: &DataRef) -> Result<EntriesRev<'_>> {
        if d.n_entries == 0 || d.entry_offset == 0 {
            return EntriesRev::new(self, 0, 0, None);
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
/// arrays link forward (`forward`, which rules out a cycle) and their items
/// never decrease; a decrease is corruption.
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
}

impl<'a> EntriesRev<'a> {
    fn new(f: &'a JournalFile, mut at: u64, n: u64, inline: Option<u64>) -> Result<Self> {
        let item = f.array_item_size();
        let n = n.min((f.limit - f.header.header_size) / ENTRY_HEADER);
        let mut arrays = Vec::new();
        let mut links = 0u64;
        while at != 0 && links < n {
            let (size, next) = f.array_head(at)?;
            let count = ((size - ENTRY_ARRAY_HEADER) / item).min(n - links);
            arrays.push((at, count));
            links += count;
            at = forward(&f.path, at, next)?;
        }
        Ok(Self {
            f,
            arrays,
            buf: Vec::new(),
            inline,
            last: None,
        })
    }

    fn advance(&mut self) -> Result<Option<u64>> {
        let item = self.f.array_item_size();
        loop {
            if let Some(off) = self.buf.pop() {
                if off == 0 {
                    continue;
                }
                match self.last {
                    Some(last) if off == last => continue,
                    Some(last) => ensure!(
                        off < last,
                        "{}: entry array {:#x} is out of order",
                        self.f.path.display(),
                        self.arrays.last().map_or(0, |a| a.0)
                    ),
                    None => {}
                }
                self.last = Some(off);
                return Ok(Some(off));
            }
            let Some((at, left)) = self.arrays.last_mut() else {
                // The in-line entry may repeat as the chain's first item on
                // systemd 249 (see above).
                // It is the oldest: one newer than the chain is corruption.
                let inline = self.inline.take();
                if let (Some(i), Some(last)) = (inline, self.last) {
                    ensure!(
                        i <= last,
                        "{}: in-line entry {i:#x} is newer than its entry array chain",
                        self.f.path.display()
                    );
                }
                return Ok(inline.filter(|&i| Some(i) != self.last));
            };
            if *left == 0 {
                self.arrays.pop();
                continue;
            }
            let k = (*left).min(CHUNK_ITEMS);
            let lo = *left - k;
            let b = self
                .f
                .read(*at + ENTRY_ARRAY_HEADER + lo * item, k * item)?;
            *left = lo;
            self.buf.clear();
            self.buf
                .extend(b.chunks_exact(usize::try_from(item)?).map(|c| {
                    if item == 4 {
                        u64::from(le32(c, 0))
                    } else {
                        le64(c, 0)
                    }
                }));
        }
    }
}

impl Iterator for EntriesRev<'_> {
    type Item = Result<u64>;

    /// The next older entry offset. After an error it ends: the chain past
    /// a corrupt spot isn't trusted.
    fn next(&mut self) -> Option<Result<u64>> {
        match self.advance() {
            Ok(off) => off.map(Ok),
            Err(e) => {
                self.arrays.clear();
                self.buf.clear();
                self.inline = None;
                Some(Err(e))
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
            .min((self.limit - self.header.header_size) / ENTRY_HEADER);
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
            let next = forward(&self.path, c.at, next)?;
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

/// A link to the next object in a chain, which journald only ever writes
/// forward: a link backwards is corruption, and refusing it ends any cycle.
fn forward(path: &Path, at: u64, next: u64) -> Result<u64> {
    ensure!(
        next == 0 || next > at,
        "{}: chain link {at:#x} -> {next:#x} goes backwards",
        path.display()
    );
    Ok(next)
}

/// Decompress a DATA payload the way systemd's compress.c writes it:
/// xz as an .xz stream, LZ4 as a little-endian u64 size then one LZ4 block,
/// zstd as a frame.
fn decompress(flags: u8, raw: Vec<u8>) -> Result<Vec<u8>> {
    match flags & (COMPRESSED_XZ | COMPRESSED_LZ4 | COMPRESSED_ZSTD) {
        0 => Ok(raw),
        COMPRESSED_XZ => {
            // The memory limit fails before the dictionary is allocated, and
            // `take` caps the output: a corrupt stream can't exhaust memory.
            // https://docs.rs/lzma-rust2/0.25.0/lzma_rust2/struct.XzReader.html
            let mut dec =
                lzma_rust2::XzReader::new_mem_limit(raw.as_slice(), false, XZ_MEM_LIMIT_KB);
            let mut out = Vec::new();
            (&mut dec)
                .take(DATA_SIZE_MAX as u64 + 1)
                .read_to_end(&mut out)
                .context("xz")?;
            ensure!(out.len() <= DATA_SIZE_MAX, "xz payload over DATA_SIZE_MAX");
            Ok(out)
        }
        COMPRESSED_LZ4 => {
            ensure!(raw.len() >= 8, "short lz4 payload");
            let size = usize::try_from(le64(&raw, 0))?;
            ensure!(size <= DATA_SIZE_MAX, "lz4 payload over DATA_SIZE_MAX");
            let out = lz4_flex::block::decompress(&raw[8..], size).context("lz4")?;
            ensure!(out.len() == size, "lz4 size mismatch");
            Ok(out)
        }
        COMPRESSED_ZSTD => {
            let mut dec = ruzstd::decoding::StreamingDecoder::new(raw.as_slice())
                .map_err(|e| anyhow::anyhow!("zstd: {e}"))?;
            let mut out = Vec::new();
            (&mut dec)
                .take(DATA_SIZE_MAX as u64 + 1)
                .read_to_end(&mut out)
                .context("zstd")?;
            ensure!(
                out.len() <= DATA_SIZE_MAX,
                "zstd payload over DATA_SIZE_MAX"
            );
            Ok(out)
        }
        other => bail!("more than one compression flag ({other:#x})"),
    }
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

#[cfg(test)]
mod tests {
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
        EntriesRev::new(f, 272, n, None)?.collect()
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
        let got: Vec<u64> = f
            .entries_of_rev(&d)
            .expect("opens")
            .collect::<Result<_>>()
            .expect("reads");
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
        let first: Vec<u64> = (1..=4).map(|i| i * 8).collect();
        let second: Vec<u64> = (5..=1204).map(|i| i * 8).collect();
        let third: Vec<u64> = (1205..=1300).map(|i| i * 8).collect();
        let (_t, f) = file_with_chain(&[&first, &second, &third]);
        let n = 1250;
        let want: Vec<u64> = (1..=n).rev().map(|i| i * 8).collect();
        assert_eq!(walk(&f, n).expect("reads"), want);
        // Only what is asked for is read: the first few come from the end,
        // from one chunk of the last array, and the others aren't touched.
        let mut w = EntriesRev::new(&f, 272, n, None).expect("opens");
        let head: Vec<u64> = w.by_ref().take(3).collect::<Result<_>>().expect("reads");
        assert_eq!(head, vec![n * 8, (n - 1) * 8, (n - 2) * 8]);
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
        let got: Result<Vec<u64>> = f.entries_of_rev(&d).expect("opens").collect();
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
}
