//! The forward page (`Position::After`): journalctl's `--after-cursor` as
//! systemd 259 (Ubuntu 26.04) reads it, emulated (journal spec, decision
//! 10). All references are to systemd v259:
//! - the seek: `sd_journal_seek_cursor`, then each file finds where the
//!   cursor falls in it (src/libsystemd/sd-journal/sd-journal.c,
//!   `find_location_with_matches` and `find_location_for_match`), by a
//!   bisect (journal-file.c `generic_array_bisect`, emulated in file.rs):
//!   - by seqnum when the file's seqnum id is the cursor's;
//!   - else by monotonic time in the cursor's boot (the `_BOOT_ID=` DATA
//!     object's list; with a query, then stepping between it and each
//!     value's list, `journal_file_move_to_entry_by_monotonic_for_data`),
//!     used only when that finds an entry;
//!   - else by realtime, over the main list, or each value's list.
//!
//!   A value bisects its DATA object's list; a term (an OR of values) takes
//!   the lowest landing, a group (an AND of terms) the highest of its
//!   terms' and then the next entry in every term (`next_for_match`), the
//!   query (an OR of groups) the lowest. Siblings are tried last-added
//!   first, as `match_new` prepends them: that order only matters through
//!   the chain cache;
//! - the landings are taken as they are, not compared with the cursor
//!   (`next_beyond_location` under `LOCATION_SEEK`), and the first step
//!   takes the lowest across files (`real_journal_next`). journalctl shows
//!   it unless it *is* the cursor's entry (`sd_journal_test_cursor`,
//!   src/journal/journalctl-show.c), so nothing after the cursor is lost
//!   when its entry has vanished or doesn't match. 24.04's journalctl
//!   does the same (systemd-stable v255.1's backport); 22.04's always
//!   steps past it: Difference 18. 26.04's (259.9) first prefers a file
//!   whose landing is the cursor's exact entry (issue #31516), which sb
//!   doesn't emulate. It differs only where another file's landing orders
//!   before the cursor's entry: across two boots the table orders against
//!   their clocks, or in a file of the cursor's boot that 259 seeks by
//!   realtime. Such a file is sought per boot here (below);
//! - a file the first step found nothing in seeks again from the entry it
//!   took (`next_beyond_location` keeps no direction after an empty seek);
//! - then forward: each file's matching entries after its landing, in file
//!   order, and across files the lowest head strictly after the last entry
//!   taken, the earlier file on a tie (`compare_with_location`, `found = k >
//!   0`). A file's later matches are walked as the backward walk matches
//!   (walk.rs, `Walk`): the rarest term's lists, the other terms by the
//!   entry's items, so a forward page agrees with a page back on which
//!   entries match.
//!
//! A filtered page reads each unsealed file's header again after its
//! lookups (journal spec decision 13). The matching entries that read adds
//! join the file's stream: before the landing the lists' seek found (or
//! where they found none) by the cursor's order, after it in file order,
//! as the lists' own entries. A bisect over a value's whole list would
//! have tested such an entry between two neighbours by the same key, so
//! that is the landing it finds, while the file's clock runs forward.
//!
//! One departure on a live file: where the realtime clock steps back among
//! the entries written during the page, a bisect over the whole list can
//! land elsewhere than the cursor's order puts such an entry, and sb
//! follows the cursor's order there.
//!
//! Departures, on corrupt files and errors only:
//! - an error is reported and the page goes on, where sd-journal leaves
//!   the whole file out ("Can't iterate through", `real_journal_next`'s
//!   `remove_file_real`): a group whose seek fails (an unreadable extra,
//!   the system's error) is left out and the other groups still seek; a
//!   file whose landing can't be read seeks again in the second step; an
//!   entry the forward walk can't read is skipped;
//! - an entry or array sd-journal's `check_object` refuses and sb doesn't
//!   (a realtime or monotonic time out of range, a null boot id, an array
//!   of no items), or the other way (an entry over `ENTRY_SIZE_MAX`), cuts
//!   a bisect short in one and not the other;
//! - a group with a term none of whose values is in the file is left out
//!   before seeking, where systemd bisects the terms it walks before that
//!   one (cache entries only; for journalctl's `-u` the term walked first
//!   is the unit's own);
//! - a zero item cuts a bisect short as systemd's does, unreported; the
//!   walks skip it (Difference 8), including a zero first item of the array
//!   a walk resumes in, which makes systemd's `next_for_match` fail and
//!   journalctl leave the file out;
//! - the query's outermost AND (`level0`, which holds one OR of groups for
//!   journalctl's `-u`) isn't stepped through again: for a file that found
//!   an entry it gives the same entry, and only its chain-cache entries
//!   would differ, which a file that found an entry never bisects by key
//!   again.
//!
//! Verified against running journalctl by Task 5's matrix: forward pages
//! page by page on 22.04, 24.04 and 26.04 (Difference 18 aside, on
//! 22.04), and the landing on a 249-written file whose clock steps back
//! (docs/platform-notes/systemd-journal.md); and by the `own boot` row
//! (Task 37): one boot in two seqnum spaces with its clock set back,
//! where 249 and 259.9 show what sb shows and 255 doesn't (Difference
//! 21).
//!
//! The order between entries, for the first step's lowest landing and for
//! every head against the entry just taken, is `Cursor::order` with the
//! page's boot table: the release's (journal spec decision 11; 249's has no
//! boot step). journalctl's seek lands in each file by one key (seqnum,
//! monotonic time in the cursor's boot, or realtime), while its merge
//! orders boots by the table, so where two boots' clocks overlap 255's and
//! 259's journalctl pass entries the table puts after the cursor, and can
//! show the cursor's own entry (Difference 20). sb's order wins there
//! (maintainer, 2026-11-10): on a release with the table, a file whose
//! seqnum id isn't the cursor's is sought per boot
//! (`FileQuery::boot_starts`), and only its entries after the cursor are
//! walked, unless that gives 259's landing anyway (no `_BOOT_ID` values
//! read, or one boot, not the cursor's, that the table leaves equal to
//! it). On 22.04 a file is sought per boot only where 259's seek falls
//! back to realtime in a file of the cursor's boot (no matching entry of
//! that boot at or after the cursor's monotonic time): it can land before
//! the cursor there, and 255 then shows entries before it and its own,
//! and pages forward for ever (Difference 21; the hardening plan's
//! decision 33). The other files, files of the cursor's seqnum space, and
//! every other file on 22.04 are sought as 259 seeks them. The order can
//! still cycle (a boot not in the table between two it orders), and so
//! can paging forward: `forward_paging_cycles_where_the_order_does`.

