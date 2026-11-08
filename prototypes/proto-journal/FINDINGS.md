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
- **rustix@1.1.6 `fs::inotify`:** following.
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
  - journalctl's `-u` rule per release;
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
  - `gen-export`.
- **compare.sh:** the release matrix.

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
  - **Idle follow:** more CPU than either. It was measured before batch
    draining and before the lazy walk below.
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
  Follow still reopens every file on each wake.
- **Not covered yet:**
  - **26.04's local-journal coredump term:** `_UID` of systemd-coredump or
    root. The containers have no `systemd-coredump` user, and only
    `--file`/`-D` (which drop the term) were compared. UNVERIFIED.
  - **Unit-name mangling:** names without a dot get `.service`, as
    journalctl's `unit_name_mangle` does for Saltbox's names. Other cases,
    such as `my.app` or the `.slice` disjunction, aren't handled.
  - **The `newer()` stop rule** assumes order within one file. Across a
    seqnum-id change inside a file, which is rare, it could stop early.
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
    UNVERIFIED against a real file with one.
  - **Repeated array items from a real 22.04 journald:** systemd 249's
    `journal_file_append_entry` keeps duplicate items when it links an
    entry (its source). journal-remote 249 didn't write any in the
    matrix, so no real file with them was read. The crafted-file tests
    cover the reader's handling: a repeat is skipped but counts as a link.
    UNVERIFIED against a real file.
  - **Crafted-file tests so far** cover the entry-array chain read back
    across arrays and read chunks, repeats, a zero item, a decreasing
    offset, the link count and an unknown format flag. Other
    corruptions (bad sizes, bad links, overflowing offsets) aren't tested
    yet, and there is no fuzzing yet.

**Verdict:** adopt with changes. The reader is small and exact on all
three supported releases in the matrix, at about a third of sdjournal's
binary size.

_Reviewed by the maintainer (2026-11-08): adopted, as its own crate,
`sb-journal`, which sb-host wraps (host spec, decision 6). Before the
crate, several agents in parallel look for performance, reliability and
feature improvements for Saltbox's use; their results are added here._

Before it becomes a crate:
1. Files kept open while following.
2. The container matrix in CI on a schedule against current images, since
   a stable update can change the rules (259.1).
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
  the evidence above and the upkeep it brings: a row per new LTS, checked
  by the matrix.
