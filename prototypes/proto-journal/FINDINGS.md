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

**What was built:** the reader is about 930 non-comment lines (hash.rs, file.rs, query.rs; about 1,080 with their tests), plus about 515 in main.rs for the CLI and test tools:
- **hash.rs:** siphash through siphasher, plus a port of lookup3's
  `hashlittle2`. Why not `hashers`' `Lookup3Hasher`:
  - no release since 2018;
  - its output halves are swapped against systemd's;
  - it returns 0 for empty input.
- **file.rs:** one journal file read with `pread` only. No mmap and no
  `unsafe`, so a file truncated under the reader is an error, not SIGBUS.
  - Every offset taken from the file is checked: alignment, arena bounds,
    type, size and overflow.
  - Chain links must point forward, which ends any cycle, and entry-array
    items must never decrease.
  - Entry arrays may repeat an offset: systemd 249 links an entry once
    per item, so an entry carrying the same value twice appears twice.
    The repeat is skipped, as sd-journal skips it, but still counts as a
    link against the entry count, as systemd's `n_entries` does.
  - Data and entry objects are capped at systemd's own limits from
    journal-def.h (DATA_SIZE_MAX 768 MiB, ENTRY_SIZE_MAX 770 MiB).
  - xz, lz4 and zstd payloads are decoded with their output capped at
    DATA_SIZE_MAX. xz also runs under a 64 MiB decoder-memory bound of
    our own (journald's xz dictionary is 1 MiB).
  - An unknown incompatible flag is an error naming the file.
- **query.rs:**
  - `RELEASES`, one row per supported release: its VERSION_ID, systemd
    version, journalctl's `-u` rule (`UnitRule`) and `-o json` shape;
  - the local machine's files in sd-journal's search order;
  - the last N picked by walking backwards across files, as
    journalctl's `-n` does, and new entries by a forward k-way merge, both
    with sd-journal's comparison, tie rule and equal-entry skip;
  - errors per file and per entry: they are reported, and the rest is
    still shown;
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
- **compare.sh, compat.sh and checks.sh:** the release matrix, files from
  before an upgrade, and the checks both share.

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
  - **One unreadable entry in a walk** ends that file's walk for the
    query: the entries it gave before stay, as sd-journal drops a file it
    can't iterate, and the error is reported. sd-journal would skip the
    bad entry and go on with that file.
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
  - **Crafted-file tests so far** cover the entry-array chain read back
    across arrays and read chunks, repeats, a zero item, a decreasing
    offset, the link count and an unknown format flag. Other
    corruptions (bad sizes, bad links, overflowing offsets) aren't tested
    yet, and there is no fuzzing yet.

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
3. Fuzzing and crafted-file tests for file.rs.
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