use crate::boots::BootOrder;
use crate::file::{ChainCache, DataRef, EntryList, JournalFile, Key, Met, Probe};
use crate::rules::Group;
use crate::walk::{Candidate, Known, driver, location, reread};
use crate::{Cursor, ReadError, ReadErrorKind, ReadErrors};
use itertools::Itertools;
use std::collections::HashMap;
use std::rc::Rc;

type Result<T, E = ReadError> = std::result::Result<T, E>;

/// The length of a `_BOOT_ID=` payload: the name, `=` and 32 hex digits.
const BOOT_FIELD_LEN: usize = 9 + 32;

/// The boot id a `_BOOT_ID=` payload names: 32 hex digits, as journald
/// writes it (`sd_id128_to_string`); `None` for anything else.
fn boot_id_of(payload: &[u8]) -> Option<[u8; 16]> {
    let hex = payload.strip_prefix(b"_BOOT_ID=")?;
    let mut id = [0u8; 16];
    // <https://docs.rs/hex/0.4.3/hex/fn.decode_to_slice.html>: exactly 32
    // digits fill the 16 bytes.
    hex::decode_to_slice(hex, &mut id).ok()?;
    Some(id)
}

/// An oldest-first stream of one file's entry offsets, each with what the
/// stream knows about it.
type Stream<'a, T = Cursor> = Box<dyn Iterator<Item = Result<(u64, T)>> + 'a>;

/// A forward page's choice: its entries, oldest first, and whether
/// matching entries lie older and newer.
#[derive(Debug)]
pub(crate) struct Forward {
    pub(crate) picked: Vec<Candidate>,
    pub(crate) more_older: bool,
    pub(crate) more_newer: bool,
    /// Per file, the entry count the page hands over (walk.rs `reread`).
    pub(crate) counts: Vec<u64>,
}

/// One file's query: per group, per term, the DATA objects of the values
/// the file holds; `None` matches every entry.
struct FileQuery<'a> {
    f: &'a JournalFile,
    groups: Option<Vec<Vec<Vec<DataRef>>>>,
    /// The chain cache of the file's seeks (one journalctl run's).
    cache: ChainCache,
    /// Where the last seek started each of the file's boots
    /// (`boot_starts`); `None` for journalctl 259's seek. An entry of a
    /// boot with no start, or before its boot's start, isn't after the
    /// cursor.
    starts: Option<Rc<Starts>>,
    /// Whether journalctl 259's seek (`seek_259`) fell back to realtime
    /// for the main list or a value in a file that holds the cursor's
    /// boot: its monotonic bisect, with a query its stepping between the
    /// boot's and the value's lists, found no entry. 22.04 then seeks the
    /// file per boot (`seek_lists`).
    fell_back: bool,
    /// The matching entries the second header read added (walk.rs
    /// `reread`), ascending by offset (file order: the binary searches and
    /// `partition_point` below rely on it), and the count the page hands
    /// over.
    extras: Rc<Vec<(u64, Cursor)>>,
    count: u64,
    /// Of `extras`, the offsets at or after the last seek's cursor in the
    /// release's order (`Cursor::order`), ascending. The cursor's own entry
    /// is among them, as a landing can be: `after` steps past a landing
    /// that is the cursor's entry, so the page never shows it.
    extras_after: Rc<Vec<u64>>,
}

/// Per boot id, the first offset of the boot's entries after the cursor,
/// if any.
type Starts = HashMap<[u8; 16], Option<u64>>;

