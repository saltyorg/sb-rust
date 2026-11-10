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
//!   when its entry has vanished or doesn't match. 22.04's and 24.04's
//!   journalctl always step past it: Difference 18;
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
//! UNVERIFIED against a running journalctl: Task 5's matrix checks forward
//! pages on 26.04, on 22.04 and 24.04 (Difference 18 aside), and on a
//! 249-written file whose clock steps back.
//!
//! The order between entries is sb's (`Cursor::order`, systemd 249's); the
//! boot table 255 and later put between boots is the plan's next step.

use crate::file::{ChainCache, DataRef, EntryList, JournalFile, Key, Met, Probe};
use crate::rules::Group;
use crate::walk::{Candidate, location};
use crate::{Cursor, ReadError, ReadErrorKind, ReadErrors};
use itertools::Itertools;

type Result<T, E = ReadError> = std::result::Result<T, E>;

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
}

/// One file's query: per group, per term, the DATA objects of the values
/// the file holds; `None` matches every entry.
struct FileQuery<'a> {
    f: &'a JournalFile,
    groups: Option<Vec<Vec<Vec<DataRef>>>>,
    /// The chain cache of the file's seeks (one journalctl run's).
    cache: ChainCache,
}

impl<'a> FileQuery<'a> {
    /// Look the query's values up in `f`. A group whose lookup fails is
    /// reported and left out, as the backward walk leaves it out; a group
    /// with a term none of whose values is here can't match, and is left
    /// out too.
    fn new(f: &'a JournalFile, groups: &[Group], errors: &mut ReadErrors) -> Self {
        let resolved = (!groups.is_empty()).then(|| {
            let mut out = Vec::new();
            'groups: for group in groups {
                let mut terms = Vec::new();
                for term in group {
                    let mut refs = Vec::new();
                    for payload in term {
                        match f.find_data(payload) {
                            Ok(Some(d)) => refs.push(d),
                            Ok(None) => {}
                            Err(e) => {
                                errors.add(e);
                                continue 'groups;
                            }
                        }
                    }
                    if refs.is_empty() {
                        continue 'groups;
                    }
                    terms.push(refs);
                }
                out.push(terms);
            }
            out
        });
        Self {
            f,
            groups: resolved,
            cache: ChainCache::default(),
        }
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

    /// Where `c` falls in the file: its landing entry's offset, or `None`.
    fn seek(&mut self, c: &Cursor, errors: &mut ReadErrors) -> Option<u64> {
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
        if let Some(boot) = self.boot_list(c)?
            && let Some(p) = self.bisect(&boot, Key::Monotonic, c.monotonic, met)?
        {
            return Ok(Some(p));
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
            return Ok(None);
        };
        // Each step's offset is at least the last one's, and a step that
        // doesn't move ends it: at most one step per entry of the lists.
        loop {
            let Some(p) = self.bisect(list, Key::Offset, z, met)? else {
                return Ok(None);
            };
            if p == z {
                return Ok(Some(z));
            }
            let Some(q) = self.bisect(&boot, Key::Offset, p, met)? else {
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

    /// The file's matching entries at or after `from`, in file order.
    fn matches_from(&self, from: u64) -> Stream<'a> {
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
                Err(e) if matches!(e.kind(), ReadErrorKind::Io { .. }) => {
                    Box::new(std::iter::once(Err(e)))
                }
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
            let Some(driver) = terms
                .iter()
                .enumerate()
                .min_by_key(|(_, r)| r.iter().fold(0u64, |a, d| a.saturating_add(d.n_entries)))
                .map(|(i, _)| i)
            else {
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
fn lowest(heads: &[Head<'_>]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, h) in heads.iter().enumerate() {
        let Some(c) = &h.head else { continue };
        let better = best.is_none_or(|b| {
            heads[b]
                .head
                .as_ref()
                .is_some_and(|bc| c.location.order(&bc.location).is_lt())
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
                .seek(&taken.location, errors)
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
fn advance(heads: &mut [Head<'_>], last: &Cursor, errors: &mut ReadErrors) {
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
                Some(c) if !c.location.order(last).is_gt() => h.head = None,
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
        // The first readable match; an error before it is reported.
        for r in q.matches_from(0) {
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
/// when that is less), oldest first.
pub(crate) fn after(
    files: &[JournalFile],
    groups: &[Group],
    n: usize,
    c: &Cursor,
    held: usize,
    errors: &mut ReadErrors,
) -> Forward {
    let mut queries: Vec<FileQuery<'_>> = files
        .iter()
        .map(|f| FileQuery::new(f, groups, errors))
        .collect();
    // The first step: each file's landing, not compared with the cursor.
    let mut landings: Vec<Option<Candidate>> = Vec::new();
    for (i, q) in queries.iter_mut().enumerate() {
        let l = q.seek(c, errors);
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
    let Some(first) = lowest(&heads_first) else {
        let older = older_than_starts(&queries, &starts, errors);
        return Forward {
            picked: Vec::new(),
            more_older: older,
            more_newer: false,
        };
    };
    let taken = landings[first].clone().expect("`lowest` names a head");
    let keep = n.min(held);
    let mut out = Vec::new();
    let mut more = false;
    // journalctl 259 steps past the landing only when it is the cursor's
    // own entry (`sd_journal_test_cursor`: every field the cursor names).
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
    let mut heads = second_step(&mut queries, &landings, &taken, &mut starts, errors);
    while !more {
        advance(&mut heads, &last, errors);
        let Some(best) = lowest(&heads) else { break };
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
    }
}
