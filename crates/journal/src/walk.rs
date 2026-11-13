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

use crate::boots::BootOrder;
use crate::file::{
    ChainCache, DataRef, EntriesRev, Entry, EntryList, JournalFile, Key, Met, Probe,
};
use crate::open::Opener;
use crate::rules::Group;
use crate::{
    Cursor, Field, FileEnd, PageEnd, Position, ReadError, ReadErrorKind, ReadErrors, Record,
};
use itertools::{Either, Itertools};
use std::cell::Cell;
use std::collections::{HashMap, HashSet, hash_map};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

type Result<T, E = ReadError> = std::result::Result<T, E>;

/// Where `e`, an entry of `f`, is.
pub(crate) fn location(f: &JournalFile, e: &Entry) -> Cursor {
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

/// A journal file's name, as bytes: `*.journal` or `*.journal~`.
pub(crate) fn is_journal_name(name: &[u8]) -> bool {
    name.ends_with(b".journal") || name.ends_with(b".journal~")
}

/// One `read_dir` of `dir`: its journal files into `files`, sorted, and
/// its machine-id subdirectories into `subdirs`. Why not walkdir (in the
/// tree): its depth-first, name-ordered walk puts a subdirectory's files
/// among the directory's own, where `journalctl -D` reads the directory's
/// own first, and it has no rule for a local root that doesn't exist.
/// A journal file's name is taken when it is a regular file or a symlink
/// (to anything, dangling too), as sd-journal's `dirent_is_journal_file`
/// takes `DT_REG` and `DT_LNK`; the open then refuses a symlink (journal
/// spec, Difference 4), so it is reported, never dropped silently. Its
/// type is the directory entry's own (`DirEntry::file_type`, which doesn't
/// follow symlinks:
/// <https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.file_type>);
/// a name whose type can't be read is listed too, for its open to report.
/// Other types (a FIFO, a socket, a device, a directory) aren't journal
/// files, as for sd-journal. Subdirectories are found through symlinks, as
/// sd-journal follows them: a 32-byte name that can't be stat'ed isn't
/// taken as one; it is only a candidate, never a journal file.
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
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                out.errors.push(failed(e));
                continue;
            }
        };
        let p = entry.path();
        // Names as bytes, as sd-journal compares them: a name that isn't
        // UTF-8 is still a journal file's.
        let name = p
            .file_name()
            .map(OsStr::as_encoded_bytes)
            .unwrap_or_default();
        let journal = is_journal_name(name);
        if !journal && name.len() != 32 {
            continue;
        }
        if journal {
            let kind = entry.file_type();
            if kind
                .as_ref()
                .map_or(true, |t| t.is_file() || t.is_symlink())
            {
                files.push(p);
            }
        } else if std::fs::metadata(&p).is_ok_and(|m| m.is_dir()) {
            subdirs.push(p);
        }
    }
    files.sort();
}