/// What seeking a file per boot gave (`FileQuery::seek_per_boot`).
enum PerBoot {
    /// The file was sought per boot: its landing, if any.
    Sought(Option<u64>),
    /// Not sought per boot: journalctl 259's seek stands.
    Not,
}

impl<'a> FileQuery<'a> {
    /// Look the query's values up in `f`. A group whose lookup fails is
    /// reported and left out, as the backward walk leaves it out; a group
    /// with a term none of whose values is here can't match, and is left
    /// out too.
    ///
    /// For a filtered query the header is then read again (walk.rs
    /// `reread`): the entries it adds that match are taken by the cursor's
    /// order where they lie before a landing (`seek`), and in file order
    /// after it, as the lists' own entries are.
    fn new(f: &'a JournalFile, groups: &[Group], errors: &mut ReadErrors) -> Self {
        let mut known = Known::default();
        let resolved = (!groups.is_empty()).then(|| {
            let mut out = Vec::new();
            'groups: for (g, group) in groups.iter().enumerate() {
                let mut terms = Vec::new();
                for term in group {
                    let mut refs = Vec::new();
                    for payload in term {
                        match f.find_data(payload) {
                            Ok(Some(d)) => refs.push(d),
                            Ok(None) => {}
                            Err(e) => {
                                errors.add(e);
                                known.failed.insert(g);
                                continue 'groups;
                            }
                        }
                    }
                    if refs.is_empty() {
                        continue 'groups;
                    }
                    terms.push(refs);
                }
                // The term `all_matches_from` walks, for the second read.
                if let Some(t) = driver(&terms) {
                    known.drivers.insert(g, t);
                }
                out.push(terms);
            }
            out
        });
        let again = reread(f, groups, &known, errors);
        Self {
            f,
            groups: resolved,
            cache: ChainCache::default(),
            starts: None,
            fell_back: false,
            extras: Rc::new(again.extras),
            count: again.n_entries,
            extras_after: Rc::new(Vec::new()),
        }
    }

    /// Where `c` falls in the file, in the release's order (decision 11,
    /// Difference 20). A file whose seqnum id isn't the cursor's is sought
    /// per boot (`boot_starts`) on a release with the boot table; on
    /// 22.04 only where journalctl 259's seek (`seek_259`) fell back to
    /// realtime in the cursor's boot (`fell_back`; Difference 21). The
    /// landing is then the first matching entry, from the lowest start, at
    /// or after its boot's start. 259's seek otherwise, and where per-boot
    /// seeking gives the same: a file with no `_BOOT_ID` values sb can
    /// read, or with one boot, one the table leaves equal to the cursor's.
    ///
    /// An entry the second header read added is taken by the cursor's
    /// order where it lies before the landing the lists gave, or where
    /// they gave none: a bisect over its value's whole list would have
    /// tested it there, between two neighbours, by the same key
    /// (`generic_array_bisect`). Not where the file's clock steps back
    /// among the entries written during the page: a departure (the module
    /// doc). A seek that met an error drops them, and the file hands over
    /// its first header read's count, as when the second read fails: they
    /// stand for list entries not linked yet, the lists' own landing was
    /// cut short, and the follow reads them from that count instead.
    fn seek(&mut self, c: &Cursor, boots: &BootOrder, errors: &mut ReadErrors) -> Option<u64> {
        let after: Vec<u64> = self
            .extras
            .iter()
            .filter(|(_, l)| !l.order(c, boots).is_lt())
            .map(|&(off, _)| off)
            .collect();
        self.extras_after = Rc::new(after);
        let mut met = ReadErrors::default();
        let landing = self.seek_lists(c, boots, &mut met);
        if !met.is_empty() {
            self.extras = Rc::new(Vec::new());
            self.extras_after = Rc::new(Vec::new());
            self.count = self.f.header.n_entries;
        }
        errors.absorb(met);
        match (landing, self.extras_after.first().copied()) {
            (Some(l), Some(e)) => Some(l.min(e)),
            (l, e) => l.or(e),
        }
    }

    /// Where `c` falls in the file by its lists (`seek`).
    fn seek_lists(
        &mut self,
        c: &Cursor,
        boots: &BootOrder,
        errors: &mut ReadErrors,
    ) -> Option<u64> {
        self.starts = None;
        if c.seqnum_id == self.f.header.seqnum_id {
            return self.seek_259(c, errors);
        }
        // 22.04: journalctl 259's seek, unless it fell back to realtime in
        // a file of the cursor's boot (the hardening plan's decision 33).
        let mut landing_259 = None;
        if !boots.is_active() {
            self.fell_back = false;
            let landing = self.seek_259(c, errors);
            if !self.fell_back {
                return landing;
            }
            landing_259 = Some(landing);
        }
        match self.seek_per_boot(c, boots, errors) {
            PerBoot::Sought(landing) => landing,
            PerBoot::Not => landing_259.unwrap_or_else(|| self.seek_259(c, errors)),
        }
    }

    /// The landing per boot (`boot_starts`), or `PerBoot::Not` where that
    /// gives journalctl 259's landing anyway or the file's boots are
    /// unknown.
    fn seek_per_boot(&mut self, c: &Cursor, boots: &BootOrder, errors: &mut ReadErrors) -> PerBoot {
        let mut met = Met::default();
        let starts = self.boot_starts(c, boots, &mut met);
        for e in met.errors {
            errors.add(e);
        }
        match starts {
            Ok(Some(starts)) => {
                let from = starts.values().flatten().min().copied();
                self.starts = Some(Rc::new(starts));
                let Some(from) = from else {
                    return PerBoot::Sought(None);
                };
                for r in self.matches_from(from) {
                    match r {
                        Ok((off, _)) => return PerBoot::Sought(Some(off)),
                        Err(e) => errors.add(e),
                    }
                }
                PerBoot::Sought(None)
            }
            Ok(None) => PerBoot::Not,
            // The file's boots unknown: journalctl's seek.
            Err(e) => {
                errors.add(e);
                PerBoot::Not
            }
        }
    }

    /// Each of the file's boots (its `_BOOT_ID` values, `field_values`)
    /// and where its entries after `c` start, by the release's order
    /// (`Cursor::order`; the file's seqnum id isn't the cursor's):
    /// - the cursor's boot by monotonic time: its list bisected as 259
    ///   bisects it (`Key::Monotonic`);
    /// - a boot the table orders after the cursor's at its first entry
    ///   (the DATA object's in-line entry: no read);
    /// - a boot it orders before nowhere;
    /// - a boot it leaves equal (not in the table, another machine) by
    ///   realtime: its list bisected (`Key::Realtime`).
    ///
    /// Within the file every entry has its seqnum id, so file order is
    /// the release's order there, and a boot's entries after `c` are those
    /// from its start on (the boot's own clocks in order, as journald
    /// writes them). A boot whose list is empty is left out: its entries,
    /// if any, are kept (`matches_from`), never dropped.
    ///
    /// `None` when per-boot seeking would give journalctl 259's landing
    /// anyway: no boot read, or one boot, not the cursor's, that the table
    /// leaves equal to it. A file of the cursor's boot is sought per boot
    /// even when it holds no other: where none of its matching entries of
    /// that boot reaches the cursor's monotonic time, 259 seeks it by
    /// realtime and can land before the cursor (the hardening plan's
    /// decision 33). The cost: the field lookup (`field_values`) and a
    /// bisect per boot started by time; then the walk from the lowest
    /// start reads the matching entries of the other boots it passes over
    /// before its landing and among the entries shown (as their boots lie
    /// in the file).
    fn boot_starts(
        &mut self,
        c: &Cursor,
        boots: &BootOrder,
        met: &mut Met,
    ) -> Result<Option<Starts>> {
        let mut starts = Starts::new();
        let mut ordered = false;
        for (d, payload) in self.f.field_values(b"_BOOT_ID", BOOT_FIELD_LEN)? {
            // A value that isn't a boot id names no entry's boot: its
            // entries, if any, are kept (`matches_from`).
            let Some(boot) = boot_id_of(&payload) else {
                continue;
            };
            if d.n_entries == 0 {
                continue;
            }
            let list = EntryList::of_data(&d);
            let start = if boot == c.boot_id {
                self.bisect(&list, Key::Monotonic, c.monotonic, met)?
            } else {
                match boots.compare(&boot, &c.boot_id) {
                    std::cmp::Ordering::Greater => {
                        ordered = true;
                        Some(d.entry_offset)
                    }
                    std::cmp::Ordering::Less => {
                        ordered = true;
                        None
                    }
                    std::cmp::Ordering::Equal => {
                        self.bisect(&list, Key::Realtime, c.realtime, met)?
                    }
                }
            };
            starts.insert(boot, start);
        }
        Ok((ordered || starts.len() > 1 || starts.contains_key(&c.boot_id)).then_some(starts))
    }

    fn bisect(
        &mut self,
        list: &EntryList,
        key: Key,
        needle: u64,
        met: &mut Met,
    ) -> Result<Option<u64>> {
        let probe = Probe { key, needle };
        Ok(self
            .f
            .bisect_list(list, probe, &mut self.cache, met)?
            .map(|found| found.offset))
    }

    /// Where `c` falls in the file by journalctl 259's seek: its landing
    /// entry's offset, or `None`.
    fn seek_259(&mut self, c: &Cursor, errors: &mut ReadErrors) -> Option<u64> {
        let mut met = Met::default();
        let landing = match self.groups.take() {
            None => self.seek_all(c, &mut met).unwrap_or_else(|e| {
                errors.add(e);
                None
            }),
            Some(groups) => {
                // The query's OR: the lowest landing of its groups.
                let mut np: Option<u64> = None;
                for group in groups.iter().rev() {
                    match self.seek_group(group, c, &mut met) {
                        Ok(Some(cp)) => np = Some(np.map_or(cp, |np| np.min(cp))),
                        Ok(None) => {}
                        Err(e) => errors.add(e),
                    }
                }
                self.groups = Some(groups);
                np
            }
        };
        for e in met.errors {
            errors.add(e);
        }
        landing
    }

    /// `find_location_with_matches` without matches: the main list.
    fn seek_all(&mut self, c: &Cursor, met: &mut Met) -> Result<Option<u64>> {
        let main = self.f.main_list();
        if c.seqnum_id == self.f.header.seqnum_id {
            return self.bisect(&main, Key::Seqnum, c.seqnum, met);
        }
        if let Some(boot) = self.boot_list(c)? {
            if let Some(p) = self.bisect(&boot, Key::Monotonic, c.monotonic, met)? {
                return Ok(Some(p));
            }
            self.fell_back = true;
        }
        self.bisect(&main, Key::Realtime, c.realtime, met)
    }

    /// The `_BOOT_ID=` DATA object's list for `c`'s boot
    /// (`find_data_object_by_boot_id`), if the file has one.
    fn boot_list(&self, c: &Cursor) -> Result<Option<EntryList>> {
        let payload = format!("_BOOT_ID={}", crate::file::hex_id(&c.boot_id));
        Ok(self
            .f
            .find_data(payload.as_bytes())?
            .map(|d| EntryList::of_data(&d)))
    }

    /// `find_location_for_match` for a group (`MATCH_AND_TERM`): the
    /// highest landing of its terms, then the next entry in every term.
    fn seek_group(
        &mut self,
        terms: &[Vec<DataRef>],
        c: &Cursor,
        met: &mut Met,
    ) -> Result<Option<u64>> {
        let mut np: Option<u64> = None;
        for values in terms.iter().rev() {
            // A term (`MATCH_OR_TERM`): the lowest landing of its values.
            let mut term: Option<u64> = None;
            for d in values.iter().rev() {
                if let Some(cp) = self.seek_value(d, c, met)? {
                    term = Some(term.map_or(cp, |t| t.min(cp)));
                }
            }
            let Some(cp) = term else { return Ok(None) };
            np = Some(np.map_or(cp, |np| np.max(cp)));
        }
        match np {
            Some(np) => self.next_in_group(terms, np, met),
            None => Ok(None),
        }
    }

    /// `find_location_for_match` for one value (`MATCH_DISCRETE`).
    fn seek_value(&mut self, d: &DataRef, c: &Cursor, met: &mut Met) -> Result<Option<u64>> {
        let list = EntryList::of_data(d);
        if c.seqnum_id == self.f.header.seqnum_id {
            return self.bisect(&list, Key::Seqnum, c.seqnum, met);
        }
        if let Some(p) = self.monotonic_for_data(&list, c, met)? {
            return Ok(Some(p));
        }
        self.bisect(&list, Key::Realtime, c.realtime, met)
    }

    /// `journal_file_move_to_entry_by_monotonic_for_data`: the boot's
    /// list bisected by monotonic time, then stepping between it and the
    /// value's list until an entry is in both.
    fn monotonic_for_data(
        &mut self,
        list: &EntryList,
        c: &Cursor,
        met: &mut Met,
    ) -> Result<Option<u64>> {
        let Some(boot) = self.boot_list(c)? else {
            return Ok(None);
        };
        let Some(mut z) = self.bisect(&boot, Key::Monotonic, c.monotonic, met)? else {
            // Not found in the cursor's boot: 259 falls back to realtime.
            self.fell_back = true;
            return Ok(None);
        };
        // Each step's offset is at least the last one's, and a step that
        // doesn't move ends it: at most one step per entry of the lists.
        loop {
            let Some(p) = self.bisect(list, Key::Offset, z, met)? else {
                self.fell_back = true;
                return Ok(None);
            };
            if p == z {
                return Ok(Some(z));
            }
            let Some(q) = self.bisect(&boot, Key::Offset, p, met)? else {
                self.fell_back = true;
                return Ok(None);
            };
            z = q;
            if p == z {
                return Ok(Some(z));
            }
        }
    }

    /// `next_for_match` for a group (`MATCH_AND_TERM`), going down: the
    /// first entry at or after `after` in every term's lists, each term
    /// moved in turn to the furthest offset any reached
    /// (`LIST_LOOP_BUT_ONE`, src/basic/list.h).
    fn next_in_group(
        &mut self,
        terms: &[Vec<DataRef>],
        after: u64,
        met: &mut Met,
    ) -> Result<Option<u64>> {
        let order: Vec<&Vec<DataRef>> = terms.iter().rev().collect();
        let Some(first) = order.first() else {
            return Ok(None);
        };
        let Some(mut np) = self.next_in_term(first, after, met)? else {
            return Ok(None);
        };
        let mut last_moved = 0;
        let mut i = 1 % order.len();
        while i != last_moved {
            let Some(cp) = self.next_in_term(order[i], np, met)? else {
                return Ok(None);
            };
            if cp > np {
                np = cp;
                last_moved = i;
            }
            i = (i + 1) % order.len();
        }
        Ok(Some(np))
    }

    /// `next_for_match` for a term (`MATCH_OR_TERM`): the lowest of its
    /// values' first entries at or after `after`.
    fn next_in_term(
        &mut self,
        values: &[DataRef],
        after: u64,
        met: &mut Met,
    ) -> Result<Option<u64>> {
        let mut np: Option<u64> = None;
        for d in values.iter().rev() {
            let list = EntryList::of_data(d);
            if let Some(cp) = self.bisect(&list, Key::Offset, after, met)? {
                np = Some(np.map_or(cp, |np| np.min(cp)));
            }
        }
        Ok(np)
    }

    /// The entry at `offset` as a merge candidate of file `file`.
    fn candidate(&self, file: usize, offset: u64, errors: &mut ReadErrors) -> Option<Candidate> {
        match self.f.entry(offset) {
            Ok(e) => Some(Candidate {
                location: location(self.f, &e),
                file,
                offset,
            }),
            Err(e) => {
                errors.add(e);
                None
            }
        }
    }

    /// The file's matching entries at or after `from`, in file order, that
    /// are after the last seek's cursor by its `starts`, if it set any:
    /// an entry of a boot without a start, or before its boot's start, is
    /// passed over. Those after the landing aren't older than the page
    /// either (a page back from its first record, in the same seqnum space,
    /// doesn't reach them), so `more_older` stays the landings' business
    /// (`older_than_starts`). An entry of a boot the file's `_BOOT_ID`
    /// values don't name is kept, never dropped.
    ///
    /// An entry the second header read added is kept by the cursor's
    /// order instead (`extras_after`): a boot's start came from its
    /// `_BOOT_ID` list, which may not list it yet.
    fn matches_from(&self, from: u64) -> Stream<'a> {
        let all = self.all_matches_from(from);
        let Some(starts) = &self.starts else {
            return all;
        };
        let (starts, extras) = (Rc::clone(starts), Rc::clone(&self.extras));
        let after = Rc::clone(&self.extras_after);
        Box::new(all.filter(move |r| {
            let Ok((off, c)) = r else { return true };
            if extras.binary_search_by_key(off, |&(o, _)| o).is_ok() {
                return after.binary_search(off).is_ok();
            }
            starts
                .get(&c.boot_id)
                .is_none_or(|s| s.is_some_and(|s| *off >= s))
        }))
    }

    /// The file's matching entries at or after `from`, in file order.
    fn all_matches_from(&self, from: u64) -> Stream<'a> {
        let f = self.f;
        // Where each list's entries at or after `from` start: an offset
        // bisect, with a scratch cache (this isn't journalctl's seek, which
        // steps through the lists with `next_for_match`). Offsets in a list
        // only grow, so it finds the start, unless a bad item cut it short
        // or sits first in the array it lands in (which systemd's `found:`
        // refuses): then the list is read from its head and the earlier
        // offsets passed over, so no entry is lost to a zero slot
        // (Difference 8). That read costs the list's items, a pread per 512,
        // on a damaged list only. The system's error is the walk's.
        let from_list = move |list: EntryList| -> Box<dyn Iterator<Item = Result<u64>> + 'a> {
            let probe = Probe {
                key: Key::Offset,
                needle: from,
            };
            let mut met = Met::default();
            match f.bisect_list(&list, probe, &mut ChainCache::default(), &mut met) {
                Ok(Some(found)) => Box::new(f.entries_from(&list, found.index)),
                Ok(None) if !met.cut => Box::new(std::iter::empty()),
                Err(e) if e.is_io() => Box::new(std::iter::once(Err(e))),
                Ok(None) | Err(_) => Box::new(
                    f.entries_from(&list, 0)
                        .filter(move |r| r.as_ref().map_or(true, |&off| off >= from)),
                ),
            }
        };
        let Some(groups) = &self.groups else {
            return Box::new(Entries {
                f,
                offsets: from_list(f.main_list()),
                others: Vec::new(),
                last: 0,
            });
        };
        let mut walks: Vec<Stream<'a>> = Vec::new();
        for terms in groups {
            // The rarest term drives, as the backward walk's does.
            let Some(driver) = driver(terms) else {
                continue;
            };
            let lists: Vec<Stream<'a, ()>> = terms[driver]
                .iter()
                .map(|d| -> Stream<'a, ()> {
                    Box::new(from_list(EntryList::of_data(d)).map(|r| r.map(|off| (off, ()))))
                })
                .collect();
            let others = terms
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != driver)
                .map(|(_, r)| r.iter().map(|d| d.offset).collect())
                .collect();
            let offsets = ascending(lists);
            walks.push(Box::new(Entries {
                f,
                offsets: Box::new(offsets.map(|r| r.map(|(off, ())| off))),
                others,
                last: 0,
            }));
        }
        // The second header read's matches, an offset the lists also give
        // taken once (`ascending`).
        let extras = Rc::clone(&self.extras);
        let start = extras.partition_point(|&(off, _)| off < from);
        walks.push(Box::new((start..extras.len()).map(move |i| Ok(extras[i]))));
        ascending(walks)
    }
}

