//! A page's walk across a journal's files: the files found and opened,
//! each file's matches walked newest first, the newest N across them
//! (`newest`, as journalctl's `-n` finds them), then only those read for
//! output (`decode`) within the output limit.
//!
//! A page opens its files itself and drops them when it returns, so a
//! file cut short between pages is read afresh by the next. An `Io` error
//! from a cut made while a page reads is that page's error, reported; the
//! next page reads the new header (the follow, which keeps files open,
//! decides its own handling).

use crate::file::{DataRef, EntriesRev, Entry, JournalFile};
use crate::rules::Group;
use crate::{Cursor, Field, ReadError, ReadErrorKind, ReadErrors, Record};
use itertools::Itertools;
use std::collections::{HashMap, HashSet, hash_map};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

type Result<T, E = ReadError> = std::result::Result<T, E>;

/// Where `e`, an entry of `f`, is.
fn location(f: &JournalFile, e: &Entry) -> Cursor {
    Cursor {
        seqnum_id: f.header.seqnum_id,
        seqnum: e.seqnum,
        boot_id: e.boot_id,
        monotonic: e.monotonic,
        realtime: e.realtime,
        xor_hash: e.xor_hash,
    }
}

/// The journal files a listing found, and what it couldn't read.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) errors: Vec<ReadError>,
}

/// The `*.journal` and `*.journal~` files directly in `dir`, sorted. A
/// directory that doesn't exist adds nothing when `missing_ok` (a local
/// journal root may not exist); any other failure is reported, never a
/// file dropped silently.
pub(crate) fn journal_files_in(dir: &Path, missing_ok: bool, out: &mut Listing) {
    let mut found = Vec::new();
    scan(dir, missing_ok, out, &mut found, &mut Vec::new());
    out.paths.extend(found);
}

/// `*.journal` and `*.journal~` files in `dir` and its machine-id
/// subdirectories (names of 32 bytes), as `journalctl -D` reads them: the
/// directory's own first, then each subdirectory's, by name.
pub(crate) fn directory_files(dir: &Path, out: &mut Listing) {
    let (mut found, mut subdirs) = (Vec::new(), Vec::new());
    scan(dir, false, out, &mut found, &mut subdirs);
    out.paths.extend(found);
    subdirs.sort();
    for sub in subdirs {
        journal_files_in(&sub, true, out);
    }
}

/// One `read_dir` of `dir`: its journal files into `files`, sorted, and
/// its machine-id subdirectories into `subdirs`. Why not walkdir (in the
/// tree): its depth-first, name-ordered walk puts a subdirectory's files
/// among the directory's own, where `journalctl -D` reads the directory's
/// own first, and it has no rule for a local root that doesn't exist.
/// Files and directories are told apart through symlinks, as sd-journal
/// follows both (journal spec, Difference 4 is the plan's Task 4). A
/// journal file's name that can't be stat'ed (a dangling symlink, a file
/// gone since the listing) is listed anyway, for its open to report.
/// A 32-byte name that can't be stat'ed isn't taken as a subdirectory:
/// it is only a candidate, never a journal file.
fn scan(
    dir: &Path,
    missing_ok: bool,
    out: &mut Listing,
    files: &mut Vec<PathBuf>,
    subdirs: &mut Vec<PathBuf>,
) {
    let failed = |source: std::io::Error| ReadError {
        path: Some(dir.to_path_buf()),
        kind: ReadErrorKind::Io {
            context: "list the directory".to_owned(),
            source,
        },
    };
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if missing_ok && e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            out.errors.push(failed(e));
            return;
        }
    };
    for entry in rd {
        let p = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                out.errors.push(failed(e));
                continue;
            }
        };
        // Names as bytes, as sd-journal compares them: a name that isn't
        // UTF-8 is still a journal file's. One stat, following symlinks.
        let name = p
            .file_name()
            .map(OsStr::as_encoded_bytes)
            .unwrap_or_default();
        let journal = name.ends_with(b".journal") || name.ends_with(b".journal~");
        if !journal && name.len() != 32 {
            continue;
        }
        let meta = std::fs::metadata(&p);
        if journal {
            if meta.as_ref().map_or(true, std::fs::Metadata::is_file) {
                files.push(p);
            }
        } else if meta.is_ok_and(|m| m.is_dir()) {
            subdirs.push(p);
        }
    }
    files.sort();
}

/// Open each file. A file that can't be opened is returned as its error,
/// never dropped silently (an unsupported format names the file).
fn open(paths: &[PathBuf]) -> (Vec<JournalFile>, Vec<ReadError>) {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        match JournalFile::open(p) {
            Ok(f) => files.push(f),
            Err(e) => errors.push(e),
        }
    }
    (files, errors)
}

/// Open the files `list` names, then list again. A rotation between
/// listing and opening renames the active file away and a new file takes
/// its name, so the archived file's entries would be missing without an
/// error; when the list changed, open again, up to three times. (journalctl
/// lists once and has the same race: Difference 11.) A rotation after the
/// second listing is harmless: an open file is read through its descriptor
/// under its new name. The errors listing and opening are one per file at
/// most, so they are shown outside the read errors' cap.
pub(crate) fn open_listed(
    mut list: impl FnMut() -> Listing,
    errors: &mut ReadErrors,
) -> Vec<JournalFile> {
    let mut listing = list();
    for _ in 0..3 {
        let (files, failed) = open(&listing.paths);
        let again = list();
        if again.paths == listing.paths {
            again
                .errors
                .into_iter()
                .chain(failed)
                .for_each(|e| errors.add_open(e));
            return files;
        }
        listing = again;
    }
    let (files, failed) = open(&listing.paths);
    listing
        .errors
        .into_iter()
        .chain(failed)
        .for_each(|e| errors.add_open(e));
    errors.add(ReadError::of_page(ReadErrorKind::ListChanging));
    files
}

/// A page's bounds, in bytes: the payload it reads and decodes, and the
/// candidates it holds before decoding.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    pub(crate) payload: usize,
    pub(crate) held: usize,
}

/// The last `n` matching entries before `before` (the newest when
/// `None`), oldest first. `groups` empty means every entry.
pub(crate) fn tail(
    files: &[JournalFile],
    groups: &[Group],
    n: usize,
    before: Option<&Cursor>,
    bounds: Bounds,
    errors: &mut ReadErrors,
) -> Vec<Record> {
    let step = |l: &Cursor| match before {
        Some(b) if !l.order(b).is_lt() => Step::Skip,
        _ => Step::Take,
    };
    let walks = walks(files, groups, step, errors);
    let held = bounds.held / std::mem::size_of::<Candidate>();
    let picked = newest(walks, n, held, errors);
    decode(files, &picked, bounds.payload, errors)
}

/// One lazy walk per file, newest first. A group whose terms can't be
/// looked up is reported in `errors` and left out, the other groups still
/// match, as sd-journal leaves out what it can't iterate and keeps the
/// rest: `.journal~` files are exactly the ones left dirty.
fn walks<'a, S>(
    files: &'a [JournalFile],
    groups: &'a [Group],
    step: S,
    errors: &mut ReadErrors,
) -> Vec<impl Iterator<Item = Result<Candidate>> + 'a>
where
    S: Fn(&Cursor) -> Step + Copy + 'a,
{
    let mut out = Vec::new();
    for (file, f) in files.iter().enumerate() {
        let walk = file_walk(f, groups, step, errors);
        out.push(walk.map(move |r| {
            r.map(|(offset, location)| Candidate {
                location,
                file,
                offset,
            })
        }));
    }
    out
}