/// Open each file through `opener`. A file that can't be opened is
/// returned as its error, never dropped silently (an unsupported format
/// names the file).
fn open(paths: &[PathBuf], opener: &Opener, output: usize) -> (Vec<JournalFile>, Vec<ReadError>) {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for p in paths {
        match JournalFile::open_with(p, opener) {
            Ok(f) => {
                f.bound_entries(output);
                files.push(f);
            }
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
/// most, so they are shown outside the read errors' cap. Each file is
/// bounded by the `output` limit before anything else reads it
/// (`JournalFile::bound_entries`): a follow's start reads tails and walks
/// back before its first poll.
pub(crate) fn open_listed(
    mut list: impl FnMut() -> Listing,
    opener: &Opener,
    output: usize,
    errors: &mut ReadErrors,
) -> Vec<JournalFile> {
    let mut listing = list();
    for _ in 0..3 {
        let (files, failed) = open(&listing.paths, opener, output);
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
    let (files, failed) = open(&listing.paths, opener, output);
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

/// A page's records, the chosen entries it reached, and whether matching
/// entries lie beyond those.
#[derive(Debug)]
pub(crate) struct Paged {
    pub(crate) records: Vec<Record>,
    pub(crate) more_older: bool,
    pub(crate) more_newer: bool,
    /// Per file, the entry count the page hands over (`end`): the second
    /// header read's for a filtered page (`reread`), else the header's.
    pub(crate) counts: Vec<u64>,
    /// Each record's file, as an index into the page's files.
    pub(crate) files: Vec<usize>,
    /// The oldest and newest chosen entries the page read, shown or not
    /// (`Page::oldest_chosen`).
    pub(crate) oldest_chosen: Option<Cursor>,
    pub(crate) newest_chosen: Option<Cursor>,
}

/// The `n` matching entries at `at`, oldest first. `groups` empty means
/// every entry.
///
/// - `Tail` and `Before(c)`: the newest `n`, before `c` for `Before`
///   (`newest`). `more_older` is whether the merge had one more to give;
///   `more_newer`, for `Before`, whether the walk passed a matching entry
///   at or after `c` (it reads all of them on its way back).
/// - `After(c)`: journalctl 259's `--after-cursor`, its seek's bisects
///   emulated, then forward (forward.rs, journal spec decision 10).
///   `more_newer` is whether more than `n` lay after the landing;
///   `more_older`, whether a matching entry lies before where the page
///   started.
///
/// Either flag is also set when the output limit left a chosen entry out
/// on that side of the entries reached (`Reached`); an entry reached but
/// not shown (unreadable, or alone over the limit) doesn't set it. Every
/// comparison, the merges' and `Before`'s against its cursor, orders boots
/// by `boots` (`Cursor::order`).
pub(crate) fn page(
    files: &[JournalFile],
    groups: &[Group],
    n: usize,
    at: &Position,
    bounds: Bounds,
    boots: &BootOrder,
    errors: &mut ReadErrors,
) -> Paged {
    let held = bounds.held / std::mem::size_of::<Candidate>();
    let beyond = Cell::new(false);
    let (picked, more_older, more_newer, from, counts) = match at {
        Position::Tail => {
            let (walks, counts) = walks(files, groups, |_: &Cursor| Step::Take, errors);
            let (picked, more) = newest(&mut Merge::new(walks, boots), n, held, errors);
            (picked, more, false, End::Newest, counts)
        }
        Position::Before(c) => {
            let step = |l: &Cursor| {
                if l.order(c, boots).is_lt() {
                    Step::Take
                } else {
                    beyond.set(true);
                    Step::Skip
                }
            };
            let (walks, counts) = walks(files, groups, step, errors);
            let (picked, more) = newest(&mut Merge::new(walks, boots), n, held, errors);
            (picked, more, beyond.get(), End::Newest, counts)
        }
        Position::After(c) => {
            let fwd = crate::forward::after(files, groups, n, c, held, boots, errors);
            let (picked, older, newer) = (fwd.picked, fwd.more_older, fwd.more_newer);
            (picked, older, newer, End::Oldest, fwd.counts)
        }
    };
    let (records, record_files, reached) = decode(files, &picked, from, bounds.payload, errors);
    Paged {
        records,
        more_older: more_older || reached.older,
        more_newer: more_newer || reached.newer,
        counts,
        files: record_files,
        oldest_chosen: reached.oldest,
        newest_chosen: reached.newest,
    }
}

/// Each file's newest readable entry in its main list, with its offset,
/// read once per page for its end and for the boot table
/// (`JournalFile::tail`). As systemd 255's and 259's `generic_array_get`
/// going up (journal-file.c, which `journal_file_next_entry(f, 0,
/// DIRECTION_UP)` calls; 249's returns the error instead), an entry or
/// array that can't be read is passed over for the next older one; here
/// each is reported too. `None` for a file with no readable entry. On a
/// file none of whose entries can be read, that is a read of each.
pub(crate) fn lasts(files: &[JournalFile], errors: &mut ReadErrors) -> Vec<Option<(u64, Entry)>> {
    files
        .iter()
        .map(|f| {
            for r in f.all_entries_rev() {
                match r.and_then(|off| Ok((off, f.entry(off)?))) {
                    Ok(l) => return Some(l),
                    Err(err) => errors.add(err),
                }
            }
            None
        })
        .collect()
}

/// Where the page ends (`PageEnd`): each opened file's inode and the
/// entry count the page hands over (`counts`, `Paged::counts`), and the
/// newest entry across them (`lasts`, one per file, from the first header
/// reads), unfiltered, by sd-journal's order with the page's `boots` (the
/// earlier file on a tie, as `newest` keeps it; spelled out for the reason
/// `Merge::best` gives), with its file's directory. A file with no newest
/// entry still has its count.
pub(crate) fn end(
    files: &[JournalFile],
    lasts: &[Option<(u64, Entry)>],
    counts: &[u64],
    boots: &BootOrder,
) -> PageEnd {
    let mut newest: Option<(Cursor, &JournalFile)> = None;
    for (f, last) in files.iter().zip(lasts) {
        let Some((_, e)) = last else { continue };
        let l = location(f, e);
        if newest
            .as_ref()
            .is_none_or(|(b, _)| l.order(b, boots).is_gt())
        {
            newest = Some((l, f));
        }
    }
    PageEnd {
        files: files
            .iter()
            .zip(counts)
            .map(|(f, &n_entries)| FileEnd {
                inode: f.inode,
                file_id: f.header.file_id,
                n_entries,
            })
            .collect(),
        newest: newest.map(|(l, f)| (l, f.path.parent().map(Path::to_path_buf))),
        shown: Vec::new(),
    }
}

/// One lazy walk per file, newest first, and each file's count to hand
/// over (`file_walk`). A group whose terms can't be looked up is reported
/// in `errors` and left out, the other groups still match, as sd-journal
/// leaves out what it can't iterate and keeps the rest: `.journal~` files
/// are exactly the ones left dirty.
fn walks<'a, S>(
    files: &'a [JournalFile],
    groups: &'a [Group],
    step: S,
    errors: &mut ReadErrors,
) -> (Vec<impl Iterator<Item = Result<Candidate>> + 'a>, Vec<u64>)
where
    S: Fn(&Cursor) -> Step + Copy + 'a,
{
    let (mut out, mut counts) = (Vec::new(), Vec::new());
    for (file, f) in files.iter().enumerate() {
        let (walk, count) = file_walk(f, groups, step, errors);
        counts.push(count);
        out.push(walk.map(move |r| {
            r.map(|(offset, location)| Candidate {
                location,
                file,
                offset,
            })
        }));
    }
    (out, counts)
}

/// Read the chosen entries for output, from the end the page pages from
/// (`from`: newest first for `Tail` and `Before`, oldest first for
/// `After`), within one output limit for the page (`payload_limit`), which
/// every read spends, failed ones too. Entries share most DATA objects (a
/// unit's _PID, _COMM, _HOSTNAME and so on): each is read and decompressed
/// once, and one that failed isn't decoded again for the next entry
/// listing it. An entry that can't be read is reported and skipped, as
/// sd-journal skips an entry it can't read (`output_json` on -EBADMSG). The
/// first entry that doesn't fit ends the output, so it has no gap: a page
/// back from the oldest chosen entry reached (a page forward from the
/// newest, `Reached`) goes on from there. The entries left out, at the far end, are reported. An entry
/// that doesn't fit the whole limit, read before anything else, is reported
/// and skipped instead (Difference 5): stopping there would show nothing at
/// all.
///
/// Also returns each record's file (an index into `files`), and the
/// chosen entries the page reached (`Reached`).
fn decode(
    files: &[JournalFile],
    picked: &[Candidate],
    from: End,
    payload_limit: usize,
    errors: &mut ReadErrors,
) -> (Vec<Record>, Vec<usize>, Reached) {
    let mut out = Vec::with_capacity(picked.len());
    // Each record's file, as an index into `files`.
    let mut from_files = Vec::with_capacity(picked.len());
    // The oldest and newest chosen entries reached (shown, or skipped as
    // unreadable or too large), as indexes into `picked`.
    let mut reached: Option<(usize, usize)> = None;
    // Indexes into `picked` (oldest first), in reading order.
    // <https://docs.rs/itertools/0.14.0/itertools/enum.Either.html>: an
    // iterator of either range, unboxed.
    let order = match from {
        End::Newest => Either::Left((0..picked.len()).rev()),
        End::Oldest => Either::Right(0..picked.len()),
    };
    let mut payloads = Payloads::within(payload_limit);
    let mut left = payload_limit;
    for (i, at) in order.enumerate() {
        let c = &picked[at];
        let f = &files[c.file];
        let whole = left == payload_limit;
        let read = f
            .entry(c.offset)
            .and_then(|e| record(f, &e, &mut payloads, c.file, &mut left));
        // Each arm but the limit's (which leaves this entry and the rest
        // out) reaches the entry: shown, or skipped and reported.
        if !matches!(&read, Err(err) if err.is_over_limit() && !whole) {
            reached = Some(reached.map_or((at, at), |(lo, hi)| (lo.min(at), hi.max(at))));
        }
        match read {
            Ok(r) => {
                out.push(r);
                from_files.push(c.file);
            }
            Err(err) if err.is_over_limit() && whole => {
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
            Err(err) if err.is_over_limit() => {
                let (limit, left_out) = (payload_limit, picked.len() - i);
                let kind = match from {
                    End::Newest => ReadErrorKind::OutputLimit { limit, left_out },
                    End::Oldest => ReadErrorKind::OutputLimitNewer { limit, left_out },
                };
                errors.add(ReadError::of_page(kind));
                break;
            }
            Err(err) => errors.add(err),
        }
    }
    if from == End::Newest {
        out.reverse();
        from_files.reverse();
    }
    // The first entry read has the whole limit, so it is reached whenever
    // one is chosen: `None` only for a page that chose nothing.
    let reached = reached.map_or_else(Reached::default, |(oldest, newest)| Reached {
        older: oldest > 0,
        newer: newest + 1 < picked.len(),
        oldest: Some(picked[oldest].location),
        newest: Some(picked[newest].location),
    });
    (out, from_files, reached)
}

/// The end of its chosen entries a page reads first: the one it pages
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Newest,
    Oldest,
}

/// The chosen entries a page reached: those it read, shown or skipped as
/// unreadable or too large (reported), not those the output limit left
/// out. Its oldest and newest, and whether chosen entries the limit left
/// out lie older or newer than them. A page from those cursors steps past
/// an entry that can't be shown, and meets the left-out ones again
/// (page-M1, page-M2). All empty for a page that chose nothing.
#[derive(Debug, Clone, Copy, Default)]
struct Reached {
    older: bool,
    newer: bool,
    oldest: Option<Cursor>,
    newest: Option<Cursor>,
}

/// An entry a file's walk chose: its location for the merge, and where to
/// read it once it is among the entries shown.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) location: Cursor,
    pub(crate) file: usize,
    pub(crate) offset: u64,
}

/// The files' walks (each newest first) merged into one stream, newest
/// first. This is how `journalctl -n` finds the newest: `sd_journal_seek_tail`,
/// then `sd_journal_previous_skip`, which is `real_journal_next` going up
/// (v249 sd-journal.c). Every file first moves past heads that aren't
/// strictly before the last entry taken, then the largest head by
/// `journal_file_compare_locations` is taken, the earlier file on a tie.
/// So a flushed copy is shown once, from the earlier file in search order,
/// and only the heads are read: a file's walk goes no further back than
/// the entries it contributes, plus one head. journalctl then walks forward
/// again from the oldest entry taken; that second walk isn't modelled
/// (Difference 15).
///
/// An error a walk yields is reported and the walk goes on: the walks
/// pass over what they can't read (an entry, an array) and yield the
/// error in its place, so only what it hides is lost.
struct Merge<'b, I> {
    walks: Vec<Option<I>>,
    heads: Vec<Option<Candidate>>,
    last: Option<Cursor>,
    /// The page's order between boots (`Cursor::order`).
    boots: &'b BootOrder,
}

impl<'b, I: Iterator<Item = Result<Candidate>>> Merge<'b, I> {
    fn new(walks: Vec<I>, boots: &'b BootOrder) -> Self {
        let heads = walks.iter().map(|_| None).collect();
        Self {
            walks: walks.into_iter().map(Some).collect(),
            heads,
            last: None,
            boots,
        }
    }

    /// The head the merge would take next, without taking it; `None` once
    /// every walk is done.
    fn best(&mut self, errors: &mut ReadErrors) -> Option<usize> {
        for (walk, head) in self.walks.iter_mut().zip(self.heads.iter_mut()) {
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
                match (head.as_ref(), self.last.as_ref()) {
                    (Some(h), Some(last)) if !h.location.order(last, self.boots).is_lt() => {
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
        for (i, head) in self.heads.iter().enumerate() {
            let Some(h) = head else { continue };
            let better = best.is_none_or(|b| {
                self.heads[b]
                    .as_ref()
                    .is_some_and(|bh| h.location.order(&bh.location, self.boots).is_gt())
            });
            if better {
                best = Some(i);
            }
        }
        best
    }

    /// Take the head `best` gave.
    fn take(&mut self, best: usize) -> Candidate {
        let c = self.heads[best].take().expect("`best` names a head");
        self.last = Some(c.location);
        c
    }
}

/// The newest `n` entries of the merge, oldest first, and whether the
/// merge had another to give (a matching entry older than them).
///
/// At most `held` candidates are kept (the page's held limit): past it the
/// older entries are left out, reported.
fn newest<I>(
    merge: &mut Merge<'_, I>,
    n: usize,
    held: usize,
    errors: &mut ReadErrors,
) -> (Vec<Candidate>, bool)
where
    I: Iterator<Item = Result<Candidate>>,
{
    let mut out: Vec<Candidate> = Vec::new();
    let more = loop {
        let Some(best) = merge.best(errors) else {
            break false;
        };
        if out.len() >= n {
            break true;
        }
        if out.len() >= held {
            errors.add(ReadError::of_page(ReadErrorKind::HeldLimit));
            break true;
        }
        out.push(merge.take(best));
    };
    out.reverse();
    (out, more)
}

/// What to do with a matching entry met while walking backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Take,
    /// Outside the range wanted: keep walking.
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
    /// (Difference 17). `step` is asked about matching entries only, so it
    /// can tell what lies beyond the page's range.
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
            let matches = self
                .others
                .iter()
                .all(|alts| alts.iter().any(|o| e.items.contains(o)));
            if !matches {
                continue;
            }
            let location = location(self.f, &e);
            match (self.step)(&location) {
                Step::Take => return Some(Ok((off, location))),
                Step::Skip => {}
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
///
/// For a filtered query, the file's header is then read again
/// (`reread`): the entries it finds the walk can show (in its driving
/// term's lists by then) join the walk, an offset met twice taken once,
/// and the count returned is the one the page hands over.
fn file_walk<'a, S>(
    f: &'a JournalFile,
    groups: &[Group],
    step: S,
    errors: &mut ReadErrors,
) -> (Stream<'a, Cursor>, u64)
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
    let mut known = Known::default();
    'groups: for (g, group) in groups.iter().enumerate() {
        let mut resolved: Vec<Vec<DataRef>> = Vec::new();
        for term in group {
            let mut refs = Vec::new();
            for payload in term {
                match f.find_data(payload) {
                    Ok(Some(d)) => refs.push(d),
                    Ok(None) => {}
                    Err(err) => {
                        errors.add(err);
                        known.failed.insert(g);
                        continue 'groups;
                    }
                }
            }
            resolved.push(refs);
        }
        if resolved.iter().any(Vec::is_empty) {
            continue;
        }
        let Some(driver) = driver(&resolved) else {
            continue;
        };
        known.drivers.insert(g, driver);
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
    let again = reread(f, groups, &known, errors);
    if !again.extras.is_empty() {
        // Already matched by their items; newest first, as the walks go,
        // and through the page's step like every other match.
        walks.push(Box::new(again.extras.into_iter().rev().filter_map(
            move |(off, location)| match step(&location) {
                Step::Take => Some(Ok((off, location))),
                Step::Skip => None,
            },
        )));
    }
    (union(walks), again.n_entries)
}

/// A group's driving term: the one with the fewest entries, whose lists a
/// walk reads, the other terms checked by the entry's items. `None` for a
/// group of no terms.
pub(crate) fn driver(terms: &[Vec<DataRef>]) -> Option<usize> {
    terms
        .iter()
        .enumerate()
        // The counts come from the file: saturate, don't overflow.
        .min_by_key(|(_, r)| r.iter().fold(0u64, |a, d| a.saturating_add(d.n_entries)))
        .map(|(i, _)| i)
}

/// What a file's lookups met, for its second header read (`reread`): the
/// groups a lookup failed in (already reported, and left out), and per
/// group walked, its driving term (`driver`).
#[derive(Debug, Default)]
pub(crate) struct Known {
    pub(crate) failed: HashSet<usize>,
    pub(crate) drivers: HashMap<usize, usize>,
}

/// A filtered page's second look at one file.
#[derive(Debug)]
pub(crate) struct Reread {
    /// The entry count the page hands over (`PageEnd`).
    pub(crate) n_entries: u64,
    /// The main-list entries from the first count's newest one up to the
    /// second count that match the query by their items and that the
    /// group's driving term's lists hold by the second read, oldest first,
    /// with their locations.
    pub(crate) extras: Vec<(u64, Cursor)>,
}

/// A filtered page's second header read of `f` (journal spec decision
/// 13), made after every lookup of `groups` (`known`). journald links an
/// entry into the main list and raises the header's count before it links
/// the entry into its values' lists (journal-file.c
/// `journal_file_link_entry`; docs/platform-notes/systemd-journal.md), so
/// every entry the lookups' lists hold is counted by this read (n1), and
/// every entry below the first read's newest (n0-1) was already in all its
/// lists. The entries from n0-1 to n1-1 are read here: the newest entry
/// the first read counted, which journald may not have linked into its
/// lists yet, and those written during the page. Every value of the query
/// is looked up again (a value first written during the page had no DATA
/// object at its lookup), and an entry that matches by its items is added
/// to the page only where the walk can show it: a group's driving term's
/// lists hold it now (the walk reads those, the other terms by items), or,
/// for a group the walk had nothing to walk in, every term's. `PageEnd`
/// then holds n1, and the follow goes on from there.
///
/// An entry that carries the query's values but isn't in their lists yet
/// can only be the newest n1 counts, on a live file (the link order): the
/// page doesn't show it, and hands over its index instead, so the follow
/// reads it by its items, as it reads every entry. One journald will never
/// link (it died, or the link failed) is then the follow's to show, as
/// Difference 12 says. An older one, which journald moved on from, was cut
/// off for good: neither shows it.
///
/// Not for an unfiltered query (it reads no value list), nor a sealed file,
/// whose count is final: reading its newest entry again would show an entry
/// journald was cut off from linking (Difference 12). When the second read
/// fails, or the main list doesn't give every entry up to n1, the page hands
/// over at n0 with nothing added, as it did before.
pub(crate) fn reread(
    f: &JournalFile,
    groups: &[Group],
    known: &Known,
    errors: &mut ReadErrors,
) -> Reread {
    let n0 = f.header.n_entries;
    let first = || Reread {
        n_entries: n0,
        extras: Vec::new(),
    };
    if groups.is_empty() || f.sealed() {
        return first();
    }
    #[cfg(any(test, feature = "testing"))]
    crate::testing::seam::before_second_read(&f.path);
    // The two silent fallbacks below happen only on a file that isn't
    // journald's live one (journald writes each main-list item before it
    // raises the count, and never lowers it): one cut short or replaced in
    // place, whose walk reports the damage itself.
    let (n1, head) = match f.count_now() {
        Ok((n1, head)) if n1 >= n0 => (n1, head),
        Ok(_) => return first(),
        Err(e) => {
            errors.add(e);
            return first();
        }
    };
    let from = n0.saturating_sub(1);
    let offsets = match f.entries_between(&mut None, from, (n1, head)) {
        Ok(o) if o.len() as u64 == n1 - from && o.iter().all(|&off| f.plausible_entry(off)) => o,
        Ok(_) => return first(),
        Err(e) => {
            errors.add(e);
            return first();
        }
    };
    // Per group, per term, its values' DATA objects as they are now, looked
    // up again: their lists as journald has linked them by this read; with
    // the group's driving term, where the walk walked one.
    let mut wanted: Vec<(Vec<Vec<DataRef>>, Option<usize>)> = Vec::new();
    'groups: for (g, group) in groups.iter().enumerate() {
        if known.failed.contains(&g) {
            continue;
        }
        let mut terms = Vec::new();
        for term in group {
            let mut values = Vec::new();
            for payload in term {
                match f.find_data(payload) {
                    Ok(Some(d)) => values.push(d),
                    Ok(None) => {}
                    Err(e) => {
                        // Repeat rather than lose: the follow reads from n0.
                        errors.add(e);
                        return first();
                    }
                }
            }
            if values.is_empty() {
                continue 'groups;
            }
            terms.push(values);
        }
        wanted.push((terms, known.drivers.get(&g).copied()));
    }
    let mut extras = Vec::new();
    let mut n_entries = n1;
    if !wanted.is_empty() {
        for (index, off) in (from..).zip(offsets) {
            let e = match f.entry(off) {
                Ok(e) => e,
                Err(err) => {
                    errors.add(err);
                    return first();
                }
            };
            let carries = |d: &DataRef| e.items.contains(&d.offset);
            let by_items =
                |terms: &[Vec<DataRef>]| terms.iter().all(|values| values.iter().any(carries));
            let mut in_lists = |values: &[DataRef]| {
                values
                    .iter()
                    .any(|d| carries(d) && lists(f, d, off, errors))
            };
            if !wanted.iter().any(|(terms, _)| by_items(terms)) {
                continue;
            }
            // What the walk can show: a group it walked, through its driving
            // term's lists, the others matched by items; one it couldn't
            // (a value first written during the page), through every
            // term's, as journalctl's match finds it.
            let listed = wanted.iter().any(|(terms, driver)| {
                by_items(terms)
                    && match driver {
                        Some(t) => in_lists(&terms[*t]),
                        None => terms.iter().all(|values| in_lists(values)),
                    }
            });
            if listed {
                extras.push((off, location(f, &e)));
            } else if index + 1 == n1 {
                // Counted, not linked yet (or never: a journald that died,
                // a link cut off). The follow reads it by its items.
                n_entries = index;
            }
        }
    }
    Reread { n_entries, extras }
}

/// Whether the DATA object `d`'s list, as read now, holds `entry`: an
/// offset bisect (journal-file.c `generic_array_bisect`) that lands on it.
/// A bisect a zero item or a bad entry cuts short or refuses doesn't,
/// unreported, as the walks pass a zero over without reporting it and read
/// such a list from its head (Difference 8); one the system's error stops
/// doesn't either, and that error is reported.
fn lists(f: &JournalFile, d: &DataRef, entry: u64, errors: &mut ReadErrors) -> bool {
    let probe = Probe {
        key: Key::Offset,
        needle: entry,
    };
    let mut met = Met::default();
    let found = f.bisect_list(
        &EntryList::of_data(d),
        probe,
        &mut ChainCache::default(),
        &mut met,
    );
    match found {
        Ok(found) => found.is_some_and(|found| found.offset == entry),
        Err(e) if e.is_io() => {
            errors.add(e);
            false
        }
        Err(_) => false,
    }
}

/// The DATA payloads one page has read, keyed by (file index, offset): a
/// field, `None` for a payload without `=` (skipped), the objects that
/// failed, so another entry listing one isn't decoded again, and those
/// that went over the whole limit (`whole`) on their own, which can never
/// fit: an entry listing one is refused without decoding it again, so a
/// page skipping several such entries decodes the object once.
#[derive(Default)]
pub(crate) struct Payloads {
    fields: HashMap<(usize, u64), Option<Field>>,
    failed: HashSet<(usize, u64)>,
    too_large: HashSet<(usize, u64)>,
    whole: usize,
}

impl Payloads {
    /// An empty cache for reads within `whole` bytes each.
    pub(crate) fn within(whole: usize) -> Self {
        Self {
            whole,
            ..Self::default()
        }
    }

    /// Drop what file `file` read: the follow's, after it skipped an entry
    /// over its whole limit, the only one that file read so far.
    pub(crate) fn forget(&mut self, file: usize) {
        self.fields.retain(|&(f, _), _| f != file);
    }

    /// The payload bytes the cache holds.
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.fields
            .values()
            .flatten()
            .map(|f| f.payload.len())
            .sum()
    }
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
pub(crate) fn record(
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
                    if !err.is_over_limit() {
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
            let (files, failed) = open(paths, &Opener::default(), crate::OUTPUT_DEFAULT);
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
            let at = before.map_or(Position::Tail, |c| Position::Before(*c));
            self.page(groups, n, &at).records
        }

        fn page(&mut self, groups: &[Group], n: usize, at: &Position) -> Paged {
            let bounds = Bounds {
                payload: self.payload_limit,
                held: self.held_limit,
            };
            page(
                &self.files,
                groups,
                n,
                at,
                bounds,
                &BootOrder::none(),
                &mut self.errors,
            )
        }
    }

    /// `end`, from the files' `lasts`, with no boot step.
    fn end_of(files: &[JournalFile], errors: &mut ReadErrors) -> PageEnd {
        let lasts = lasts(files, errors);
        let counts: Vec<u64> = files.iter().map(|f| f.header.n_entries).collect();
        end(files, &lasts, &counts, &BootOrder::none())
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
        let (out, _more) = newest(
            &mut Merge::new(walks, &BootOrder::none()),
            n,
            usize::MAX,
            &mut errors,
        );
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
        assert!(x.location.order(&y.location, &BootOrder::none()).is_lt());
        assert!(y.location.order(&z.location, &BootOrder::none()).is_lt());
        assert!(z.location.order(&x.location, &BootOrder::none()).is_lt());
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
        let (out, _more) = newest(
            &mut Merge::new(walks, &BootOrder::none()),
            10,
            usize::MAX,
            &mut errors,
        );
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
        let (out, _more) = newest(
            &mut Merge::new(vec![good.into_iter(), bad.into_iter()], &BootOrder::none()),
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
        let (out, _more) = newest(
            &mut Merge::new(walks, &BootOrder::none()),
            10,
            usize::MAX,
            &mut errors,
        );
        let got: Vec<u64> = out.iter().map(|c| c.location.seqnum).collect();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    /// The output limit `k` entries of `journal` take, read newest or
    /// oldest first: the shared `U=x` object once (16 + 48 + 3 bytes),
    /// then per entry its MESSAGE object (16 + 48 + 10) and two fields.
    fn per_record(k: usize) -> usize {
        per_record_in(Format::Regular, k)
    }

    /// `per_record` in `format`: a compact DATA object's header is 8 bytes
    /// longer.
    fn per_record_in(format: Format, k: usize) -> usize {
        let header = usize::try_from(format.payload_at()).expect("small");
        header + 3 + k * (header + 10 + 2 * std::mem::size_of::<Field>())
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
        assert!(err.is_over_limit(), "{err}");
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
        assert!(err.is_over_limit(), "{err}");
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
        let files = open_listed(
            || lists.pop().expect("listed at most 3 times"),
            &Opener::default(),
            crate::OUTPUT_DEFAULT,
            &mut errors,
        );
        assert_eq!(files.len(), 2);
        assert!(errors.is_empty(), "{:?}", texts(&errors));
    }

    /// Security audit (Task 11, M2): every file opened for a page or a
    /// follow is bounded by its output limit from the start, before any
    /// entry is read (a follow's start reads tails and walks back before
    /// its first poll): an entry listing more items than that limit could
    /// show is refused.
    #[test]
    fn opened_files_bound_their_entries_by_the_output_limit() {
        let mut c = Crafted::new();
        c.hash_table(1);
        let d = c.data(b"A=1");
        let e = c.entry(1, &[d; 3]);
        let (t, _f) = c.write();
        let held = std::mem::size_of::<crate::Field>();
        let open_within = |output| {
            let mut errors = ReadErrors::default();
            let listing = || Listing {
                paths: vec![t.path().to_path_buf()],
                errors: Vec::new(),
            };
            open_listed(listing, &Opener::default(), output, &mut errors)
        };
        let refused = open_within(3 * held - 1);
        let err = refused[0].entry(e).expect_err("over the bound");
        assert!(err.is_over_limit(), "{err}");
        assert_eq!(
            open_within(3 * held)[0]
                .entry(e)
                .expect("at the bound")
                .items
                .len(),
            3
        );
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
            &Opener::default(),
            crate::OUTPUT_DEFAULT,
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
            &Opener::default(),
            crate::OUTPUT_DEFAULT,
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
        with_entries_in(Format::Regular, n, extra)
    }

    /// `with_entries`, in `format`.
    fn with_entries_in(
        format: Format,
        n: u64,
        extra: impl Fn(&mut crafted::Crafted) -> Vec<Vec<u64>>,
    ) -> crafted::Crafted {
        let (mut c, l) = crafted::journal_in(format, n);
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
        assert!(over.is_over_limit(), "{over}");
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
            // Not decoded again: no xz stage (the temp path can hold "xz").
            assert!(!later.contains(": xz: "), "{shown:?}");
        }
        assert!(j.errors.shown.iter().all(ReadError::is_too_large));
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
        each_format(before_a_cursor_gives_the_page_before_it_in);
    }

    fn before_a_cursor_gives_the_page_before_it_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 6);
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
        each_format(a_tail_walks_past_an_out_of_order_item_in);
    }

    fn a_tail_walks_past_an_out_of_order_item_in(format: Format) {
        let mut c = Crafted::new_in(format);
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
            c.set_array_item(older, k as u64, item);
        }
        // e[2]'s slot written again with a stale e[2]: e[3] is lost.
        for (k, &item) in [e[2], e[3], e[2], e[4]].iter().enumerate() {
            c.set_array_item(newer, k as u64, item);
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

    /// A chain of three arrays, `[e0, e1]`, `[e2, junk, e3]` and
    /// `[e4, e5]`: the middle one holds a bogus item, a plausible offset
    /// that names no entry. `high` puts it past every entry (a DATA object
    /// written after them), else before them (`m0`'s DATA object).
    fn chain_with_a_bogus_item(format: Format, high: bool) -> tempfile::NamedTempFile {
        let mut c = Crafted::new_in(format);
        c.hash_table(7);
        let arrays = [c.array(&[], 2, 0), c.array(&[], 3, 0), c.array(&[], 2, 0)];
        let (mut e, mut low) = (Vec::new(), 0);
        for i in 0..6u64 {
            let m = c.data(format!("MESSAGE=m{i}").as_bytes());
            if i == 0 {
                low = m;
            }
            e.push(c.entry(i + 1, &[m]));
        }
        let junk = if high { c.data(b"JUNK=1") } else { low };
        c.set(arrays[0] + A_NEXT, arrays[1]);
        c.set(arrays[1] + A_NEXT, arrays[2]);
        let items: [&[u64]; 3] = [&[e[0], e[1]], &[e[2], junk, e[3]], &[e[4], e[5]]];
        for (&array, items) in arrays.iter().zip(items) {
            for (k, &item) in items.iter().enumerate() {
                c.set_array_item(array, k as u64, item);
            }
        }
        // Seven items: the count covers the bogus one, as journald's
        // `n_entries` covered the slot.
        c.set(H_N_ENTRIES, 7);
        c.set(H_ENTRY_ARRAY_OFFSET, arrays[0]);
        c.file()
    }

    /// The forward page and the tail over one file with a bogus item:
    /// each leaves the damaged array at the break as it meets it (from its
    /// own side, so they lose a different entry there), reports it, and
    /// walks on: both show every entry of the arrays either side.
    fn after_and_tail(format: Format, high: bool) -> (Vec<String>, Vec<String>) {
        let t = chain_with_a_bogus_item(format, high);
        let mut j = one(t.path());
        let tail = messages(&j.tail(&[], 10, None));
        assert!(
            texts(&j.errors)
                .iter()
                .any(|m| m.contains("is out of order")),
            "{:?}",
            texts(&j.errors)
        );
        let mut j = one(t.path());
        let mut before = cursor(7, 0, 7, 0, 0);
        before.seqnum_id = j.files[0].header.seqnum_id;
        let after = messages(&j.page(&[], 10, &Position::After(before)).records);
        assert!(
            texts(&j.errors)
                .iter()
                .any(|m| m.contains("is out of order")),
            "{:?}",
            texts(&j.errors)
        );
        (after, tail)
    }

    /// A bogus item higher than every entry: going forward it makes each
    /// later item look out of order unless the order check starts afresh
    /// after the array it left (file-M1), as the backward walk's does.
    #[test]
    fn after_and_tail_walk_past_a_bogus_high_item() {
        each_format(after_and_tail_walk_past_a_bogus_high_item_in);
    }

    fn after_and_tail_walk_past_a_bogus_high_item_in(format: Format) {
        let (after, tail) = after_and_tail(format, true);
        assert_eq!(after, strs(&["m0", "m1", "m2", "m4", "m5"]));
        assert_eq!(tail, strs(&["m0", "m1", "m3", "m4", "m5"]));
        // From a cursor just before the damaged array: the page starts
        // inside the chain (an offset bisect), not at its head.
        let t = chain_with_a_bogus_item(format, true);
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        assert_eq!(messages(&all[1..2]), ["m1"]);
        let pg = j.page(&[], 10, &Position::After(all[1].cursor()));
        assert_eq!(messages(&pg.records), ["m2", "m4", "m5"]);
    }

    /// A bogus item lower than every entry: going backward it makes each
    /// older item look out of order unless the order check starts afresh
    /// after the array it left.
    #[test]
    fn after_and_tail_walk_past_a_bogus_low_item() {
        each_format(after_and_tail_walk_past_a_bogus_low_item_in);
    }

    fn after_and_tail_walk_past_a_bogus_low_item_in(format: Format) {
        let (after, tail) = after_and_tail(format, false);
        assert_eq!(after, strs(&["m0", "m1", "m2", "m4", "m5"]));
        assert_eq!(tail, strs(&["m0", "m1", "m3", "m4", "m5"]));
    }

    /// Difference 19, a damaged array: a chain of three arrays, `[e0, e1]`,
    /// `[e2, e3, junk, e4, e5]` and `[e6, e7]`, the junk item at the
    /// middle array's midpoint naming `m0`'s DATA object (as Checkpoint B's
    /// probe damaged a 249-written file). A forward page from a cursor past
    /// it, e4's or e5's, bisects onto the junk item: sb reports it and shows
    /// nothing, as journalctl 249, 255 and 259 show nothing after such a
    /// cursor (they leave the file out). The tail walks back past it and
    /// reports it; the junk offset lies below every entry's, so the
    /// middle array is out of order there and left (e2 and e3 with it).
    #[test]
    fn a_forward_page_past_a_damaged_item_shows_nothing_and_reports_it() {
        each_format(a_forward_page_past_a_damaged_item_shows_nothing_and_reports_it_in);
    }

    fn a_forward_page_past_a_damaged_item_shows_nothing_and_reports_it_in(format: Format) {
        let mut c = Crafted::new_in(format);
        c.hash_table(7);
        let arrays = [c.array(&[], 2, 0), c.array(&[], 5, 0), c.array(&[], 2, 0)];
        let mut e = Vec::new();
        let mut junk = 0;
        for i in 0..8u64 {
            let m = c.data(format!("MESSAGE=m{i}").as_bytes());
            if i == 0 {
                junk = m;
            }
            e.push(c.entry(i + 1, &[m]));
        }
        c.set(arrays[0] + A_NEXT, arrays[1]);
        c.set(arrays[1] + A_NEXT, arrays[2]);
        let items: [&[u64]; 3] = [
            &[e[0], e[1]],
            &[e[2], e[3], junk, e[4], e[5]],
            &[e[6], e[7]],
        ];
        for (&array, items) in arrays.iter().zip(items) {
            for (k, &item) in items.iter().enumerate() {
                c.set_array_item(array, k as u64, item);
            }
        }
        // Nine items: the count covers the junk one.
        c.set(H_N_ENTRIES, 9);
        c.set(H_ENTRY_ARRAY_OFFSET, arrays[0]);
        let t = c.file();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        assert_eq!(messages(&all), ["m0", "m1", "m4", "m5", "m6", "m7"]);
        let errors = texts(&j.errors);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("is type 1, wanted 3"), "{errors:?}");
        assert!(errors[1].contains("is out of order"), "{errors:?}");
        for past in &all[2..4] {
            let mut j = one(t.path());
            let pg = j.page(&[], 10, &Position::After(past.cursor()));
            assert_eq!(messages(&pg.records), [""; 0], "{:?}", past.cursor());
            let errors = texts(&j.errors);
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert!(errors[0].contains("is type 1, wanted 3"), "{errors:?}");
        }
    }

    /// An entry journald was cut off from linking into its DATA objects'
    /// lists, then wrote again whole (Difference 12): it carries `U=x`
    /// among its items but isn't in that object's list, so the tail
    /// matching `U=x` shows the written-again copy only, as journalctl's
    /// match does.
    #[test]
    fn a_tail_shows_one_copy_of_an_entry_cut_off_while_linking() {
        each_format(a_tail_shows_one_copy_of_an_entry_cut_off_while_linking_in);
    }

    fn a_tail_shows_one_copy_of_an_entry_cut_off_while_linking_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 2);
        let again = c.data(b"MESSAGE=again");
        // The cut-off copy: in the file's list, in no DATA object's.
        let cut = c.entry(3, &[l.shared, again]);
        let whole = c.entry(4, &[l.shared, again]);
        let main = c.array(&[l.entries[0], l.entries[1], cut, whole], 4, 0);
        let shared = c.array(&[l.entries[1], whole], 2, 0);
        c.set(l.shared + D_ENTRY_ARRAY, shared);
        c.set_tail(l.shared, shared, 2);
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
    /// symlink) is listed, so its open reports it (a symlink, not followed:
    /// Difference 4); another name that can't be stat'ed adds nothing, and
    /// neither does a directory named like a journal file.
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
            &Opener::default(),
            crate::OUTPUT_DEFAULT,
            &mut errors,
        );
        assert!(files.is_empty());
        assert_eq!(
            texts(&errors),
            [format!(
                "{}: a symlink, not followed (only directories are)",
                d.join("dangling.journal").display()
            )]
        );
    }

    /// A journal file's name that is a symlink is listed whatever it points
    /// at (a FIFO, a directory), so its open reports it (Difference 4),
    /// never dropped silently; a FIFO itself isn't listed, as sd-journal's
    /// `dirent_is_journal_file` takes only regular files and symlinks.
    #[test]
    fn a_symlinked_journal_name_is_listed_whatever_it_names() {
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            d.join("fifo.journal"),
            rustix::fs::Mode::RUSR,
        )
        .expect("mkfifo");
        std::fs::create_dir(d.join("sub")).expect("dir");
        std::os::unix::fs::symlink(d.join("fifo.journal"), d.join("to-fifo.journal"))
            .expect("link");
        std::os::unix::fs::symlink(d.join("sub"), d.join("to-dir.journal")).expect("link");
        let mut out = Listing::default();
        journal_files_in(d, false, &mut out);
        assert_eq!(
            out.paths,
            [d.join("to-dir.journal"), d.join("to-fifo.journal")]
        );
        let mut errors = ReadErrors::default();
        let files = open_listed(
            || {
                let mut out = Listing::default();
                journal_files_in(d, false, &mut out);
                out
            },
            &Opener::default(),
            crate::OUTPUT_DEFAULT,
            &mut errors,
        );
        assert!(files.is_empty());
        assert!(
            errors.shown.iter().all(ReadError::is_symlink),
            "{:?}",
            texts(&errors)
        );
        assert_eq!(errors.shown.len(), 2);
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

    /// A page's messages and its two flags.
    fn paged(pg: &Paged) -> (Vec<String>, bool, bool) {
        (messages(&pg.records), pg.more_older, pg.more_newer)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    /// A forward page: the `n` entries just after the cursor, oldest
    /// first, the cursor's own entry not shown; both flags exact at either
    /// end.
    #[test]
    fn after_a_cursor_gives_the_page_after_it() {
        each_format(after_a_cursor_gives_the_page_after_it_in);
    }

    fn after_a_cursor_gives_the_page_after_it_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 6);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&u_is_x(), 10, None);
        let after =
            |j: &mut Opened, i: usize, n| j.page(&u_is_x(), n, &Position::After(all[i].cursor()));
        assert_eq!(
            paged(&after(&mut j, 1, 2)),
            (strs(&["m2", "m3"]), true, true)
        );
        assert_eq!(
            paged(&after(&mut j, 3, 10)),
            (strs(&["m4", "m5"]), true, false)
        );
        assert_eq!(
            paged(&after(&mut j, 3, 2)),
            (strs(&["m4", "m5"]), true, false)
        );
        // At the newest: nothing after, the cursor's entry before.
        assert_eq!(paged(&after(&mut j, 5, 10)), (Vec::new(), true, false));
        // Unfiltered too.
        let pg = j.page(&[], 3, &Position::After(all[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m1", "m2", "m3"]), true, true));
        // From before the first entry: nothing older.
        let mut first = all[0].cursor();
        first.seqnum = 0;
        let pg = j.page(&u_is_x(), 2, &Position::After(first));
        assert_eq!(paged(&pg), (strs(&["m0", "m1"]), false, true));
        // None asked for: still whether entries lie on each side.
        assert_eq!(paged(&after(&mut j, 2, 0)), (Vec::new(), true, true));
        assert_eq!(paged(&after(&mut j, 5, 0)), (Vec::new(), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// The tail's and a page back's flags at both ends; `n` of 0 says
    /// whether anything lies on each side.
    #[test]
    fn tail_and_before_flags_are_exact() {
        each_format(tail_and_before_flags_are_exact_in);
    }

    fn tail_and_before_flags_are_exact_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 5);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let tail = |j: &mut Opened, n| paged(&j.page(&u_is_x(), n, &Position::Tail));
        assert_eq!(tail(&mut j, 2), (strs(&["m3", "m4"]), true, false));
        assert!(!tail(&mut j, 5).1);
        assert!(!tail(&mut j, 10).1);
        assert_eq!(tail(&mut j, 0), (Vec::new(), true, false));
        let all = j.tail(&u_is_x(), 10, None);
        let before = |j: &mut Opened, i: usize, n| {
            paged(&j.page(&u_is_x(), n, &Position::Before(all[i].cursor())))
        };
        assert_eq!(before(&mut j, 2, 2), (strs(&["m0", "m1"]), false, true));
        assert_eq!(before(&mut j, 2, 1), (strs(&["m1"]), true, true));
        assert_eq!(before(&mut j, 0, 10), (Vec::new(), false, true));
        assert_eq!(before(&mut j, 3, 0), (Vec::new(), true, true));
        // Nothing matches: nothing on either side.
        let none = vec![vec![vec![b"U=nothing".to_vec()]]];
        for at in [
            Position::Tail,
            Position::Before(all[2].cursor()),
            Position::After(all[2].cursor()),
        ] {
            assert_eq!(
                paged(&j.page(&none, 10, &at)),
                (Vec::new(), false, false),
                "{at:?}"
            );
        }
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// The flags are about *matching* entries: the raw entry next to the
    /// page doesn't match, one beyond it does (and, when none does, the
    /// flag is off). `A=1` is on m0, m2, m4 and m6.
    #[test]
    fn the_flags_look_past_entries_that_do_not_match() {
        each_format(the_flags_look_past_entries_that_do_not_match_in);
    }

    fn the_flags_look_past_entries_that_do_not_match_in(format: Format) {
        let with_a: &[&[u8]] = &[b"A=1"];
        let (mut c, _e) =
            crafted::journal_of_in(format, &[with_a, &[], with_a, &[], with_a, &[], with_a]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let q = vec![vec![vec![b"A=1".to_vec()]]];
        let all = j.tail(&q, 10, None);
        assert_eq!(messages(&all), ["m0", "m2", "m4", "m6"]);
        let page = |j: &mut Opened, n, at: Position| paged(&j.page(&q, n, &at));
        assert_eq!(
            page(&mut j, 2, Position::Tail),
            (strs(&["m4", "m6"]), true, false)
        );
        let at = |i: usize| all[i].cursor();
        assert_eq!(
            page(&mut j, 1, Position::Before(at(2))),
            (strs(&["m2"]), true, true)
        );
        assert_eq!(
            page(&mut j, 1, Position::After(at(1))),
            (strs(&["m4"]), true, true)
        );
        assert_eq!(
            page(&mut j, 1, Position::After(at(2))),
            (strs(&["m6"]), true, false)
        );
        assert_eq!(
            page(&mut j, 1, Position::Before(at(1))),
            (strs(&["m0"]), false, true)
        );
        // Two terms: the walk is driven by `B=1`'s entries, of which m3
        // lacks `C=1`; m1 has both.
        let both: &[&[u8]] = &[b"B=1", b"C=1"];
        let (mut c, _e) =
            crafted::journal_of_in(format, &[&[b"C=1"], both, &[b"C=1"], &[b"B=1"], both]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let q = vec![vec![vec![b"B=1".to_vec()], vec![b"C=1".to_vec()]]];
        let pg = j.page(&q, 1, &Position::Tail);
        assert_eq!(paged(&pg), (strs(&["m4"]), true, false));
        let pg = j.page(&q, 1, &Position::Before(pg.records[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m1"]), false, true));
        let pg = j.page(&q, 1, &Position::After(pg.records[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// Of the entries the page chose but couldn't show, the output limit's
    /// left-out older ones count as beyond it, a page from there meets them
    /// again; a newest entry too large to show, and an unreadable one on a
    /// page that shows nothing, were reached and reported, and don't: the
    /// page's chosen cursors step past them (page-M1, page-M2).
    #[test]
    fn entries_chosen_but_not_shown_set_the_flags() {
        each_format(entries_chosen_but_not_shown_set_the_flags_in);
    }

    fn entries_chosen_but_not_shown_set_the_flags_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 10);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = per_record_in(format, 3) + 5;
        let pg = j.page(&[], 10, &Position::Tail);
        assert_eq!(paged(&pg), (strs(&["m7", "m8", "m9"]), true, false));
        // The left-out ones weren't reached: the oldest chosen is the
        // oldest shown, so a page back from it meets them.
        assert_eq!(pg.oldest_chosen, Some(pg.records[0].cursor()));

        let mut c = with_entries_in(format, 3, |c| vec![oversized(c, 0)]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = 1000;
        let pg = j.page(&[], 10, &Position::Tail);
        assert_eq!(paged(&pg), (strs(&["m0", "m1", "m2"]), false, false));

        let (mut c, l) = crafted::journal_in(format, 1);
        c.set(l.messages[0] + O_SIZE, 8);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        assert_eq!(
            paged(&j.page(&[], 10, &Position::Tail)),
            (Vec::new(), false, false)
        );
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
    }

    /// page-M1: a page whose chosen entries all fail to decode shows
    /// nothing, but exposes the cursors of the oldest and newest it chose;
    /// `Before` and `After` from them step past those entries.
    #[test]
    fn a_page_of_unreadable_entries_can_be_paged_past() {
        each_format(a_page_of_unreadable_entries_can_be_paged_past_in);
    }

    fn a_page_of_unreadable_entries_can_be_paged_past_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        let (t, _f) = c.write();
        let all = one(t.path()).tail(&[], 10, None);
        // m3 to m5 unreadable: their MESSAGE objects cut short.
        for i in 3..6 {
            c.set(l.messages[i] + O_SIZE, 8);
        }
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let pg = j.page(&[], 3, &Position::Tail);
        assert!(pg.records.is_empty());
        assert_eq!(
            (pg.oldest_chosen, pg.newest_chosen),
            (Some(all[3].cursor()), Some(all[5].cursor()))
        );
        let back = j.page(&[], 3, &Position::Before(pg.oldest_chosen.expect("chosen")));
        assert_eq!(paged(&back), (strs(&["m0", "m1", "m2"]), false, true));
        let on = j.page(&[], 3, &Position::After(pg.newest_chosen.expect("chosen")));
        assert_eq!(paged(&on), (Vec::new(), true, false));
        assert_eq!(on.newest_chosen, None);
        assert_eq!(j.errors.shown.len(), 3, "{:?}", texts(&j.errors));
    }

    /// page-M1 going forward: a forward page whose entries all fail to
    /// decode goes on from its `newest_chosen`.
    #[test]
    fn a_forward_page_of_unreadable_entries_can_be_paged_past() {
        each_format(a_forward_page_of_unreadable_entries_can_be_paged_past_in);
    }

    fn a_forward_page_of_unreadable_entries_can_be_paged_past_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        let (t, _f) = c.write();
        let all = one(t.path()).tail(&[], 10, None);
        for i in 1..3 {
            c.set(l.messages[i] + O_SIZE, 8);
        }
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let pg = j.page(&[], 2, &Position::After(all[0].cursor()));
        assert!(pg.records.is_empty());
        assert_eq!(
            (pg.oldest_chosen, pg.newest_chosen),
            (Some(all[1].cursor()), Some(all[2].cursor()))
        );
        let on = j.page(&[], 2, &Position::After(pg.newest_chosen.expect("chosen")));
        assert_eq!(paged(&on), (strs(&["m3", "m4"]), true, true));
    }

    /// page-M2: at a tail whose newest entry can't be shown (too large),
    /// `After(newest_chosen)` finds nothing newer, so a viewer reaches the
    /// tail and can follow.
    #[test]
    fn after_the_newest_chosen_at_a_tail_it_cant_show_is_the_end() {
        each_format(after_the_newest_chosen_at_a_tail_it_cant_show_is_the_end_in);
    }

    fn after_the_newest_chosen_at_a_tail_it_cant_show_is_the_end_in(format: Format) {
        let mut c = with_entries_in(format, 3, |c| vec![oversized(c, 0)]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        j.payload_limit = 1000;
        let pg = j.page(&[], 10, &Position::Tail);
        assert_eq!(messages(&pg.records), strs(&["m0", "m1", "m2"]));
        let newest = pg.newest_chosen.expect("chosen");
        assert_ne!(Some(newest), pg.records.last().map(Record::cursor));
        let on = j.page(&[], 10, &Position::After(newest));
        assert_eq!(paged(&on), (Vec::new(), true, false));
    }

    /// A forward page under the output limit keeps the entries next to the
    /// cursor and leaves out the newer ones, reported and flagged, so the
    /// next forward page goes on from the last one shown with no gap.
    #[test]
    fn a_forward_page_over_the_output_limit_leaves_out_the_newer_end() {
        each_format(a_forward_page_over_the_output_limit_leaves_out_the_newer_end_in);
    }

    fn a_forward_page_over_the_output_limit_leaves_out_the_newer_end_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 10);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&u_is_x(), 10, None);
        j.payload_limit = per_record_in(format, 3) + 5;
        let pg = j.page(&u_is_x(), 10, &Position::After(all[1].cursor()));
        assert_eq!(paged(&pg), (strs(&["m2", "m3", "m4"]), true, true));
        assert!(
            matches!(
                j.errors.shown[..],
                [ReadError {
                    kind: ReadErrorKind::OutputLimitNewer { left_out: 5, .. },
                    ..
                }]
            ),
            "{:?}",
            texts(&j.errors)
        );
        assert_eq!(
            texts(&j.errors),
            [format!(
                "output limit of {} bytes reached; 5 newer entries left out",
                per_record_in(format, 3) + 5
            )]
        );
        // The left-out ones weren't reached: the newest chosen is the
        // newest shown, so a page on from it meets them.
        assert_eq!(pg.newest_chosen, Some(pg.records[2].cursor()));
        let next = j.page(&u_is_x(), 10, &Position::After(pg.records[2].cursor()));
        assert_eq!(messages(&next.records), ["m5", "m6", "m7"]);
        // An oldest entry alone over the whole limit is skipped, as the
        // tail's newest is, and isn't older than the page: it was reached
        // and reported.
        let mut c = with_entries_in(format, 0, |c| {
            let m = c.data(b"MESSAGE=newer");
            vec![oversized(c, 0), vec![m]]
        });
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let mut before_all = j.tail(&[], 10, None)[0].cursor();
        before_all.seqnum = 0;
        j.payload_limit = 1000;
        let pg = j.page(&[], 10, &Position::After(before_all));
        assert_eq!(paged(&pg), (strs(&["newer"]), false, false));
        assert!(matches!(
            j.errors.shown[0].kind(),
            ReadErrorKind::TooLarge { limit: 1000, .. }
        ));
    }

    /// The held limit leaves out older entries of a tail, reported and
    /// flagged; a forward page holds as many, the newer ones reported and
    /// flagged. A forward page of fewer than the limit reports nothing.
    #[test]
    fn the_held_limit_sets_the_flags() {
        each_format(the_held_limit_sets_the_flags_in);
    }

    fn the_held_limit_sets_the_flags_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 10);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        j.held_limit = std::mem::size_of::<Candidate>() * 4;
        let pg = j.page(&[], 10, &Position::Tail);
        assert_eq!(paged(&pg), (strs(&["m6", "m7", "m8", "m9"]), true, false));
        assert_eq!(
            texts(&j.errors),
            ["limit of held entries reached; older entries left out"]
        );
        let mut j = one(t.path());
        j.held_limit = std::mem::size_of::<Candidate>() * 4;
        let pg = j.page(&[], 10, &Position::After(all[2].cursor()));
        assert_eq!(paged(&pg), (strs(&["m3", "m4", "m5", "m6"]), true, true));
        assert_eq!(
            texts(&j.errors),
            ["limit of held entries reached; newer entries left out"]
        );
        let mut j = one(t.path());
        j.held_limit = std::mem::size_of::<Candidate>() * 4;
        let pg = j.page(&[], 3, &Position::After(all[2].cursor()));
        assert_eq!(paged(&pg), (strs(&["m3", "m4", "m5"]), true, true));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A forward page reads only what its seek and its entries need: with
    /// one seqnum id the bisect lands on the cursor's own entry (m3, not
    /// shown), and an unreadable entry the bisect never tests (m0's chain
    /// neighbour m1, chain index 0) isn't read, so not reported. A page back
    /// goes past it, reported.
    #[test]
    fn a_forward_page_reads_only_what_its_seek_needs() {
        each_format(a_forward_page_reads_only_what_its_seek_needs_in);
    }

    fn a_forward_page_reads_only_what_its_seek_needs_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        c.set(l.entries[1] + O_SIZE, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&u_is_x(), 3, None);
        let mut j = one(t.path());
        let pg = j.page(&u_is_x(), 10, &Position::After(all[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m4", "m5"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
        let pg = j.page(&u_is_x(), 10, &Position::Before(all[0].cursor()));
        assert_eq!(messages(&pg.records), ["m0", "m2"]);
        assert_eq!(j.errors.shown.len(), 1, "{:?}", texts(&j.errors));
    }

    /// A value list whose in-line entry can't be read: systemd's bisect
    /// returns that error (`generic_array_bisect_for_data` tests it first)
    /// and journalctl leaves the file out. sb reports it, and the group
    /// finds nothing in the file.
    #[test]
    fn an_unreadable_in_line_entry_is_reported_going_forward() {
        each_format(an_unreadable_in_line_entry_is_reported_going_forward_in);
    }

    fn an_unreadable_in_line_entry_is_reported_going_forward_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        c.set(l.entries[0] + O_SIZE, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&u_is_x(), 3, None);
        let mut j = one(t.path());
        let pg = j.page(&u_is_x(), 10, &Position::After(all[0].cursor()));
        assert!(pg.records.is_empty());
        assert!(!j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A cursor of another seqnum id and boot is placed by realtime, by
    /// systemd 259's bisect (decision 10). Realtimes 10, 50, 20, 30, 45 and a
    /// cursor at 40: the bisect tests 45 (right of the needle), 20 (left), 30
    /// (left) and lands on 45 (journal-file.c `generic_array_bisect`), so
    /// only m4 is shown, though m1 (50) is after the cursor by realtime: an
    /// entry before the landing isn't shown going forward, and a page back
    /// shows it (`more_older`).
    #[test]
    fn a_forward_page_ordered_by_realtime_starts_at_the_landing() {
        each_format(a_forward_page_ordered_by_realtime_starts_at_the_landing_in);
    }

    fn a_forward_page_ordered_by_realtime_starts_at_the_landing_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 5);
        // Seqnums 1 to 5, boot 7.
        for (&e, real) in l.entries.iter().zip([10u64, 50, 20, 30, 45]) {
            c.set(e + 24, real);
        }
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let elsewhere = Cursor {
            seqnum_id: [9; 16],
            seqnum: 1,
            boot_id: [9; 16],
            monotonic: 1,
            realtime: 40,
            xor_hash: 0,
        };
        let pg = j.page(&[], 10, &Position::After(elsewhere));
        assert_eq!(paged(&pg), (strs(&["m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// The same boot and another seqnum id: by monotonic time, bisecting
    /// the `_BOOT_ID=` object's list (`journal_file_move_to_entry_by_monotonic`).
    /// The cursor's monotonic time is m1's (2), so the bisect lands on m1,
    /// which isn't the cursor's entry (another seqnum id): it is shown.
    /// Without a `_BOOT_ID=` object the seek falls back to realtime.
    #[test]
    fn a_forward_page_of_the_same_boot_bisects_by_monotonic_time() {
        each_format(a_forward_page_of_the_same_boot_bisects_by_monotonic_time_in);
    }

    fn a_forward_page_of_the_same_boot_bisects_by_monotonic_time_in(format: Format) {
        let boot = format!("_BOOT_ID={}", "07".repeat(16));
        let in_boot: &[&[u8]] = &[boot.as_bytes()];
        let with_a: &[&[u8]] = &[boot.as_bytes(), b"A=1"];
        let (mut c, _e) =
            crafted::journal_of_in(format, &[with_a, in_boot, with_a, in_boot, with_a]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        // Monotonic time 2 is m1's; the realtime (before every entry) would
        // land on m0, so m1 first shows the boot's list was bisected.
        let same_boot = Cursor {
            seqnum_id: [9; 16],
            seqnum: 1,
            boot_id: [7; 16],
            monotonic: 2,
            realtime: 1,
            xor_hash: 1,
        };
        let pg = j.page(&[], 10, &Position::After(same_boot));
        assert_eq!(paged(&pg), (strs(&["m1", "m2", "m3", "m4"]), true, false));
        // With a query: the boot's list lands on m1, which lacks `A=1`, so
        // the seek steps to `A=1`'s next entry (m2), which is in the boot's
        // list too.
        let q = vec![vec![vec![b"A=1".to_vec()]]];
        let pg = j.page(&q, 10, &Position::After(same_boot));
        assert_eq!(paged(&pg), (strs(&["m2", "m4"]), true, false));
        // No `_BOOT_ID=`: by realtime, which lands on m1, whose realtime is
        // the cursor's (found counts as at or after, going down).
        let (mut c, _l) = crafted::journal_in(format, 5);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let by_realtime = Cursor {
            realtime: 1_000_002,
            ..same_boot
        };
        let pg = j.page(&[], 10, &Position::After(by_realtime));
        assert_eq!(paged(&pg), (strs(&["m1", "m2", "m3", "m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// systemd 259's forward page lands where its realtime bisect lands, in
    /// a file whose clock stepped back (decision 10). Realtimes 10, 50, 5,
    /// 45 and a cursor at 40 of another seqnum id and boot: the bisect
    /// tests 45 (right of the needle), 50 (right), 10 (left) and lands on
    /// 50, which isn't the cursor's entry, so it is shown, then the file's
    /// entries after it: 50, 5, 45 (journal-file.c `generic_array_bisect`,
    /// journalctl-show.c's `sd_journal_test_cursor`).
    #[test]
    fn a_forward_page_lands_where_the_realtime_bisect_does() {
        each_format(a_forward_page_lands_where_the_realtime_bisect_does_in);
    }

    fn a_forward_page_lands_where_the_realtime_bisect_does_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 4);
        for (&e, real) in l.entries.iter().zip([10u64, 50, 5, 45]) {
            c.set(e + 24, real);
        }
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let elsewhere = Cursor {
            seqnum_id: [9; 16],
            seqnum: 1,
            boot_id: [9; 16],
            monotonic: 1,
            realtime: 40,
            xor_hash: 0,
        };
        let pg = j.page(&[], 10, &Position::After(elsewhere));
        assert_eq!(paged(&pg), (strs(&["m1", "m2", "m3"]), true, false));
        let real: Vec<u64> = pg.records.iter().map(|r| r.cursor().realtime).collect();
        assert_eq!(real, [50, 5, 45]);
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// Difference 18: journalctl 255.1 and later (24.04, 26.04) step past
    /// their landing only when it is the cursor's own entry, and so does
    /// sb; 249 (22.04) always steps past it. A cursor whose entry has
    /// vanished (here: one whose XOR hash no entry has) lands on the next
    /// entry, which is shown; so is the next match after a cursor whose
    /// entry doesn't match the query.
    #[test]
    fn difference_18_nothing_after_a_vanished_cursor_is_dropped() {
        each_format(difference_18_nothing_after_a_vanished_cursor_is_dropped_in);
    }

    fn difference_18_nothing_after_a_vanished_cursor_is_dropped_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 5);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let mut vanished = all[2].cursor();
        vanished.xor_hash ^= 1;
        let pg = j.page(&[], 10, &Position::After(vanished));
        assert_eq!(paged(&pg), (strs(&["m2", "m3", "m4"]), true, false));
        // The cursor's own entry is stepped past.
        let pg = j.page(&[], 10, &Position::After(all[2].cursor()));
        assert_eq!(messages(&pg.records), ["m3", "m4"]);
        // A cursor whose entry doesn't match: the next match is shown.
        let with_a: &[&[u8]] = &[b"A=1"];
        let (mut c, _e) = crafted::journal_of_in(format, &[with_a, &[], with_a, &[], with_a]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let unfiltered = j.tail(&[], 10, None);
        let q = vec![vec![vec![b"A=1".to_vec()]]];
        let pg = j.page(&q, 10, &Position::After(unfiltered[1].cursor()));
        assert_eq!(paged(&pg), (strs(&["m2", "m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A file the first step finds nothing in seeks again from the entry
    /// the first step took (`next_beyond_location` keeps no direction after
    /// an empty seek). Two files of one seqnum id, the second's clock behind
    /// (as a 249 rotation can leave it), and a cursor of another seqnum id
    /// and boot at realtime 150: the first file lands on its 200, the
    /// second's realtimes (50, 60) are all before the cursor, so it finds
    /// nothing; seeking again from the 200 entry, by seqnum, it lands on its
    /// first entry, and the page goes on by seqnum.
    #[test]
    fn a_file_with_no_landing_seeks_again_from_the_first_entry() {
        each_format(a_file_with_no_landing_seeks_again_from_the_first_entry_in);
    }

    fn a_file_with_no_landing_seeks_again_from_the_first_entry_in(format: Format) {
        let file = |seqnums: [u64; 2], reals: [u64; 2]| {
            let (mut c, l) = crafted::journal_in(format, 2);
            for i in 72..88 {
                c.b[i] = 0x5a; // the header's seqnum id
            }
            for ((&e, seq), real) in l.entries.iter().zip(seqnums).zip(reals) {
                c.set(e + 16, seq);
                c.set(e + 24, real);
            }
            c.write()
        };
        let (a, _f) = file([1, 2], [100, 200]);
        let (b, _g) = file([3, 4], [50, 60]);
        let mut j = Opened::new(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        let elsewhere = Cursor {
            seqnum_id: [9; 16],
            seqnum: 1,
            boot_id: [9; 16],
            monotonic: 1,
            realtime: 150,
            xor_hash: 0,
        };
        let pg = j.page(&[], 10, &Position::After(elsewhere));
        let got: Vec<(usize, u64)> = pg
            .records
            .iter()
            .map(|r| (usize::from(r.cursor().realtime > 99), r.cursor().seqnum))
            .collect();
        assert_eq!(got, [(1, 2), (0, 3), (0, 4)]);
        assert!(pg.more_older && !pg.more_newer);
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A group's landing: the highest of its terms' landings, then the next
    /// entry in every term (`next_for_match`). `B=1` is on m1, m2 and m4,
    /// `C=1` on m0, m3 and m4; at m1's cursor B lands on m1 (a match at
    /// the cursor counts) and C on m3, and the first entry with both is m4.
    #[test]
    fn a_groups_landing_is_the_next_entry_in_every_term() {
        each_format(a_groups_landing_is_the_next_entry_in_every_term_in);
    }

    fn a_groups_landing_is_the_next_entry_in_every_term_in(format: Format) {
        let with_b: &[&[u8]] = &[b"B=1"];
        let with_c: &[&[u8]] = &[b"C=1"];
        let both: &[&[u8]] = &[b"B=1", b"C=1"];
        let (mut c, _e) = crafted::journal_of_in(format, &[with_c, with_b, with_b, with_c, both]);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let q = vec![vec![vec![b"B=1".to_vec()], vec![b"C=1".to_vec()]]];
        let pg = j.page(&q, 10, &Position::After(all[1].cursor()));
        assert_eq!(paged(&pg), (strs(&["m4"]), false, false));
        // Two groups: the lower landing of the two.
        let q = vec![vec![vec![b"B=1".to_vec()]], vec![vec![b"C=1".to_vec()]]];
        let pg = j.page(&q, 10, &Position::After(all[1].cursor()));
        assert_eq!(paged(&pg), (strs(&["m2", "m3", "m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// One crafted entry's payloads.
    type Row = &'static [&'static [u8]];

    /// `c` written in place at `path` with its header counting `n`.
    fn counted(c: &Crafted, path: &Path, n: u64) {
        let mut c = c.clone();
        c.set(H_N_ENTRIES, n);
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        std::fs::write(path, &c.b).expect("write");
    }

    /// The DATA object of `payload` in `c`, found by its bytes.
    fn data_of(c: &Crafted, payload: &[u8]) -> u64 {
        let at = c.b.windows(payload.len()).position(|w| w == payload);
        at.expect("the payload") as u64 - c.format().payload_at()
    }

    /// A forward page over a file written during it (decision 13), from a
    /// cursor at m1 (seqnum 2): `A=1` on m0 and m2, `C=1` on m1, `B=1` on
    /// m3 when `b` is set. The header counts m0 to m2 at the page's first
    /// read, and m2 isn't in `A=1`'s list yet (the mirror case); `B=1`'s
    /// list holds m3, which the seam's header read counts.
    fn forward_over_a_written_file(format: Format, with_b: bool) -> Vec<String> {
        let (in_a, in_c, in_b): (Row, Row, Row) = (&[b"A=1"], &[b"C=1"], &[b"B=1"]);
        let mut rows = vec![in_a, in_c, in_a];
        if with_b {
            rows.push(in_b);
        }
        let (whole, _e) = crafted::journal_of_in(format, &rows);
        let mut early = whole.clone();
        early.set(data_of(&early, b"A=1") + D_N_ENTRIES, 1);
        let t = tempfile::NamedTempFile::new().expect("temp file");
        counted(&early, t.path(), 3);
        let all = rows.len() as u64;
        let mut j = one(t.path());
        let mut groups = vec![vec![vec![b"A=1".to_vec()]]];
        if with_b {
            groups.push(vec![vec![b"B=1".to_vec()]]);
        }
        let at = Position::After(cursor(0, 2, 7, 2, 1_000_002));
        let pg = crate::testing::seam::between_header_reads(
            move |p| counted(&whole, p, all),
            || j.page(&groups, 10, &at),
        );
        messages(&pg.records)
    }

    /// A forward page takes an entry its second header read added by the
    /// cursor's order, also when it lies before the landing the seek found:
    /// `A=1`'s list has nothing after the cursor, `B=1`'s lands on m3, and
    /// m2, re-read and matching `A=1`, is after the cursor, so it is shown
    /// first.
    #[test]
    fn a_forward_page_takes_a_reread_entry_before_its_landing() {
        each_format(|format| {
            assert_eq!(forward_over_a_written_file(format, true), ["m2", "m3"]);
        });
    }

    /// The landing itself can be an entry the second header read added:
    /// no list has anything after the cursor, and the re-read m2 is after
    /// it.
    #[test]
    fn a_forward_pages_landing_can_be_a_reread_entry() {
        each_format(|format| {
            assert_eq!(forward_over_a_written_file(format, false), ["m2"]);
        });
    }

    /// A zero slot in a value's list (a crash before journald wrote it)
    /// doesn't hide the list's other entries going forward: the walk's
    /// start is found around it, and the zero is skipped (Difference 8).
    /// `U=x`'s chain is m1 to m5; m5's slot is zeroed. m5 isn't shown: the
    /// page's second header read (decision 13) finds it carrying `U=x` but
    /// not in the list, as journalctl's view doesn't hold it either.
    #[test]
    fn a_zero_slot_hides_nothing_going_forward() {
        each_format(a_zero_slot_hides_nothing_going_forward_in);
    }

    fn a_zero_slot_hides_nothing_going_forward_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        let shared_array = c.get(l.shared + D_ENTRY_ARRAY);
        c.set_array_item(shared_array, 4, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let pg = j.page(&u_is_x(), 10, &Position::After(all[2].cursor()));
        assert_eq!(paged(&pg), (strs(&["m3", "m4"]), true, false));
        let pg = j.page(&u_is_x(), 1, &Position::After(all[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m1"]), true, true));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A zero slot in the middle of a value's list: the bisect for the
    /// walk's start is cut short there and finds nothing, so the list is
    /// read from its head, and the entries after the zero are shown. `U=x`'s
    /// chain is m1, m2, 0, m4, m5 (m3's slot zeroed).
    #[test]
    fn a_zero_slot_mid_list_hides_nothing_going_forward() {
        each_format(a_zero_slot_mid_list_hides_nothing_going_forward_in);
    }

    fn a_zero_slot_mid_list_hides_nothing_going_forward_in(format: Format) {
        let (mut c, l) = crafted::journal_in(format, 6);
        let shared_array = c.get(l.shared + D_ENTRY_ARRAY);
        c.set_array_item(shared_array, 2, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let pg = j.page(&u_is_x(), 10, &Position::After(all[2].cursor()));
        assert_eq!(paged(&pg), (strs(&["m4", "m5"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
        // An array whose first slot is zero can't be landed in: systemd's
        // bisect takes the array's first item when it finds an entry
        // (`found:` in `generic_array_bisect`) and fails with -EBADMSG on a
        // 0, so journalctl leaves the file out. sb reports it.
        let (mut c, l) = crafted::journal_in(format, 6);
        c.set_array_item(l.main_array, 0, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let pg = j.page(&[], 10, &Position::After(all[2].cursor()));
        assert!(pg.records.is_empty());
        assert!(
            texts(&j.errors).iter().any(|e| e.ends_with("lists 0")),
            "{:?}",
            texts(&j.errors)
        );
    }

    /// The walk's start can't fail on a zero either: with `U=x`'s chain
    /// [0, m2, m3, m4] (m1's slot zeroed) and the landing from `B=1` (m2),
    /// the walk's bisect for `U=x` lands in the array whose first item is
    /// the zero, which systemd's `found:` refuses; the walk reads the list
    /// from its head instead, and m3 and m4 are shown.
    #[test]
    fn a_zero_first_slot_hides_nothing_from_the_walk() {
        each_format(a_zero_first_slot_hides_nothing_from_the_walk_in);
    }

    fn a_zero_first_slot_hides_nothing_from_the_walk_in(format: Format) {
        let with_u: &[&[u8]] = &[b"U=x"];
        let both: &[&[u8]] = &[b"U=x", b"B=1"];
        let (mut c, entries) =
            crafted::journal_of_in(format, &[with_u, with_u, both, with_u, with_u]);
        let shared =
            c.b.windows(3).position(|w| w == b"U=x").expect("U=x") as u64 - format.payload_at();
        let chain = c.get(shared + D_ENTRY_ARRAY);
        assert_eq!(c.array_item(chain, 0), entries[1]);
        c.set_array_item(chain, 0, 0);
        let (t, _f) = c.write();
        let mut j = one(t.path());
        let all = j.tail(&[], 10, None);
        let q = vec![vec![vec![b"U=x".to_vec()]], vec![vec![b"B=1".to_vec()]]];
        let pg = j.page(&q, 10, &Position::After(all[1].cursor()));
        assert_eq!(paged(&pg), (strs(&["m2", "m3", "m4"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A flushed copy in a second file is shown once going forward too,
    /// from the earlier file.
    #[test]
    fn a_forward_page_shows_a_flushed_copy_once() {
        each_format(a_forward_page_shows_a_flushed_copy_once_in);
    }

    fn a_forward_page_shows_a_flushed_copy_once_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 4);
        let (run, _f) = c.write();
        let (var, _g) = c.write();
        let mut j = Opened::new(&[run.path().to_path_buf(), var.path().to_path_buf()]);
        let all = j.tail(&u_is_x(), 10, None);
        assert_eq!(messages(&all), ["m0", "m1", "m2", "m3"]);
        let pg = j.page(&u_is_x(), 10, &Position::After(all[0].cursor()));
        assert_eq!(paged(&pg), (strs(&["m1", "m2", "m3"]), true, false));
        assert!(j.errors.is_empty(), "{:?}", texts(&j.errors));
    }

    /// A page's end: each file's inode and count, and the newest entry
    /// across them, unfiltered, the earlier file's on a tie, with its
    /// directory; a file with no entries adds no newest entry; one whose
    /// newest can't be read adds its error and gives the next readable one.
    #[test]
    fn the_end_names_the_files_and_the_newest_entry() {
        each_format(the_end_names_the_files_and_the_newest_entry_in);
    }

    fn the_end_names_the_files_and_the_newest_entry_in(format: Format) {
        // Two copies of one journal in two directories, as a flush leaves
        // /run's entries in /var: the tie goes to the earlier file's.
        let (mut c, _l) = crafted::journal_in(format, 3);
        let (run, var) = (
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
        );
        let first = run.path().join("system.journal");
        let second = var.path().join("system.journal");
        std::fs::copy(c.file().path(), &first).expect("write");
        std::fs::copy(c.file().path(), &second).expect("write");
        let (empty, _h) = Crafted::new_in(format).write();
        let j = Opened::new(&[empty.path().to_path_buf(), first.clone(), second.clone()]);
        let mut errors = ReadErrors::default();
        let e = end_of(&j.files, &mut errors);
        assert!(errors.is_empty(), "{:?}", texts(&errors));
        let counts: Vec<u64> = e.files.iter().map(|f| f.n_entries).collect();
        assert_eq!(counts, [0, 3, 3]);
        assert_eq!(e.files[1].inode, j.files[1].inode);
        let (newest, dir) = e.newest.expect("an entry");
        assert_eq!(newest.seqnum, 3);
        assert_eq!(dir.as_deref(), Some(run.path()));
        let swapped = Opened::new(&[second.clone(), first.clone()]);
        let e2 = end_of(&swapped.files, &mut errors);
        assert_eq!(e2.newest.expect("an entry").1.as_deref(), Some(var.path()));
        // The newer of two files, whichever is first; seqnum 3 against 4.
        let (mut c, l) = crafted::journal_in(format, 4);
        c.set(l.entries[3] + 16, 4);
        let (newer, _n) = c.write();
        let j = Opened::new(&[first.clone(), newer.path().to_path_buf()]);
        let e = end_of(&j.files, &mut errors);
        assert_eq!(e.newest.expect("an entry").0.seqnum, 4);
        // An unreadable newest entry: reported, and the next readable one
        // is the newest, as systemd's `generic_array_get` going up skips a
        // bad entry (v259 journal-file.c); none readable, no entry.
        let (mut c, l) = crafted::journal_in(format, 2);
        c.set(l.entries[1] + O_SIZE, 0);
        let (bad, _b) = c.clone().write();
        let j = Opened::new(&[bad.path().to_path_buf()]);
        let e = end_of(&j.files, &mut errors);
        assert_eq!(e.files.len(), 1);
        assert_eq!(e.newest.map(|(c, _)| c.seqnum), Some(1));
        assert_eq!(errors.shown.len(), 1, "{:?}", texts(&errors));
        c.set(l.entries[0] + O_SIZE, 0);
        let (none, _n) = c.write();
        let j = Opened::new(&[none.path().to_path_buf()]);
        let mut errors = ReadErrors::default();
        let e = end_of(&j.files, &mut errors);
        assert_eq!((e.files.len(), e.newest), (1, None));
        assert_eq!(errors.shown.len(), 2, "{:?}", texts(&errors));
    }

    /// The position a page is read at goes through to the walk.
    #[test]
    fn a_page_reads_at_its_position() {
        each_format(a_page_reads_at_its_position_in);
    }

    fn a_page_reads_at_its_position_in(format: Format) {
        let (mut c, _l) = crafted::journal_in(format, 5);
        let (t, _f) = c.write();
        let journal = crate::Journal::open(
            crate::Source::Files(vec![t.path().to_path_buf()]),
            &crate::Release::parse("26.04").expect("a release"),
        )
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
