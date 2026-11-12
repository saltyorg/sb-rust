//! Test support, under the `testing` feature (journal spec, Project
//! Structure): one file's reading, re-exported for the tools, the
//! crafted-file builder, the injectable roots and opener counts, the
//! tools' logic and JSON form (`tools`), and the fuzz harness's mutations
//! and failure rules (`fuzz`).

use crate::{Journal, OpenError, Release, Source};

pub mod fuzz;
pub mod tools;

// One file's reading, for the tools (the journal spec, Project
// Structure: the tools need file-level access).
pub use crate::file::{
    ArrayCursor, DataRef, EntriesRev, Entry, Header, INCOMPAT_COMPACT, INCOMPAT_KEYED_HASH,
    INCOMPAT_KNOWN, INCOMPAT_LZ4, INCOMPAT_XZ, INCOMPAT_ZSTD, JournalFile, SIGNATURE, hex_id,
};
// Where `Source::Local` looks, and what a journal's opener counted.
pub use crate::open::{Counts, Host};

/// `Journal::open`, with `Source::Local`'s machine-id file and roots taken
/// from `host` instead of the system's (journal spec, Testing Strategy:
/// "Roots"; M-MOCKABLE-SYSCALLS).
///
/// # Errors
///
/// As `Journal::open`.
pub fn open_on(source: Source, release: &Release, host: &Host) -> Result<Journal, OpenError> {
    Journal::open_on(source, release, host)
}

/// The files `journal`'s pages have opened and closed, and the preads they
/// made, since it was opened. Each journal counts its own.
#[must_use]
pub fn counts(journal: &Journal) -> Counts {
    journal.opener.counts()
}

/// A test seam between a filtered page's two header reads of a file
/// (journal spec decision 13; sb-journal hardening, Task 33): the tests
/// place journald's writes there, as a journal written during the page
/// would hold them.
pub mod seam {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static BETWEEN: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Removes the hook when `between_header_reads` returns or unwinds.
    struct Reset;

    impl Drop for Reset {
        fn drop(&mut self) {
            BETWEEN.take();
        }
    }

    /// Run `run` with `hook` called, on this thread, with each file's path
    /// just before a filtered page reads that file's header again.
    pub fn between_header_reads<R>(
        hook: impl FnMut(&Path) + 'static,
        run: impl FnOnce() -> R,
    ) -> R {
        BETWEEN.set(Some(Box::new(hook)));
        let _reset = Reset;
        run()
    }

    /// The seam itself, called by the page. The hook is taken out while it
    /// runs, so one that reads the journal again isn't called from inside
    /// itself, and put back after.
    pub(crate) fn before_second_read(path: &Path) {
        let Some(mut hook) = BETWEEN.take() else {
            return;
        };
        hook(path);
        // <https://doc.rust-lang.org/std/thread/struct.LocalKey.html#method.with_borrow_mut>
        BETWEEN.with_borrow_mut(|slot| {
            if slot.is_none() {
                *slot = Some(hook);
            }
        });
    }
}

/// Crafted journal files for the corruption tests: an unkeyed file, regular
/// or compact (`Format`), laid out object by object, as journald appends
/// them (journal-def.h layouts). "Valid" here means valid to sb's reader
/// only: sd-journal 255 and later refuses these files, their header tail
/// fields being zero.
pub mod crafted {
    use crate::file::{
        INCOMPAT_COMPACT, JournalFile, OBJECT_DATA, OBJECT_ENTRY, OBJECT_ENTRY_ARRAY,
        OBJECT_HEADER, SIGNATURE, le32, le64,
    };
    use crate::hash;
    use std::io::Write;

    const OBJECT_FIELD: u8 = 2;
    const OBJECT_DATA_HASH_TABLE: u8 = 4;
    const OBJECT_FIELD_HASH_TABLE: u8 = 5;

    // Header fields' offsets (journal-def.h `struct Header`).

