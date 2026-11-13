# Spec: `sb-journal` (approved 2026-11-09)

`sb-journal` is a crate of its own, wrapped by sb-host (the host spec's
decision 6). It reads systemd's journal files in pure Rust for
`sb logs`:
- a unit's newest entries;
- paging back and forward;
- following.

It replaces sb-go's `journalctl -o json --cursor` polling (cmd/logs.go)
and proto-logs' `journalctl -f` child. The design and its evidence come
from the proto-journal prototype: its findings (adopted with changes,
maintainer 2026-11-08, including the security audit and the feature
research), now [docs/prototypes/proto-journal.md](../prototypes/proto-journal.md),
and its code, which this crate superseded (removed in the plan's Task 12;
in git history). Verified systemd
behaviour is in docs/platform-notes/systemd-journal.md.

## Assumptions

1. **The reader is ported from proto-journal's tested code, not
   rewritten:**
   - file.rs;
   - the newest-first walk (`EntriesRev`, `Walk`, `newest`);
   - `Follow` (query.rs);
   - the follow loop's watching and event handling (main.rs `watch`,
     `watch_dirs`, `classify`, rescans), which moves into the crate;
   - the limits, errors and per-release rules.

   Its 112 tests and its harnesses come along: the container scripts
   with checks.sh, the fuzz harness and the crafted-file builder.
2. **Supported releases only.** sb refuses other releases (sb's startup
   check, `sb_host::check_support`), so the reader knows exactly the
   formats and `-u` rules of 22.04 (systemd 249), 24.04 (255) and 26.04
   (259.x). A new LTS is a
   `RELEASES` row. Files left by 18.04 and 20.04 on an upgraded host
   read too (compat.sh). sb-host builds the `Release` from
   `ubuntu_release()`. A test in sb-host asserts that `RELEASES` and
   `SUPPORTED_RUNTIME_RELEASES` name the same releases.
3. **Blocking I/O, no tokio.**
   - `page` and the follow's drain do preads and decompression, up to
     the output limit per call. Callers run them under
     `spawn_blocking` (capability map, decision 8).
   - The follow's inotify descriptor is created non-blocking. A
     duplicate of it (`wake_fd`) is what the caller waits on, through
     sb-host's `journal::FollowWait` (decision 12), so the follow itself
     can move into `spawn_blocking`.
   - The crate spawns no thread or child.
   - Public types are `Send`, asserted by a test.
4. **Exactness is checked against journalctl** of each release, value
   for value in `-o json --all` form. Every place sb differs is listed
   under Differences and pinned by a test.
5. **Records come out raw.** The crate hands out journal bytes as they
   are; turning them into terminal text is one type, `DisplayText`,
   under decision 2. Error messages are escaped on their own (audit L5).

## Objective

`sb logs` reads a service's log the way an operator needs it on a
Saltbox server:
- the newest entries at once;
- older and newer pages on demand;
- new lines as they arrive;
- everything a viewer needs to give pasteable text for support
  requests. Saltbox's support page asks for complete `sb logs` output,
  which sb-go's full-screen viewer can't give. How the viewer offers it
  is the tui spec's.

The first consumer is the unified log picker and viewer (decision 8).
Both kinds of source sit behind one log-source interface: last N, a page
back, a page forward and follow, each line with its time, source name,
message bytes and an opaque position. sb-journal (through sb-host)
implements it with cursors; sb-docker implements it with bollard's logs,
with timestamps as positions. The viewer and its inline fallback consume
only that interface. This crate supplies what the journal side needs;
the interface itself is defined with the tui spec.

It does this without a child process, exactly as journalctl would show
the same entries, and bounded in memory and time on any file content.

### API (first slice)

The shape, not final names. The plan settles names with the
api-and-interface-design skill and the Pragmatic Rust Guidelines:
- newtypes for cursors, ids and names;
- thiserror-derived errors, the workspace's convention (proto-journal's
  hand-written `OverLimit` and `KnownBad` become derives);
- `Debug` everywhere;
- `#[non_exhaustive]` on public enums.

```rust
pub enum Source { Local, Directory(PathBuf), Files(Vec<PathBuf>) }

pub struct Query { /* journalctl's model: an OR of groups, each an AND of terms, each an OR of values */ }
impl Query {
    /// journalctl's `-u` for this release. `coredump_uid` is the
    /// systemd-coredump user's uid if it resolves (sb-host's lookup);
    /// the coredump term follows the table below.
    pub fn unit(release: &Release, unit: &UnitName, coredump_uid: Option<Uid>, source: &Source) -> Self;
    /// FIELD=value. Repeating the same field with `or` matches either
    /// value, as sd-journal ORs repeated matches on one field.
    pub fn field(name: &FieldName, value: &[u8]) -> Self;
    /// Every group of `self` ANDed with every group of `other`: a cross
    /// product, refused past 64 groups.
    pub fn and(self, other: Query) -> Result<Self, QueryError>;
    pub fn or(self, other: Query) -> Self;    // the groups of both
}

pub enum Position { Tail, Before(Cursor), After(Cursor) }

pub struct Page {
    pub records: Vec<Record>,
    /// Whether a matching entry lies beyond the entries the page chose and
    /// reached, older or newer. One the output limit left out counts; one
    /// reached but not shown (unreadable, or alone over the limit; in
    /// `errors`) doesn't.
    pub more_older: bool,
    pub more_newer: bool,
    /// The oldest and newest entries the page chose and reached, shown or
    /// not; `None` when it chose none (decision 10 of the hardening plan).
    /// `Before(oldest_chosen)` and `After(newest_chosen)` page past
    /// entries that can't be shown: a page of them all still has a way
    /// on, and a tail whose newest entry can't be shown isn't
    /// `more_newer` (page-M1, page-M2).
    pub oldest_chosen: Option<Cursor>,
    pub newest_chosen: Option<Cursor>,
    /// Where the page ended, even when it matched nothing: each file's
    /// entry count and the newest entry examined (see "Starting a
    /// follow"). Opaque.
    pub end: PageEnd,
    /// Read errors (ten distinct, then a count) and open errors (one per
    /// file, outside that cap).
    pub errors: ReadErrors,
}

pub struct Journal { /* the source; no descriptors held between calls */ }
impl Journal {
    /// Checks the source (machine id, directories); an `OpenError` is a
    /// source that can't be read at all. Files that fail to open on a
    /// later call are errors of that call, not an `OpenError`. Pages
    /// order boots as `release`'s journalctl does (decision 11).
    pub fn open(source: Source, release: &Release) -> Result<Self, OpenError>;
    /// The `n` entries at `at`. Every call lists and reopens the files.
    pub fn page(&self, q: &Query, at: &Position, n: usize, limits: &Limits) -> Page;
    /// A follow continuing from where `from` ended, with what was written
    /// since as its first `Batch` (see "Starting a follow").
    pub fn follow(&self, q: Query, from: &PageEnd, limits: &Limits)
        -> Result<(Follower, Batch), OpenError>;
}

pub struct Follower { /* files kept open by inode, a position per file, a non-blocking inotify fd */ }
impl Follower {
    /// Everything new since the last call. Reads inotify until EAGAIN,
    /// then the files.
    pub fn drain(&mut self, limits: &Limits) -> Batch;
    /// A duplicate (CLOEXEC) of the inotify fd for the caller's
    /// readiness wait, so the Follower itself can move into
    /// `spawn_blocking` for `drain`.
    pub fn wake_fd(&self) -> io::Result<OwnedFd>;
}
/// `more`: the output limit stopped this batch; drain again before
/// waiting.
pub struct Batch { pub records: Vec<Record>, pub more: bool, pub errors: ReadErrors }
pub struct ReadErrors { pub shown: Vec<ReadError>, pub omitted: usize } // ten distinct, then a count

pub struct Record { /* location, its file's directory, fields in item order */ }
impl Record {
    pub fn cursor(&self) -> Cursor;
    pub fn realtime(&self) -> Option<jiff::Timestamp>; // None if out of jiff's range
    pub fn boot_id(&self) -> BootId;
    pub fn message(&self) -> Option<&[u8]>;
    pub fn field(&self, name: &FieldName) -> Option<&[u8]>;
    pub fn fields(&self) -> impl Iterator<Item = (&[u8], &[u8])>; // raw names and values
    pub fn priority(&self) -> Option<Priority>;
}

/// The output limit: 256 MiB by default, never above the format's own
/// DATA_SIZE_MAX, built through a constructor that caps it.
pub struct Limits { /* private */ }

// In sb-term-ui (decision 5), not this crate:
pub struct DisplayText(/* sanitized */);
impl DisplayText { pub fn from_bytes(bytes: &[u8], colour: bool) -> Self; }
```

