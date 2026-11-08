# Prototype: proto-journal

**Question:** Can a small pure-Rust reader of systemd's journal files serve
`sb logs` (the last 500 entries of a unit, live follow, paging back) as
exactly as `journalctl -u`, on every supported Ubuntu release?

sb refuses any other release (cli-core startup check), so the reader only
has to know the formats and `-u` rules of 22.04, 24.04 and 26.04. Each new
LTS adds its own deliberately (host spec, decision 4; tasks/prototypes
Task 11).

**Crates tried:**
- **siphasher@1.0:** siphash24, for keyed-hash files.
- **Payload decompression:**
  - lz4_flex@0.14.0 (`safe-decode`);
  - ruzstd@0.9.1, which carries the RUSTSEC-2026-0344 fix;
  - lzma-rust2@0.25.0 (`std`, `xz`, without `optimization`), which builds
    as `forbid(unsafe_code)` and has a memory-limited reader.
- **rustix@1.1.6 `fs::inotify`:** following. Why not `notify` 8.2, which
  keeps the same watch-to-path map and flags an overflow for a rescan: it
  runs a thread and a channel for a loop that blocks on one fd here, and
  it isn't in sb's tree, for about 30 lines.
- **nix@0.28 `user`:** the systemd-coredump uid. This is the nix version sb
  already links through portable-pty.
- **hex:** ids.
- **sysinfo:** the release.
- **serde_json, clap, anyhow:** the CLI.

Compared against:
- `journalctl`;
- sdjournal@0.1.22, in a separate hands-on run.

**What was built:** the reader is about 1,890 non-comment lines (hash.rs, file.rs, query.rs; about 3,490 with their tests), plus about 660 in main.rs for the CLI and test tools and about 320 in fuzz.rs:
- **hash.rs:** siphash through siphasher, plus a port of lookup3's
  `hashlittle2`. Why not `hashers`' `Lookup3Hasher`:
  - no release since 2018;
  - its output halves are swapped against systemd's;
  - it returns 0 for empty input.
- **file.rs:** one journal file read with `pread` only. No mmap and no
  `unsafe`, so a file truncated under the reader is an error, not SIGBUS.
  - Every offset taken from the file is checked: alignment, arena bounds,
    type, size and overflow.
  - A read past the arena as it was at open re-reads the arena size and
    the file's length once before it fails, as sd-journal re-stats in
    `journal_file_move_to`: links read live can name objects journald
    appended since.
  - A chain link must point past the end of the object holding it:
    journald appends objects and links a new one from an older one. That
    ends any cycle, and a walk reads each byte of the file at most once
    (a link into the object's own body was accepted before, and made a
    walk quadratic). Entry-array items must never decrease.
  - A chain walk never ends early on corruption: a chain whose next link
    can't be followed ends there and the arrays before it are still
    read; an array whose items go out of order, or can't be read, is
    left at that point and the walk goes on with the older arrays (going
    up, sd-journal's `generic_array_get` "moves back to the previous
    entry array"). Each error is yielded where it was met: a chain cut
    short first (the entries past the break are the newest), an array
    left at its gap, so a tail that stops after N entries still reports
    it. An item that can't be an entry offset (unaligned, outside the
    arena) is refused before it can make the older items look out of
    order.
  - The data hash table is checked as sd-journal checks it on open
    (`hash_table_is_valid`): whole items, inside the arena. sd-journal
    then refuses the whole file; this reader refuses lookups only, so
    the entries are still read in file order. Before, a size that wasn't
    whole items silently found nothing.
  - hash-check's walk of every bucket requires each object to sit in its
    own hash's bucket, as journald links it, so buckets can't share a
    chain and make the walk quadratic.
  - A lookup decodes a candidate with the right hash only as far as the
    value it looks for. A payload over the caller's limit is a distinct
    error (`OverLimit`), refused before reading the rest when its stored
    size already exceeds the limit (journald stores a payload compressed
    only when that makes it shorter).
  - An entry needs at least one whole item and a sequence number, as
    sd-journal's `check_object` requires, and must end before the next
    entry (objects never overlap), checked before its body is read.
  - Entry arrays may repeat an offset: systemd 249 links an entry once
    per item, so an entry carrying the same value twice appears twice.
    The repeat is skipped, as sd-journal skips it, but still counts as a
    link against the entry count, as systemd's `n_entries` does.
  - Data and entry objects are capped at systemd's own limits from
    journal-def.h (DATA_SIZE_MAX 768 MiB, ENTRY_SIZE_MAX 770 MiB).
  - xz, lz4 and zstd payloads are decoded with their output capped at
    DATA_SIZE_MAX, or less where the caller asks. xz also runs under a
    64 MiB decoder-memory bound of our own (journald's xz dictionary is
    1 MiB). An LZ4 size claim is refused past 255 times the compressed
    size, the format's most: lz4_flex allocates the claimed size before
    decoding. A zstd frame must declare its content size and decode to
    exactly that, as sd-journal's `decompress_blob_zstd` (libzstd)
    requires; ruzstd 0.9 checks neither and has no checksum without its
    `hash` feature, so a corrupt frame decoded to the wrong length was
    returned as good data. Its window is capped at twice the content
    (at least 1 KiB): ruzstd holds back a window of output, so that also
    bounds the work a failing frame does unseen. It is decoded only to
    its declared size, and a declared size past what the stored bytes can
    hold (128 KiB per 4-byte block) is corruption.
  - An unknown incompatible flag is an error naming the file.