/// Streams of one file merged into one, ascending by offset, each offset
/// once; an error first, so it is reported where it was met. The mirror of
/// walk.rs's `union`
/// (<https://docs.rs/itertools/0.14.0/itertools/trait.Itertools.html#method.kmerge_by>):
/// offsets are a total order, which `kmerge_by`'s heap needs. Why not
/// `dedup_by`: it compares neighbours only, and an error can come between
/// two copies of an offset.
fn ascending<'a, T: 'a>(streams: Vec<Stream<'a, T>>) -> Stream<'a, T> {
    fn key<T>(r: &Result<(u64, T)>) -> u64 {
        r.as_ref().map_or(0, |x| x.0)
    }
    let mut last = None;
    Box::new(
        streams
            .into_iter()
            .kmerge_by(|a, b| key(a) < key(b))
            .filter(move |r| match r {
                Ok((off, _)) => last.replace(*off) != Some(*off),
                Err(_) => true,
            }),
    )
}

/// A group's entries from its driver's offsets: each read once and
/// checked against the other terms' DATA offsets, as walk.rs's `Walk` does
/// going back. An offset at or before the last read is corruption the
/// lists' own checks report, and is passed over.
struct Entries<'a> {
    f: &'a JournalFile,
    offsets: Box<dyn Iterator<Item = Result<u64>> + 'a>,
    /// Per other term, its values' DATA offsets.
    others: Vec<Vec<u64>>,
    last: u64,
}

