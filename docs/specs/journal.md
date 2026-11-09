# Spec: `sb-journal` (draft, 2026-11-08)

`sb-journal` is a crate of its own, wrapped by sb-host (the host spec's
decision 6). It reads systemd's journal files in pure Rust for
`sb logs`:
- a unit's newest entries;
- paging back and forward;
- following.

It replaces sb-go's `journalctl -o json --cursor` polling (cmd/logs.go)
and proto-logs' `journalctl -f` child. The design and its evidence come
from `prototypes/proto-journal`: its FINDINGS.md (adopted with changes,
maintainer 2026-11-08, including the security audit and the feature
research) and its code, which this crate supersedes. Verified systemd
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
     duplicate of it (`wake_fd`) is what the caller awaits with tokio's
     `AsyncFd`, so the follow itself can move into `spawn_blocking`.
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
    pub more_older: bool,
    pub more_newer: bool,
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
    /// later call are errors of that call, not an `OpenError`.
    pub fn open(source: Source) -> Result<Self, OpenError>;
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
carries the per-release rule and its `-o json` shape (`prints_seqnum`);
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
That covers each file it read, by inode, with the entry count it read
up to, and the newest entry it examined, unfiltered, with that entry's
directory. It exists even when the page matched nothing.

`Journal::follow(from)` then:
1. adds the watches;
2. opens the files;
3. reads each file from the count `from` recorded for it. A file `from`
   didn't see (new since the page) is walked back with `EntriesRev` to
   the newest entry at or below `from`'s newest entry. `from`'s
   directory decides between sd-journal's order and the flush-copy
   rule, as the prototype's `seen` does: that stop test is monotone
   within a file.
4. merges what it read through the same merge as `drain`, so an entry
   written to /run after the page and flushed to /var before the follow
   shows once;
5. returns it as the first `Batch`, under the per-file output limit
   (`more` set if it stopped there).

Each file's position is then the entry count it read to. No write
between the page and the follow is lost, even without an event, and
even after an empty page. The prototype's `check_until` window is kept
only for files found by a later rescan.

A crafted test and a follow.sh scenario write between a page and its
follow, also after an empty page: 0 lost, 0 doubled.

**Waiting for new entries** (tokio's `AsyncFd`,
https://docs.rs/tokio/latest/tokio/io/unix/struct.AsyncFd.html):
1. The caller wraps `wake_fd()` in an `AsyncFd` and awaits readable.
2. It runs `drain` under `spawn_blocking`; `drain` reads the inotify fd
   to EAGAIN. The duplicate shares the open file description, so that
   empties what the `AsyncFd` registration sees.
3. While a `Batch` has `more`, it drains again.
4. It clears the readiness of step 1's guard, and waits again.

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
  - follow.sh: 0 lost and 0 doubled in nine scenarios per release.
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
- **`/etc/machine-id` validated** as 32 hex characters, a `MachineId`
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
states. console's `AnsiCodeIterator`, which `clean` uses today,
recognises only CSI, OSC and DCS.

**The 8-bit C1 forms are ours.** anstyle-parse and vte both document
"only supports 7-bit codes", and anstyle-parse's ground table gives
0x90, 0x9B and 0x9D to 0x9F no transition. So a pre-pass maps the
code points U+0090, U+0098, U+009B, U+009D, U+009E and U+009F to their
`ESC` forms, so the parser drops their payloads, and drops the rest of
U+0080 to U+009F. Raw 0x80 to 0x9F bytes become U+FFFD first, through
lossy decoding. Why not the library: both parsers are 7-bit only.

The sanitizer lives in sb-term-ui and is shared (decision 5). term-ui's
`clean` becomes public, takes bytes, and is tightened to this policy,
which amends term-ui's spec. The runner and the log viewer then use one
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
   is a zstd frame needing a window over 128 MiB.
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
12. **When journald is cut off while linking an entry** it writes again,
    the follow shows both copies (it matches by items). journalctl and
    the tail show one. stress.sh allows exactly that.
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

The rules are keyed by Ubuntu release (`VERSION_ID`), not by systemd
version. journalctl on a 26.04 host still at systemd 259.0 matches
`_PID=1`, while sb uses the row's init.scope. Ubuntu's 26.04 archive
carries 259.9, and the matrix's systemd-major check flags an image that
doesn't match its row.

UNVERIFIED and carried from FINDINGS:
- sealed (FSS) files;
- systemd 249 journald's own repeated items;
- a zero item in a real file;
- the local-journal coredump term, until the coredump row runs.

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
- **deny.toml:** `sb-journal` gets `wrappers = ["sb-host"]`. The deny
  gate runs on crates/sb's graph, so sb-host depends on sb-journal in
  the first slice, which brings it under the gate.
- **ADR-012:** owning the reader, against the library-first rule, with
  proto-journal's evidence, the upkeep (a `RELEASES` row per LTS,
  checked by the scheduled matrix) and decisions 1 and 2's divergences.

## Tech Stack

- Rust 1.99.1, edition 2024, `unsafe_code = "forbid"`, the workspace
  lints (pedantic).
- **Libraries:**
  - siphasher;
  - lz4_flex (`safe-decode`), ruzstd 0.9.1 and lzma-rust2 (`std`, `xz`,
    no `optimization`);
  - rustix (`fs` with inotify, `openat`, `fstat`);
  - hex;
  - itertools (`kmerge_by`);
  - jiff;
  - thiserror;
  - for the `testing` feature only: serde_json (the JSON form), clap
    (the examples' CLI), fastrand (the fuzz PRNG) and rustix's `process`
    feature (the fuzz address-space limit); as dev-dependencies,
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
Fuzz:    systemd-run --scope -p MemoryMax=4G -p CPUQuota=200% cargo run -p sb-journal --features testing --example fuzz -- SEEDS ...
CI:      .github/workflows/journal-matrix.yml (daily; local until pushed)
```

## Project Structure

```
crates/journal/src/lib.rs      the public API
crates/journal/src/file.rs     one file, pread only
crates/journal/src/hash.rs     siphash, lookup3
crates/journal/src/walk.rs     newest-first walks and the merge
crates/journal/src/follow.rs   Follower: positions, watches, classify, rescans
crates/journal/src/rules.rs    RELEASES and the unit rules
crates/journal/src/testing.rs  crafted-file builder, JSON output, file-level access (`testing` feature)
crates/journal/tests/          the container scripts and checks.sh
crates/journal/examples/       the comparison CLI and the fuzz harness (on `testing`)
docs/decisions/ADR-012-*.md    the ADR
```

The tools need file-level internals (`entry_before`, `all_entries`,
`data_payload_within`) and the JSON form. Those sit behind the
`testing` feature, the repository's convention as in sb-docker. Tests
live in the library, so `cargo test` runs them all, the four `classify`
tests included. `prototypes/proto-journal` is removed once the crate
passes the matrix, and its FINDINGS move to `docs/prototypes/`.

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
  decision 1's local rule. The local-journal coredump term is
  UNVERIFIED until then.
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
  and the project ratchet. The port is large (about 3,800 lines), so it
  lands in slices: file and hash, then walk and rules, then follow, then
  the API. A ratchet ADR follows if the project figures move, as with
  ADR-011. The binary-size ratchet applies: about 240 KiB expected.

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
     has entries beyond the page;
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
   - ADR-012;
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
   - every other escape is dropped, and so are C0 (other than tab and
     newline), DEL and C1;
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

## Open questions

None for this spec. The log-source interface's exact shape, and how the
viewer offers pasteable output, belong to the tui spec.