    /// `incompatible_flags`.
    pub const H_INCOMPATIBLE: u64 = 12;
    /// `arena_size`.
    pub const H_ARENA_SIZE: u64 = 96;
    /// `data_hash_table_offset`.
    pub const H_HASH_TABLE_OFFSET: u64 = 104;
    /// `data_hash_table_size`.
    pub const H_HASH_TABLE_SIZE: u64 = 112;
    /// `field_hash_table_offset`.
    pub const H_FIELD_TABLE_OFFSET: u64 = 120;
    /// `field_hash_table_size`.
    pub const H_FIELD_TABLE_SIZE: u64 = 128;
    /// `n_entries`.
    pub const H_N_ENTRIES: u64 = 152;
    /// `entry_array_offset`.
    pub const H_ENTRY_ARRAY_OFFSET: u64 = 176;

    // Object fields' offsets, from the object's start: `O_` any object,
    // `D_` a DATA object, `E_` an ENTRY, `A_` an ENTRY_ARRAY.

    /// The object's `size`.
    pub const O_SIZE: u64 = 8;
    /// `hash`.
    pub const D_HASH: u64 = 16;
    /// `next_hash_offset`.
    pub const D_NEXT_HASH: u64 = 24;
    /// `next_field_offset`: the next (older) DATA object of the field.
    pub const D_NEXT_FIELD: u64 = 32;
    /// A FIELD object's `head_data_offset`: its newest DATA object.
    pub const F_HEAD_DATA: u64 = 32;
    /// `entry_offset`: the in-line first entry.
    pub const D_ENTRY_OFFSET: u64 = 40;
    /// `entry_array_offset`.
    pub const D_ENTRY_ARRAY: u64 = 48;
    /// `n_entries`.
    pub const D_N_ENTRIES: u64 = 56;
    /// A compact DATA object's `tail_entry_array_offset` (32 bits).
    pub const D_TAIL_ARRAY: u64 = 64;
    /// A compact DATA object's `tail_entry_array_n_entries` (32 bits).
    pub const D_TAIL_N: u64 = 68;
    /// The first item.
    pub const E_ITEMS: u64 = 64;
    /// `next_entry_array_offset`.
    pub const A_NEXT: u64 = 16;
    /// The first item.
    pub const A_ITEMS: u64 = 24;