impl Iterator for Entries<'_> {
    type Item = Result<(u64, Cursor)>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let off = match self.offsets.next()? {
                Ok(off) => off,
                Err(e) => return Some(Err(e)),
            };
            if off <= self.last {
                continue;
            }
            self.last = off;
            let e = match self.f.entry(off) {
                Ok(e) => e,
                Err(err) => return Some(Err(err)),
            };
            let matches = self
                .others
                .iter()
                .all(|alts| alts.iter().any(|o| e.items.contains(o)));
            if matches {
                return Some(Ok((off, location(self.f, &e))));
            }
        }
    }
}

/// The head of a file's forward stream.
struct Head<'a> {
    head: Option<Candidate>,
    rest: Option<Stream<'a>>,
}

/// The lowest head, compared in file order against the running best, the
/// earlier file on a tie (`real_journal_next`: `found = k < 0`, the head
/// compared with the best so far). Spelled out, as walk.rs's merge is:
/// the order isn't transitive, and std's `min_by` documents its result for
/// a total order only (and compares the other way round); itertools'
/// `kmerge_by` needs a transitive order and has no tie rule.
fn lowest(heads: &[Head<'_>], boots: &BootOrder) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, h) in heads.iter().enumerate() {
        let Some(c) = &h.head else { continue };
        let better = best.is_none_or(|b| {
            heads[b]
                .head
                .as_ref()
                .is_some_and(|bc| c.location.order(&bc.location, boots).is_lt())
        });
        if better {
            best = Some(i);
        }
    }
    best
}