- **query.rs:**
  - `RELEASES`, one row per supported release: its VERSION_ID, systemd
    version, journalctl's `-u` rule (`UnitRule`) and `-o json` shape;
  - the local machine's files in sd-journal's search order;
  - the last N picked by walking backwards across files, as
    journalctl's `-n` does, and new entries by a forward k-way merge, both
    with sd-journal's comparison, tie rule and equal-entry skip;
  - errors per file and per entry: they are reported, and the rest is
    still shown. An entry that can't be read is skipped and the walk goes
    on, as sd-journal's `generic_array_get` skips it; an entry with an
    unreadable field is left out whole, as journalctl's `output_json`
    leaves it; a group whose lookup fails is left out, the others still
    match. The first ten distinct errors of a query (or of a follow's
    wake) are reported in full, then a count of the rest;
  - the file list is read again after the tail's opening, and the files
    opened again if it changed (up to three times): a rotation between
    listing and opening would otherwise drop the archived file's entries
    without an error. journalctl lists once and has the same race;
  - one output limit per query, and per followed file and wake:
    256 MiB, our own bound, below journald's ENTRY_SIZE_MAX so it bounds one entry too. A
    corrupt file could otherwise give each entry a bomb, or an entry
    listing one object millions of times. Every read spends the whole
    object plus what it decoded, failed reads too, and every field held
    its size; the DATA cache reads a shared object once, and one that
    failed isn't decoded again for the next entry listing it. The first
    entry that doesn't fit ends the output, reported, so it has no gap:
    a tail keeps the newest; a follow stops that file at the entry,
    unmoved, and the next wake goes on from it (an entry larger than the
    whole limit is skipped, reported). The candidates a tail holds before
    decoding have the same bound;
  - output in `journalctl -o json --all`'s shape.

  Each match group walks the entry list of its rarest term backwards,
  reading the chain lazily from its end. A
  term that ORs several values of one field drives through the union of
  their lists.
- **main.rs:** `tail` (with `--follow` and `--before`) and the test tools:
  - `compare`: per field, exact JSON values, so a string-vs-byte-array
    choice and the order of a repeated field both count;
  - `hash-check`;
  - `decode-all`;
  - `gen-export`, with `--generation` for later entries on other boots;
  - `releases`: the table's rows, the scripts' default list.
  - `fuzz`: the corruption harness below.
- **compare.sh, compat.sh and checks.sh:** the release matrix, files from
  before an upgrade, and the checks both share.
- **stress.sh:** reading while journald writes and rotates, below.

**Why not the alternatives:**
- **sdjournal 0.1.22** (hands-on run):
  - it skips `.journal~` files (left by unclean shutdowns, which
    journalctl reads) by default, with no error, so their entries are
    missing from its output;
  - its follow engine can die on a rotation race, and the subscriber
    only sees a disconnect;
  - it can't page back from a cursor;
  - its `match_unit` isn't journalctl's rule;
  - it carries a RustSec advisory through ruzstd 0.8;
  - its bus factor is 1;
  - it adds about 640 KB to the binary.

  In its favour: it was exact once worked around, and it used much less
  memory than journalctl or this reader.
- **netdata's systemd-journal-sdk:** it reads files through a 32 MiB
  mmap window with a SIGBUS handler, and links the C `zstd` crate. Both go
  against this reader's pread / no-SIGBUS / pure-Rust constraint. It
  wasn't run hands-on.
- **The libsystemd `systemd` crate:** it can't link into the static musl
  binary, and systemd's musl support is "incomplete".
- **A `journalctl` child:** exact by definition, but it is a host CLI
  (the maintainer prefers pure Rust). Its JSON has quirks of its own:
  ANSI lines arrive as byte arrays, fields of 4096 bytes or more are `null`
  without `--all`, `-- Boot --` lines appear without `-q`, and
  `-f -n 0` loses writes made while it starts.

**How it was checked:**
- **Containers.** `compare.sh` runs one unprivileged container per release,
  each with that release's own systemd packages:
  - `ubuntu:22.04`: systemd 249.11;
  - `ubuntu:24.04`: 255.4;
  - `ubuntu:26.04`: 259.9.

  systemd is not PID 1 in them: that would need a writable host cgroup
  tree.
- **Generated entries.** `gen-export` writes 3,000 entries, using synthetic
  ids only. They cover every `-u` branch for `demo.service`, plus decoys:
  - not PID 1;
  - a non-root `OBJECT_SYSTEMD_UNIT`;
  - a coredump without `MESSAGE_ID`;
  - a non-root coredump;
  - an init.scope helper;
  - a unit-name prefix.

  The values include ANSI, invalid UTF-8, NUL, C1 controls, newlines and
  tabs, repeated fields, large repetitive and large random messages, and
  two boots.
