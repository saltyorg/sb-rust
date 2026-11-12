# sb-journal: tasks

Plan: [tasks/journal/plan.md](plan.md) (its API names, traceability
table and risks). Spec: [docs/specs/journal.md](../../docs/specs/journal.md).
Closed at Checkpoint C (maintainer, 2026-11-11). Its open follow-ups are
in [tasks/todo.md](../todo.md), "Follow-ups from the journal reader".

Every task follows CLAUDE.md:
- the skills;
- TDD: each ported test passes on moved code before adapting, and each
  new case fails first;
- `scripts/check.sh fast` after edits, `task` before handing back,
  `full` before committing;
- the code-reviewer on each commit, plus the library-reviewer where I/O,
  process or terminal code changes;
- `Library-check:` and `Review:` lines.

sb runs as root only with `SB_LOG_DIR` in `target/` or a temporary directory.
Container scripts run with a unique `SBJ_PREFIX`. A matrix mismatch that
isn't a listed Difference stops for the maintainer. Nothing is pushed.

## Task 0 (before Task 1): time the mutation run (done)

- [x] One mutant took 85 s judged by every crate's tests (59 s build,
      25 s tests) and 2 s judged by its own crate's.
- [x] ADR-012: per-crate mutation scope in `check.sh task`
      (`MUTATION_TESTERS`), with the bar unchanged.
- [x] Measured in place with cargo-mutants' own copy of the tree; no
      worktree was needed. Its outputs were removed,
      and `df -h` showed /opt at 62%.

## Task 1: crate skeleton, ADR-013, `hash.rs`, file objects and codecs

**Description:**
- Create `crates/journal` (sb-journal), with:
  - the `testing` feature and the self dev-dependency;
  - a `MUTATION_TESTERS[sb-journal]=""` entry in check.sh (ADR-012);
  - module-level `cfg_attr(..., expect(dead_code))` for code with no
    user yet, each with a temporary CONSTRAINTS Exceptions row (plan,
    "Code with no user yet").
- Port `hash.rs`, and from `file.rs` the following, all pread only with
  every offset and size checked:
  - the header and objects;
  - the data hash table and lookups;
  - the codecs and `Limits`;
  - `OverLimit` and `KnownBad`.
- Wire it in:
  - sb-host depends on sb-journal;
  - deny.toml gets `{ crate = "sb-journal", wrappers = ["sb-host"] }`;
  - the capability map gets the `sb-journal` row under `host`, and
    host's corrected dependencies.
- Write ADR-013 (owning the reader, the divergences of decisions 1 and
  2, the upkeep per release).

**Acceptance criteria:**
- [x] hash.rs's test and file.rs's header, object, hash-table, codec and
      limit tests pass in the crate: 31 of file.rs's 57 ported
      unchanged, plus 2 walk-free stand-ins (see Task 2). Each crafted case is
      built with `testing`'s builder.
- [x] `Limits::default()` is 256 MiB; `with_output` caps at
      `DATA_SIZE_MAX`. xz refuses a decoder needing over 64 MiB, and
      zstd a window over 128 MiB (Differences 5, 6). A bad hash table
      fails lookups and not the file (Difference 9).
- [x] `check.sh fast` gains `cargo clippy --workspace --lib --bins --
      -D warnings`, which lints the plain build. floor-guard.mjs flags
      `cfg_attr(..., expect(` too.
- [x] `cargo clippy -p sb-journal --lib -- -D warnings` and
      `cargo clippy -p sb-journal --all-targets -- -D warnings` both
      pass. `cargo tree -e features -p sb-journal` confirms that the self
      dev-dependency unifies `testing`, and the plan's UNVERIFIED mark
      is dropped. There is no `#[allow]`.
- [x] cargo-deny accepts sb-journal's dependencies through sb → sb-host:
      siphasher, lz4_flex, ruzstd 0.9.1, lzma-rust2 without
      `optimization`, hex and rustix.

**Verification:**
- [x] `cargo test -p sb-journal`; `cargo deny --manifest-path
      crates/sb/Cargo.toml check`; `scripts/check.sh task`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 0