    /// How a file lays out its objects (journal-def.h). A regular file's
    /// entry items are a 64-bit offset and a hash, its entry-array items
    /// 64-bit offsets, and its DATA payloads start at 64. A compact file
    /// (`HEADER_INCOMPATIBLE_COMPACT`, journald 252 and later) has 32-bit
    /// entry and entry-array items, and DATA objects 8 bytes longer: their
    /// tail entry array's offset and item count come before the payload.
    ///
    /// Raw `set` and `get` at `A_ITEMS` or `E_ITEMS` are for regular files
    /// only: in a compact file a 64-bit write there overwrites two items
    /// (`Crafted::set_array_item` writes either width).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Format {
        Regular,
        Compact,
    }

    impl Format {
        /// Both formats, for a test run over each.
        pub const ALL: [Self; 2] = [Self::Regular, Self::Compact];

        /// An entry-array item's width in bytes: 8 in a regular file, 4 in
        /// a compact one.
        #[must_use]
        pub fn array_item_width(self) -> u64 {
            match self {
                Self::Regular => 8,
                Self::Compact => 4,
            }
        }

        /// Where a DATA object's payload starts, from the object's start:
        /// 64 in a regular file, 72 in a compact one.
        #[must_use]
        pub fn payload_at(self) -> u64 {
            match self {
                Self::Regular => 64,
                Self::Compact => 72,
            }
        }
    }

    /// Run `test` once in each format, naming the format first, so a
    /// failing run's captured output says which one failed.
    pub fn each_format(mut test: impl FnMut(Format)) {
        for format in Format::ALL {
            eprintln!("format: {format:?}");
            test(format);
        }
    }

    /// A journal file's bytes, built up object by object. The format is
    /// the header's: with `INCOMPAT_COMPACT` set, the objects appended are
    /// compact.
    #[derive(Debug, Clone)]
    pub struct Crafted {
        pub b: Vec<u8>,
    }

    fn at(v: u64) -> usize {
        usize::try_from(v).expect("small")
    }

    /// A compact file's 32-bit offset or count, little-endian (journald
    /// keeps a compact file under 4 GiB for this).
    fn small32(v: u64) -> [u8; 4] {
        u32::try_from(v).expect("fits 32 bits").to_le_bytes()
    }

    impl Default for Crafted {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Crafted {
        /// A 272-byte header with the signature and its size, no objects.
        #[must_use]
        pub fn new() -> Self {
            let mut b = vec![0u8; 272];
            b[0..8].copy_from_slice(SIGNATURE);
            let mut c = Self { b };
            c.set(88, 272); // header_size
            c
        }

        /// A 272-byte header of `format`, no objects.
        #[must_use]
        pub fn new_in(format: Format) -> Self {
            let mut c = Self::new();
            if format == Format::Compact {
                c.b[at(H_INCOMPATIBLE)..at(H_INCOMPATIBLE) + 4]
                    .copy_from_slice(&INCOMPAT_COMPACT.to_le_bytes());
            }
            c
        }

        /// The format the header names.
        #[must_use]
        pub fn format(&self) -> Format {
            if le32(&self.b, at(H_INCOMPATIBLE)) & INCOMPAT_COMPACT == 0 {
                Format::Regular
            } else {
                Format::Compact
            }
        }

        /// Write `v` little-endian at `offset`.
        ///
        /// # Panics
        ///
        /// `offset + 8` lies past the bytes built so far.
        pub fn set(&mut self, offset: u64, v: u64) {
            self.b[at(offset)..at(offset) + 8].copy_from_slice(&v.to_le_bytes());
        }

        /// The little-endian `u64` at `offset`.
        ///
        /// # Panics
        ///
        /// `offset + 8` lies past the bytes built so far.
        #[must_use]
        pub fn get(&self, offset: u64) -> u64 {
            le64(&self.b, at(offset))
        }

        /// Set item `i` of the entry array at `array` to `entry`, 64 bits
        /// wide in a regular file, 32 in a compact one. Raw `set` and `get`
        /// at `A_ITEMS` or `E_ITEMS` are for regular files only: in a
        /// compact file a 64-bit write there overwrites two items.
        ///
        /// # Panics
        ///
        /// The item lies past the bytes built so far, or in a compact file
        /// `entry` doesn't fit 32 bits.
        pub fn set_array_item(&mut self, array: u64, i: u64, entry: u64) {
            if self.format() == Format::Compact {
                self.set32(array + A_ITEMS + 4 * i, entry);
            } else {
                self.set(array + A_ITEMS + 8 * i, entry);
            }
        }

        /// Item `i` of the entry array at `array`, read at the file's
        /// width (`set_array_item`).
        ///
        /// # Panics
        ///
        /// The item lies past the bytes built so far.
        #[must_use]
        pub fn array_item(&self, array: u64, i: u64) -> u64 {
            if self.format() == Format::Compact {
                u64::from(le32(&self.b, at(array + A_ITEMS + 4 * i)))
            } else {
                self.get(array + A_ITEMS + 8 * i)
            }
        }

        /// In a compact file, set the DATA object `data`'s tail entry
        /// array and its item count, as journald keeps them
        /// (`link_entry_into_array`); a regular DATA object has no such
        /// fields, and this does nothing.
        ///
        /// # Panics
        ///
        /// The object lies past the bytes built so far, or `array` or `n`
        /// doesn't fit 32 bits.
        pub fn set_tail(&mut self, data: u64, array: u64, n: u64) {
            if self.format() == Format::Compact {
                self.set32(data + D_TAIL_ARRAY, array);
                self.set32(data + D_TAIL_N, n);
            }
        }

        fn set32(&mut self, offset: u64, v: u64) {
            self.b[at(offset)..at(offset) + 4].copy_from_slice(&small32(v));
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

        /// The field hash table, with `buckets` empty buckets.
        pub fn field_table(&mut self, buckets: u64) {
            let o = self.object(OBJECT_FIELD_HASH_TABLE, 0, &vec![0; at(buckets * 16)]);
            self.set(H_FIELD_TABLE_OFFSET, o + OBJECT_HEADER);
            self.set(H_FIELD_TABLE_SIZE, buckets * 16);
        }

        /// A FIELD object named `name` under its Jenkins hash, linked into
        /// its bucket's chain, listing the DATA objects `data` (given
        /// oldest first) as journald links them (`journal_file_link_data`
        /// prepends): the head is the newest, each pointing to the one
        /// before.
        ///
        /// # Panics
        ///
        /// There is no field hash table (`field_table`) yet.
        pub fn field(&mut self, name: &[u8], data: &[u64]) -> u64 {
            let hash = hash::jenkins64(name);
            let mut body = vec![0u8; 24];
            body[0..8].copy_from_slice(&hash.to_le_bytes());
            body.extend_from_slice(name);
            let f = self.object(OBJECT_FIELD, 0, &body);
            let buckets = self.get(H_FIELD_TABLE_SIZE) / 16;
            let bucket = self.get(H_FIELD_TABLE_OFFSET) + hash % buckets * 16;
            match self.get(bucket + 8) {
                0 => self.set(bucket, f),
                tail => self.set(tail + D_NEXT_HASH, f),
            }
            self.set(bucket + 8, f);
            let mut next = 0;
            for &d in data {
                self.set(d + D_NEXT_FIELD, next);
                next = d;
            }
            self.set(f + F_HEAD_DATA, next);
            f
        }

        /// A DATA object storing `stored` (compressed per `flags`) under
        /// `hash`, linked into its bucket's chain.
        ///
        /// # Panics
        ///
        /// There is no hash table (`hash_table`) yet.
        pub fn data_raw(&mut self, flags: u8, stored: &[u8], hash: u64) -> u64 {
            // A compact DATA object's tail array fields come before the
            // payload; `set_tail` fills them.
            let mut body = vec![0u8; at(self.format().payload_at() - OBJECT_HEADER)];
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

        /// A DATA object storing `payload` uncompressed, under its Jenkins
        /// hash.
        pub fn data(&mut self, payload: &[u8]) -> u64 {
            self.data_raw(0, payload, hash::jenkins64(payload))
        }

        /// An ENTRY object; items are DATA offsets: in a regular file each
        /// with a hash, zero here, in a compact file 32 bits alone.
        ///
        /// # Panics
        ///
        /// In a compact file, an item doesn't fit 32 bits.
        pub fn entry(&mut self, seqnum: u64, items: &[u64]) -> u64 {
            let compact = self.format() == Format::Compact;
            let mut body = vec![0u8; 48];
            body[0..8].copy_from_slice(&seqnum.to_le_bytes());
            body[8..16].copy_from_slice(&(1_000_000 + seqnum).to_le_bytes()); // realtime
            body[16..24].copy_from_slice(&seqnum.to_le_bytes()); // monotonic
            body[24..40].copy_from_slice(&[7; 16]); // boot id
            for &i in items {
                if compact {
                    body.extend_from_slice(&small32(i));
                } else {
                    body.extend_from_slice(&i.to_le_bytes());
                    body.extend_from_slice(&[0; 8]);
                }
            }
            self.object(OBJECT_ENTRY, 0, &body)
        }

        /// An `ENTRY_ARRAY` object with room for `capacity` items, filled
        /// with `items`: 64 bits each in a regular file, 32 in a compact
        /// one.
        ///
        /// # Panics
        ///
        /// In a compact file, an item doesn't fit 32 bits.
        pub fn array(&mut self, items: &[u64], capacity: usize, next: u64) -> u64 {
            let compact = self.format() == Format::Compact;
            let width = if compact { 4 } else { 8 };
            let mut body = next.to_le_bytes().to_vec();
            for &i in items {
                if compact {
                    body.extend_from_slice(&small32(i));
                } else {
                    body.extend_from_slice(&i.to_le_bytes());
                }
            }
            body.resize(8 + width * capacity.max(items.len()), 0);
            self.object(OBJECT_ENTRY_ARRAY, 0, &body)
        }

        /// Write the file with `arena_size` covering every object, and open it.
        ///
        /// # Panics
        ///
        /// The temporary file can't be written, or the file doesn't open.
        pub fn write(&mut self) -> (tempfile::NamedTempFile, JournalFile) {
            let tmp = self.file();
            let jf = JournalFile::open(tmp.path()).expect("opens");
            (tmp, jf)
        }

        /// Write the file with `arena_size` covering every object, without
        /// opening it (for a file meant to be refused).
        ///
        /// # Panics
        ///
        /// The temporary file can't be written, or the bytes are shorter
        /// than the header.
        pub fn file(&mut self) -> tempfile::NamedTempFile {
            self.set(H_ARENA_SIZE, self.b.len() as u64 - 272);
            let mut tmp = tempfile::NamedTempFile::new().expect("temp file");
            tmp.write_all(&self.b).expect("write");
            tmp
        }
    }

    /// The offsets of `journal`'s objects.
    #[derive(Debug, Clone)]
    pub struct Layout {
        pub main_array: u64,
        pub shared: u64,
        pub entries: Vec<u64>,
        pub messages: Vec<u64>,
    }

    /// A journal valid to sb's reader of `n` entries, each carrying `MESSAGE=m<i>` and the
    /// shared `U=x`. The arrays are allocated before the entries, with room
    /// for all of them, so a file cut short keeps its arrays.
    #[must_use]
    pub fn journal(n: u64) -> (Crafted, Layout) {
        journal_in(Format::Regular, n)
    }

    /// `journal`, in `format`.
    #[must_use]
    pub fn journal_in(format: Format, n: u64) -> (Crafted, Layout) {
        let mut c = Crafted::new_in(format);
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
            c.set_array_item(main_array, i, e);
            if i == 0 {
                c.set(shared + D_ENTRY_OFFSET, e);
            } else {
                c.set_array_item(shared_array, i - 1, e);
            }
            entries.push(e);
            messages.push(m);
        }
        c.set(shared + D_ENTRY_ARRAY, shared_array);
        if n > 1 {
            c.set_tail(shared, shared_array, n - 1);
        }
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

    /// A journal valid to sb's reader of one entry per element of `entries`, entry `i`
    /// carrying `MESSAGE=m<i>` and then the payloads given, in that order.
    /// Each distinct payload is one DATA object listing its entries (the
    /// first in-line, the rest in one array, empty for one entry). Returns
    /// the entries' offsets.
    #[must_use]
    pub fn journal_of(entries: &[&[&[u8]]]) -> (Crafted, Vec<u64>) {
        journal_of_in(Format::Regular, entries)
    }

    /// `journal_of`, in `format`.
    #[must_use]
    pub fn journal_of_in(format: Format, entries: &[&[&[u8]]]) -> (Crafted, Vec<u64>) {
        let mut c = Crafted::new_in(format);
        c.hash_table(7);
        let main = c.array(&[], entries.len(), 0);
        // Each payload's DATA object, in the order first met.
        let mut objects: Vec<(Vec<u8>, u64)> = Vec::new();
        let mut items_of = Vec::new();
        for (i, extra) in entries.iter().enumerate() {
            let message = format!("MESSAGE=m{i}").into_bytes();
            let mut items = Vec::new();
            for payload in std::iter::once(&message[..]).chain(extra.iter().copied()) {
                let d = if let Some(&(_, d)) = objects.iter().find(|(p, _)| p == payload) {
                    d
                } else {
                    let d = c.data(payload);
                    objects.push((payload.to_vec(), d));
                    d
                };
                items.push(d);
            }
            items_of.push(items);
        }
        let offsets: Vec<u64> = items_of
            .iter()
            .enumerate()
            .map(|(i, items)| c.entry(i as u64 + 1, items))
            .collect();
        for (k, &e) in offsets.iter().enumerate() {
            c.set_array_item(main, k as u64, e);
        }
        for &(_, d) in &objects {
            let listing: Vec<u64> = offsets
                .iter()
                .zip(&items_of)
                .filter(|(_, items)| items.contains(&d))
                .map(|(&e, _)| e)
                .collect();
            c.set(d + D_ENTRY_OFFSET, listing[0]);
            c.set(d + D_N_ENTRIES, listing.len() as u64);
            // The rest, possibly none (an array a lone entry never reads).
            let rest = c.array(&listing[1..], 0, 0);
            c.set(d + D_ENTRY_ARRAY, rest);
            if listing.len() > 1 {
                c.set_tail(d, rest, listing.len() as u64 - 1);
            }
        }
        c.set(H_N_ENTRIES, offsets.len() as u64);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        (c, offsets)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `journal_of`'s entries carry their fields in order, and each
        /// payload's object lists exactly the entries carrying it, in
        /// either format.
        #[test]
        fn journal_of_links_each_payload_to_its_entries() {
            for format in Format::ALL {
                journal_of_links_in(format);
            }
        }

        fn journal_of_links_in(format: Format) {
            let (mut c, offsets) =
                journal_of_in(format, &[&[b"A=1"], &[b"B=1", b"A=1"], &[b"A=1"]]);
            let (_t, file) = c.write();
            assert_eq!(file.header.compact(), format == Format::Compact);
            assert_eq!(file.header.n_entries, 3);
            assert_eq!(file.all_entries().expect("entries"), offsets);
            let a = file.find_data(b"A=1").expect("lookup").expect("found");
            let b = file.find_data(b"B=1").expect("lookup").expect("found");
            assert_eq!((a.n_entries, b.n_entries), (3, 1));
            let rev = |d| {
                file.entries_of_rev(d)
                    .collect::<Result<Vec<_>, _>>()
                    .expect("walks")
            };
            assert_eq!(rev(&a), [offsets[2], offsets[1], offsets[0]]);
            assert_eq!(rev(&b), [offsets[1]]);
            if format == Format::Compact {
                // A=1's two later entries are in its array, its tail; B=1
                // has none.
                let array = u32::try_from(c.get(a.offset + D_ENTRY_ARRAY)).expect("small");
                let tail = |d: u64| {
                    (
                        le32(&c.b, at(d + D_TAIL_ARRAY)),
                        le32(&c.b, at(d + D_TAIL_N)),
                    )
                };
                assert_eq!(tail(a.offset), (array, 2));
                assert_eq!(tail(b.offset), (0, 0));
            }
            let second = file.entry(offsets[1]).expect("entry");
            let payloads: Vec<Vec<u8>> = second
                .items
                .iter()
                .map(|&d| file.data_payload(d).expect("payload"))
                .collect();
            assert_eq!(payloads, [&b"MESSAGE=m1"[..], b"B=1", b"A=1"]);
            assert_eq!(second.seqnum, 2);
        }

        #[test]
        fn default_is_new() {
            assert_eq!(Crafted::default().b, Crafted::new().b);
        }

        /// A regular header is `new`'s; a compact one differs only by
        /// `INCOMPAT_COMPACT` in `incompatible_flags`, and the reader
        /// takes it as compact.
        #[test]
        fn new_in_sets_the_compact_flag_alone() {
            assert_eq!(Crafted::new_in(Format::Regular).b, Crafted::new().b);
            assert_eq!(Crafted::new().format(), Format::Regular);
            let mut c = Crafted::new_in(Format::Compact);
            assert_eq!(c.format(), Format::Compact);
            let mut expected = Crafted::new().b;
            expected[12..16].copy_from_slice(&INCOMPAT_COMPACT.to_le_bytes());
            assert_eq!(c.b, expected);
            let (_t, f) = c.write();
            assert!(f.header.compact() && !f.header.keyed());
        }

        /// In a compact file, entries hold 32-bit items with no hash,
        /// entry arrays 32-bit items, and DATA objects their tail array's
        /// offset and count (32 bits each) before the payload, at 72
        /// (journal-def.h `DataObject`, `EntryObject`, `EntryArrayObject`).
        #[test]
        fn compact_objects_take_the_compact_layout() {
            let mut c = Crafted::new_in(Format::Compact);
            c.hash_table(1);
            let data = c.data(b"A=1");
            assert_eq!(c.get(data + O_SIZE), 72 + 3);
            assert_eq!(&c.b[at(data + 72)..at(data + 75)], b"A=1");
            assert_eq!(c.get(data + D_HASH), hash::jenkins64(b"A=1"));
            let entry = c.entry(5, &[0x400, 0x500]);
            assert_eq!(c.get(entry + O_SIZE), 64 + 2 * 4);
            assert_eq!(c.get(entry + 16), 5); // seqnum
            let item = |c: &Crafted, offset: u64| le32(&c.b, at(offset));
            assert_eq!(
                (item(&c, entry + E_ITEMS), item(&c, entry + E_ITEMS + 4)),
                (0x400, 0x500)
            );
            let array = c.array(&[entry, data], 3, 0x1234);
            assert_eq!(c.get(array + O_SIZE), 24 + 3 * 4);
            assert_eq!(c.get(array + A_NEXT), 0x1234);
            assert_eq!(
                (
                    u64::from(item(&c, array + A_ITEMS)),
                    u64::from(item(&c, array + A_ITEMS + 4))
                ),
                (entry, data)
            );
            assert_eq!(item(&c, array + A_ITEMS + 8), 0);
            c.set_array_item(array, 2, 0x88);
            assert_eq!(item(&c, array + A_ITEMS + 8), 0x88);
            assert_eq!(
                (c.array_item(array, 1), c.array_item(array, 2)),
                (data, 0x88)
            );
            // The regular layout's item, for comparison.
            let mut regular = Crafted::new();
            let array = regular.array(&[], 2, 0);
            regular.set_array_item(array, 1, 0x88);
            assert_eq!(regular.get(array + A_ITEMS + 8), 0x88);
            assert_eq!(regular.array_item(array, 1), 0x88);
        }

        /// `each_format` runs its test once per format, regular first.
        #[test]
        fn each_format_runs_both_formats_in_order() {
            let mut ran = Vec::new();
            each_format(|format| ran.push(format));
            assert_eq!(ran, Format::ALL);
        }

        /// `journal_in` reads back in either format: the entries in file
        /// order both ways, their fields in order, and each payload's
        /// entries backwards and forwards. A compact file's shared DATA
        /// object names its tail array and that array's item count.
        #[test]
        fn journal_in_reads_back_in_either_format() {
            for format in Format::ALL {
                let (mut c, layout) = journal_in(format, 5);
                let (_t, f) = c.write();
                assert_eq!(f.header.compact(), format == Format::Compact);
                assert_eq!(f.all_entries().expect("entries"), layout.entries);
                let rev: Vec<u64> = f
                    .all_entries_rev()
                    .collect::<Result<_, _>>()
                    .expect("walks");
                let newest_first: Vec<u64> = layout.entries.iter().rev().copied().collect();
                assert_eq!(rev, newest_first);
                assert_eq!(
                    f.entries_since(&mut None, 1).expect("forward"),
                    layout.entries[1..]
                );
                for (i, &e) in layout.entries.iter().enumerate() {
                    let entry = f.entry(e).expect("entry");
                    assert_eq!(entry.seqnum, i as u64 + 1);
                    let payloads: Vec<Vec<u8>> = entry
                        .items
                        .iter()
                        .map(|&d| f.data_payload(d).expect("payload"))
                        .collect();
                    assert_eq!(
                        payloads,
                        [b"U=x".to_vec(), format!("MESSAGE=m{i}").into_bytes()]
                    );
                }
                let shared = f.find_data(b"U=x").expect("lookup").expect("found");
                assert_eq!((shared.offset, shared.n_entries), (layout.shared, 5));
                let of_shared: Vec<u64> = f
                    .entries_of_rev(&shared)
                    .collect::<Result<_, _>>()
                    .expect("walks");
                assert_eq!(of_shared, newest_first);
                let list = crate::file::EntryList::of_data(&shared);
                let forward: Vec<u64> = f
                    .entries_from(&list, 0)
                    .collect::<Result<_, _>>()
                    .expect("walks");
                assert_eq!(forward, layout.entries);
                let m3 = f.find_data(b"MESSAGE=m3").expect("lookup").expect("found");
                assert_eq!(
                    (m3.offset, m3.entry_offset),
                    (layout.messages[3], layout.entries[3])
                );
                if format == Format::Compact {
                    let tail = (
                        le32(&c.b, at(layout.shared + D_TAIL_ARRAY)),
                        le32(&c.b, at(layout.shared + D_TAIL_N)),
                    );
                    let array = c.get(layout.shared + D_ENTRY_ARRAY);
                    assert_eq!(tail, (u32::try_from(array).expect("small"), 4));
                }
            }
            // One entry is in-line only: its array holds nothing, and the
            // tail names none.
            let (c, layout) = journal_in(Format::Compact, 1);
            let tail = (
                le32(&c.b, at(layout.shared + D_TAIL_ARRAY)),
                le32(&c.b, at(layout.shared + D_TAIL_N)),
            );
            assert_eq!(tail, (0, 0));
        }

        /// A bare header: the signature, a 272-byte header size, nothing
        /// else; written, its arena is empty and it opens.
        #[test]
        fn new_is_an_empty_journal() {
            let mut c = Crafted::new();
            assert_eq!(c.b.len(), 272);
            assert_eq!(&c.b[0..8], SIGNATURE);
            assert_eq!(c.get(88), 272);
            assert!(c.b[8..88].iter().chain(&c.b[96..]).all(|&x| x == 0));
            let (_t, f) = c.write();
            assert_eq!((f.header.size, f.header.arena_size), (272, 0));
        }

        /// Objects are appended 8-byte aligned, with their type, flags and
        /// size.
        #[test]
        fn objects_are_appended_aligned() {
            let mut c = Crafted::new();
            let a = c.object(9, 2, b"abc");
            let b = c.object(1, 0, &[0; 8]);
            assert_eq!((a, b), (272, 296));
            assert_eq!((c.b[272], c.b[273], c.get(a + O_SIZE)), (9, 2, 19));
            assert_eq!(&c.b[288..291], b"abc");
            assert_eq!(c.get(b + O_SIZE), 24);
            assert_eq!(c.b.len(), 320);
        }

        /// DATA objects of one bucket are chained in order, the bucket
        /// holding the head and the tail.
        #[test]
        fn data_objects_chain_in_their_bucket() {
            let mut crafted = Crafted::new();
            crafted.hash_table(2);
            let table = crafted.get(H_HASH_TABLE_OFFSET);
            assert_eq!((table, crafted.get(H_HASH_TABLE_SIZE)), (288, 32));
            let first = crafted.data_raw(0, b"X=1", 4);
            let second = crafted.data_raw(0, b"Y=1", 6);
            let other = crafted.data_raw(0, b"Z=1", 1);
            assert_eq!(
                (crafted.get(table), crafted.get(table + 8)),
                (first, second)
            );
            assert_eq!(crafted.get(first + D_NEXT_HASH), second);
            assert_eq!(
                (crafted.get(table + 16), crafted.get(table + 24)),
                (other, other)
            );
            assert_eq!(crafted.get(first + D_HASH), 4);
            let hashed = crafted.data(b"A=1");
            assert_eq!(crafted.get(hashed + D_HASH), hash::jenkins64(b"A=1"));
        }

        #[test]
        fn entries_and_arrays_are_laid_out() {
            let mut c = Crafted::new();
            let e = c.entry(5, &[0x400, 0x500]);
            assert_eq!(c.get(e + O_SIZE), 64 + 2 * 16);
            assert_eq!(c.get(e + 16), 5); // seqnum
            assert_eq!(c.get(e + 24), 1_000_005); // realtime
            assert_eq!(c.get(e + E_ITEMS), 0x400);
            assert_eq!(c.get(e + E_ITEMS + 16), 0x500);
            let a = c.array(&[e], 3, 0x1234);
            assert_eq!(c.get(a + O_SIZE), 24 + 3 * 8);
            assert_eq!(c.get(a + A_NEXT), 0x1234);
            assert_eq!(c.get(a + A_ITEMS), e);
        }

        /// `file` sets the arena to cover every object.
        #[test]
        fn file_covers_every_object() {
            let mut c = Crafted::new();
            c.object(1, 0, &[0; 40]);
            let t = c.file();
            assert_eq!(c.get(H_ARENA_SIZE), 56);
            assert_eq!(std::fs::read(t.path()).expect("read"), c.b);
        }

        /// The crafted journal's links, as `Layout` names them.
        #[test]
        fn the_journal_links_its_objects() {
            let (c, layout) = journal(3);
            assert_eq!(
                (c.get(H_N_ENTRIES), c.get(H_ENTRY_ARRAY_OFFSET)),
                (3, layout.main_array)
            );
            for (i, (&entry, &message)) in layout.entries.iter().zip(&layout.messages).enumerate() {
                let i = i as u64;
                assert_eq!(c.get(layout.main_array + A_ITEMS + 8 * i), entry);
                assert_eq!(c.get(entry + 16), i + 1); // seqnum
                assert_eq!(c.get(entry + E_ITEMS), layout.shared);
                assert_eq!(c.get(entry + E_ITEMS + 16), message);
                assert_eq!(
                    (
                        c.get(message + D_ENTRY_OFFSET),
                        c.get(message + D_N_ENTRIES)
                    ),
                    (entry, 1)
                );
            }
            let shared_array = c.get(layout.shared + D_ENTRY_ARRAY);
            assert_eq!(c.get(layout.shared + D_ENTRY_OFFSET), layout.entries[0]);
            assert_eq!(c.get(layout.shared + D_N_ENTRIES), 3);
            assert_eq!(c.get(shared_array + A_ITEMS), layout.entries[1]);
            assert_eq!(c.get(shared_array + A_ITEMS + 8), layout.entries[2]);
        }
    }
}