sb-host re-exports these types (`pub use`, decision 6), adding only host
glue: the release, the coredump uid, and `spawn_blocking` helpers.

`UnitName` mangles as journalctl does for Saltbox's names: a name
without a dot gets `.service`, and a `.service` name passes unchanged.
Other mangling (globs, `.slice`) is refused, not guessed. `Release`
carries the per-release `-u` rule, its order between boots (`BootRule`,
decision 11) and its `-o json` shape (`prints_seqnum`);
the latter is used only by the test tools, behind the `testing` feature.
sb-host adds tokio for its `spawn_blocking` helpers: a host spec
correction.

**The coredump term** of `Query::unit` (decisions 1 and 4):

| Source | systemd-coredump uid | Term |
|---|---|---|
| Local | resolves to a uid other than 0 | `_UID=` that uid, or `_UID=0` (journalctl's 259 rule) |
| Local | doesn't resolve, or resolves to 0 | `_UID=0` (journalctl: none) |
| Directory, Files | any | `_UID=0` (journalctl: none; a local uid may not apply to foreign files) |

On 249 and 255 the term is `_UID=0` for every source, as journalctl's.

**Starting a follow.** A `Page` records where it ended, as its `end`.
That covers each file it read, by inode and file id, with the entry
count it read up to, and the newest entry its first header reads gave,
unfiltered, with that entry's directory. It exists even when the page
matched nothing. The count is the header's for an unfiltered page or a
sealed file. A filtered page reads each other file's header again after
its lookups and examines the entries that read added and the newest one
before it. It takes those its walk can show, held by then in their
driving term's lists, and holds the second count, or the index of a
newest entry not linked yet. An older entry not in its lists, which
journald moved on from, is examined and not shown (decision 13). Every
entry below the count was examined by the page. The end also carries
each entry the page showed past its newest entry (written during the
page) by what a flush copy keeps of it: boot id, monotonic and realtime
stamps and XOR hash (docs/platform-notes/systemd-journal.md, "Flush").
It doesn't raise its newest entry to them, which would make a new
file's walk back pass over the copy of an older entry the page didn't
show.

`Journal::follow(from)` then:
1. adds the watches;
2. opens the files;
3. reads each file from the count `from` recorded for it, found by its
   inode and file id: an archived file renamed by a rotation keeps its
   position. A file `from` didn't see (new since the page: the new
   system.journal of a rotation, a /var file a flush created, one the
   page couldn't open) is walked back with `EntriesRev` to the newest
   entry at or below `from`'s newest entry. `from`'s directory decides
   between sd-journal's order and the flush-copy rule, as the
   prototype's `seen` does: that stop test is monotone within a file.
   From such a file, and from one a later rescan finds, the follow drops
   one copy of each entry `from` carries as shown past its newest, so a
   flush to a new /var file between the page and the follow shows
   nothing twice (the hardening plan's decision 26). Copies a flush
   appends to a /var file the page read are shown again (Difference 13).
4. merges what it read through the same merge as `drain`, so an entry
   written to /run after the page and flushed to /var before the follow
   shows once;
5. returns it as the first `Batch`, under the per-file output limit
   (`more` set if it stopped there).

**The limit that remains.** A page reads its files' headers at different
instants, so `from`'s newest entry can be newer than an entry appended to
another file after the page read that file's header, which the page
didn't read. The follow delivers that entry from the file's count. It is
lost only if the file is gone before the follow opens it: a flush between
the two removes /run, and the entry's copy in the new /var file lies
before `from`'s newest entry, where that file's walk back stops. A flush
happens once per boot, or when someone runs `journalctl --relinquish-var`
then `journalctl --flush` by hand. Closing it would take one call that
pages and follows with the files held open between, which sb doesn't
have. Where the page read the /var file, the flush's copies are appended
to it and shown again instead (Difference 13).

Each file's position is then the entry count it read to. No write
between the page and the follow is lost, even without an event, and
even after an empty page. The prototype's `check_until` window is kept
only for files found by a later rescan.

A crafted test and a follow.sh scenario write between a page and its
follow, also after an empty page: 0 lost, 0 doubled. Crafted tests also
write during a filtered page, through a seam between its two header
reads (an entry linked before the second read, two lists read at
different moments, the newest counted entry not yet listed, a value
first written during the page, forward pages landing around such
entries). A follow.sh scenario takes filtered pages while a writer runs
and follows from each: every entry once.

A source with no directory to watch (neither local root, or no given
file's directory) is refused, `OpenError::NothingToWatch`: no event could
make its wake fd readable (sb-journal plan, Task 7).

**Waiting for new entries** (decision 12): sb-host's
`journal::FollowWait`, built from the `Follower`'s `wake_fd()`, waits
with rustix `poll` on the wake fd and a stop eventfd of its own, inside
`spawn_blocking`.
1. The caller awaits `FollowWait::wait`: `Woken` once the wake fd is
   readable, `Stopped` once its `FollowStop` handle stops it or the
   waiter is dropped (the viewer closed), which ends the follow.
2. It runs `drain` under `spawn_blocking` (sb-host's `journal::drain`);
   `drain` reads the inotify fd to EAGAIN. The duplicate shares the open
   file description, so that empties what the wait polls.
3. While a `Batch` has `more`, it drains again.
4. After a batch with errors it drains once more: reading the events may
   have failed before EAGAIN.
5. It waits again. `poll` is level-triggered, so the wake fd reads
   readable for as long as an event is queued, and there is no readiness
   to clear (the `AsyncFd` protocol's `clear_ready` step is gone).

The wait is cancel-safe (a wait dropped in a `select!` leaves its poll
running for the next wait to take over, one blocking thread per waiter);
each `drain` is awaited to its end. A stop is for good, and wins over a
queued event, also over a cancelled wait's poll woken before the stop.
Each waiter holds two descriptors: the wake fd's duplicate and the stop
eventfd; it is dropped with its follower. A wake fd whose events every
drain failed to read would wake the loop at once each time, with the
drain's error to show; inotify's events fit the drain's buffer, so this
isn't expected.

The `Follower` and its wake fd are `Send`.

### Behaviour: proven in proto-journal, ported

- **Files:**
  - The local machine's `/run/log/journal/<machine-id>` and
    `/var/log/journal/<machine-id>`, in sd-journal's search order.
  - `.journal~` files are included.
  - A page lists the files again after opening them, so a rotation in
    between isn't lost.
- **Format:**
  - Regular and compact files.
  - Keyed (siphash24) and Jenkins hashes.
  - xz, lz4 and zstd payloads; zstd needs its content size and is
    checked against it.
  - An unknown incompatible flag is an error naming the file.
  - Every offset and size is checked. Links only point forward, and
    entry arrays never decrease (a repeat is skipped).
  - Corruption is reported per file and per entry, and what is readable
    around it is shown.
- **The unit rule:** journalctl's `-u` per release
  (docs/platform-notes/systemd-journal.md), with decision 1 for the
  coredump branch.
- **Order:**
  - sd-journal's location comparison.
  - The tail walks backwards and takes the last N, as journalctl's `-n`
    does.
  - Following merges forward.
  - Flushed copies (/run against /var) show once.
  - journald 249's rotation double-writes show twice, as journalctl
    shows them.
- **Paging back:** `Before(cursor)` gives the page before it, matched in
  the matrix against journalctl.
- **Follow:**
  - Files are kept open by inode, with a position per file.
  - Each wake costs one header read per active file plus the new
    entries.
  - Rotation, vacuum, renames, archive rewrites, a journald restart and
    an inotify overflow are handled.
  - follow.sh: 0 lost and 0 doubled in eleven scenarios per release
    (nine beside `journalctl -f`, two sb-journal only: a follow from a
    page's end).
- **Limits:**
  - The output limit holds per page, and per file per wake when
    following.
  - An oversized entry read first is skipped and reported; one met later
    ends the page there, reported, with no gap.
  - Decompression is bounded by size, window and ratio, and so are the
    hash chains and the work per entry.
- **Errors:** at most ten distinct ones per query, then a count. Open
  errors are one per file.

### Behaviour: new in the crate, to be tested

- **`After(cursor)` and exact `more_older`/`more_newer`.** The prototype
  pages only backwards. `After` needs a forward match walk (or a bounded
  backward walk down to the cursor), with a matrix check beside "one
  page back". The flags replace sb-go's "at least 499" guess.
- **Safe opening (audit L2):**
  - `O_NONBLOCK|O_NOCTTY|O_CLOEXEC`;
  - an `fstat` check for a regular file, so a FIFO can't block;
  - a journal *file* that is a symlink isn't followed (`O_NOFOLLOW` on
    the last component only), and is reported as an open error (ELOOP),
    never dropped silently. sd-journal does follow it: v249
    sd-journal.c accepts `DT_LNK` entries (`dirent_is_journal_file`)
    and opens them with a plain `open`. That makes Difference 4.
  - directories are followed as they are, so a `/var/log` on another
    disk through a symlink still reads.
- **`/etc/machine-id` validated** as 32 hex digits in either case, not
  all zero; lowercased, as journald names the directory. A `MachineId`
  newtype (audit L1).
- **Typed `ReadError { path, kind }`** whose `Display` escapes paths and
  field names (audit L5).
- **`DisplayText`** (decision 2).
- **The follow API:** starting from a page's `end`, `Batch::more`, and
  the wake protocol, all above.
- **`ReadErrors`:** ten distinct errors, then an omitted count.
- **The coredump rule of decisions 1 and 4,** for every source.
- **The fuzz harness enforcing its own bounds.** Today it fails only on
  a panic or a hang. It also fails:
  - a case over the memory bound;
  - an unreadable peak or a failed `clear_refs` write;
  - a run with fewer than 9,000 cases or 8 seeds.

### Display (decision 2)

`DisplayText::from_bytes` is the only way from journal bytes to the
screen, for values and field names alike:
- invalid UTF-8 is shown lossily;
- newline splits lines, and tab expands;
- carriage return is the one C0 control applied rather than dropped,
  by term-ui's existing rule for progress lines: trailing ones are
  ignored, then the text after the last one is kept;
- every escape sequence other than SGR (colour) is dropped, payloads
  included: OSC (titles, OSC 8 links, OSC 52 clipboard), DCS, APC, PM,
  SOS, cursor and mode CSIs, and terminal queries;
- other C0 controls, DEL and C1 (8-bit CSI, OSC, DCS, APC, PM and SOS
  included, with their string payloads) are dropped;
- every SGR is kept when colour is on, and dropped when it is off.
  Decision 2 knowingly keeps SGR 8 (conceal) and blink. Colour is off
  under term-ui's ascii profile and whenever term-ui keeps no codes
  (`--color=never`, NO_COLOR);
- bidi and zero-width characters pass through unmarked.

A CSI counts as SGR only when it matches `ESC [ [0-9;:]* m`. Tests cover
each class, 7-bit and 8-bit.

**The parser** is `anstyle-parse` 1.0.0, already in sb's tree through
anstream: https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/. It runs
the DEC state machine. `ESC X`, `ESC ^` and `ESC _` enter its
`SosPmApcString` state, which ignores everything up to ST
(src/state/codegen.rs at 1.0.0), and OSC and DCS have their own string
states. console's `AnsiCodeIterator`, which term-ui's `clean` used
before `DisplayText` replaced it, recognises only CSI, OSC and DCS.

**The 8-bit C1 forms are ours.** anstyle-parse and vte both document
"only supports 7-bit codes", and anstyle-parse's ground table gives
0x90, 0x9B and 0x9D to 0x9F no transition. So a pre-pass maps the
code points U+0090, U+0098, U+009B, U+009D, U+009E and U+009F to their
`ESC` forms, so the parser drops their payloads, and drops the rest of
U+0080 to U+009F. Raw 0x80 to 0x9F bytes become U+FFFD first, through
lossy decoding. Why not the library: both parsers are 7-bit only.

The sanitizer lives in sb-term-ui and is shared (decision 5). It
replaced term-ui's private `clean` with the public `DisplayText`, which
takes bytes and follows this policy; term-ui's spec is amended
("Escape handling"). The runner and the log viewer then use one
sanitizer, and `DisplayText` is term-ui's type. sb-journal itself hands
out only raw bytes. Viewer notices are a channel variant, never an
in-band prefix; proto-logs' `\u{1}` marker is not carried over.

### Differences from journalctl (the complete list, each pinned by a test)

Deliberate:
1. **26.04's coredump rule is stricter** (decisions 1 and 4; the table
   under API). When the systemd-coredump user doesn't resolve or
   resolves to 0, and for files given with `--file`/`-D` (where
   journalctl drops the uid term), coredump entries still need
   `_UID=0`. The 26.04 matrix expects this:
   its uid-1000 coredump decoy no longer matches.
2. **The follow loses and doubles nothing across a flush.** `journalctl
   -f` 255 prints flushed entries twice when `/var/log/journal` exists
   before it starts. 259 loses those written after a mid-follow `/var`
   creation (follow.sh).
3. **No JSON and no 4096-byte `null`:** raw bytes and `DisplayText`.
4. **A symlinked journal file isn't followed,** and is reported as an
   open error; sd-journal follows it. Directories are followed as
   sd-journal follows them.

Limits by design:

5. **An entry over the output limit** isn't shown, but reported; nor
   is a zstd frame needing a window over 128 MiB. An entry listing more
   items than the limit could show (each field shown spends some of it)
   is refused before its body is read, wherever a page or a follow reads
   an entry, so one entry costs no more memory than the limit allows
   (security audit, Task 11). Seeks and the boot table then take it as
   unreadable: a bisect stops at it, and a file's tail is the newest
   entry before it. journald writes at most 1024 fields an entry
   (`ENTRY_FIELD_COUNT_MAX`; UNVERIFIED here, from the audit), so only a
   limit under about 24 KiB, or a crafted file, meets this. Such an
   entry at a page's edge, or an unreadable one, doesn't count as
   beyond the page (`more_older`/`more_newer`), where journalctl, which
   shows it, has an entry there: the page's `oldest_chosen` and
   `newest_chosen` page past it (page-M1, page-M2).
6. **xz decoding has a 64 MiB decoder-memory bound** (journald's xz
   dictionary is 1 MiB).
7. **Unit names:** only "no dot → `.service`" mangling; other forms are
   refused.

Only on corrupt, live or foreign files:

8. **A zero entry-array item is skipped.** sd-journal's match path cuts
   the array short or drops the file.
9. **An invalid data hash table refuses lookups, not the whole file.**
   sd-journal refuses the file.
10. **A stale item naming a real older entry** loses the rest of its
    array (reported, never shown wrong).
11. **The file list is read again after opening.** journalctl lists
    once, so a rotation in between loses entries in journalctl but not
    here.
12. **An entry journald was cut off from linking into its values'
    lists** is shown by the follow, for entries it reads, which it matches
    by their items; a filtered journalctl, and a filtered page, find
    entries through those lists (a page through its driving term's lists)
    and don't show it (an unfiltered view shows every counted entry).
    Where journald writes the entry again (a link that failed), the
    follow shows both copies and journalctl and the page one; where it
    never links it (journald died mid-link), the follow shows it and
    journalctl doesn't. stress.sh allows exactly the first.
13. **Flush copies appended to a /var file already being followed** are
    shown again (rare; declined in the prototype).
14. **Field names aren't refused** for failing journalctl's validity
    rule; they are shown, sanitized.
15. **journalctl's second forward walk** after `-n` (non-transitive
    orders) isn't modelled; the backward walk's result is shown. The
    matrix never hit such a case.
16. **A lookup that fails while following** is reported once per file,
    and entries with that value don't match until a later wake's lookup
    succeeds.
17. **A plausible item out of order** (a stale slot) stops a followed
    file, retried once, then reported, and nothing more is read from
    that file while it stays so. The tail's walk goes past it.
18. **The entry after a vanished or non-matching cursor isn't lost**
    (22.04 only). 22.04's `journalctl --after-cursor` seeks and then
    always steps once past where it landed (v249 journalctl.c,
    `sd_journal_next_skip(j, 1 + after_cursor)`), so when the cursor's
    entry has vanished (vacuumed, rotated away) or doesn't match the
    query, the first real entry after it is lost. sb steps past the
    landing only when it is the cursor's own entry, as 24.04's and
    26.04's journalctl do (`sd_journal_test_cursor`). 24.04 has it
    although the v255 tag doesn't: systemd-stable v255.1 cherry-picked
    "journalctl: don't skip over messages not matching the cursor"
    (systemd-stable d68d9019c483, from upstream 4207a5577a37, issue
    #30288), and Ubuntu's 255.4 is built on that stable release; it is
    in the upstream tarball, not a Debian patch, so the package changelog
    doesn't mention it. On a 249-written file whose realtime steps back,
    22.04's landing can differ too: sb lands where 259's bisect does
    (24.04 and 26.04 land there as well), and 249's bisect is built
    differently (v249 journal-file.c `generic_array_bisect`). The
    matrix's runs: docs/platform-notes/systemd-journal.md.
19. **A forward page on a corrupt file goes on where journalctl stops**
    (crates/journal/src/forward.rs lists the departures):
    - an error in the seek or the walk is reported and the page goes on
      with the other groups and files, where journalctl drops the file;
    - that includes a zero first item in the array a walk resumes in
      (as Difference 8);
    - systemd's `check_object` and sb's object checks don't refuse
      quite the same corrupt objects, so a bisect can be cut short in
      one and not the other;
    - a forward page whose seek bisects onto a damaged entry-array item
      (one naming a DATA object, not an entry), as one from a cursor
      just past the item does, shows nothing from that file on every
      release, as journalctl 249, 255 and 259 show nothing after such a
      cursor; sb reports the damage ("object ... is type 1, wanted 3"),
      journalctl says nothing. In both layouts:
      `a_forward_page_past_a_damaged_item_shows_nothing_and_reports_it`
      (walk.rs);
    - on such a file sb's tail can show more than journalctl's `-n`: it
      walks back past the damaged item and reports it. On a 300-entry
      file written by 249 and damaged so, `-n 10` and `-n 100` showed
      as many entries as sb on all three releases, while `-n 500`
      showed none and sb's tail 286, with two errors reported
      (docs/platform-notes/systemd-journal.md, "A damaged entry-array
      item").
20. **Forward pages over two boots whose clocks overlap follow sb's
    order** (24.04 and 26.04, decision 11). journalctl 255 and 259 seek
    each file by one key but merge by the boot table, so where a boot's
    realtime range overlaps another's (a clock set back across a
    reboot) `--after-cursor` passes entries the table puts after the
    cursor, and on 24.04 a page after an entry can show that entry
    again, later in the page; a viewer paging forward from the last
    record shown can then lose entries or get the same page again. sb
    shows the entries after the cursor in the release's order, each
    once. With boot 0xa's newest tail older than 0xb's, a1 and a2 at
    realtimes 100 and 300, b1 and b2 at 200 and 400 (the matrix's
    `boots overlap` fixture, run on 255.4 and 259.9):
    - after a2, both releases' journalctl show b2 (b1 is passed), and
      sb shows b1 and b2; paging one entry at a time from a1, journalctl
      shows a2 and b2, sb a2, b1 and b2;
    - after b1, 24.04's journalctl shows a2, b1 and b2: its first step
      is the lowest landing, a2 (file a sought by realtime 200), which
      isn't the cursor's entry, so it is shown, and b1 follows it in
      the table's order. 26.04's shows b2, as sb does: 259.9 prefers
      the file whose landing is the cursor's exact entry for the first
      step (systemd-stable v259.6 to v259.9, "sd-journal: prefer exact
      cursor match after seek", issue #31516; not in the v259 tag), so
      b1 is the first step and skipped.

    (`boots_order_as_each_releases_journalctl`,
    `forward_pages_show_every_entry_of_another_boot`). Boots whose
    clocks don't overlap give journalctl's pages. Where the order itself
    cycles, so do sb's pages, as journalctl's (decision 11).
21. **A forward page over one boot in two seqnum spaces, its clock set
    back between them, shows only the entries after the cursor**
    (24.04 only; the hardening plan's decision 33). A journald restart
    on a 249 host, or journal-remote, can write one boot into files of
    two seqnum ids. Where the realtime clock was set back between them,
    journalctl 255 seeks the older file by realtime (no matching entry
    of it in the cursor's boot reaches the cursor's monotonic time),
    lands before the cursor, and shows that file's entries, then the
    cursor's own entry and the rest; paging forward one entry at a time
    never ends. sb seeks such a file per boot and shows only the
    entries after the cursor. journalctl 249 and 259.9 show what sb
    shows. With file b holding b1 and b2 (monotonic 1.001 and 1.002 s,
    realtimes 300 and 400 s) and file x holding x1 to x4 (monotonic
    1.003 to 1.006 s, realtimes 10 to 40 s), all of one boot (the
    matrix's `own boot` fixture, run on 249.11, 255.4 and 259.9, also
    filtered with a later non-matching entry in file b): after x3,
    24.04's journalctl shows b1, b2, x3 and x4, and sb x4; one at a
    time from x1, journalctl shows b1, b2, x1, b1, ... and sb x2, x3,
    x4 (`a_forward_page_on_the_cursors_own_boot_shows_only_later_entries`).

The rules are keyed by Ubuntu release (`VERSION_ID`), not by systemd
version. journalctl on a 26.04 host still at systemd 259.0 matches
`_PID=1`, while sb uses the row's init.scope. Ubuntu's 26.04 archive
carries 259.9, and the matrix's systemd-major check flags an image that
doesn't match its row.

UNVERIFIED and carried from FINDINGS:
- sealed (FSS) files;
- systemd 249 journald's own repeated items;
- a zero item in a real file.

The local-journal coredump term was verified by the coredump row
(compare.sh, Task 5): with systemd-coredump installed, 26.04's
journalctl and sb take that user's coredumps, and 22.04's and 24.04's
neither.

Not a difference but a documentation note (audit I1): root inside a
container that can reach journald's sockets matches the `_UID=0` groups,
as it does for journalctl.

### Scope

**In (first slice):**
- The crate with the API above, ported, plus the "new in the crate"
  items, each test-first.
- The tests, crafted files and harnesses.
- sb-host's re-export and glue (decision 6).
- The term-ui sanitizer change (decision 5), with its spec amendment.
- The CI workflow pointed at the crate.
- The platform note kept current.

**Later slices**, each a small spec amendment with its matrix rows:
- `problems()`: priority ≤ 3, or the MESSAGE_IDs of PID 1's process
  exit, unit failure, restart and OOM messages, systemd-coredump's
  coredump and journald's "messages dropped" (the exact set is settled,
  with sources, in that slice);
- priority ranges;
- decoded unit events;
- invocations (the last run, the last failed run);
- this boot;
- since/until with a realtime bisect;
- a budgeted, resumable search;
- a boot list (FIELD objects);
- several systemd units at once, such as a service and its timer. This
  is not log-picker.md's declined merged multi-source view.

**Out (viewer, CLI or host):**
- line formatting, including the unit or identifier column;
- the ring buffer;
- regex;
- time parsing;
- the picker and service status;
- redaction for export (its own decision);
- docker logs;
- tokio.

### Capability map correction

As listed in the host spec's decision 6:
- **`sb-journal`:** a row under `host`, depending on nothing of sb's;
  `sb-host` wraps it.
- **Stack table:** a row for the reader, rejecting sdjournal,
  systemd-journal-sdk and the libsystemd crate.
- **The rejected list's "replaced by `journalctl -f`"** is corrected.
- **deny.toml:** `sb-journal` gets `wrappers = ["sb-host", "sb-journal"]`:
  its own name because its self dev-dependency (the `testing` feature
  for its tests) is a parent in cargo-deny's graph. The deny gate runs on
  crates/sb's graph, so sb-host depends on sb-journal in the first slice,
  which brings it under the gate.
- **ADR-013:** owning the reader, against the library-first rule, with
  proto-journal's evidence, the upkeep (a `RELEASES` row per LTS,
  checked by the scheduled matrix) and decisions 1 and 2's divergences.

## Tech Stack

- Rust 1.99.1, edition 2024, `unsafe_code = "forbid"`, the workspace
  lints (pedantic).
- **Libraries:**
  - siphasher;
  - lz4_flex (`safe-decode`), ruzstd 0.9.1 and lzma-rust2 (`std`, `xz`,
    no `optimization`);
  - rustix (the workspace entry): `fs` for inotify and for the open
    flags std's `OpenOptionsExt::custom_flags` passes (`O_NOFOLLOW`,
    `O_NONBLOCK`, `O_NOCTTY`, `O_CLOEXEC`), and its always-on `io`
    module for `Errno` and `fcntl_dupfd_cloexec` (the wake fd); the open
    and its `fstat` are std's (`File::metadata`);
  - hex;
  - itertools (`kmerge_by`);
  - jiff;
  - thiserror;
  - for the `testing` feature only: serde_json (the JSON form), clap
    (the examples' CLI) and fastrand (the fuzz PRNG); rustix's `event`
    (`poll`, the tools' and tests' wait on a wake fd); rustix's `process`
    feature (the fuzz address-space limit) is on in the workspace already;
    as dev-dependencies,
    tempfile and crc32fast. All are already in the tree. testing.rs
    counts toward the coverage bar, with its own tests.
- **lookup3's `hashlittle2`** stays a port, with its "why not hashers"
  note.
- **No nix and no sysinfo:** sb-host passes the coredump uid and the
  release in.

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-journal
Matrix:  crates/journal/tests/compare.sh [RELEASE]...   (compat.sh, follow.sh, stress.sh beside it)
Seeds:   crates/journal/tests/seeds.sh DIR   (the fuzz seeds, regenerated for each run, never committed)
Fuzz:    systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% cargo run -q -p sb-journal --features testing --example fuzz -- --work DIR/work DIR/*.journal
CI:      .github/workflows/journal-matrix.yml (daily; local until pushed)
```

The fuzz command runs as it is on systemd 249, 255 and 259. Its scope
sets no `OOMPolicy=`: systemd 249 (22.04) refuses that for a scope
("Unknown assignment"; 249's systemd.scope(5) has no such setting, and
systemd's NEWS for 253 says "Scope units now support OOMPolicy="). The
harness doesn't need it: its 3 GiB address-space limit stops it below
the scope's 4G `MemoryMax`, so the kernel's OOM killer, and the policy
with it, doesn't come into play (examples/fuzz.rs).

## Project Structure

```
crates/journal/src/lib.rs      the public API
crates/journal/src/file.rs     one file, pread only (and systemd's bisect over its entry lists)
crates/journal/src/forward.rs  the forward page: journalctl 259's seek emulated, then forward
crates/journal/src/hash.rs     siphash, lookup3
crates/journal/src/open.rs     safe opening (L2), Source::Local's machine-id file and roots, the opener's counts
crates/journal/src/walk.rs     newest-first walks and the merge
crates/journal/src/boots.rs    the order between boots of 255 and later (`compare_boot_ids`'s table; decision 11)
crates/journal/src/follow.rs   Follower: positions, watches, classify, rescans
crates/journal/src/rules.rs    RELEASES and the unit rules
crates/journal/src/testing.rs  crafted-file builder, file-level access (`testing` feature)
crates/journal/src/testing/tools.rs  the tools' JSON output, verdicts and export fixtures
crates/journal/src/testing/fuzz.rs   the fuzz harness's seeds, mutations, reads and failure rules
crates/journal/tests/          the container scripts and checks.sh
crates/journal/examples/       the comparison CLI and the fuzz harness (on `testing`)
docs/decisions/ADR-013-*.md    the ADR
```

The tools need file-level internals (`entry_before`, `all_entries`,
`data_payload_within`) and the JSON form. Those sit behind the
`testing` feature, the repository's convention as in sb-docker. Tests
live in the library, so `cargo test` runs them all, the four `classify`
tests included. `prototypes/proto-journal` was removed once the crate
passed the matrix (the plan's Task 12), and its FINDINGS moved to
[docs/prototypes/proto-journal.md](../prototypes/proto-journal.md).

## Testing Strategy

- **Unit and crafted-file tests (most):** every format rule, every
  corruption class and every follow case the prototype pins, ported.
  Each "new in the crate" item and each Difference gets its own test.
- **The container matrix, per supported release:**
  - writer variants;
  - the whole file, `-u`, pages both ways and field matches;
  - files from older releases;
  - the follow scenarios;
  - live stress.

  All value-exact against that release's journalctl, the listed
  differences excepted. A row with systemd-coredump installed checks
  decision 1's local rule (verified in Task 5).
- **Fuzzing on stable:**
  - at least 9,000 cases over the eight journal-remote seeds (249 and
    255 output, four writer variants each), regenerated from the
    matrix, not committed;
  - a 10 s watchdog per case and a 3 GiB address-space limit, inside a
    `systemd-run` scope;
  - at most 16 MiB over the seeds' base memory per case.

  The harness itself fails a run on any panic, hang or case over the
  memory bound, and when peak memory can't be read. A run passes or
  fails without anyone reading its output. afl.rs is a later step.
- **Roots:** `Source::Local`'s machine-id file and journal roots can be
  injected under the `testing` feature (M-MOCKABLE-SYSCALLS), so the
  machine-id check, the search order and the parent-directory watch are
  unit-tested, not only tested in containers.
- **CI:** the scheduled matrix with interim and devel canaries, running
  once the maintainer pushes and authorizes it (decision 7); afl.rs
  fuzzing then; a check.sh workflow as its own task.
- **The CONSTRAINTS bar:** changed-line coverage ≥ 90%, mutation ≥ 80%,
  and the project ratchet. The port is large, so it lands in slices:
  file and hash, then walk and rules, then follow, then the API. It was
  planned at about 3,800 lines; on 2026-11-12 it is about 4,710: the
  nine reader modules' code lines outside their tests, without blank or
  comment lines or the `testing` feature's modules (re-counted at the
  hardening plan's Checkpoint C). A ratchet ADR follows if the project
  figures move, as with ADR-011. The binary-size ratchet applies: about
  240 KiB expected.

## Boundaries

- **Always:**
  - pread only;
  - every file value checked;
  - exact against journalctl except the listed differences;
  - raw bytes out, `DisplayText` to screens;
  - a new release is a row plus matrix rows.
- **Ask first:**
  - a new dependency;
  - a difference from journalctl not listed;
  - changing the display policy;
  - pushing or running the CI workflow.
- **Never:**
  - mmap or `unsafe`;
  - a child process;
  - silently dropping a file or entry;
  - live host data in committed files.

## Success Criteria

1. **sb-host's tests run `page`** (Tail, Before and After) **and a
   follow** on crafted files through the re-export. The unified picker
   and viewer, the first consumer (decision 8), come with the tui spec.
2. **Every prototype test passes in the crate** (112), plus tests for:
   - each "new in the crate" item and each Difference;
   - `Send` on the public types and the wake fd;
   - `RELEASES` naming the same releases as sb-host's
     `SUPPORTED_RUNTIME_RELEASES`.
3. **The container scripts pass:** compare.sh, compat.sh, follow.sh and
   stress.sh, on 22.04, 24.04 and 26.04, with the listed differences
   pinned. That includes:
   - a forward page check;
   - exact `more_older`/`more_newer`, matched against whether journalctl
     has entries beyond the page (except past an edge entry sb can't
     show, Difference 5);
   - writes between a page and its follow;
   - the coredump row.
4. **The fuzz harness passes** with the bounds above, enforced by
   itself.
5. **Performance:** a matrix fixture's tail, page back and forward page
   take at most a stated number of preads, recorded with the first
   slice. Repeated `Before` paging costs grow with the depth paged (each
   page walks the newer entries again): that cost is accepted until a
   seqnum bisect is needed.
6. **`scripts/check.sh full` passes,** within the ratchet.
7. **Documents updated:**
   - ADR-013;
   - the capability map (the `host` row and its dependencies, tokio for
     sb-host), the stack table and deny.toml;
   - term-ui's spec amendment (decision 5);
   - the platform note;
   - proto-journal's FINDINGS moved to `docs/prototypes/`.
8. **A security-auditor pass on the port** covers L1, L2, L5, I3 and the
   display sanitizer, with no open high finding.

## Decisions (maintainer, 2026-11-08)

1. **The coredump rule is stricter than journalctl's on 26.04:**
   `_UID=0` is required when the systemd-coredump user doesn't resolve.
2. **The display keeps all colour:**
   - every SGR passes through when colour is on;
   - every other escape is dropped, and so are C0 (other than tab,
     newline and carriage return, which is applied: trailing ones are
     ignored, then what follows the last one on its line is kept, as in
     "Display (decision 2)"), DEL and C1 (carriage return's wording
     corrected 2026-11-12, hardening Task 6, docs-L8; the rule itself is
     unchanged);
   - bidi and zero-width characters aren't marked.
3. **The CI matrix workflow is added locally**; pushing and running it
   needs its own go-ahead.
4. **The stricter coredump rule applies to every source,** external
   files (`--file`/`-D`) included.
5. **The sanitizer lives in sb-term-ui, shared:** public, taking bytes,
   and tightened. term-ui's spec is amended.
6. **sb-host re-exports sb-journal's types.**
7. **FINDINGS' gates are relaxed:** the crate is built now. The CI
   matrix and afl.rs fuzzing come once the maintainer pushes and
   authorizes them. A workflow running `check.sh`, which CONSTRAINTS.md
   asks for once CI exists, is its own task.
8. **No source by name on the command line.** The unified TUI picker
   chooses the unit or container, so separate namespaces can't
   collide, and log-picker.md's "not picked for now" stands. The picker
   and viewer are the crate's first consumer; both kinds of source sit
   behind one log-source interface (Objective).
9. **Paging forward (`After`) and exact `more_*` flags** are in the
   first slice.
10. **A forward page is journalctl's `--after-cursor` of 26.04
    (systemd 259), its seek emulated** (maintainer, 2026-11-10, two
    decisions: first to match journalctl's seek, then to emulate its
    bisect).
    - The seek, per file (v259 sd-journal.c `find_location_with_matches`
      and `find_location_for_match`): a seqnum bisect if the file's
      seqnum id is the cursor's; else a monotonic bisect in the
      `_BOOT_ID=` DATA object's list, used only if it finds an entry;
      else a realtime bisect. A value bisects its DATA object's list
      (journal-file.c `generic_array_bisect_for_data`, which tests the
      in-line entry first); a term takes the lowest landing, a group
      the highest and then the next entry in every term
      (`next_for_match`), the query the lowest. The bisect is
      `generic_array_bisect` step for step: the chain cache, the last
      index, each array's last item first, cutting an array short at a
      bad item. On a file out of order (only journald 249 writes one
      whose realtime steps back: 255 and later rotate instead) the
      result is where its probes land: realtimes 10, 50, 5, 45 and a
      cursor at 40 land on 50, and 26.04 shows 50, 5, 45. A zero item
      cuts a bisect short there, as systemd's does; the walks themselves
      skip it (Difference 8).
    - The landings are taken without comparing them to the cursor; the
      lowest across files is the first step, shown unless it is the
      cursor's own entry (`sd_journal_test_cursor`, journalctl-show.c);
      a file that found nothing seeks again from it; then forward, the
      lowest head strictly after the last entry taken.
    - 22.04's journalctl always steps past the landing: Difference 18.
      24.04's tests the cursor as 26.04's does (systemd-stable v255.1).
    - 26.04's journalctl (259.9) also takes the file whose landing is
      the cursor's exact entry as the first step, before the lowest
      landing (stable backport, issue #31516). sb doesn't emulate that
      step: it changes the first step only where another file's landing
      orders before the cursor's own entry. That takes two boots the
      boot table orders against their clocks (decision 11), or a file of
      the cursor's boot and another seqnum space that 259 seeks by
      realtime, as none of its matching entries of that boot reaches the
      cursor's monotonic time (the hardening plan's decision 33). sb
      seeks such a file per boot in both cases, so its pages are what
      they would be if it emulated that step (they still differ from
      259.9's where Difference 20 says).
    - Verified against running journalctl by Task 5's matrix: every
      forward page of the fixtures, page by page, on 22.04, 24.04 and
      26.04 (Difference 18 aside, on 22.04), and the stepped-back
      landing of a 249-written file (docs/platform-notes/systemd-journal.md).
    - The order between entries of different boots is the release's
      (decision 11), for the forward page as for the others.
11. **Boot order per release** (maintainer, 2026-11-10): the merge
    across files orders entries of different boots as each release's
    journalctl does, in every comparison: the tail's and `Before`'s
    merge and its cursor test, the forward page's first step and its
    "after the entry just taken" test, and the page's end.
    - 22.04 (systemd 249) keeps `journal_file_compare_locations`
      (v249 journal-file.c): seqnum in one seqnum space, monotonic time
      in one boot, then realtime and the XOR hash.
    - 24.04 and 26.04 (255 and 259, the same code in both) put
      `compare_boot_ids` between the monotonic and the realtime step
      (v255 and v259 sd-journal.c, `compare_locations` and
      `compare_with_location`). Its table, `newest_by_boot_id`, holds
      every file sd-journal added, matching or not, under its tail
      entry's boot (`journal_file_read_tail_timestamp`): the entry
      `tail_entry_offset` names, or in an older header the tail object
      if it is an entry; else an archived file's header `tail_entry_*`
      (with `HEADER_COMPATIBLE_TAIL_ENTRY_BOOT_ID`); else the main
      list's newest readable entry (255's and 259's `generic_array_get`
      going up passes over an entry it can't read; so does the page's
      end, on every release). A zero
      offset or a monotonic time ahead of the
      realtime puts the file in no boot. Per boot, the file with the
      highest tail monotonic time counts, the first added on a tie
      (prioq.c `shuffle_up`). Two boots order by those files' tail
      realtime when both are in the table and the files' machine ids are
      equal; otherwise they compare equal and realtime decides. The
      machine id is the file header's on 24.04 (255.4); on 26.04
      (259.9) it is the tail entry's `_MACHINE_ID=` where that entry
      (the one that gave the times, not an archived header's) has a valid
      one, else the header's (v259-stable commit 2d8b0b6a0aaf, "sd-journal:
      prefer machine ID from the tail entry", issue #34169, first tagged
      in v259.9; `journal_file_entry_get_machine_id`). journal-remote's
      files carry the receiving machine in their header and the
      source's in each entry, so on 26.04 imports of several machines
      interleave by realtime. Its items are read in order up to the
      first `_MACHINE_ID=` (one pread each; all of them without one);
      an invalid first one keeps the header's, an unreadable item is
      passed over and a refused read ends the search, as in systemd
      (`BootRule::NewestTailEntryMachine`). file.rs `entry_machine_id`
      lists two departures, neither on files as journald writes them: a
      malformed compressed `_MACHINE_ID=` before another, and a file cut
      short after it was opened.
      Verified against running 249, 255.4 and 259.9 by compare.sh's
      `machines` fixtures (Task 6; docs/platform-notes/systemd-journal.md).
    - The release is the `Journal`'s (`Journal::open(source, &release)`),
      the caller's for every source, as for `Query::unit`.
    - The table reuses the newest entry each page reads for its end, and
      reads one more entry only for a file whose header names another
      tail (journald before 254, or a corrupt file): asserted in
      `the_boot_table_reads_at_most_one_entry_per_file`. On 26.04, with two
      boots or more in the table, it also reads each boot's newest file's
      tail entry's items up to its `_MACHINE_ID=`, one pread each
      (`the_tail_entrys_machine_id_costs_a_pread_per_item_read`): only
      those files' ids are compared, and the newest is chosen by
      monotonic time alone, so the order is the same as reading every
      file's.
    - **sb's order wins on 24.04 and 26.04** (maintainer, 2026-11-10):
      a forward page is the entries after the cursor in the release's
      order (the boot table, then realtime), each shown once. The
      mechanism behind the difference: journalctl seeks each file by one
      key (seqnum, the cursor's boot's monotonic time, or realtime) but
      merges by the boot table, so where two boots' clocks overlap it
      passes entries and can show the cursor's own one (Difference 20).
      sb seeks such a file per boot instead: it enumerates the file's
      boots through the `_BOOT_ID` field's DATA objects (the field hash
      table and `next_field_offset`, as `sd_journal_enumerate_unique`
      reads them); the cursor's boot starts by monotonic time, a boot
      the table orders after it at its first entry, one it leaves equal
      by realtime, one it orders before nowhere; and only entries at or
      after their boot's start are walked (within a file every entry has
      one seqnum id, so file order is the release's order there). 259's
      seek is kept where per-boot seeking gives its landing anyway: a
      file of the cursor's seqnum space, one with no `_BOOT_ID` values
      sb can read, or one of a single boot, not the cursor's, that the
      table leaves equal to it. On 24.04 and 26.04 a file that holds the
      cursor's boot is sought per boot (the hardening plan's decision
      33): where none of
      its matching entries of that boot reaches the cursor's monotonic
      time, 259 seeks it by realtime and can land before the cursor
      (Difference 21). On 22.04 only such a file is sought per boot, its
      other boots by realtime (249 has no table): sb takes 259's seek
      and seeks again per boot where it fell back to realtime in the
      cursor's boot; every other file keeps 259's seek there, at no
      extra cost. The cost is the field lookup (a pread for the bucket,
      one per FIELD object in its chain, one per boot) and a bisect per
      boot started by time, per such file and page (asserted in
      `the_boot_seek_reads_the_files_boots`), plus the matching entries
      of other boots the walk passes over. On 24.04 and 26.04 every file
      outside the cursor's seqnum space pays the field lookup on each
      forward page, even when the cursor's boot is in no file's tail:
      the price of knowing its boots.
    - The order isn't transitive, and the boot step adds cycles: a boot
      in no file's tail compares equal to every boot, so realtime orders
      its pairs while the table orders the rest. Where three boots
      cycle, paging forward one page after another cycles as well
      (`forward_paging_cycles_where_the_order_does`); a cursor alone
      can't break it, so a caller paging on stops at a cursor it has
      shown. journalctl's order cycles the same way.
    - Verified by Task 5's matrix: two boots in two files, clocks
      overlapping or apart, paged both ways on 22.04, 24.04 and 26.04
      against each release's journalctl (Difference 20 for what 24.04's
      and 26.04's forward pages do instead).
    - The follow keeps the table current as its files grow or new ones
      appear, as sd-journal reads a changed file's tail again before each
      step (v255 `next_beyond_location` on a changed mtime, v259 on a new
      tail offset): each drain reads every followed file's header first,
      reads the tail again of each whose entry count or tail offsets
      changed, and builds the table from all of them for that drain's
      merge (follow.rs `Follow::boots`;
      `the_follow_keeps_its_boot_table_current_as_files_grow`). The table
      is the drain's, where sd-journal's can change between two steps of
      one read; within a drain the files' headers are read once.

12. **The wake wait is sb-host's, not tokio's `AsyncFd`** (maintainer,
    2026-11-11). tokio 1.54.1 deprecates every `AsyncFd` constructor
    (`new`, `with_interest`, `try_new`, `try_with_interest`) as "unsound
    due to lack of I/O safety" (tokio PR #8587,
    https://github.com/tokio-rs/tokio/pull/8587, checked in the 1.54.1
    source, `src/io/async_fd.rs`), and the replacement,
    `AsyncFd::register`, is an `unsafe fn`, which the workspace forbids.
    So sb-host provides the wait (`journal::FollowWait`): rustix `poll`
    on the follower's wake fd and a stop eventfd (`CLOEXEC | NONBLOCK`),
    inside `spawn_blocking`. Closing the viewer (`FollowStop::stop`), or
    dropping the waiter, writes the stop fd, so the blocked poll returns
    at once and its blocking thread is freed. The protocol is "Waiting
    for new entries" above, which replaces the `AsyncFd` steps.

13. **A filtered page reads each file's header again after its lookups**
    (maintainer, 2026-11-12; sb-journal hardening plan, decisions 20, 23
    and 24, from Task 32's options). A page on a journal journald is
    writing could show an entry past the count its `PageEnd` held, which
    the follow then showed again, and could miss the newest counted
    entry, not yet linked into its values' lists, which the follow
    started past (hardening findings follow-H1, page-H1, fwd-M2 and
    follow-N1).
    - **What the page does:** per file that isn't sealed (archived, or
      `.journal~`), after every lookup it reads the header again (n1).
      It reads the main-list entries from the first read's newest one
      (n0-1) up to n1, looks the query's values up again (a value first
      written during the page had no DATA object at its lookup), and
      takes an entry that matches by its items where the page's walk can
      show it: its group's driving term's lists hold it now (the walk reads
      that term's lists and checks the other terms by items), or, for a
      group the walk had nothing to walk in (a value first written during
      the page), every term's. It merges those into the file's stream by
      offset, an offset met twice taken once. `PageEnd` holds n1.
    - **An entry counted but not in its lists yet** at the second read
      (by the link order, on a live file, only the newest n1 counts) isn't
      shown: the file's count in `PageEnd` is that entry's index, so the
      follow reads it by its items, as it reads every entry. If journald
      never links it (it died mid-link, or the file was archived with the
      link cut off), the follow shows it and journalctl doesn't:
      Difference 12. An older one journald moved on from was cut off for
      good, and neither the page nor the follow shows it.
    - **Why it is exact:** journald links an entry into the main list
      and raises the count before it links the entry into its values'
      lists, so an entry the lookups' lists hold is counted by n1, and
      only the newest entry n0 counts can be half linked
      (docs/platform-notes/systemd-journal.md, "The order journald links
      an entry in", source and a probe on 22.04, 24.04 and 26.04).
    - **Unchanged:** the follow's start, the flush and rotation rules,
      and unfiltered pages (they read no value list). A sealed file's
      count is final, and reading its newest entry again would show an
      entry journald was cut off from linking (Difference 12).
    - **A forward page** takes such an entry by the cursor's order where
      it lies before the landing the lists' seek found, or where they
      found none, and in file order after the landing, as the lists'
      own entries. Where the file's realtime steps back among the entries
      written during the page, that can differ from the landing a bisect
      over the whole list finds (forward.rs lists it). A seek that meets
      an error leaves them out, and the file hands over the first read's
      count, as when the second read fails, so the follow reads them.
    - **A forward page at the journal's end stops at the count it read**
      (n0 unfiltered, n1 filtered); the follow delivers the entries
      appended meanwhile. Every entry the page shows is in journalctl's
      view, and the page plus its follow is the whole view (the
      hardening plan's decision 24).
    - **The cost,** per filtered page and unsealed file: the header, one
      pread per array of the main list up to the newest counted entry
      (they double in size, so about log2 of the count, as the page's walk
      to its end already reads), that entry's item and the entry, each
      value looked up again, and for an entry that carries one, an offset
      bisect of its list (O(log n)): 18 preads on a one-array fixture of
      200 entries. More when entries were written during the page.

## Open questions

None for this spec. The log-source interface's exact shape, and how the
viewer offers pasteable output, belong to the tui spec.