**Files:** `crates/journal/{Cargo.toml,src/lib.rs,src/hash.rs,src/file.rs,src/testing.rs}`,
`crates/host/Cargo.toml`, `Cargo.toml`, `deny.toml`,
`docs/specs/capability-map.md`, `docs/decisions/ADR-013-*.md`,
`CONSTRAINTS.md`, `scripts/check.sh`, `scripts/floor-guard.mjs`
**Scope:** M (a large port, but one subsystem)

## Task 2: entry arrays, chains and `EntriesRev`

**Description:** Port the rest of file.rs: entry arrays and chains, the
inline entry, repeats, `all_entries`, `entries_since` and `EntriesRev`.

**Acceptance criteria:**
- [x] file.rs's remaining tests (24, plus the 2 originals the stand-ins
      replace: 26) pass. The commit records that the 57 (31 + 26) are
      all ported.
- [x] A zero item is skipped (Difference 8). A stale item naming an
      older entry ends its array, and that is reported (Difference 10).
- [x] Task 1 ported 31 of file.rs's 57 tests unchanged and deferred 26
      that need the entry-array side: 24, plus
      `the_crafted_journal_reads_back` and
      `a_bad_hash_table_size_fails_lookups_not_the_file`, which Task 2
      ports whole, walks included. Their walk-free stand-ins from Task 1,
      `the_crafted_journal_reads_back_without_the_walks` and
      `a_bad_hash_table_size_fails_lookups_and_entries_still_read`, then go.
      `file_with_chain` and `ENTRY_ARRAY_HEADER` come with them.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 1
**Files:** `crates/journal/src/{file.rs,testing.rs}`, `CONSTRAINTS.md` (an exception row if still needed)
**Scope:** S

## Task 3: walks, rules and `page` (Tail, Before)

**Description:**
- Port the newest-first walk (`Walk`, `newest`, the merge).
- Port `RELEASES` with `UnitRule`.
- Add the API names of the plan for queries and records:
  - `Query`: the unit rule with decisions 1 and 4's coredump table,
    `field`, and fallible `and`/`or`;
  - `Uid`, `Release`, `UnitName`, `FieldName`, `Cursor`, `BootId`,
    `Record`.
- Add `Journal::open` and `page` for `Tail` and `Before`, with
  `ReadError`/`ReadErrors`. `Limits` gets its first user, so file.rs's
  dead-code expectations and their Exceptions rows go.

**Acceptance criteria:**
- [x] query.rs's tail, merge and rule tests (25; plan, Traceability) pass. They cover:
      newest, ties, non-transitive cases, flushed copies, 249 double
      writes, limits, skipping an oversized first entry, and walking
      past an out-of-order item (Differences 5, 15, 17). An entry cut off
      while linking shows once in the tail (Difference 12).
- [x] The coredump table holds for every release, source and uid,
      `Some(0)` included: on 249 and 255 always `_UID=0` (Difference 1).
      `UnitName` mangles "no dot", refuses globs and `.slice`
      (Difference 7). `Query::and` refuses past 64 groups.
      The cursor tests round-trip sd-journal's cursor text, and a
      journalctl `__CURSOR` parses.
- [x] `ReadError`'s `Display` escapes paths and field names (L5).
      `ReadErrors` shows ten distinct errors, then `omitted`.
- [x] `Record`:
      - a 5 KiB field comes back whole as bytes (Difference 3);
      - `fields()` yields an invalid field name raw (Difference 14);
      - `realtime()` is `None` out of jiff's range;
      - `message()` is `None` without `MESSAGE`.
- [x] Static assertions: `Journal` is `Send + Sync`; `Page`, `Record`,
      `Query`, `Position` and `Limits` are `Send + 'static`.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [x] library-reviewer (directory listing and file opening) and
      code-reviewer on the commit.

**Dependencies:** Task 2
**Files:** `crates/journal/src/{walk.rs,rules.rs,lib.rs,file.rs}`, `crates/journal/Cargo.toml` (itertools, jiff), `CONSTRAINTS.md`
**Scope:** M