/// The second step's heads: the file the first step took from goes on
/// after it; another goes on from its landing; one the first step found
/// nothing in seeks again from the entry taken, its landing recorded in
/// `starts`.
fn second_step<'a>(
    queries: &mut [FileQuery<'a>],
    landings: &[Option<Candidate>],
    taken: &Candidate,
    starts: &mut [Option<u64>],
    boots: &BootOrder,
    errors: &mut ReadErrors,
) -> Vec<Head<'a>> {
    let mut heads = Vec::new();
    for (i, q) in queries.iter_mut().enumerate() {
        if i == taken.file {
            heads.push(Head {
                head: None,
                rest: Some(q.matches_from(taken.offset + 1)),
            });
            continue;
        }
        let landing = match &landings[i] {
            Some(l) => Some(l.clone()),
            None => q
                .seek(&taken.location, boots, errors)
                .and_then(|l| q.candidate(i, l, errors)),
        };
        if starts[i].is_none() {
            starts[i] = landing.as_ref().map(|l| l.offset);
        }
        let rest = landing.as_ref().map(|l| q.matches_from(l.offset + 1));
        heads.push(Head {
            head: landing,
            rest,
        });
    }
    heads
}

/// Fill each file's head with its next entry strictly after `last`
/// (`next_beyond_location`'s `found = k > 0`).
fn advance(heads: &mut [Head<'_>], last: &Cursor, boots: &BootOrder, errors: &mut ReadErrors) {
    for (i, h) in heads.iter_mut().enumerate() {
        loop {
            if h.head.is_none() {
                match h.rest.as_mut().and_then(Iterator::next) {
                    Some(Ok((offset, location))) => {
                        h.head = Some(Candidate {
                            location,
                            file: i,
                            offset,
                        });
                    }
                    Some(Err(e)) => {
                        errors.add(e);
                        continue;
                    }
                    None => {
                        h.rest = None;
                        break;
                    }
                }
            }
            match &h.head {
                Some(c) if !c.location.order(last, boots).is_gt() => h.head = None,
                _ => break,
            }
        }
    }
}

/// Whether some file has a matching entry before where its forward stream
/// starts (`starts`, its landing), or any when it has none: entries a page
/// back from the page can show. A cheap probe: each file's first readable
/// match.
fn older_than_starts(
    queries: &[FileQuery<'_>],
    starts: &[Option<u64>],
    errors: &mut ReadErrors,
) -> bool {
    let mut older = false;
    for (q, start) in queries.iter().zip(starts) {
        // The first readable match, after the cursor or not; an error
        // before it is reported.
        for r in q.all_matches_from(0) {
            match r {
                Ok((off, _)) => {
                    older |= start.is_none_or(|s| off < s);
                    break;
                }
                Err(e) => errors.add(e),
            }
        }
    }
    older
}

/// The forward page after `c`: at most `n` matching entries (or `held`,
/// when that is less), oldest first, ordered with `boots`.
pub(crate) fn after(
    files: &[JournalFile],
    groups: &[Group],
    n: usize,
    c: &Cursor,
    held: usize,
    boots: &BootOrder,
    errors: &mut ReadErrors,
) -> Forward {
    let mut queries: Vec<FileQuery<'_>> = files
        .iter()
        .map(|f| FileQuery::new(f, groups, errors))
        .collect();
    // The first step: each file's landing, not compared with the cursor.
    let mut landings: Vec<Option<Candidate>> = Vec::new();
    for (i, q) in queries.iter_mut().enumerate() {
        let l = q.seek(c, boots, errors);
        landings.push(l.and_then(|l| q.candidate(i, l, errors)));
    }
    let starts: Vec<Option<u64>> = landings
        .iter()
        .map(|l| l.as_ref().map(|c| c.offset))
        .collect();
    let heads_first: Vec<Head<'_>> = landings
        .iter()
        .map(|l| Head {
            head: l.clone(),
            rest: None,
        })
        .collect();
    let Some(first) = lowest(&heads_first, boots) else {
        let older = older_than_starts(&queries, &starts, errors);
        return Forward {
            picked: Vec::new(),
            more_older: older,
            more_newer: false,
            counts: queries.iter().map(|q| q.count).collect(),
        };
    };
    let taken = landings[first].clone().expect("`lowest` names a head");
    let keep = n.min(held);
    let mut out = Vec::new();
    let mut more = false;
    // journalctl 255.1 and later step past the landing only when it is the
    // cursor's own entry (`sd_journal_test_cursor`: every field the cursor
    // names).
    let skipped = taken.location == *c;
    let mut last = taken.location;
    if !skipped {
        if keep > 0 {
            out.push(taken.clone());
        } else {
            more = true;
        }
    }
    let mut starts = starts;
    let mut heads = second_step(&mut queries, &landings, &taken, &mut starts, boots, errors);
    while !more {
        advance(&mut heads, &last, boots, errors);
        let Some(best) = lowest(&heads, boots) else {
            break;
        };
        if out.len() >= keep {
            more = true;
            break;
        }
        let c = heads[best].head.take().expect("`lowest` names a head");
        last = c.location;
        out.push(c);
    }
    if more && keep < n {
        errors.add(ReadError::of_page(ReadErrorKind::HeldLimitNewer));
    }
    let more_older = skipped || older_than_starts(&queries, &starts, errors);
    Forward {
        picked: out,
        more_older,
        more_newer: more,
        counts: queries.iter().map(|q| q.count).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted::*;

    /// The system's error reading the list a forward walk resumes in (here
    /// the file cut short after it was opened, inside the second array) is
    /// the walk's: the list yields that error and nothing from its head.
    /// Corruption instead makes it read the list from its head (Difference
    /// 8). The first array is out of order, so a read from the head would
    /// yield its first entry, which lies at or after `from`.
    #[test]
    fn the_systems_error_in_the_resumed_list_is_the_walks() {
        each_format(the_systems_error_in_the_resumed_list_is_the_walks_in);
    }

    fn the_systems_error_in_the_resumed_list_is_the_walks_in(format: Format) {
        let mut crafted = Crafted::new_in(format);
        // One item each: an entry lists at least one DATA object.
        let entries: Vec<u64> = (1..=4).map(|s| crafted.entry(s, &[8])).collect();
        // An array of two items: its 24-byte header and the items (16
        // bytes, or 8 in a compact file).
        let second = crafted.b.len() as u64 + 24 + 2 * format.array_item_width();
        let first = crafted.array(&[entries[2], entries[0]], 2, second);
        assert_eq!(crafted.array(&[entries[1], entries[3]], 2, 0), second);
        crafted.set(H_N_ENTRIES, 4);
        crafted.set(H_ENTRY_ARRAY_OFFSET, first);
        let (tmp, file) = crafted.write();
        std::fs::OpenOptions::new()
            .write(true)
            .open(tmp.path())
            .expect("open")
            .set_len(second + 8)
            .expect("truncate");
        let q = FileQuery::new(&file, &[], &mut ReadErrors::default());
        let got: Vec<_> = q.all_matches_from(entries[1]).collect();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].as_ref().is_err_and(ReadError::is_io), "{got:?}");
    }
}