/// Read the chosen entries for output, newest first, within one output
/// limit for the page (`payload_limit`), which every read spends, failed
/// ones too. Entries share most DATA objects (a unit's _PID, _COMM,
/// _HOSTNAME and so on): each is read and decompressed once, and one that
/// failed isn't decoded again for the next entry listing it. An entry that
/// can't be read is reported and skipped, as sd-journal skips an entry it
/// can't read (`output_json` on -EBADMSG). The first entry that doesn't fit
/// ends the output, so it has no gap: a page back from the first entry
/// shown goes on from there. The entries left out are reported. An entry
/// that doesn't fit the whole limit, read before anything else, is
/// reported and skipped instead (Difference 5): stopping there would show
/// nothing at all.
fn decode(
    files: &[JournalFile],
    picked: &[Candidate],
    payload_limit: usize,
    errors: &mut ReadErrors,
) -> Vec<Record> {
    let mut out = Vec::with_capacity(picked.len());
    let mut payloads = Payloads {
        whole: payload_limit,
        ..Payloads::default()
    };
    let mut left = payload_limit;
    for (i, c) in picked.iter().rev().enumerate() {
        let f = &files[c.file];
        let whole = left == payload_limit;
        let read = f
            .entry(c.offset)
            .and_then(|e| record(f, &e, &mut payloads, c.file, &mut left));
        match read {
            Ok(r) => out.push(r),
            Err(err) if is_over(&err) && whole => {
                errors.add(ReadError {
                    kind: ReadErrorKind::TooLarge {
                        context: err.kind.to_string(),
                        limit: payload_limit,
                    },
                    path: err.path,
                });
                // Nothing was read before it, so all the cache holds is
                // its own fields: drop them with it.
                payloads.fields.clear();
                left = payload_limit;
            }
            Err(err) if is_over(&err) => {
                errors.add(ReadError::of_page(ReadErrorKind::OutputLimit {
                    limit: payload_limit,
                    left_out: picked.len() - i,
                }));
                break;
            }
            Err(err) => errors.add(err),
        }
    }
    out.reverse();
    out
}

fn is_over(e: &ReadError) -> bool {
    matches!(e.kind(), ReadErrorKind::OverLimit { .. })
}

/// An entry a file's walk chose: its location for the merge, and where to
/// read it once it is among the entries shown.
#[derive(Debug, Clone)]
struct Candidate {
    location: Cursor,
    file: usize,
    offset: u64,
}

/// The newest `n` entries across the files' walks (each newest first),
/// returned oldest first. This is how `journalctl -n` finds them:
/// `sd_journal_seek_tail`, then `sd_journal_previous_skip`, which is
/// `real_journal_next` going up (v249 sd-journal.c). Every file first moves
/// past heads that aren't strictly before the last entry taken, then the
/// largest head by `journal_file_compare_locations` is taken, the earlier
/// file on a tie. So a flushed copy is shown once, from the earlier file in
/// search order, and only the heads are read: a file's walk goes no
/// further back than the entries it contributes. journalctl then walks
/// forward again from the oldest entry taken; that second walk isn't
/// modelled (Difference 15).
///
/// An error a walk yields is reported and the walk goes on: the walks
/// pass over what they can't read (an entry, an array) and yield the
/// error in its place, so only what it hides is lost.
///
/// At most `held` candidates are kept (the page's held limit): past it
/// the older entries are left out, reported.
fn newest<I>(walks: Vec<I>, n: usize, held: usize, errors: &mut ReadErrors) -> Vec<Candidate>
where
    I: Iterator<Item = Result<Candidate>>,
{
    let mut walks: Vec<Option<I>> = walks.into_iter().map(Some).collect();
    let mut heads: Vec<Option<Candidate>> = walks.iter().map(|_| None).collect();
    let mut out: Vec<Candidate> = Vec::new();
    while out.len() < n {
        if out.len() >= held {
            errors.add(ReadError::of_page(ReadErrorKind::HeldLimit));
            break;
        }
        for (walk, head) in walks.iter_mut().zip(heads.iter_mut()) {
            loop {
                if head.is_none() {
                    match walk.as_mut().and_then(Iterator::next) {
                        Some(Ok(c)) => *head = Some(c),
                        Some(Err(err)) => {
                            errors.add(err);
                            continue;
                        }
                        None => {
                            *walk = None;
                            break;
                        }
                    }
                }
                match (head.as_ref(), out.last()) {
                    (Some(h), Some(last)) if !h.location.order(&last.location).is_lt() => {
                        *head = None;
                    }
                    _ => break,
                }
            }
        }
        // The largest head, compared in file order against the running
        // best, which keeps the earlier file on a tie: real_journal_next's
        // loop, spelled out because the order isn't transitive (std's
        // `min_by`/`max_by` document results for a total order only). Why
        // not itertools' `kmerge_by`: its heap needs a transitive order and
        // has no tie rule.
        let mut best: Option<usize> = None;
        for (i, head) in heads.iter().enumerate() {
            let Some(h) = head else { continue };
            let better = best.is_none_or(|b| {
                heads[b]
                    .as_ref()
                    .is_some_and(|bh| h.location.order(&bh.location).is_gt())
            });
            if better {
                best = Some(i);
            }
        }
        let Some(best) = best else { break };
        out.extend(heads[best].take());
    }
    out.reverse();
    out
}

/// What to do with an entry met while walking backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Take,
    /// Newer than the range wanted: keep walking.
    Skip,
}

/// A newest-first stream of entry offsets from one file, each with what
/// the stream knows about it.
type Stream<'a, T> = Box<dyn Iterator<Item = Result<(u64, T)>> + 'a>;

/// Several newest-first streams of one file merged into one, newest
/// first; an offset in several streams comes out once, errors between its
/// copies included. Offsets are file order, so the largest is the newest:
/// a total order, which `kmerge_by`'s heap needs. An error sorts first, so
/// it is yielded as soon as a stream meets one; the callers report it and
/// go on. Why not `dedup_by`: it compares neighbours only, and an error
/// can come between two copies of an offset.
/// <https://docs.rs/itertools/0.14.0/itertools/trait.Itertools.html#method.kmerge_by>
fn union<'a, T: 'a>(streams: Vec<Stream<'a, T>>) -> Stream<'a, T> {
    fn key<T>(r: &Result<(u64, T)>) -> u64 {
        r.as_ref().map_or(u64::MAX, |x| x.0)
    }
    let mut last = None;
    Box::new(
        streams
            .into_iter()
            .kmerge_by(|a, b| key(a) > key(b))
            .filter(move |r| match r {
                Ok((off, _)) => last.replace(*off) != Some(*off),
                Err(_) => true,
            }),
    )
}

/// One group's matches in one file, newest first: its driver term's entry
/// list walked backwards, each entry read once and checked against the
/// other terms' DATA offsets ("Reading": multiple matches).
struct Walk<'a, S> {
    f: &'a JournalFile,
    driver: Stream<'a, ()>,
    /// Per other term, the DATA offsets of its values; the entry must
    /// carry one of each.
    others: Vec<Vec<u64>>,
    step: S,
    /// The last entry read: an older one must end before it.
    end: u64,
}