## Task 4: `After`, exact `more_*`, `PageEnd`, safe opening, `MachineId`, the pread bound

**Description:** New behaviour, test-first:
- `Position::After`: a forward match walk;
- exact `more_older`/`more_newer` for Tail, Before and After;
- `PageEnd`;
- opening with `O_NONBLOCK|O_NOCTTY|O_CLOEXEC`, an `fstat` regular-file
  check, and `O_NOFOLLOW` on the file (ELOOP reported) (L2);
- `MachineId` (L1);
- the injectable roots and opener, and a per-`Journal` pread counter (no static), under `testing`.

Opening goes in `open.rs`, a module the spec's Project Structure now
lists.

**Acceptance criteria:**
- [x] Crafted tests:
      - a forward page;
      - `more_older` and `more_newer` for every `Position`, at both
        ends, meaning a *matching* entry beyond the page (also when the
        next raw entry doesn't match);
      - an empty page's `PageEnd`.
- [x] Opening:
      - a FIFO doesn't block;
      - a symlinked file is an open error and a symlinked directory is
        read (Difference 4);
      - an invalid machine id is refused;
      - open errors get one entry per file, outside the ten cap.
- [x] Each `page` call reads the file list again: a file replaced or
      deleted between two calls is seen by the second one, and the
      opener's counts show every open closed (Difference 11).
- [x] The pread counter bounds a crafted fixture's tail, page back and
      forward page. The figure is stated in the test and in the plan.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 3
**Files:** `crates/journal/src/{walk.rs,lib.rs,open.rs,testing.rs}`, `docs/specs/journal.md` (Project Structure)
**Scope:** M

## Task 5: the comparison tools and container scripts on the crate

**Description:**
- Move the prototype's CLI into `examples/journal.rs` on `testing`, as a
  thin wrapper with clap. Its commands: tail, pages both ways, compare,
  gen-export, hash-check, decode-all, releases.
  - The compare, hash-check and decode-all verdicts become pure
    functions in testing.rs, with unit tests.
  - The example takes `--release` and `--coredump-uid`, since sb-journal
    has no sysinfo or nix.
- Move compare.sh, compat.sh and checks.sh to `crates/journal/tests/`.
  - The scripts fill those flags:
    - `--release` is the row the image runs under, so a canary passes
      its mapped row, not its own `VERSION_ID`;
    - `--coredump-uid` comes from `id -u systemd-coredump`, and the
      flag is left out when the user doesn't exist.
  - They gain checks for the forward page, the `more_*` flags against
    whether journalctl has entries beyond the page, decision 4's
    coredump row, and the pread bound on a matrix fixture.
- Rename `PJ_BIN` to `SBJ_BIN`, and point the CI workflow at the crate's
  example.

**Acceptance criteria:**
- [x] compare.sh and compat.sh pass on 22.04, 24.04 and 26.04 against
      the example, with the listed differences pinned.
- [x] The coredump row runs with `systemd-coredump` installed and checks
      the local rule. If it can't run, Checkpoint A doesn't pass:
      success criterion 3 goes unmet and the maintainer decides.
- [x] Forward pages (`After`, journal spec decision 10, UNVERIFIED
      until these run):
      - on 26.04, against `journalctl --after-cursor`, page by page;
      - on 22.04 and 24.04, the same, except for Difference 18 (a
        vanished or non-matching cursor entry: journalctl there drops
        the first entry after it; on a file whose clock steps back, its
        older bisect lands elsewhere). The runs showed this on 22.04
        only: 24.04's 255.4 carries systemd-stable v255.1's
        `sd_journal_test_cursor` backport, and matches sb (spec,
        Difference 18, amended);
      - in a 249-written file whose clock steps back, against 26.04's
        journalctl reading it (the emulated bisect's landing).
- [x] Boot order (journal spec decision 11, UNVERIFIED until this runs):
      on 24.04 and 26.04, two boots of one machine whose realtime ranges
      overlap (a clock set back across a reboot), checked against
      journalctl page by page: the tail, pages back and pages forward
      show all of the boot with the older newest tail first; on 22.04,
      the same files interleave by realtime.
- [x] Difference 20 (UNVERIFIED until this runs): a matrix fixture of
      two boots in two files, the second boot's clock behind the
      first's, on 24.04 and 26.04, asserting the listed difference
      against the real journalctl: sb's pages forward show every entry
      once in the boot order, where journalctl's pass the entries the
      emulation predicts (paging one at a time from a1, b1 is lost) and
      a page after b1 is [a2, b1] on 26.04 and [b1, b2] on 24.04; with
      clocks that don't overlap, sb's pages are journalctl's. The runs
      corrected the page after b1: [b2] on 26.04 (259.9's stable
      backport prefers the cursor's exact entry, issue #31516), as sb,
      and [a2, b1, b2] on 24.04 (255.1's cursor test, no skip); b1 is
      lost paging from a1 on both (spec, Difference 20, amended).
- [x] The workflow builds the example; actionlint and shellcheck are
      clean.

**Verification:**
- [x] The scripts' runs (logs outside the repo, never committed);
      `scripts/check.sh task`.
- [x] library-reviewer (a CLI doing file I/O) and code-reviewer on the
      commit.

**Dependencies:** Task 4
**Files:** `crates/journal/examples/journal.rs`, `crates/journal/src/testing.rs`,
`crates/journal/Cargo.toml` (clap and serde_json optional; `[[example]]`
entries), `crates/journal/tests/{compare.sh,compat.sh,checks.sh}`,
`.github/workflows/journal-matrix.yml`
**Scope:** M

## Checkpoint A: the reader against journalctl
- [x] Tasks 1-5 pass `scripts/check.sh full`; the matrix is green on
      three releases. (Recorded late, at Task 12: check.sh full over
      3c75519..0fac785 passed with mutants 1231 caught of 1298 viable;
      the matrix was green on 22.04, 24.04 and 26.04 at 0fac785, Task 5.)
- [x] Ratchet re-measured: size, startup, project coverage. `crates/`
      coverage 98.47% of lines and 98.02% of functions, startup 1.17 /
      1.32 ms, binary 11924448 bytes: all within the ratchet (CONSTRAINTS.md
      tolerances; ADR-011's limits stand).
- [x] Review with the maintainer, including plan.md's API names, before
      following (approved 2026-11-11).
- [x] Decide on `is_*` helpers on `ReadError` (`is_over_limit`,
      `is_known_bad`) in place of matching the public `ReadErrorKind`
      (M-ERRORS-CANONICAL-STRUCTS), with the API names. Done
      (maintainer, 2026-11-11): those two, `is_io`, `is_too_large` and
      `is_symlink`, used at every call site in the crate; `kind()` stays
      public for the fields and `ReadErrorKind` `#[non_exhaustive]`.
- [x] Decision 11's machine id on 26.04 (systemd 259.9, v259-stable commit
      2d8b0b6a0aaf, issue #34169): the boot table takes each file's
      machine from its tail entry's `_MACHINE_ID=`, else the header's
      (`BootRule::NewestTailEntryMachine`); 24.04 and 22.04 keep the
      header. Verified by the Task 6 matrix item below.

## Task 6: the follow ported, with the prototype's start

**Description:**
- Port `Follow`, `ArrayCursor` and the follow side of query.rs.
- Move `watch`, `watch_dirs`, `classify` and the rescans from main.rs
  into `follow.rs`, behind `Journal::follow` and `Follower::drain`.
- Keep the prototype's start for now.

**Acceptance criteria:**
- [x] query.rs's follow tests (21, plus `merge` and `is_old`'s 3) and main.rs's four `classify`
      tests pass in the library. The commit records that all 112 are
      ported.
- [x] Differences 2, 12, 13, 16 and 17 (the follow side) each have a
      crafted test (`difference_*` in follow.rs; Difference 2 through
      the follower's own watches and injected roots).
- [x] The follow orders boots as the page does (decision 11), with its
      boot table kept current as files grow: sd-journal reads a file's
      tail again before each step (v255 `next_beyond_location`, a
      changed mtime; v259, a new tail offset) and reorders the table
      (`the_follow_keeps_its_boot_table_current_as_files_grow`).
- [x] follow.sh and stress.sh (moved) pass on the three releases:
      follow.sh 0 lost and 0 doubled in all nine scenarios on each;
      stress.sh every check, the follow 0 mismatches (22.04 showed one
      Difference 12 first copy).
- [x] A matrix fixture with `_MACHINE_ID=` (compare.sh `machines` and
      `machines-untagged`; verified, as predicted, on all three):
      journal-remote's import of two source machines' entries (each
      carrying its `_MACHINE_ID=`, one receiving header machine), with
      overlapping clocks, paged both ways against journalctl on 24.04
      and 26.04: 26.04 interleaves the machines by realtime, 24.04
      orders them by boot; a tail entry without `_MACHINE_ID=` keeps the
      header's on 26.04. The default variant's pread bounds
      (compare.sh `PREADS_*_MAX`) re-measured on 26.04, which now reads
      each file's tail entry's items (tasks/journal/plan.md, "The pread bound"):
      1711, 2393 and 1747, unchanged.

**Verification:**
- [x] `cargo test -p sb-journal`; the two scripts; `scripts/check.sh task`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 5
**Files:** `crates/journal/src/{follow.rs,lib.rs}`, `crates/journal/examples/journal.rs`,
`crates/journal/tests/{follow.sh,stress.sh}`
**Scope:** M

## Task 7: the page-anchor start, the wake fd and `Batch::more`

**Description:**
- `Journal::follow(from: &PageEnd)`: the watches first, positions from
  the anchor, and the first `Batch` through the merge under the limit.
- A non-blocking inotify fd and `wake_fd()`.
- `drain` to EAGAIN, and `Batch::more`.
- `Send` assertions for `Follower` and the fd.

**Acceptance criteria:**
- [x] Writes between a page and its follow lose and double nothing,
      after a full page and after an empty one. `more` is set when the
      limit stops a batch.
- [x] Wake protocol, tested with rustix `poll`:
      - a write makes the duplicated fd readable;
      - `drain` clears it;
      - the duplicate is CLOEXEC and non-blocking.
      The parent-directory watch is tested through the injectable roots.
- [x] follow.sh gains the between-page-and-follow scenario. It and
      stress.sh pass on the three releases: follow.sh 0 lost and 0
      doubled in all eleven scenarios on each; stress.sh every check, the
      follow 0 mismatches.

**Verification:**
- [x] `cargo test -p sb-journal`; the two scripts; `scripts/check.sh task`
      (every check but the floor guard, whose re-targeted assertions the
      commit lists).
- [x] library-reviewer and code-reviewer on the working tree.

- From Task 6, decided here:
  - `ReadErrorKind::OutputLimitDeferred` is gone: `Batch::more` says the
    same (a file stopped at the limit, the rest next drain), nothing is
    lost, so it was a notice in `errors`, where a viewer would show it
    as an error; nothing depended on it but the kinds test.
  - The drain no longer blocks: the descriptor is created `NONBLOCK`,
    `drain` reads it to `EAGAIN`, and the crafted drain tests wait on
    `wake_fd` with rustix `poll` under a five-second deadline (`woken`)
    in place of `drain_within`'s thread.
  - A follow with no directory to watch (both local roots missing, or no
    given file's directory existing) is refused at the start,
    `OpenError::NothingToWatch`: no event could make its wake fd
    readable.
  - RLIMIT_NOFILE: each `wake_fd` call adds one descriptor, held by the
    caller; past the limit it fails with `EMFILE` as an `io::Error`. The
    follow-up in [tasks/todo.md](../todo.md) stands as it was.

**Dependencies:** Task 6
**Files:** `crates/journal/src/{follow.rs,lib.rs}`, `crates/journal/examples/journal.rs`, `crates/journal/tests/follow.sh`
**Scope:** M

## Task 8: the fuzz harness enforcing its bounds

**Description:** Port fuzz.rs as `examples/fuzz.rs`, a thin wrapper.
Its failure rules become pure functions in testing.rs, with unit tests.
A run fails on:
- a panic or a hang;
- a case over 16 MiB above the seeds' base memory;
- an unreadable peak or a failed `clear_refs`;
- fewer than 9,000 cases or 8 seeds.

Seeds come from the matrix's journal-remote output, regenerated each
time and never committed.

**Acceptance criteria:**
- [x] Each failure rule has a unit test that makes it fire (testing/fuzz.rs).
- [x] A run of at least 9,000 cases over eight seeds passes inside a
      `systemd-run` scope (seeds from tests/seeds.sh).

**Verification:**
- [x] The harness run (log outside the repo); `scripts/check.sh task`.
- [x] library-reviewer (rlimit, /proc reads, the `clear_refs` write) and
      code-reviewer on the working tree.

**Dependencies:** Task 7
**Files:** `crates/journal/examples/fuzz.rs`, `crates/journal/src/testing.rs`, `crates/journal/Cargo.toml`
**Scope:** S

## Checkpoint B: follow and robustness
- [x] Tasks 6-8 (and Task 9) pass `scripts/check.sh full`; follow.sh,
      stress.sh and the fuzz harness are green. Recorded 2026-11-11,
      corrected after review:
      - check.sh full on d0bd3c9 against base ebaa782: the floor guard
        reported only the three Task 7 re-targets listed in f71ee7c;
        the remaining steps passed in a run with only that step skipped.
        Changed-line coverage 99%; mutants 386 caught of 401 viable (the
        15 survivors are listed with their reasons in c716137, f71ee7c
        and d0bd3c9: equivalent mutants and timeouts, two TailCache
        cases that only cost extra reads, and one untested error sum); cargo-deny ok (with the warnings
        it already had at Checkpoint A).
      - follow.sh and stress.sh passed on 22.04, 24.04 and 26.04 at
        f71ee7c (Task 7's binary).
      - The fuzz harness passed one run at d0bd3c9: 9,000 cases over 8
        seeds from 22.04 and 24.04 journal-remote.
- [x] Ratchet re-measured:
      - crates/ coverage 98.58% of lines and 98.10% of functions, above
        the floors;
      - startup 1.18 / 1.44 ms: `sb --help` is 0.09 ms over the recorded
        limit, within its tolerance; run-to-run noise (Checkpoint A
        measured 1.17 ms, and Tasks 6-9 add no startup work);
      - production binary 11932640 bytes: 8192 bytes (0.07%) more than
        at Checkpoint A, likely from term-ui's DisplayText in the
        runner (580a124; not measured commit by commit). It is 2.47% over the recorded 11645408; the rest
        came before this plan, from b7ebf17 (sb docker start, stop and
        restart), recorded with a cargo bloat breakdown in 50ede52, and
        was already present at Checkpoint A. Within the 5% tolerance.
      - Task 12 writes the measured values into CONSTRAINTS.md with a
        ratchet ADR.
- [x] Approved by the maintainer, 2026-11-11 (follow, robustness and the
      ratchet), on the first record (4a3a477). This correction followed
      a review of it and was reported to the maintainer.

## Task 9: term-ui's shared sanitizer (`DisplayText`)

**Description:**
- Make term-ui's `clean` public as `DisplayText::from_bytes(bytes,
  colour: bool)`, with callers passing `keeps_codes(..)` (plan, API
  names), on anstyle-parse 1.0.0 (a workspace
  dependency), with the 8-bit C1 pre-pass (decisions 2 and 5).
  - A kept SGR is emitted from its captured raw bytes, never rebuilt
    from `Params`.
- The runner's screen output uses it.
- So does its log, with colour off, in place of
  `console::strip_ansi_codes`.
- term-ui's spec is amended.

**Acceptance criteria:**
- [x] Tests for every class, 7-bit and 8-bit:
      - SGR kept or dropped by `colour`, byte-exact (`38:2::r:g:b`);
      - OSC, DCS, APC, PM and SOS dropped with their payloads;
      - cursor and mode CSIs dropped, and a CSI past the parameter limit
        dropped;
      - `ESC[?1m` isn't SGR;
      - C0 other than tab and newline, DEL and C1 dropped;
      - CR's rule;
      - lossy UTF-8.
- [x] The runner's existing tests pass. Colour is off under ascii,
      `--color=never` and NO_COLOR. The log drops APC/PM/SOS payloads.
- [x] The term-ui spec is amended (escape handling, the public type, the
      log).

**Verification:**
- [x] `cargo test -p sb-term-ui -p sb`; `scripts/check.sh task`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** None (it can run beside Tasks 1-8)
**Files:** `crates/term-ui/src/{text.rs,runner.rs,lib.rs}`,
`crates/term-ui/Cargo.toml`, `Cargo.toml`, `docs/specs/term-ui.md`
**Scope:** M

## Task 10: sb-host's release, uid lookup, re-export and glue

**Description:**
- Implement `ubuntu_release()` to host.md (sysinfo, with tests), and
  build the `Release` from it.
- Add the systemd-coredump uid lookup. It uses nix `user` through a
  workspace entry `nix = { version = "0.28.0", default-features = false,
  features = ["user"] }`. nix 0.28 already reaches sb through
  portable-pty, so this adds no second copy. Its error reads
  `look up user <u>: <cause>`, as host.md gives it.
- Re-export sb-journal's items by name (`#[doc(inline)] pub use`, no
  globs).
- Add tokio `spawn_blocking` helpers. Their signatures are
  `page(Arc<Journal>, Query, Position, usize, Limits) -> Page`, with
  owned `'static` values, and
  `drain(Follower, Limits) -> (Follower, Batch)`: the `Follower` moves
  in and comes back out.
- Add tokio to host.md's dependency list.
- Name sb-host in check.sh's `MUTATION_TESTERS[sb-journal]` (ADR-012).

**Acceptance criteria:**
- [x] sb-host's tests run `page` (Tail, Before, After) and a follow on
      crafted files through the re-export, the helpers on a
      current-thread runtime (success criterion 1).
- [x] A test asserts that `RELEASES` and `SUPPORTED_RUNTIME_RELEASES`
      name the same releases.
- [x] `ubuntu_release()` handles a missing or unparsable `VERSION_ID`
      with a typed error.

**Verification:**
- [x] `cargo test -p sb-host`; `scripts/check.sh task`.
- [x] library-reviewer (spawn_blocking, the getpwnam lookup) and
      code-reviewer on the commit.

**Dependencies:** Task 7
**Files:** `crates/host/{Cargo.toml,src/lib.rs,src/journal.rs}`, `Cargo.toml`, `docs/specs/host.md`, `scripts/check.sh`
**Scope:** M

## Task 11: the security audit and its fixes

**Description:** The security-auditor reviews the crate and the
sanitizer: L1, L2, L5, I3, opening with `O_NOFOLLOW`/`fstat`, and the C1
pre-pass. Each finding is fixed test-first, or declined with its reason
in the commit body. The matrix runs again where a fix touches reading.

**Acceptance criteria:**
- [x] No open high finding (success criterion 8): the audit found none.
      Its two Mediums are fixed test-first (M1, a live file's object read
      short at the arena's end; M2, entry objects read whole); its Lows
      and Infos are fixed or declined in the commit body, and recorded in
      proto-journal's FINDINGS.

**Verification:**
- [x] `scripts/check.sh full`; the affected container scripts;
      code-reviewer on the fixes.

**Dependencies:** Tasks 8, 9 and 10
**Files:** as the findings require
**Scope:** S-M

## Task 12: docs, the map's stack rows, the prototype removed

**Description:**
- The capability map:
  - tokio for sb-host;
  - the reader's stack row, rejecting sdjournal, systemd-journal-sdk
    and the libsystemd crate;
  - capability-map.md's "Journal cursor polling … replaced by
    `journalctl -f`" corrected (journal.md, "Capability map
    correction").
- The platform note brought up to date.
- proto-journal removed, its FINDINGS moved to
  `docs/prototypes/proto-journal.md`, and the links to it in the spec
  and the plan updated.
- The ratchet measured, and its values written into CONSTRAINTS.md's
  "Measured, not yet enforced" table with a ratchet ADR (raised
  coverage floors; the size and startup figures, a loosening, with
  their reasons).

**Acceptance criteria:**
- [x] Spec success criteria 6 and 7:
      - 7: the capability map (tokio for sb-host in the async row; the
        reader's stack row; sdjournal, systemd-journal-sdk and the
        libsystemd crate rejected; "Journal cursor polling" corrected),
        the platform note (the crate's runs, package versions, journald
        raising the arena before linking an object), and the FINDINGS
        moved to `docs/prototypes/proto-journal.md` with `git mv`. ADR-013,
        deny.toml and term-ui's amendment landed in Tasks 1 and 9.
      - The prototype removed after checking that the crate holds it all:
        its 112 tests are in sb-journal (111 by name, one re-targeted:
        `init_scope_rule_on_external_files_requires_uid_0`), its CLI and
        fuzz harness are the `journal` and `fuzz` examples, and its
        scripts were already in crates/journal/tests. Cargo.lock lost its
        package entry only; the CI workflow, scripts and deny.toml named
        it nowhere.
- [x] `scripts/check.sh full` passes except the floor guard's two
      findings, which ADR-014 (2026-11-11) explains; on the working tree
      against fc9b7c1:
      - the floor guard reports only ADR-014's two loosenings
        (`threshold-loosened` on the binary size and `sb --help` rows);
        every other step passed in a run with only that step skipped;
      - tests 1340 passed; no changed crate code, so no changed-line
        coverage or mutants; startup 1.19 / 1.43 ms; cargo-deny ok (its known
        warnings); binary 11932640 bytes.
      - Ratchet on the final tree: `crates/` coverage 98.59% of lines
        (29247 of 29666) and 98.12% of functions (3549 of 3617, rounded
        up from 98.119%); startup 1.18 / 1.36 ms; binary 11932640 bytes.
        ADR-014 raises the coverage floors to 98.58% and 98.11% and
        records the size and
        the `sb --help` median as new limits.

**Verification:**
- [x] `scripts/check.sh full`; code-reviewer on the working tree.

**Dependencies:** Task 11
**Files:** `docs/specs/capability-map.md`, `docs/platform-notes/systemd-journal.md`,
`docs/prototypes/proto-journal.md`, `docs/specs/journal.md`, `tasks/plan.md`,
`prototypes/proto-journal/` (removed)
**Scope:** M

## Checkpoint C: complete
- [x] The spec's success criteria 1-8, with the test count from plan.md's
      traceability table; Checkpoints A and B held. Prepared 2026-11-11
      for the maintainer's review:
      - 1: sb-host's tests page (Tail, Before, After) and follow crafted
        files through the re-export (Task 10; the follow through
        `FollowWait` since dee2ecb).
      - 2: all 112 prototype tests are in the crate (111 by name, one
        re-targeted in Task 3), with the traceability table's new items,
        `Send` and the `RELEASES` check (Tasks 3, 7 and 10).
      - 3: compare.sh, compat.sh, follow.sh and stress.sh passed on
        22.04, 24.04 and 26.04 on Task 11's tree (fc9b7c1); Task 12
        changed no crate code.
      - 4: the fuzz harness passed 9,000 cases over 8 seeds (Task 11).
      - 5: the pread bounds are asserted exactly (Task 4) and by
        compare.sh (Task 5).
      - 6 and 7: Task 12 above (check.sh full passes except the floor
        guard's two ADR-014 loosenings; ADR-014 is Proposed until this
        review).
      - 8: the security audit found no high finding (Task 11).
      - Checkpoint A's first box was met at Task 5 and ticked late, at
        Task 12, with its evidence (Checkpoint A above); its matrix and
        check.sh conditions are met again by Task 11's matrix run and
        Task 12's check (corrected in the hardening plan's Task 5,
        docs-M5). Checkpoint B held.
- [x] Review with the maintainer (approved 2026-11-11, with ADR-014 accepted); the plan archived to `tasks/journal/`.