- **Writer variants.** That release's `systemd-journal-remote` writes the
  entries under 7 variants: default, regular, unkeyed, xz, lz4, zstd, and
  old style (regular, unkeyed and lz4).
- **Checks per variant:**
  - hash-check and decode-all with no errors;
  - the whole file, `-u demo.service` last 500, and one page back;
  - an AND across fields with an OR within one;
  - an OR on one field alone;
  - a value its entries carry twice. No writer here kept the repeat (each
    release's journalctl shows the value once), so this checks matching
    only. systemd 249's repeated array links are pinned by file.rs's
    crafted-file tests instead.
- **Live checks.** The release's own `systemd-journald`, run standalone
  and fed by `systemd-cat`:
  - the last 500 entries of the local files after a flush to persistent
    storage. On the 22.04 and 26.04 images the archived runtime file
    stayed in /run with copies of the flushed entries; that was the
    flushed-copy case below;
  - a match by identifier;
  - following 200 entries across a `journalctl --rotate`, next to
    `journalctl -f`. Both followers must print a marker before the
    numbered writes start, and must have all 200.
- **Strictness.** A check fails if:
  - its expected side is empty;
  - the entry counts differ;
  - any command fails;
  - the flush didn't produce a persistent journal;
  - the rotation didn't archive a file.
- **Files from before an upgrade (compat.sh).** A host upgraded from
  20.04 or 18.04 keeps its archived journal files, and journalctl reads
  them. `ubuntu:20.04` (systemd 245.4) and `ubuntu:18.04` (237) write
  them:
  - their journal-remote writes gen-export's entries, compressed and with
    `--compress=no` (these releases have no per-file compression or hash
    choice);
  - their journald, standalone, writes 300 entries across a rotation
    into a persistent journal and is stopped cleanly.

  Then each supported release's container reads them with its own
  journalctl:
  - each old fixture alone, with compare.sh's fixture checks;
  - an old fixture merged with a later generation of entries from the
    release's own journal-remote, as one `-D` directory, so `-u` last 500
    and the page back reach into the old file;
  - an upgraded host: the old journal directory and machine id in place,
    the release's journald started on it and writing across a rotation,
    and the local journal read whole, last 500, by identifier and by
    `-u` (the fixtures copied into the machine directory, so no
    `--file`).
- **Corruption, crafted** (file.rs's `crafted` builder lays out a
  regular, unkeyed file object by object; 60 new tests in file.rs and
  query.rs): object sizes of zero, below the header, past the arena and
  overflowing; links backwards, to themselves, into their own object and
  outside the arena, in entry-array and hash chains; offsets near
  `u64::MAX`; a file cut inside its header and inside its arena; hash
  tables with no size, a size of part items, past the arena, inside the
  header, with an overflowing offset, and with an object in the wrong
  bucket; LZ4 claims past the format's ratio, a zstd window of 2^41, an
  xz dictionary past the memory limit, zstd frames without a content
  size, decoding to another length or claiming more than their bytes
  hold; every single-byte corruption and every truncation of an LZ4,
  zstd and xz payload; entries without items, without a seqnum, with a
  part item or overlapping the next; an object appended after open. At
  the query level: a bad entry in the middle (unfiltered, matched, and
  following), a bad field, a truncated file, a failed lookup in one
  group, the error cap, entry counts that overflow when added, the
  output and held limits (newest kept by a tail across files, oldest by
  a follow), empty payloads repeated, a failing object shared by many
  entries, and a file list that changes while opening.
- **Corruption, fuzzed:** `proto-journal fuzz` (fuzz.rs) on the stable
  toolchain. Seeds: the 21 matrix fixtures (300 gen-export entries
  through each release's journal-remote, each writer variant), cut to
  their last object. Each case applies one to four mutations: an object
  field or item (8 or 4 bytes) set to a value a corrupt file holds (zero,
  one off, near overflow, another object's offset, the file's length), a
  type or flags byte, a header field, a cut, a splice from any seed, or
  random bytes. Then it reads as hash-check, decode-all and `tail` do
  (unfiltered, `-u demo.service` under a random release, an AND/OR match,
  a page back, and a follow reading the file from its start). Bounds: a 10 s watchdog per case, RLIMIT_AS
  of 3 GiB for the run, peak resident memory per case (VmHWM, reset per
  case), all in a `systemd-run` scope; built with overflow checks and
  debug assertions. A case reproduces from `--seed` and `--start`.
- **Reading while journald writes:** `stress.sh` runs the release's own
  journald standalone in a capped container (1 MiB files, a rotation
  every half second, nothing vacuumed, no rate limit). Four writers log
  20,000 numbered lines each (short, 3 KB compressible, random, ANSI)
  while the reader takes `tail -n 300` snapshots, filtered and not, runs
  decode-all and hash-check, and follows. With journald idle, a check
  fails if a read crashed or hung, if any snapshot entry differs from
  journalctl's final view (all fields, cursor included), if a snapshot
  that reported no error skips a writer's number, if the follower
  doesn't equal journalctl, or if the idle tail differs.
- **A real Saltbox install,** read-only (maintainer, 2026-11-08), with
  the same tools:
  - hash-check and decode-all over the local journal;
  - `tail -u`, a page back, and an unfiltered tail against journalctl for
    a few services;
  - timing and memory.

  This file records the method only. The outcomes and measurements went
  to the maintainer, not into the repository.

**Result:**
- **Matrix: 135 of 135 checks pass with 0 mismatches**, value for value
  in each field's JSON form, cursors included. The writers produced these
  flags:
  - **22.04:** keyed + zstd (0xc), and unkeyed (0x8). systemd 249 ignores
    the per-file compression variable, so xz and lz4 fixtures exist only on
    the newer releases.
  - **24.04 and 26.04:** compact + keyed + zstd (0x1c), regular (0xc),
    unkeyed (0x18), xz (0x15), lz4 (0x16), and old style (0x2).
- **Files from before an upgrade: 184 of 184 compat.sh checks pass with
  0 mismatches**, for 20.04 and 18.04 files on 22.04, 24.04 and 26.04,
  and on the 26.10 canary. No reader change was needed:
  - 237's and 245's files have a 240-byte header (no
    `data_hash_chain_depth` or tail-entry fields) and flags 0x2 (lz4) or
    0x0, never keyed. The reader reads only fields up to offset 184 and
    requires 208 bytes, the size journald itself accepts
    (HEADER_SIZE_MIN).
  - 237 and 245 link an entry carrying `SAME=again` twice into that
    value's entry array (n_entries 856 for 428 entries), and journalctl
    prints the value twice. Both readers agree, so the repeat handling is
    now checked against real files too, though not yet a 249 journald's.
  - On the upgraded host, 255 and 259 refuse to append to the old active
    `system.journal` (a writable open needs the current header size,
    journal-file.c) and rename it to an archived `.journal~` file; 249 appends
    to it and archives it later, leaving one file written by both
    releases. Both shapes read exactly.
  - Harness findings, not reader ones: standalone, 237, 245 and 255 were
    seen to keep writing to /run after `journalctl --flush` until the
    next rotation, and 237's `journalctl --rotate` needs PID 1. compat.sh
    sets the flushed flag and rotates with SIGUSR2. 249's journald split
    some 3000-byte `systemd-cat` lines into several entries, a different
    number each run; both readers showed the same entries.
- **26.10 canary:** `ubuntu:26.10` (systemd 261.3) read under 26.04's
  row passes all 45 compare.sh checks: same flags, `-u` rule and JSON.
- **`.journal~` files:** unclean shutdowns leave them. journalctl reads
  them, and so does this reader.
- **Mismatches found and fixed along the way**, each now pinned by the
  matrix:
  1. **`_BOOT_ID`:** journalctl prints it once, from the entry header, and
     skips the data field of the same name (logs-show.c output_json).
  2. **Flushed copies:** after a flush, the archived runtime file in /run
     holds copies of entries under another sequence-number id. sd-journal
     shows them once and keeps the copy from the file it opened first
     (/run before /var). The merge does the same.
  3. **26.04's `-u` rule changed inside a stable point release.** From
     systemd 259.1, the "messages from PID 1" branch matches
     `_SYSTEMD_CGROUP=/init.scope` instead of `_PID=1`; the v259 tag
     itself still has `_PID=1`. **So an Ubuntu stable update can change
     journalctl's unit match within an LTS.** Only the container matrix
     caught it.

     The generated entries now tell the two rules apart: PID 1 outside
     init.scope, and an init.scope helper that isn't PID 1.
- **The review hardened the harness.** The first harness could pass on two
  empty streams, and compared values as flattened bytes. The stricter one
  above still passes everywhere.
- **Corruption and concurrency (2026-11-08),** first on the reader
  before the newest-first rework, then ported onto it (below): the
  matrix still passes 135 of 135 with every change below. Found and
  fixed, each pinned by a crafted-file test:
  1. **A panic:** an OR term's entry counts, read from the file, were
     summed with `+`; counts near `u64::MAX` overflowed (a panic with
     overflow checks, a wrong driver choice without). Found by the fuzz
     harness in its second round (seed 4, case 1064); the counts now
     saturate.
  2. **Memory without bound:** one DATA object shared by every entry
     (`_TRANSPORT=journal`), its size grown to the arena's end: each
     record held a copy, and the merge cloned them. `tail -n 300` of an
     8 MiB fixture took 2.7 GB and aborted under a 1 GiB address-space
     limit; now 57 entries, the limit reported, 0.4 GB.
  3. **An allocation abort:** an LZ4 payload claiming 700 MiB from 80
     bytes was allocated before decoding; under a 600 MiB address-space
     limit the read aborted. Now refused by the 255x rule.
  4. **Silently nothing:** a hash table whose size isn't whole items sent
     lookups to the wrong bucket: `--match` printed nothing, with no
     error. Now an error.
  5. **One bad entry hid a file:** a single unreadable entry dropped the
     file's matches (`-u demo.service` printed 0 of 150). Now it is
     skipped and reported.
  6. **Wrong data from zstd:** a frame decoding to another length than
     it declares was shown as good data (ruzstd checks neither the size
     nor, without `hash`, a checksum).
  7. **Work without bound** (from code review, crafted): links into an
     object's own body and buckets sharing a chain made walks quadratic;
     a chain of objects with one hash decoded each in full on a lookup;
     overlapping entries each read the rest of the file; an entry
     listing an empty object millions of times read without end; a
     failing zstd frame decoded a window unseen. All now bounded, as
     above.
  8. **Live files:** an object appended after open, named by a link read
     later, was reported as corrupt; the file list could change between
     listing and opening (a rotation), dropping the archived file's
     entries without an error. Both fixed (re-stat, list again).
- **Fuzzing:** about 118,000 cases over seven seeds in about 85 minutes
  (each run one CPU, 4 GiB, 3 GiB address space), the later rounds
  after each fix. One panic (1 above), no hang (slowest case 0.24 s), no
  case past 9 MiB of resident memory over the loaded seeds. About three
  quarters of the cases reported an error, so the mutations reach the
  checks.
- **Reading while journald writes,** final run, every release: 80,000
  lines, 108 to 211 files, 30 to 60 snapshot pairs; every snapshot entry
  equal to journalctl's final view, no gap in any snapshot, the follower
  equal to journalctl, the idle tail equal, nothing reported. Runs before
  the last two fixes reported transient errors (a file renamed away
  between listing and opening; a file still being created: a short
  header, a hash table past the arena as first read), never wrong data.
  journald 249 wrote up to 24 lines twice across rotations, 255 up to two;
  journalctl shows both copies, and so does the reader.
- **Ported onto the newest-first reader and the follow rework** (the
  same day, on eeb5845): the tests (103), the matrix (compare.sh, 135 of
  135), compat.sh (138 of 138, the default releases), follow.sh (every
  scenario on the three releases, 0 lost and 0 duplicated) and a
  9,000-case fuzz run over three seeds (no panic or hang, slowest case
  0.11 s, no case past 9 MiB) all pass. The tail and page back were
  checked read-only on a real Saltbox install (the outcome went to the
  maintainer). stress.sh passes every check on 24.04 and 26.04, and on
  22.04 every check but one: the follower printed 80,000 lines, and
  journalctl 80,011. journald 249 wrote 11 lines twice across a
  rotation, and the follow rework's flush-copy rule (`is_old`: the same
  boot, monotonic time, realtime and XOR hash as an entry shown) most
  likely drops the second copy, where journalctl prints both; the first
  stress round, before that rework, printed both. Not yet confirmed
  entry by entry.
- **Cost, relative** (the figures went to the maintainer). Measured
  wall time and peak RSS (wait4 `ru_maxrss`), the median of repeated runs,
  and `pread` counts from perf's syscall tracepoints:
  - **Last 500 of a quiet or a busy unit, and one page back:** faster
    than journalctl and sdjournal.
  - **Memory:** less than journalctl, about sdjournal's.
  - **Idle follow:** more CPU than either before the follow rework
    below; after it, in journalctl's range.
  - **Static binary (from the build, not the host):** about 240 KiB of
    code, this crate with its tools and decompressors. sdjournal added
    about 640 KB; a journalctl child adds nothing.

  The first version was slower than both on a busy unit, and used
  several times sdjournal's memory. Profiling showed `pread` calls
  dominating, and that each file decoded up to N whole records of which
  most then lost the merge. What changed, in order of effect:
  1. **Merge on locations, decode only the N shown.**
  2. **Walk lazily, newest first, across files** (`newest`, as
     journalctl's `-n` does): a chain's arrays are found from their 24-byte
     heads, then items are read in chunks from the end. A file's walk goes
     no further back than the entries it contributes.
  3. **One `pread` per object:** an entry or DATA object is read with a
     1 KiB guess, and only a larger one needs a second read.
  4. **Each DATA object read once while decoding,** shared between the
     records that carry it (most of a unit's fields repeat).

  Tried and not kept: bisecting the chain for `--before`. For a unit, a
  page back, and even its oldest page, stayed within a few milliseconds
  of the tail, so it would add code for nothing measurable. Without a
  unit filter a page far back is slow (every newer entry of every file is
  read and skipped); `sb logs` always pages a unit, so that stays open. A
  seqnum bisect, where the cursor's seqnum id is the file's, would fix it.
  Also not kept: the compact format's
  `tail_entry_array_offset` hint (finding the arrays from their heads costs
  a few reads per term; that setup is a small part of the total).
  Follow reopened every file on each wake until the rework below.

**Follow rework** (`tail -f`):
- **Before:** every inotify wake reopened every journal file and walked
  each group's match list backwards (`Journal::newer`), stopping at the
  first entry not newer than the last shown.
- **Now** (query.rs `Follow`, file.rs `entries_since`):
  - files stay open, keyed by inode, each with the number of entries of
    its main entry list already read and where in the array chain that
    read stopped. A wake reads the header (one pread), then only the new
    array items and their entries, and checks each entry's items against
    the filter's DATA offsets (looked up again only for values not in the
    file yet). The matches are merged as locations (`Candidate`s), as
    the tail merges them. Unlike the tail, they are decoded as they are
    read, each DATA object once per wake, with copies of shown entries
    dropped before decoding: a DATA object that fails to read then gets
    the entry's one-wake retry, where decoding after the merge would
    have lost the entry (the port review found it);
  - every wake reads every open file that isn't archived and read to its
    end: one header pread each, then only what is new. Reading only the
    file an IN_MODIFY names was tried and rejected (the review found it):
    journald raises each file's IN_MODIFY from its own coalesced 250 ms
    timer (`schedule_post_change`), so with two files written at once
    (a `User=` unit's user-1000.journal next to system.journal) the newer
    entry could be shown first and the older one, read later, dropped as
    old. The extra cost is a pread per active file, usually two to four;
  - events only decide whether to rescan the directories first: a create,
    rename or delete of a journal file, a directory appearing, a watch
    dropped, an unknown watch or IN_Q_OVERFLOW rescan, as sd-journal's
    `process_inotify_event` does; other names are ignored. Positions live
    in the open files, so an overflow loses nothing;
  - rotation renames the file: same inode, same position. A file missing
    from one listing but still linked (renamed between the listing and
    the stat) stays open for the next rescan. Vacuum deletes it: the open
    fd is read to its end (or until its error is reported), then closed. Where journald can't
    clear FS_NOCOW_FL on an archived file (btrfs, and overlayfs in the
    containers) it rewrites it as a copy renamed over the name: the copy
    is recognised by its header's file id and keeps the position. A
    deleted file that reads as zeros (vacuum deallocates before it
    unlinks) is no error once it was read to its end;
  - an archived or `.journal~` file read to its end isn't refreshed
    again, once a header read after the seal gave its final count;
  - a file read while it had no entry array yet is read from its head
    later (the matrix caught a cursor kept at offset 0, which lost every
    entry of files created mid-follow);
  - an entry that fails to read is retried on the next wake, then
    reported and skipped;
  - copies from a flush are dropped by time, not by sequence number (see
    below).
- **The old Stop rule and seqnum ids.** A file has one seqnum id: it is
  in the header, and journald rotates rather than append an entry from
  another sequence-number space to a non-empty file. So no seqnum-id
  change happens inside a file. What the rule missed was across files:
  - **systemd 255's flush** copies the runtime journal's entries into
    /var under the runtime file's seqnum id with new, higher seqnums, and
    the originals' boot, times and XOR hash. sd-journal's order calls them
    newer, so the old follow printed every flushed entry twice. So does
    `journalctl -f` 255 when /var/log/journal exists when it starts.
    249's copies get another seqnum id, so neither duplicates there.
  - The follow drops an entry as old when, in the last shown entry's
    boot, its monotonic time is lower (or boot, times and hash equal it);
    across boots it keeps sd-journal's order. That check applies only
    where an already shown entry can be: in files open from the start,
    to entries below the count read right after the tail; in files
    opened while following, to the leading run of old entries (a flush
    writes all its copies before any new entry). After that, the
    position alone decides, so a late file's older entry is still shown.
- **Residual:** an entry linked into a file opened mid-follow, before
  that file's first new entry and in the microseconds between two files'
  header reads in one wake, would be taken for a copy. sd-journal has
  the same window for every file.
- **Declined (recorded):** flush copies appended to a /var file already
  being followed would be shown again. That happens when the flush
  service is restarted (relinquish, then flush) while following. It is
  rare on Saltbox; dropping exact copies of recently shown entries
  would cover it.
- **Corruption and the output limit while following** (reliability
  round): each file reads within the same 256 MiB output limit per poll
  as a tail. A file stops at the entry that doesn't fit, without moving
  past it, so the next wake goes on from there and nothing is lost or
  doubled; an entry larger than the whole limit is skipped, reported.
  The limit is per file, not one for the poll: with one in all, a burst
  in one file deferred the files after it, whose entries could then fall
  before the last one shown and be taken for copies, and an oversized
  entry stayed deferred behind a busy file. An entry must
  end before the next one read, checked before its body is read. The
  errors one wake reports are capped as a query's are. The retry rules
  above (an unreadable entry or a DATA object waited for once, then
  skipped and reported) are unchanged.
- **follow.sh** checks it: one container per release with its own
  standalone journald, as compare.sh does. Each scenario starts both
  followers with `-n 0`, writes markers until both have printed one, then
  numbered entries, and counts lost and duplicated numbers per follower:
  - flush, with /var/log/journal created mid-stream, and flush-var, with
    it there (empty) before the followers start;
  - paced writes 20 ms apart, for latency (receipt time minus
    `__REALTIME_TIMESTAMP`);
  - a burst of 3,000 from one process;
  - two rotations mid-stream;
  - a rotation and `--vacuum-files=1` mid-stream;
  - journald killed (SIGKILL, leaving a `.journal~`), restarted, stopped
    cleanly (SIGTERM) and restarted;
  - both followers stopped (SIGSTOP) while entries, a rotation and more
    create/delete events than `max_queued_events` happen, then resumed:
    an inotify queue overflow;
  - root and uid 1000 writing at the same time, so system.journal and
    user-1000.journal are written together (split).
- **Result (lost / duplicated, of 200 per flush scenario):**

  | Release | Follower | flush | flush-var | others |
  |---|---|---|---|---|
  | 22.04 | journalctl -f | 0 / 0 | 0 / 0 | 0 / 0 |
  | 22.04 | old follow | 0 / 0 | 0 / 0 | 0 / 0 |
  | 22.04 | new follow | 0 / 0 | 0 / 0 | 0 / 0 |
  | 24.04 | journalctl -f | 0 / 0 | 0 / 100 | 0 / 0 |
  | 24.04 | old follow | 0 / 100 | 0 / 100 | 0 / 0 |
  | 24.04 | new follow | 0 / 0 | 0 / 0 | 0 / 0 |
  | 26.04 | journalctl -f | 100 / 0 | 0 / 0 | 0 / 0 |
  | 26.04 | old follow | 0 / 0 | 0 / 0 | 0 / 0 |
  | 26.04 | new follow | 0 / 0 | 0 / 0 | 0 / 0 |

  "Others" covers paced, burst, rotate, vacuum, restart, overflow and
  split. journalctl 259 loses the entries written after the flush when
  /var/log/journal didn't exist when it started: it doesn't watch a root
  directory that is missing. The split scenario hits the two-file race
  only by chance; the unit test
  `follow_keeps_an_older_entry_of_a_file_read_later` pins it.
- **Latency** is journald's, not the reader's: journald raises the
  wake-up (its post-change truncate) on a timer, so both followers saw
  the same p50 of about 140 to 150 ms and p95 of about 260 ms, within a
  millisecond of each other, on all three releases.
- **Cost:** on a real host, idle follow of one unit now costs about the
  same CPU as `journalctl -f`, and a fraction of the old follow's. The
  figures went to the maintainer. In the 3,000-entry burst the follow on
  the first reader used about twice journalctl's CPU (8 to 12 against 3
  to 6 clock ticks), in reading and decoding entries for output. On the
  newest-first reader (one pread per object, each DATA object decoded
  once per wake) it uses the same as journalctl, 4 ticks against 4 to 5.
- **Not covered yet:**
  - **26.04's local-journal coredump term:** `_UID` of systemd-coredump or
    root. The containers have no `systemd-coredump` user, and only
    `--file`/`-D` (which drop the term) were compared. UNVERIFIED.
  - **Unit-name mangling:** names without a dot get `.service`, as
    journalctl's `unit_name_mangle` does for Saltbox's names. Other cases,
    such as `my.app` or the `.slice` disjunction, aren't handled.
  - **The last N walk backwards, as journalctl's `-n` does**
    (`sd_journal_previous_skip`), and follow merges forward
    (`sd_journal_next`). journalctl then prints the last N by walking
    forward again from the oldest of them, seeking each file by seqnum,
    monotonic time or realtime. Where the comparator isn't transitive, that
    second walk can differ from the first; the reader doesn't model it. A
    unit test pins one such case, where the backward walk matches
    journalctl's output as read from its source; the matrix never hit one.
  - **A zero entry-array item** (a slot never written, after an unclean
    shutdown) is skipped. That is our choice: sd-journal's unfiltered walk
    skips a bad item (`generic_array_get`), but its match path bisects
    (`generic_array_bisect`) and cuts the array short there, or with v249's
    chain cache drops the file. The first version ended the chain there.
    Following waits one wake for such a slot (it may be mid-write), then
    skips it the same way. UNVERIFIED against a real file with one.
  - **Repeated array items from a real 22.04 journald:** systemd 249's
    `journal_file_append_entry` keeps duplicate items when it links an
    entry (its source). journal-remote 249 didn't write any in the
    matrix. Files from 237 and 245 do have them, and read exactly
    (compat.sh), as do the crafted-file tests: a repeat is skipped but
    counts as a link. UNVERIFIED against a 249 journald's own file.
  - **Sealed (FSS) files from old releases:** not written by the matrix.
    Their TAG objects sit outside the entry arrays the reader walks.
    UNVERIFIED.
  - **The fuzz harness isn't coverage-guided.** afl.rs (cargo-afl) would
    mutate the same seeds with coverage feedback; it needs the AFL++
    toolchain on the host. Not tried.
  - **The output limit is 256 MiB per query.** An entry larger than that,
    which only a coredump stored in the journal could be, isn't shown;
    journalctl prints it. A zstd frame needing a window over 128 MiB
    (ruzstd's default) isn't decoded either.
  - **A follow's output limit is per file:** a wake holds at most the
    limit for each file with new entries (in practice the active system
    and user files), not one limit in all.
  - **A lookup that fails while following** (`Followed::resolve`, from
    the follow rework) is still passed over without a report: the value
    is looked up again on the next wake.
  - **A bogus item that names a real older entry** (a stale slot) passes
    the plausibility check; leaving its array then reports the valid
    entries above it as overlapping, and the rest of that array is lost
    (reported, never shown wrong). A one-item lookahead in `EntriesRev`
    would refuse just the lone bad item. Not built.
  - **An array overwritten with garbage** costs a re-stat and an error
    per implausible item, bounded by the entry count but slow on a large
    file; re-stating once per chunk, or leaving the array after a few in
    a row, would cut it.
  - **`entries_since`** (following) still stops a file at an
    out-of-order array, retried once, then reported; it doesn't go on
    past it as the tail's walk does.
  - **The open-and-list-again rule** applies to the tail, not to the
    follow's rescans, which match files by inode already.
  - **Field names aren't validated.** journalctl's `output_json` refuses
    an entry whose field name isn't valid; this shows it, made valid
    UTF-8.
  - **Entries the merge drops** (not beyond the last one taken, as
    sd-journal drops them) aren't counted in the errors.
  - **The JSON output** turns a binary value into one `serde_json::Value`
    per byte, about 32 bytes each, for the one record being printed. The
    TUI won't use this path.
  - **hash-check and decode-all** decode each object up to DATA_SIZE_MAX
    with no query limit: test tools, not `sb logs`'s path.
  - **The live stress** covers rotation and appends, not journald killed
    mid-write or a full disk.

**Future releases** (systemd NEWS v259 to main, 263 in development, and
logs-show.c, journalctl-filter.c and journal-def.h at v259.9, v260, v261,
v262 and main, read 2026-11-08):

| | systemd | New incompatible flags | Format or reader changes | `-u` rule | `-o json` | Needs |
|---|---|---|---|---|---|---|
| 26.04 | 259.9 | none | none | InitScope (259.1 on) | 259.1 shape | its row |
| 26.10 | 261.3 | none | none | the same (identical to main) | the same (v260 moved it into `journal_entry_to_json`, unchanged) | no row: sb refuses interim releases. The canary passes. |
| next LTS (28.04) | 262 or later | none on main | v262: readers salvage intact entries from active files with a truncated header or tail; main skips a wider set of corruption errors per entry | the same so far | the same so far | a row like 26.04's, plus crafted-file tests for the salvage case (dirty `.journal~` files) |

- **Incompatible flags:** main still defines the same five (xz, lz4,
  keyed hash, zstd, compact). Its new compatible flag
  (`SEALED_CONTINUOUS`) doesn't change how entries are read.
- **The varlink journal API:** v260's journalctl serves
  `io.systemd.JournalAccess.GetEntries` on
  /run/systemd/io.systemd.JournalAccess. Its parameters are units, user
  units, uid, namespace, priority and limit (at most 10000), with
  journalctl's own `-u` rule; v262 adds `follow`. It takes no cursor, so
  it can't page back. 22.04 to 26.04 don't have it. It doesn't replace
  the reader for the supported releases, but on 28.04 it would be an
  exact source to cross-check against.
- **Other changes:** v259 makes persistent storage the default, and v262
  moves sealing to OpenSSL. Neither changes what the reader sees.
- **Adding a release** is one `RELEASES` row: VERSION_ID, systemd major,
  `UnitRule`, and whether `-o json` prints the seqnum. The scripts take
  their default lists from `proto-journal releases`. A release needs
  code only for a new rule (a `UnitRule` variant), a new JSON shape or a
  new incompatible flag (file.rs). An image whose systemd major differs
  from its row fails the matrix; a canary only notes it.

**Verdict:** adopt with changes. The reader is small and exact on all
three supported releases in the matrix, at about a third of sdjournal's
binary size.

_Reviewed by the maintainer (2026-11-08): adopted, as its own crate,
`sb-journal`, which sb-host wraps (host spec, decision 6). Before the
crate, several agents in parallel look for performance, reliability and
feature improvements for Saltbox's use; their results are added here._

Before it becomes a crate:
1. Files kept open while following: done (Follow rework above).
2. The container matrix (compare.sh and compat.sh, with an interim-release
   canary) in CI on a schedule against current images, since a stable
   update can change the rules (259.1).
3. Coverage-guided fuzzing (afl.rs) on top of the stable harness, run
   on a schedule.
4. The usual bar: coverage and mutation testing.

**Map impact:**
- **Stack table:** a row for the journal reader, with sdjournal,
  systemd-journal-sdk and the libsystemd crate rejected for the reasons
  above. It is our own code, on siphasher, lz4_flex, ruzstd and lzma-rust2,
  with rustix's inotify. It replaces sb-go's `journalctl --cursor` polling
  and proto-logs' `journalctl -f` child.
- **Modules:** `host` keeps the systemd journal, as a reader rather than a
  `journalctl -f` stream, and the `tui` log viewer consumes it. The reader
  lives in its own crate, `sb-journal`, which host wraps (host spec,
  decision 6).
- **ADR:** one records the divergence from the library-first rule, with
  the evidence above and the upkeep it brings: a `RELEASES` row per new
  LTS, checked by the matrix.