impl<S: Fn(&Cursor) -> Step> Iterator for Walk<'_, S> {
    type Item = Result<(u64, Cursor)>;

    /// The next older match. An entry that can't be read is yielded as
    /// its error and the walk goes on, as sd-journal's `generic_array_get`
    /// skips a bad entry ("Entry item is bad, skipping over it"). Skipping
    /// is sound for `step`: the next readable entry is checked on its own.
    /// So is an array left out of order (`EntriesRev`): the walk goes on
    /// past it to the older arrays, where a follow stops the file
    /// (Difference 17).
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (off, ()) = match self.driver.next()? {
                Ok(x) => x,
                Err(err) => return Some(Err(err)),
            };
            // Entries never overlap (journald appends): checked before the
            // body is read, so a corrupt size can't make each entry read
            // the rest of the file.
            let e = match self.f.entry_before(off, self.end) {
                Ok(e) => e,
                Err(err) => return Some(Err(err)),
            };
            self.end = off;
            let location = location(self.f, &e);
            match (self.step)(&location) {
                Step::Skip => {}
                Step::Take => {
                    let matches = self
                        .others
                        .iter()
                        .all(|alts| alts.iter().any(|o| e.items.contains(o)));
                    if matches {
                        return Some(Ok((off, location)));
                    }
                }
            }
        }
    }
}

/// One file's matches for `groups` and `step`, newest first, read lazily:
/// one walk per group, driven from the term with the fewest entries, the
/// groups' matches merged. A term that ORs several values of one field
/// drives through the union of their entry lists. Every term is looked up
/// in the file's hash table first; a term with no DATA object here means
/// its group can't match in this file, and one that can't be looked up
/// leaves its group out, reported in `errors`: the other groups still
/// match. Since the walk drives from the DATA objects' own entry lists, an
/// entry journald was cut off from linking there isn't found through them
/// (Difference 12: the tail shows one copy of an entry written again).
fn file_walk<'a, S>(
    f: &'a JournalFile,
    groups: &[Group],
    step: S,
    errors: &mut ReadErrors,
) -> Stream<'a, Cursor>
where
    S: Fn(&Cursor) -> Step + Copy + 'a,
{
    fn offsets(it: EntriesRev<'_>) -> Stream<'_, ()> {
        Box::new(it.map(|r| r.map(|off| (off, ()))))
    }
    let walk = |driver, others| -> Stream<'a, Cursor> {
        Box::new(Walk {
            f,
            driver,
            others,
            step,
            end: u64::MAX,
        })
    };
    let mut walks: Vec<Stream<'a, Cursor>> = Vec::new();
    if groups.is_empty() {
        walks.push(walk(offsets(f.all_entries_rev()), Vec::new()));
    }
    'groups: for group in groups {
        let mut resolved: Vec<Vec<DataRef>> = Vec::new();
        for term in group {
            let mut refs = Vec::new();
            for payload in term {
                match f.find_data(payload) {
                    Ok(Some(d)) => refs.push(d),
                    Ok(None) => {}
                    Err(err) => {
                        errors.add(err);
                        continue 'groups;
                    }
                }
            }
            resolved.push(refs);
        }
        if resolved.iter().any(Vec::is_empty) {
            continue;
        }
        let Some(driver) = resolved
            .iter()
            .enumerate()
            // The counts come from the file: saturate, don't overflow.
            .min_by_key(|(_, r)| r.iter().fold(0u64, |a, d| a.saturating_add(d.n_entries)))
            .map(|(i, _)| i)
        else {
            continue;
        };
        let lists = resolved[driver]
            .iter()
            .map(|d| offsets(f.entries_of_rev(d)))
            .collect();
        let others = resolved
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != driver)
            .map(|(_, r)| r.iter().map(|d| d.offset).collect())
            .collect();
        walks.push(walk(union(lists), others));
    }
    union(walks)
}

/// The DATA payloads one page has read, keyed by (file index, offset): a
/// field, `None` for a payload without `=` (skipped), the objects that
/// failed, so another entry listing one isn't decoded again, and those
/// that went over the whole limit (`whole`) on their own, which can never
/// fit: an entry listing one is refused without decoding it again, so a
/// page skipping several such entries decodes the object once.
#[derive(Default)]
struct Payloads {
    fields: HashMap<(usize, u64), Option<Field>>,
    failed: HashSet<(usize, u64)>,
    too_large: HashSet<(usize, u64)>,
    whole: usize,
}

/// `kind` met reading entry `seqnum`'s fields.
fn in_fields(seqnum: u64, err: ReadError) -> ReadError {
    ReadError {
        kind: crate::file::in_context(&format!("entry {seqnum}'s fields"), err.kind),
        path: err.path,
    }
}

