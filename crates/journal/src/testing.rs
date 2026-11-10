//! Test support, under the `testing` feature (journal spec, Project
//! Structure): one file's reading, re-exported for the tools, the
//! crafted-file builder, the injectable roots and opener counts, and the
//! tools' logic and JSON form (`tools`).

use crate::{Journal, OpenError, Release, Source};

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

/// Crafted journal files for the corruption tests: a regular (not
/// compact), unkeyed file laid out object by object, as journald appends
/// them (journal-def.h layouts).
pub mod crafted {
    use crate::file::{
        JournalFile, OBJECT_DATA, OBJECT_ENTRY, OBJECT_ENTRY_ARRAY, OBJECT_HEADER, SIGNATURE, le64,
    };
    use crate::hash;
    use std::io::Write;

    const OBJECT_FIELD: u8 = 2;
    const OBJECT_DATA_HASH_TABLE: u8 = 4;
    const OBJECT_FIELD_HASH_TABLE: u8 = 5;

    // Header fields' offsets (journal-def.h `struct Header`).

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
    /// The first item.
    pub const E_ITEMS: u64 = 64;
    /// `next_entry_array_offset`.
    pub const A_NEXT: u64 = 16;
    /// The first item.
    pub const A_ITEMS: u64 = 24;

    /// A journal file's bytes, built up object by object.
    #[derive(Debug, Clone)]
    pub struct Crafted {
        pub b: Vec<u8>,
    }

    fn at(v: u64) -> usize {
        usize::try_from(v).expect("small")
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

        /// A DATA object storing `payload` uncompressed, under its Jenkins
        /// hash.
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

        /// An `ENTRY_ARRAY` object with room for `capacity` items, filled
        /// with `items`.
        pub fn array(&mut self, items: &[u64], capacity: usize, next: u64) -> u64 {
            let mut body = next.to_le_bytes().to_vec();
            for i in items {
                body.extend_from_slice(&i.to_le_bytes());
            }
            body.resize(8 + 8 * capacity.max(items.len()), 0);
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

    /// A valid journal of `n` entries, each carrying `MESSAGE=m<i>` and the
    /// shared `U=x`. The arrays are allocated before the entries, with room
    /// for all of them, so a file cut short keeps its arrays.
    #[must_use]
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

    /// A valid journal of one entry per element of `entries`, entry `i`
    /// carrying `MESSAGE=m<i>` and then the payloads given, in that order.
    /// Each distinct payload is one DATA object listing its entries (the
    /// first in-line, the rest in one array, empty for one entry). Returns
    /// the entries' offsets.
    #[must_use]
    pub fn journal_of(entries: &[&[&[u8]]]) -> (Crafted, Vec<u64>) {
        let mut c = Crafted::new();
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
            c.set(main + A_ITEMS + 8 * k as u64, e);
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
        }
        c.set(H_N_ENTRIES, offsets.len() as u64);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        (c, offsets)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// `journal_of`'s entries carry their fields in order, and each
        /// payload's object lists exactly the entries carrying it.
        #[test]
        fn journal_of_links_each_payload_to_its_entries() {
            let (mut c, offsets) = journal_of(&[&[b"A=1"], &[b"B=1", b"A=1"], &[b"A=1"]]);
            let (_t, file) = c.write();
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
