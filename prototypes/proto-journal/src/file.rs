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
use std::os::unix::fs::FileExt;
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

const OBJECT_HEADER: u64 = 16;
const ENTRY_HEADER: u64 = 64;
const DATA_HEADER: u64 = 64;
const DATA_HEADER_COMPACT: u64 = 72;
const ENTRY_ARRAY_HEADER: u64 = 24;

/// The header fields this reader uses (journal-def.h `struct Header`).
#[derive(Debug, Clone)]
pub struct Header {
    pub incompatible: u32,
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
    file: File,
    pub header: Header,
    /// Where objects may lie: header_size + arena_size, capped by the
    /// file's length when the header was read.
    limit: u64,
}

impl JournalFile {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut jf = Self {
            path: path.to_path_buf(),
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
        let len = self.file.metadata()?.len();
        let mut buf = [0u8; 272];
        let n = read_up_to(&self.file, &mut buf, 0)?;
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

    /// An object's type and full size, checked for alignment and bounds.
    fn object(&self, offset: u64, want: u8) -> Result<(u8, u64)> {
        ensure!(
            offset.is_multiple_of(8),
            "{}: unaligned object {offset:#x}",
            self.path.display()
        );
        let h = self.read(offset, OBJECT_HEADER)?;
        let (kind, flags, size) = (h[0], h[1], le64(&h, 8));
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
        Ok((flags, size))
    }

    pub fn entry(&self, offset: u64) -> Result<Entry> {
        let (_, size) = self.object(offset, OBJECT_ENTRY)?;
        ensure!(
            (ENTRY_HEADER..=ENTRY_SIZE_MAX).contains(&size),
            "{}: entry {offset:#x} has a bad size",
            self.path.display()
        );
        let b = self.read(offset, size)?;
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
        let (flags, size) = self.object(offset, OBJECT_DATA)?;
        let start = self.data_header();
        ensure!(
            size >= start && size - start <= DATA_SIZE_MAX as u64,
            "{}: data object {offset:#x} has a bad size",
            self.path.display()
        );
        let raw = self.read(offset + start, size - start)?;
        decompress(flags, raw).with_context(|| format!("{}: data {offset:#x}", self.path.display()))
    }

    fn data_ref(&self, offset: u64) -> Result<(DataRef, u64, u64)> {
        let (_, size) = self.object(offset, OBJECT_DATA)?;
        ensure!(
            size >= self.data_header(),
            "{}: short data object",
            self.path.display()
        );
        let b = self.read(offset, DATA_HEADER)?;
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
        self.chain(self.header.entry_array_offset, self.header.n_entries)
    }

    /// Entry offsets referencing a DATA object, in file order.
    pub fn entries_of(&self, d: &DataRef) -> Result<Vec<u64>> {
        if d.n_entries == 0 || d.entry_offset == 0 {
            return Ok(Vec::new());
        }
        let mut out = vec![d.entry_offset];
        out.extend(self.chain(d.entry_array_offset, d.n_entries - 1)?);
        // The in-line first entry may repeat as the chain's first item on
        // systemd 249 (see `chain`).
        out.dedup();
        Ok(out)
    }

    /// Walk an entry-array chain, up to `n` links ("Entry Array Objects").
    /// A zero item is a slot not yet written: the chain ends there. journald
    /// only appends, so arrays link forward (`forward`, which rules out a
    /// cycle) and their items never decrease; a decrease is corruption. An
    /// item may repeat (see below). `n` counts links, repeats included,
    /// as systemd's n_entries does; it comes from the file, so it is capped
    /// by the most entries the arena could hold.
    fn chain(&self, mut at: u64, n: u64) -> Result<Vec<u64>> {
        let item = if self.header.compact() { 4 } else { 8 };
        let n = n.min((self.limit - self.header.header_size) / ENTRY_HEADER);
        let mut out: Vec<u64> = Vec::new();
        let mut links = 0u64;
        while at != 0 && links < n {
            let (_, size) = self.object(at, OBJECT_ENTRY_ARRAY)?;
            ensure!(
                size >= ENTRY_ARRAY_HEADER,
                "{}: short entry array",
                self.path.display()
            );
            let b = self.read(at, size)?;
            let next = le64(&b, 16);
            let count = (size - ENTRY_ARRAY_HEADER) / item;
            for i in 0..count {
                if links >= n {
                    break;
                }
                let p = 24 + usize::try_from(i * item)?;
                let off = if item == 4 {
                    u64::from(le32(&b, p))
                } else {
                    le64(&b, p)
                };
                if off == 0 {
                    return Ok(out);
                }
                // Each link counts against `n`, a repeat too: n_entries rose
                // once per link, and reading past it would reach slots
                // written after the header was read.
                links += 1;
                // Equal neighbours are valid: systemd 249 (22.04) links an
                // entry once per item, and an entry carrying the same
                // FIELD=value twice lists its offset twice in a row
                // (journal_file_append_entry sorts items but keeps
                // duplicates). sd-journal skips the repeat; so does this.
                match out.last() {
                    Some(&last) if off == last => continue,
                    Some(&last) => ensure!(
                        off > last,
                        "{}: entry array {at:#x} is out of order",
                        self.path.display()
                    ),
                    None => {}
                }
                out.push(off);
            }
            at = forward(&self.path, at, next)?;
        }
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
    /// array at 272 holding `items` with no next link. The items are bare
    /// offsets: `chain` doesn't read the entries they point to.
    fn file_with_array(items: &[u64]) -> (tempfile::NamedTempFile, JournalFile) {
        let size = ENTRY_ARRAY_HEADER + 8 * items.len() as u64;
        // Room in the arena for the entries the offsets name, so the cap on
        // `n` (entries that fit the arena) doesn't bite.
        let pad = 4096u64;
        let mut b = vec![0u8; 272];
        b[0..8].copy_from_slice(SIGNATURE);
        b[88..96].copy_from_slice(&272u64.to_le_bytes()); // header_size
        b[96..104].copy_from_slice(&(size + pad).to_le_bytes()); // arena_size
        let mut arr = vec![0u8; 24];
        arr[0] = OBJECT_ENTRY_ARRAY;
        arr[8..16].copy_from_slice(&size.to_le_bytes());
        for i in items {
            arr.extend_from_slice(&i.to_le_bytes());
        }
        b.extend_from_slice(&arr);
        b.resize(b.len() + usize::try_from(pad).expect("small"), 0);
        let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
        tmp.write_all(&b).expect("write");
        let jf = JournalFile::open(tmp.path()).expect("opens");
        (tmp, jf)
    }

    #[test]
    fn a_repeated_offset_is_skipped() {
        let (_t, f) = file_with_array(&[1000, 1000, 2000]);
        assert_eq!(f.chain(272, 3).expect("reads"), vec![1000, 2000]);
    }

    #[test]
    fn a_decreasing_offset_is_corruption() {
        let (_t, f) = file_with_array(&[2000, 1000]);
        assert!(f.chain(272, 2).is_err());
    }

    /// `n` counts links, repeats included (systemd 249 raises n_entries
    /// once per item): an item past the n-th link was written after the
    /// header was read and isn't returned.
    #[test]
    fn n_counts_raw_links_including_repeats() {
        let (_t, f) = file_with_array(&[1000, 1000, 2000, 3000]);
        assert_eq!(f.chain(272, 3).expect("reads"), vec![1000, 2000]);
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
        assert_eq!(f.entries_of(&d).expect("reads"), vec![1000, 2000]);
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