/// An entry's fields, each DATA object read once through `payloads`
/// within the page's `left` bytes, which every read spends (`OverLimit`
/// when they run out). Each field held spends its own size too, so an
/// entry listing one cached object millions of times is bounded as well.
fn record(
    f: &JournalFile,
    e: &Entry,
    payloads: &mut Payloads,
    file: usize,
    left: &mut usize,
) -> Result<Record> {
    let mut fields = Vec::with_capacity(e.items.len());
    for &item in &e.items {
        if payloads.failed.contains(&(file, item)) {
            return Err(f.error(crate::KnownBad { item }.into()));
        }
        if payloads.too_large.contains(&(file, item)) {
            let over = ReadErrorKind::OverLimit {
                context: format!("data {item:#x} (read before)"),
                over: crate::OverLimit { limit: *left },
            };
            return Err(in_fields(e.seqnum, f.error(over)));
        }
        let allowed = *left;
        let field = match payloads.fields.entry((file, item)) {
            hash_map::Entry::Occupied(o) => o.into_mut(),
            hash_map::Entry::Vacant(v) => match f.data_payload_spending(item, left) {
                Ok(p) => v.insert(Field::new(p)),
                Err(err) => {
                    if !is_over(&err) {
                        payloads.failed.insert((file, item));
                    } else if allowed == payloads.whole {
                        payloads.too_large.insert((file, item));
                    }
                    return Err(in_fields(e.seqnum, err));
                }
            },
        };
        let Some(rest) = left.checked_sub(std::mem::size_of::<Field>()) else {
            let allowed = *left;
            *left = 0;
            let over = ReadErrorKind::OverLimit {
                context: String::new(),
                over: crate::OverLimit { limit: allowed },
            };
            return Err(in_fields(e.seqnum, f.error(over)));
        };
        *left = rest;
        fields.extend(field.clone());
    }
    Ok(Record {
        cursor: location(f, e),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::crafted::{self, *};
    use crate::{KnownBad, Position};

    /// A page's files and errors, with its bounds set apart: the
    /// prototype's `Journal`, which its tests drive.
    struct Opened {
        files: Vec<JournalFile>,
        errors: ReadErrors,
        payload_limit: usize,
        held_limit: usize,
    }

    impl Opened {
        fn new(paths: &[PathBuf]) -> Self {
            let mut errors = ReadErrors::default();
            let (files, failed) = open(paths);
            for e in failed {
                errors.add_open(e);
            }
            Self {
                files,
                errors,
                payload_limit: crate::OUTPUT_DEFAULT,
                held_limit: crate::OUTPUT_DEFAULT,
            }
        }

        fn tail(&mut self, groups: &[Group], n: usize, before: Option<&Cursor>) -> Vec<Record> {
            let bounds = Bounds {
                payload: self.payload_limit,
                held: self.held_limit,
            };
            tail(&self.files, groups, n, before, bounds, &mut self.errors)
        }
    }

    fn one(path: &Path) -> Opened {
        Opened::new(&[path.to_path_buf()])
    }

    fn texts(errors: &ReadErrors) -> Vec<String> {
        errors.shown.iter().map(ToString::to_string).collect()
    }

    fn cursor(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, real: u64) -> Cursor {
        Cursor {
            seqnum_id: [seqnum_id; 16],
            seqnum,
            boot_id: [boot; 16],
            monotonic: mono,
            realtime: real,
            xor_hash: 0,
        }
    }

    fn rec(seqnum_id: u8, seqnum: u64, boot: u8, mono: u64, real: u64) -> Candidate {
        Candidate {
            location: cursor(seqnum_id, seqnum, boot, mono, real),
            file: 0,
            offset: 0,
        }
    }

    /// `newest` over walks given newest first, as realtimes oldest first.
    fn newest_of(walks: Vec<Vec<Candidate>>, n: usize) -> Vec<u64> {
        let mut errors = ReadErrors::default();
        let walks = walks.into_iter().map(|w| w.into_iter().map(Ok)).collect();
        let out = newest(walks, n, usize::MAX, &mut errors);
        assert!(errors.is_empty(), "{:?}", texts(&errors));
        out.iter().map(|c| c.location.realtime).collect()
    }

    /// Newest first across files, a flushed copy once (from the earlier
    /// file), and no more than `n`.
    #[test]
    fn newest_takes_the_last_n_and_drops_flushed_copies() {
        let run = vec![rec(1, 2, 9, 20, 200), rec(1, 1, 9, 10, 100)];
        let var = vec![
            rec(2, 3, 9, 30, 300),
            rec(2, 2, 9, 20, 200),
            rec(2, 1, 9, 10, 100),
        ];
        let other = vec![rec(3, 7, 8, 5, 150)];
        let all = newest_of(vec![run.clone(), var.clone(), other.clone()], 10);
        assert_eq!(all, vec![100, 150, 200, 300]);
        assert_eq!(newest_of(vec![run, var, other], 2), vec![200, 300]);
    }

    /// The comparator isn't transitive: x < y (same seqnum id), y < z (same
    /// boot), z < x (realtime). Going backwards, as journalctl's `-n` does
    /// (`seek_tail`, `previous_skip`), x is taken, then y isn't strictly before
    /// it and is dropped, then z is. journalctl then prints forward from z:
    /// z, then x (its forward seek from z finds nothing in y's file by
    /// monotonic time, and x by realtime).
    #[test]
    fn newest_walks_backwards_like_journalctl() {
        let x = rec(1, 1, 0xa, 5, 300);
        let y = rec(1, 2, 0xb, 1, 100);
        let z = rec(2, 1, 0xb, 9, 200);
        assert!(x.location.order(&y.location).is_lt());
        assert!(y.location.order(&z.location).is_lt());
        assert!(z.location.order(&x.location).is_lt());
        assert_eq!(
            newest_of(vec![vec![y], vec![z], vec![x]], 2),
            vec![200, 300]
        );
    }

    /// Equal heads (a flushed copy): the earlier file's is kept.
    #[test]
    fn newest_keeps_the_earlier_file_on_a_tie() {
        let mut copies: Vec<Candidate> = (0..3).map(|_| rec(1, 1, 9, 10, 100)).collect();
        for (i, c) in copies.iter_mut().enumerate() {
            c.file = i;
            c.location.seqnum_id = [u8::try_from(i).expect("small"); 16];
        }
        let walks = copies
            .into_iter()
            .map(|c| vec![Ok(c)].into_iter())
            .collect();
        let mut errors = ReadErrors::default();
        let out = newest(walks, 10, usize::MAX, &mut errors);
        assert_eq!(out.iter().map(|c| c.file).collect::<Vec<_>>(), vec![0]);
    }

    /// A walk's error is reported and the walk goes on: what it gives
    /// before and after stands.
    #[test]
    fn newest_reports_a_walk_error_and_goes_on() {
        let good = vec![Ok(rec(1, 2, 9, 20, 200)), Ok(rec(1, 1, 9, 10, 100))];
        let bad = vec![
            Ok(rec(2, 5, 8, 50, 250)),
            Err(ReadError::of_page(ReadErrorKind::Corrupt("broken".into()))),
            Ok(rec(2, 4, 8, 40, 150)),
        ];
        let mut errors = ReadErrors::default();
        let out = newest(
            vec![good.into_iter(), bad.into_iter()],
            10,
            usize::MAX,
            &mut errors,
        );
        let got: Vec<u64> = out.iter().map(|c| c.location.realtime).collect();
        assert_eq!(got, vec![100, 150, 200, 250]);
        assert_eq!(texts(&errors), vec!["broken".to_string()]);
    }

    /// journald 249 writes an entry again as the first of the next file
    /// when a rotation interrupts it: the same times and hash, the next
    /// seqnum of the same seqnum id. journalctl's tail shows both; so does
    /// `newest`, as sd-journal's order calls the second newer.
    #[test]
    fn newest_keeps_a_rotation_double_write() {
        let old = vec![rec(1, 2, 9, 20, 200), rec(1, 1, 9, 10, 100)];
        let new = vec![rec(1, 4, 9, 30, 300), rec(1, 3, 9, 20, 200)];
        let mut errors = ReadErrors::default();
        let walks = vec![old.into_iter().map(Ok), new.into_iter().map(Ok)];
        let out = newest(walks, 10, usize::MAX, &mut errors);
        let got: Vec<u64> = out.iter().map(|c| c.location.seqnum).collect();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    /// The output limit `k` entries of `journal` take, read newest or
    /// oldest first: the shared `U=x` object once (16 + 48 + 3 bytes),
    /// then per entry its MESSAGE object (16 + 48 + 10) and two fields.
    fn per_record(k: usize) -> usize {
        67 + k * (74 + 2 * std::mem::size_of::<Field>())
    }

    /// An entry listing one DATA object over and over reads it once (the
    /// page's cache), and spends a field's size on every listing: it is
    /// refused once the limit runs out.
    #[test]
    fn an_entry_over_the_output_limit_is_refused() {
        let (mut c, layout) = crafted::journal(1);
        let items = vec![layout.messages[0]; 50];
        let big = c.entry(9, &items);
        let (_t, f) = c.write();
        let e = f.entry(big).expect("entry");
        let field = std::mem::size_of::<Field>();
        let mut left = 74 + 49 * field;
        let err = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect_err("over");
        assert!(is_over(&err), "{err}");
        let mut left = 74 + 50 * field;
        let r = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect("fits exactly");
        assert_eq!((r.fields.len(), left), (50, 0));
    }

    /// Empty payloads cost their object and their fields too, so an entry
    /// listing one over and over can't read without end under the limit.
    #[test]
    fn empty_payloads_spend_the_output_limit() {
        let (mut c, _l) = crafted::journal(1);
        let empty = c.data(b"");
        let big = c.entry(9, &vec![empty; 1000]);
        let (_t, f) = c.write();
        let e = f.entry(big).expect("entry");
        let mut left = 64 + 999 * std::mem::size_of::<Field>();
        let err = record(&f, &e, &mut Payloads::default(), 0, &mut left).expect_err("over");
        assert!(is_over(&err), "{err}");
    }

    fn messages(records: &[Record]) -> Vec<String> {
        records
            .iter()
            .flat_map(Record::fields)
            .filter(|(name, _)| *name == b"MESSAGE")
            .map(|(_, value)| String::from_utf8_lossy(value).into_owned())
            .collect()
    }

    fn u_is_x() -> Vec<Group> {
        vec![vec![vec![b"U=x".to_vec()]]]
    }

    /// One unreadable entry is skipped and reported; the entries on both
    /// sides of it are still shown, unfiltered and through a match.
    #[test]
    fn an_unreadable_entry_is_skipped_not_the_file() {
        let (mut c, l) = crafted::journal(5);
        c.set(l.entries[2] + O_SIZE, 0);
        let (t, _f) = c.write();
        for filter in [Vec::new(), u_is_x()] {
            let mut j = one(t.path());
            let got = j.tail(&filter, 10, None);
            assert_eq!(messages(&got), ["m0", "m1", "m3", "m4"], "{filter:?}");
            assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
        }
    }

    /// The last N are still N when one in the way can't be read.
    #[test]
    fn an_unreadable_entry_does_not_count_against_n() {
        let (mut c, l) = crafted::journal(5);
        c.set(l.entries[3] + O_SIZE, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        assert_eq!(messages(&j.tail(&u_is_x(), 2, None)), ["m2", "m4"]);
    }

    /// An entry whose fields can't be read is dropped whole, as
    /// journalctl drops it (`output_json` on -EBADMSG), and reported.
    #[test]
    fn an_entry_with_an_unreadable_field_is_dropped_and_reported() {
        let (mut c, l) = crafted::journal(4);
        c.set(l.messages[1] + O_SIZE, 8);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let got = j.tail(&[], 10, None);
        assert_eq!(messages(&got), ["m0", "m2", "m3"]);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
        assert!(
            texts(&j.errors)[0].contains("entry 2's fields: "),
            "{:?}",
            texts(&j.errors)
        );
    }

    /// A cut-off file shows what lies before the cut.
    #[test]
    fn a_truncated_file_shows_the_entries_before_the_cut() {
        let (mut c, l) = crafted::journal(6);
        let (t, _f) = c.write();
        std::fs::OpenOptions::new()
            .write(true)
            .open(t.path())
            .expect("open")
            .set_len(l.entries[4] + 8)
            .expect("truncate");
        let mut j = one(t.path());
        let got = j.tail(&u_is_x(), 10, None);
        assert_eq!(messages(&got), ["m0", "m1", "m2", "m3"]);
        assert_eq!(j.errors.shown.len(), 2, "{:?}", texts(&j.errors));
    }

    /// A broken lookup leaves out its group only: the other group of the
    /// filter still matches.
    #[test]
    fn a_failed_lookup_leaves_the_other_groups() {
        let (mut c, l) = crafted::journal(3);
        // The bucket chain U=x sits in now loops back on itself.
        c.set(l.shared + D_NEXT_HASH, l.shared);
        let (t, _f) = c.write();
        let filter = vec![
            vec![vec![b"U=x".to_vec()]],
            vec![vec![b"MESSAGE=m1".to_vec()]],
        ];
        let mut j = one(t.path());
        let got = j.tail(&filter, 10, None);
        assert!(messages(&got).contains(&"m1".to_string()));
        assert!(!j.errors.is_empty());
    }

    /// A file list that changed while the files were opened (a rotation)
    /// is listed and opened again.
    #[test]
    fn a_list_that_changed_while_opening_is_opened_again() {
        let (mut c, _l) = crafted::journal(2);
        let (a, _f) = c.write();
        let (b, _g) = c.write();
        let listing = |paths: &[&Path]| Listing {
            paths: paths.iter().map(|p| p.to_path_buf()).collect(),
            errors: Vec::new(),
        };
        let mut lists = vec![
            listing(&[a.path(), b.path()]),
            listing(&[a.path(), b.path()]),
            listing(&[a.path()]),
        ];
        let mut errors = ReadErrors::default();
        let files = open_listed(|| lists.pop().expect("listed at most 3 times"), &mut errors);
        assert_eq!(files.len(), 2);
        assert!(errors.is_empty(), "{:?}", texts(&errors));
    }

    /// A list that changes at every listing is opened as last listed, and
    /// that is reported.
    #[test]
    fn a_list_that_keeps_changing_is_reported() {
        let (mut c, _l) = crafted::journal(2);
        let (a, _f) = c.write();
        let mut calls = 0;
        let mut errors = ReadErrors::default();
        let files = open_listed(
            || {
                calls += 1;
                Listing {
                    paths: vec![a.path().to_path_buf(); calls],
                    errors: Vec::new(),
                }
            },
            &mut errors,
        );
        assert_eq!((calls, files.len()), (4, 4));
        assert_eq!(
            texts(&errors),
            ["the journal's file list kept changing while it was opened; entries may be missing"]
        );
        assert!(errors.shown[0].path().is_none());
    }

    /// The errors of the listing that stood, and of opening its files, are
    /// reported, outside the read errors' cap.
    #[test]
    fn listing_and_open_errors_are_reported() {
        let dir = tempfile::tempdir().expect("dir");
        let missing = dir.path().join("gone.journal");
        let mut errors = ReadErrors::default();
        for _ in 0..ReadErrors::SHOWN {
            errors.add(ReadError::of_page(ReadErrorKind::HeldLimit));
        }
        let files = open_listed(
            || Listing {
                paths: vec![missing.clone()],
                errors: vec![ReadError::of_page(ReadErrorKind::Corrupt("listed".into()))],
            },
            &mut errors,
        );
        assert!(files.is_empty());
        let shown = texts(&errors);
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert_eq!(shown[1], "listed");
        assert!(shown[2].ends_with("gone.journal: open"), "{shown:?}");
        assert_eq!(errors.omitted, 0);
    }

    /// Entry counts come from the file: an OR term whose values claim
    /// counts that overflow when added still matches (found by the fuzz
    /// harness: the sum overflowed, a panic with overflow checks on).
    #[test]
    fn entry_counts_that_overflow_when_added_still_match() {
        let (mut c, l) = crafted::journal(3);
        c.set(l.messages[0] + D_N_ENTRIES, u64::MAX);
        c.set(l.messages[1] + D_N_ENTRIES, u64::MAX);
        let (t, _f) = c.write();
        let filter = vec![vec![
            vec![b"MESSAGE=m0".to_vec(), b"MESSAGE=m1".to_vec()],
            vec![b"U=x".to_vec()],
        ]];
        let mut j = one(t.path());
        assert_eq!(messages(&j.tail(&filter, 10, None)), ["m0", "m1"]);
    }

    /// Many bad entries: each distinct error once, ten in full, then a
    /// count, so a ruined file can't flood the output.
    #[test]
    fn errors_per_query_are_capped() {
        let (mut c, l) = crafted::journal(30);
        for (i, &e) in l.entries.iter().enumerate() {
            if i % 2 == 1 {
                c.set(e + O_SIZE, 0);
            }
        }
        let (t, _f) = c.write();
        let mut j = one(t.path());
        assert_eq!(j.tail(&[], 100, None).len(), 15);
        assert_eq!(
            j.errors.shown.len(),
            ReadErrors::SHOWN,
            "{:?}",
            texts(&j.errors)
        );
        assert_eq!(j.errors.omitted, 5);
    }

    /// A shared DATA object that decodes a long way and then fails is
    /// decoded once, not once per entry listing it: the failure spends
    /// the limit and is remembered. The entries that don't list it are
    /// still shown.
    #[test]
    fn a_failing_shared_object_is_decoded_once() {
        let text = b"MESSAGE="
            .iter()
            .chain(&vec![b'z'; 99_992])
            .copied()
            .collect::<Vec<u8>>();
        // A zstd frame (a 128 KiB window, a 4-byte content size): a raw block
        // of the 100,000 bytes, then a block of the reserved type, so the
        // decoder gives part of the text and then fails.
        let mut zstd = vec![0x28, 0xb5, 0x2f, 0xfd, 0x80, 0x38];
        zstd.extend_from_slice(&100_000u32.to_le_bytes());
        zstd.extend_from_slice(&(100_000u32 << 3).to_le_bytes()[..3]);
        zstd.extend_from_slice(&text);
        zstd.extend_from_slice(&[0x07, 0x00, 0x00]);
        let (mut c, layout) = crafted::journal(1);
        let bomb = c.data_raw(4, &zstd, crate::hash::jenkins64(&text));
        let mut odd = Vec::new();
        for i in 0..20u64 {
            let items = if i % 2 == 0 {
                vec![layout.messages[0]]
            } else {
                vec![layout.messages[0], bomb]
            };
            odd.push(c.entry(10 + i, &items));
        }
        c.set(H_N_ENTRIES, 21);
        let main = c.array(&[&layout.entries[..], &odd[..]].concat(), 21, 0);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let (t, f) = c.write();
        let mut left = usize::MAX;
        assert!(f.data_payload_spending(bomb, &mut left).is_err());
        // The raw bytes read, and what was decoded before the failure.
        assert!(
            usize::MAX - left > zstd.len() + 32 * 1024,
            "spent {}",
            usize::MAX - left
        );
        let mut j = one(t.path());
        assert_eq!(j.tail(&[], 100, None).len(), 11);
        let shown = texts(&j.errors);
        let decoded = shown.iter().filter(|e| e.contains("zstd")).count();
        let remembered = shown.iter().filter(|e| e.contains("can't be read")).count();
        assert_eq!((decoded, remembered), (1, 1), "{shown:?}");
        assert!(j.errors.shown.iter().any(
            |e| matches!(e.kind(), ReadErrorKind::KnownBad(KnownBad { item }) if *item == bomb)
        ));
    }

    /// One page holds at most its payload limit: the newest entries that
    /// fit are shown, the limit is reported, and the older ones are left
    /// out. (A corrupt file can grow one DATA object shared by every
    /// entry; without the limit, each record would hold a copy.)
    #[test]
    fn a_query_over_its_payload_limit_keeps_the_newest() {
        let (mut c, _l) = crafted::journal(10);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        // U=x is read once, each MESSAGE once (see per_record).
        j.payload_limit = per_record(3) + 5;
        let got = j.tail(&[], 10, None);
        assert_eq!(messages(&got), ["m7", "m8", "m9"]);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
        assert!(
            texts(&j.errors)[0].contains("output limit"),
            "{:?}",
            texts(&j.errors)
        );
        assert!(matches!(
            j.errors.shown[0].kind(),
            ReadErrorKind::OutputLimit { left_out: 7, .. }
        ));
    }

    /// Fields are read after the merge, newest first across files, so the
    /// limit leaves out the oldest entries of the whole journal, not the
    /// files listed last.
    #[test]
    fn the_output_limit_keeps_the_newest_across_files() {
        let (mut c, _l) = crafted::journal(4);
        let (older, _f) = c.write();
        let (mut c, l) = crafted::journal(4);
        for (i, &e) in l.entries.iter().enumerate() {
            c.set(e + 16, 100 + i as u64); // seqnum
            c.set(e + 32, 100 + i as u64); // monotonic, same boot
        }
        let (newer, _g) = c.write();
        let mut j = Opened::new(&[newer.path().to_path_buf(), older.path().to_path_buf()]);
        j.payload_limit = per_record(3);
        let got = j.tail(&[], 10, None);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|r| r.cursor.seqnum >= 100), "{got:?}");
    }

    /// The entries held before the merge are bounded too.
    #[test]
    fn held_entries_are_bounded() {
        let (mut c, _l) = crafted::journal(10);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.held_limit = std::mem::size_of::<Candidate>() * 4;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m6", "m7", "m8", "m9"]);
        assert!(
            texts(&j.errors).iter().any(|e| e.contains("held entries")),
            "{:?}",
            texts(&j.errors)
        );
    }

    /// A crafted journal: `crafted::journal(n)`'s entries, then one entry
    /// per element of `extra` listing those DATA objects, all in the main
    /// entry array.
    fn with_entries(
        n: u64,
        extra: impl Fn(&mut crafted::Crafted) -> Vec<Vec<u64>>,
    ) -> crafted::Crafted {
        let (mut c, l) = crafted::journal(n);
        let mut all = l.entries.clone();
        for (i, items) in extra(&mut c).into_iter().enumerate() {
            all.push(c.entry(100 + i as u64, &items));
        }
        let main = c.array(&all, all.len(), 0);
        c.set(H_N_ENTRIES, all.len() as u64);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        c
    }

    /// An entry of two 600-byte fields, over a 1000-byte limit: the first
    /// field is read (and cached) before the second runs out.
    fn oversized(c: &mut crafted::Crafted, i: u64) -> Vec<u64> {
        let a = c.data(format!("A{i}={}", "a".repeat(600)).as_bytes());
        let b = c.data(format!("B{i}={}", "b".repeat(600)).as_bytes());
        vec![a, b]
    }

    /// The tail's newest entry alone over the limit is reported and
    /// skipped, and the older entries are shown, as a follow skips it
    /// (Difference 5).
    #[test]
    fn a_tail_skips_a_newest_entry_larger_than_the_limit() {
        let mut c = with_entries(3, |c| vec![oversized(c, 0)]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = 1000;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m0", "m1", "m2"]);
        assert!(
            texts(&j.errors)
                .iter()
                .any(|e| e.contains("larger than the output limit")),
            "{:?}",
            texts(&j.errors)
        );
        let e = &j.errors.shown[0];
        assert_eq!(e.path(), Some(t.path()));
        assert!(matches!(
            e.kind(),
            ReadErrorKind::TooLarge { limit: 1000, .. }
        ));
    }

    /// An entry over the limit after others were read still ends the tail
    /// there, with no gap: only an entry read with the limit whole is
    /// skipped.
    #[test]
    fn a_tail_still_stops_at_a_later_entry_over_the_limit() {
        let mut c = with_entries(3, |c| {
            let m = c.data(b"MESSAGE=newest");
            vec![oversized(c, 0), vec![m]]
        });
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = 1000;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["newest"]);
        assert!(
            texts(&j.errors)
                .iter()
                .any(|e| e.contains("older entries left out")),
            "{:?}",
            texts(&j.errors)
        );
    }

    /// Skipping a newest entry over the limit empties the cache of what
    /// it read: the entries after it read their objects afresh, within the
    /// whole limit again, and an object it shares with them isn't taken as
    /// already read.
    #[test]
    fn a_skipped_entry_leaves_the_limit_whole() {
        let mut c = with_entries(0, |c| {
            let shared = c.data(format!("S={}", "s".repeat(500)).as_bytes());
            let big = c.data(format!("B={}", "b".repeat(500)).as_bytes());
            let m = c.data(b"MESSAGE=older");
            vec![vec![shared, m], vec![shared, big]]
        });
        let (t, _f) = c.write();
        let mut j = one(t.path());
        // The shared object and the big one don't fit together; the shared
        // one, the message and two fields do.
        j.payload_limit = 900;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["older"]);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
    }

    /// `MESSAGE=` and 2,000 zeros, as `xz --format=xz --check=none
    /// --lzma2=preset=6,dict=1MiB` writes it (journald's dictionary): 84
    /// bytes stored, 2,008 decoded.
    fn xz_expanding() -> Vec<u8> {
        hex::decode(concat!(
            "fd377a585a000000ff12d9410200210110000000a8708e86e007d7001a5d0026",
            "9146c0d19457e48ceb296e3f26df58d52f85436ee57c4c000000000000012ed8",
            "0f0000005594151fa8000afc020000000000595a",
        ))
        .expect("hex")
    }

    /// Entries sharing one object that decodes past the whole limit: the
    /// newest is skipped after decoding it, and the others listing it are
    /// skipped without decoding it again (their errors carry no `xz`); the
    /// entries before them are shown.
    #[test]
    fn an_object_over_the_whole_limit_is_decoded_once() {
        let bytes = xz_expanding();
        let bomb = std::cell::Cell::new(0);
        let mut c = with_entries(2, |c| {
            bomb.set(c.data_raw(1, &bytes, 0));
            vec![vec![bomb.get()]; 3]
        });
        let (t, f) = c.write();
        // It decodes whole without a limit, and goes over 1000 bytes.
        let whole = f.data_payload(bomb.get()).expect("decodes");
        assert_eq!((whole.len(), &whole[..8]), (2008, &b"MESSAGE="[..]));
        let over = f
            .data_payload_spending(bomb.get(), &mut 1000)
            .expect_err("over");
        assert!(is_over(&over), "{over}");
        let mut j = one(t.path());
        j.payload_limit = 1000;
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m0", "m1"]);
        let shown = texts(&j.errors);
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(
            shown[0].contains("entry 102's fields: data 0x") && shown[0].contains(": xz: "),
            "{shown:?}"
        );
        for later in &shown[1..] {
            assert!(
                later.contains(" (read before): payload over its limit of 1000 bytes"),
                "{shown:?}"
            );
            assert!(!later.contains("xz"), "{shown:?}");
        }
        assert!(
            j.errors
                .shown
                .iter()
                .all(|e| matches!(e.kind(), ReadErrorKind::TooLarge { .. }))
        );
    }

    /// An object over only what an entry had left (its first field spent
    /// part of the limit) isn't taken as too large: an older entry listing
    /// it alone still reads it.
    #[test]
    fn an_object_over_what_was_left_still_reads_alone() {
        let mut c = with_entries(0, |c| {
            let b = c.data(format!("B={}", "b".repeat(600)).as_bytes());
            let a = c.data(format!("A={}", "a".repeat(600)).as_bytes());
            vec![vec![b], vec![a, b]]
        });
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = 1000;
        let got = j.tail(&[], 10, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cursor.seqnum, 100);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
    }

    /// An error between two copies of one offset (two values of a term
    /// listing the same entry) doesn't let the second copy through.
    #[test]
    fn union_yields_an_offset_once_across_an_error() {
        let a: Stream<'_, ()> = Box::new(
            vec![
                Ok((9, ())),
                Err(ReadError::of_page(ReadErrorKind::HeldLimit)),
                Ok((5, ())),
            ]
            .into_iter(),
        );
        let b: Stream<'_, ()> = Box::new(vec![Ok((9, ())), Ok((5, ())), Ok((3, ()))].into_iter());
        let got: Vec<Option<u64>> = union(vec![a, b]).map(|r| r.ok().map(|x| x.0)).collect();
        assert_eq!(got, [Some(9), None, Some(5), Some(3)]);
    }

    /// Before a cursor: the page ends just before it, the cursor's entry
    /// itself not shown; walking backwards skips the newer entries.
    #[test]
    fn before_a_cursor_gives_the_page_before_it() {
        let (mut c, _l) = crafted::journal(6);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&u_is_x(), 10, None);
        let at = all[4].cursor();
        assert_eq!(messages(&j.tail(&u_is_x(), 2, Some(&at))), ["m2", "m3"]);
        assert_eq!(
            messages(&j.tail(&[], 10, Some(&at))),
            ["m0", "m1", "m2", "m3"]
        );
        assert_eq!(messages(&j.tail(&[], 10, Some(&all[0].cursor()))), [""; 0]);
    }

    /// A chain of two arrays where the newer holds a stale slot naming an
    /// older entry (Difference 17): the tail reports the array out of
    /// order, leaves it there (Difference 10), and walks on past it to the
    /// older array; a follow would stop the file.
    #[test]
    fn a_tail_walks_past_an_out_of_order_item() {
        let mut c = Crafted::new();
        c.hash_table(7);
        let older = c.array(&[], 2, 0);
        let newer = c.array(&[], 4, 0);
        let mut e = Vec::new();
        for i in 0..5u64 {
            let m = c.data(format!("MESSAGE=m{i}").as_bytes());
            e.push(c.entry(i + 1, &[m]));
        }
        c.set(older + A_NEXT, newer);
        for (k, &item) in [e[0], e[1]].iter().enumerate() {
            c.set(older + A_ITEMS + 8 * k as u64, item);
        }
        // e[2]'s slot written again with a stale e[2]: e[3] is lost.
        for (k, &item) in [e[2], e[3], e[2], e[4]].iter().enumerate() {
            c.set(newer + A_ITEMS + 8 * k as u64, item);
        }
        c.set(H_N_ENTRIES, 6);
        c.set(H_ENTRY_ARRAY_OFFSET, older);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        assert_eq!(messages(&j.tail(&[], 10, None)), ["m0", "m1", "m2", "m4"]);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
        assert!(
            texts(&j.errors)[0].contains("is out of order"),
            "{:?}",
            texts(&j.errors)
        );
    }

    /// An entry journald was cut off from linking into its DATA objects'
    /// lists, then wrote again whole (Difference 12): it carries `U=x`
    /// among its items but isn't in that object's list, so the tail
    /// matching `U=x` shows the written-again copy only, as journalctl's
    /// match does.
    #[test]
    fn a_tail_shows_one_copy_of_an_entry_cut_off_while_linking() {
        let (mut c, l) = crafted::journal(2);
        let again = c.data(b"MESSAGE=again");
        // The cut-off copy: in the file's list, in no DATA object's.
        let cut = c.entry(3, &[l.shared, again]);
        let whole = c.entry(4, &[l.shared, again]);
        let main = c.array(&[l.entries[0], l.entries[1], cut, whole], 4, 0);
        let shared = c.array(&[l.entries[1], whole], 2, 0);
        c.set(l.shared + D_ENTRY_ARRAY, shared);
        c.set(l.shared + D_N_ENTRIES, 3);
        c.set(again + D_ENTRY_OFFSET, whole);
        c.set(again + D_N_ENTRIES, 1);
        c.set(H_N_ENTRIES, 4);
        c.set(H_ENTRY_ARRAY_OFFSET, main);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let got = j.tail(&u_is_x(), 10, None);
        assert_eq!(messages(&got), ["m0", "m1", "again"]);
        assert_eq!(got[2].cursor().seqnum, 4);
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
        // Unfiltered, the file's own list holds both.
        assert_eq!(
            messages(&j.tail(&[], 10, None)),
            ["m0", "m1", "again", "again"]
        );
    }

    /// A term's several values (`or` of one field) drive through the union
    /// of their lists, each entry once, and the other terms must all hold.
    #[test]
    fn a_group_is_an_and_of_terms_each_an_or() {
        let (mut c, _l) = crafted::journal(4);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let both = vec![vec![
            vec![
                b"MESSAGE=m1".to_vec(),
                b"MESSAGE=m3".to_vec(),
                b"MESSAGE=m3".to_vec(),
            ],
            vec![b"U=x".to_vec()],
        ]];
        assert_eq!(messages(&j.tail(&both, 10, None)), ["m1", "m3"]);
        let none = vec![vec![
            vec![b"MESSAGE=m1".to_vec()],
            vec![b"MESSAGE=m2".to_vec()],
        ]];
        assert!(j.tail(&none, 10, None).is_empty());
        let absent = vec![vec![vec![b"MESSAGE=nowhere".to_vec()]]];
        assert!(j.tail(&absent, 10, None).is_empty());
        let groups = vec![
            vec![vec![b"MESSAGE=m0".to_vec()]],
            vec![vec![b"MESSAGE=m2".to_vec()]],
            vec![vec![b"MESSAGE=m0".to_vec()]],
        ];
        assert_eq!(messages(&j.tail(&groups, 10, None)), ["m0", "m2"]);
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// The driver is the term with the fewest entries: a walk driven from
    /// `MESSAGE=m1` (one entry) reads that entry only, not the shared
    /// object's list, even when its other term is listed first.
    #[test]
    fn the_rarest_term_drives_the_walk() {
        let (mut c, l) = crafted::journal(4);
        // The shared object's list is broken: driving from it would fail.
        c.set(l.shared + D_ENTRY_ARRAY, 8);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let group = vec![vec![vec![b"U=x".to_vec()], vec![b"MESSAGE=m1".to_vec()]]];
        assert_eq!(messages(&j.tail(&group, 10, None)), ["m1"]);
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A payload without `=` isn't a field: left out of the record, the
    /// rest of it read.
    #[test]
    fn a_payload_without_an_equals_sign_is_left_out() {
        let mut c = with_entries(0, |c| {
            let bare = c.data(b"NOEQUALS");
            let m = c.data(b"MESSAGE=kept");
            vec![vec![bare, m]]
        });
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let got = j.tail(&[], 10, None);
        assert_eq!(got[0].fields().count(), 1);
        assert_eq!(messages(&got), ["kept"]);
    }

    /// Directory listings: the journal files of the directory, sorted,
    /// then each machine-id subdirectory's; other names and other
    /// subdirectories are left out. A missing local root adds nothing; a
    /// directory that can't be listed is reported.
    #[test]
    fn listings_find_journal_files() {
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path();
        let mid = "0123456789abcdef0123456789abcdef";
        for sub in [mid, "short", "fedcba9876543210fedcba9876543210"] {
            std::fs::create_dir(d.join(sub)).expect("subdir");
        }
        for name in [
            "b.journal",
            "a.journal~",
            "c.txt",
            "short/x.journal",
            &format!("{mid}/system.journal"),
            &format!("{mid}/notes"),
            "fedcba9876543210fedcba9876543210/user.journal",
        ] {
            std::fs::write(d.join(name), b"").expect("file");
        }
        // A file with a machine id's length is not a directory.
        std::fs::write(d.join("ffffffffffffffffffffffffffffffff"), b"").expect("file");
        let mut out = Listing::default();
        directory_files(d, &mut out);
        let names: Vec<String> = out
            .paths
            .iter()
            .map(|p| p.strip_prefix(d).expect("inside").display().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "a.journal~".to_string(),
                "b.journal".to_string(),
                format!("{mid}/system.journal"),
                "fedcba9876543210fedcba9876543210/user.journal".to_string(),
            ]
        );
        assert!(out.errors.is_empty());

        let mut out = Listing::default();
        journal_files_in(&d.join("none"), true, &mut out);
        assert!(out.paths.is_empty() && out.errors.is_empty());
        journal_files_in(&d.join("none"), false, &mut out);
        journal_files_in(&d.join("c.txt"), true, &mut out);
        directory_files(&d.join("none"), &mut out);
        let failed: Vec<String> = out.errors.iter().map(ToString::to_string).collect();
        assert_eq!(failed.len(), 3, "{failed:?}");
        assert!(failed.iter().all(|e| e.contains(": list the directory")));
        assert_eq!(out.paths, Vec::<PathBuf>::new());
    }

    /// A journal file's name that can't be stat'ed (here a dangling
    /// symlink) is listed, so its open reports it; another name that can't
    /// be stat'ed adds nothing, and neither does a directory named like a
    /// journal file.
    #[test]
    fn a_dangling_journal_file_is_listed_for_its_open_to_report() {
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path();
        std::os::unix::fs::symlink(d.join("gone"), d.join("dangling.journal")).expect("link");
        std::os::unix::fs::symlink(d.join("gone"), d.join("0123456789abcdef0123456789abcdef"))
            .expect("link");
        std::fs::create_dir(d.join("dir.journal")).expect("dir");
        let mut out = Listing::default();
        directory_files(d, &mut out);
        assert_eq!(out.paths, [d.join("dangling.journal")]);
        assert!(out.errors.is_empty());
        let mut errors = ReadErrors::default();
        let files = open_listed(
            || {
                let mut out = Listing::default();
                directory_files(d, &mut out);
                out
            },
            &mut errors,
        );
        assert!(files.is_empty());
        assert_eq!(
            texts(&errors),
            [format!("{}: open", d.join("dangling.journal").display())]
        );
    }

    /// A journal file whose name isn't UTF-8 is still listed (names are
    /// compared as bytes, as sd-journal compares them).
    #[test]
    fn a_name_that_is_not_utf8_is_listed() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().expect("dir");
        let name = OsStr::from_bytes(b"\xffuser.journal");
        std::fs::write(dir.path().join(name), b"").expect("file");
        let mut out = Listing::default();
        journal_files_in(dir.path(), false, &mut out);
        assert_eq!(out.paths, [dir.path().join(name)]);
    }

    /// The prototype's `Journal::open` reported an unreadable file and kept
    /// the others: a file that isn't a journal is its own error.
    #[test]
    fn a_file_that_fails_to_open_is_its_own_error() {
        let (mut c, _l) = crafted::journal(2);
        let (good, _f) = c.write();
        let bad = tempfile::NamedTempFile::new().expect("temp");
        let j = Opened::new(&[bad.path().to_path_buf(), good.path().to_path_buf()]);
        assert_eq!(j.files.len(), 1);
        assert_eq!(j.errors.shown.len(), 1);
        assert_eq!(j.errors.shown[0].path(), Some(bad.path()));
    }

    /// The position a page is read at goes through to the walk.
    #[test]
    fn a_page_reads_at_its_position() {
        let (mut c, _l) = crafted::journal(5);
        let (t, _f) = c.write();
        let journal = crate::Journal::open(crate::Source::Files(vec![t.path().to_path_buf()]))
            .expect("opens");
        let q = crate::Query::field(&crate::FieldName::new(b"U").expect("name"), b"x");
        let limits = crate::Limits::default();
        let tail = journal.page(&q, &Position::Tail, 2, &limits);
        assert_eq!(messages(&tail.records), ["m3", "m4"]);
        let back = journal.page(&q, &Position::Before(tail.records[0].cursor()), 2, &limits);
        assert_eq!(messages(&back.records), ["m1", "m2"]);
        assert!(tail.errors.is_empty() && back.errors.is_empty());
    }
}
