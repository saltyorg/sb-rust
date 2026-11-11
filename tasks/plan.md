# Implementation Plan: `sb-journal` (approved 2026-11-09)

Spec: [docs/specs/journal.md](../docs/specs/journal.md) (approved
2026-11-09, decisions 1-9). Evidence:
[prototypes/proto-journal/FINDINGS.md](../prototypes/proto-journal/FINDINGS.md)
(moves to `docs/prototypes/proto-journal.md` in Task 12) and
[docs/platform-notes/systemd-journal.md](../docs/platform-notes/systemd-journal.md).
Quality bar: [CONSTRAINTS.md](../CONSTRAINTS.md). Earlier plans are in
`tasks/<module>/`. Tasks: [tasks/todo.md](todo.md).

## Overview

The prototype's reader becomes `crates/journal` (`sb-journal`), ported
in slices from its tested code. That is about 6,300 lines including
tests, or about 3,800 lines of non-test code; the spec counts the
second figure. sb-host depends on sb-journal and re-exports its types.
term-ui's sanitizer becomes public and shared. The container scripts and
the CI workflow move to the crate, and the prototype is removed at the
end.

No `sb` command uses the crate yet. Its first consumer is the tui picker
and viewer (decision 8), so sb-host's tests and the container matrix are
what prove it.

## Architecture decisions

- **Port, don't rewrite.** Each slice moves prototype code together with
  its tests, then adapts it to the spec's API. A slice is done when the
  ported tests pass with their meaning unchanged: assertions are renamed
  or re-targeted, never weakened. The CONSTRAINTS floor guard watches
  for this.
- **The crate is under the gates from the first slice.**
  - Task 1 adds:
    - sb-host's dependency on sb-journal;
    - the deny.toml entry;
    - the capability-map row;
    - ADR-013, so every `Library-check:` line can cite the decision to
      own the reader.
  - Binary size is unaffected until a consumer calls the crate, because
    the linker drops unused code. Startup is likewise unaffected.
- **Code with no user yet.** Some ported items only get a library user
  in a later slice; file.rs's walk helpers, for example, are first used
  by Task 3.
  - Under `testing` those items are re-exported by the `testing` module,
    as the spec intends for the tools. Each module is declared once:
    cargo-mutants walks every `mod` without evaluating `cfg`, so a `pub`
    and a private declaration under opposite `cfg`s doubles its mutants.
  - A self dev-dependency (`sb-journal = { workspace = true, features =
    ["testing"] }`) turns `testing` on for `--all-targets` builds.
  - For the plain build, the module carries
    `#![cfg_attr(not(any(test, feature = "testing")), expect(dead_code, reason = "..."))]`.
    It has a temporary row in the CONSTRAINTS Exceptions table, which is
    deleted in the slice that adds the user.
  - Every no-user-yet item is re-exported by, or reachable from,
    `testing`; a new `pub` item in a private module joins the re-export
    list, since a `pub(crate)` helper with no caller would
    warn in the `testing` build, where no expectation applies.
  - Dev-dependency features are on whenever a test or example target
    is built. So `--all-targets` builds never lint the plain
    configuration.
    - Task 1 adds `cargo clippy --workspace --lib --bins -- -D warnings`
      to `check.sh fast`, a tightening, so no ADR. That builds no
      dev-dependencies.
    - There, a forgotten expectation (`unfulfilled_lint_expectations`)
      fails the check, so it cannot outlive its need.
  - Task 1 also extends floor-guard.mjs's suppression pattern to the
    `cfg_attr(..., expect(` form, so a missing Exceptions row is
    flagged.
  - Confirmed in Task 1: `cargo tree -e features -p sb-journal` lists
    `sb-journal feature "testing"` under its own `[dev-dependencies]`,
    and `cargo build -p sb-journal --all-targets -v` passes
    `feature="testing"` to both the library and its test build.
    cargo-deny sees the self dev-dependency as a parent, so deny.toml
    lists sb-journal among its own wrappers.
- **Lints from day one.** The workspace lints include pedantic,
  `missing_debug_implementations` and `trivial_numeric_casts`, which the
  prototype didn't run. Each slice meets them as it lands, with no
  `#[allow]`. Any justified `#[expect]` gets a CONSTRAINTS Exceptions
  row.
- **Typed errors.** These use thiserror: `ReadError`, `OpenError`,
  `QueryError`, `OverLimit` and `KnownBad`. anyhow stays only in the
  examples.
- **The `testing` feature** follows sb-docker's convention. It holds:
  - the crafted-file builder;
  - file-level access for the tools;
  - the JSON form;
  - the injectable roots and opener;
  - a pread counter, kept on the injected opener per `Journal`, never
    in a static (M-AVOID-STATICS).

  The examples are thin wrappers over it, each with `[[example]]
  required-features = ["testing"]`. Their decision logic lives in
  `testing.rs` as pure functions with unit tests: the compare verdicts,
  hash-check, decode-all, and the fuzz harness's failure rules. That way
  coverage and cargo-mutants reach it; neither runs example targets.
- **No nix, sysinfo or tokio in sb-journal** (spec). The examples take
  `--release` and `--coredump-uid`, and the scripts fill them in from
  `/etc/os-release` and `id -u systemd-coredump`.
- **Risk first.** File parsing and the walk (Tasks 1-3) carry most of
  the logic and most of the tests. The new behaviour is the least
  proven, so each piece comes right after its foundations and is checked
  against the container matrix (Task 5) before the next slice builds on
  it:
  - forward paging and the page anchor (Task 4);
  - the follow start (Task 7).

## API names

The spec gives the shape and leaves the names to the plan (journal.md,
"API sketch"). These names follow api-and-interface-design and the
Pragmatic Rust Guidelines: M-STRONG-TYPES-GUARD,
M-ERRORS-CANONICAL-STRUCTS, M-BUILD-RESULT, M-NO-GLOB-REEXPORTS and
M-DOC-INLINE. The maintainer reviews them at Checkpoint A, before the
tools in Task 5 depend on them.

| Name | Kind | Invariant or role |
|---|---|---|
| `Uid` | `struct Uid(u32)`, `Copy` | A user id. A crate newtype, so no dependency's type appears in the API. `Uid::new(u32)`, `get()`. |
| `Release` | struct, from `Release::parse("24.04")` | A release in `RELEASES`. Parsing any other release fails with `ReleaseError::Unsupported`. Its rules: `unit_rule()` (`UnitRule`) and `boot_rule()` (`BootRule`). |
| `BootRule` | `#[non_exhaustive] enum { Realtime, NewestTail, NewestTailEntryMachine }`, as `UnitRule` | The order between boots: 249's realtime (22.04), or 255 and 259's `compare_boot_ids` table (24.04, 26.04; spec decision 11), on 26.04 (259.9) with each file's machine from its tail entry's `_MACHINE_ID=` (issue #34169). A variant rather than a column: it refines `NewestTail` only (Checkpoint A). |
| `UnitName` | `UnitName::new(&str) -> Result<_, QueryError>` | A name with no dot gets `.service`; a `.service` name passes unchanged; anything else (other suffixes, dotted names, globs) is refused (Difference 7). |
| `FieldName` | `FieldName::new(&[u8]) -> Result<_, QueryError>` | journald's field-name rule, for queries and `Record::field`. `Record::fields()` yields raw byte pairs, unvalidated (Difference 14). |
| `MachineId` | `MachineId::parse(&str) -> Result<_, OpenError>`, `Display` | 32 hex digits in either case, not all zero; lowercased, as journald names the directory (L1; systemd v255 `id128_read_fd` and `sd_id128_from_string`'s `unhexchar` take either case; maintainer, Task 4). An invalid id is `OpenError::InvalidMachineId { path: Option<PathBuf> }`: `None` from `parse`, the file from `Journal::open(Source::Local)`, which refuses the source rather than join other text into a path; the file may hold one trailing newline (machine-id(5)) (Task 4). |
| `BootId` | `struct BootId([u8; 16])`, `Display` as hex | A boot id from the entry header. |
| `Cursor` | private fields; `Display` and `FromStr` (`QueryError::BadCursor`) | sd-journal's cursor text (`s=…;i=…;b=…;m=…;t=…;x=…`), documented and round-trip tested. It parses journalctl's `__CURSOR`, which the matrix pages from. |
| `Source` | `enum { Local, Directory(PathBuf), Files(Vec<PathBuf>) }` | |
| `Query` | `Query::unit(&Release, &UnitName, Option<Uid>, &Source)`, `Query::field(&FieldName, &[u8])`, `and(self, other) -> Result<Self, QueryError>` (at most 64 groups), `or(self, other) -> Self` | As the spec. |
| `Position` | `enum { Tail, Before(Cursor), After(Cursor) }` | |
| `Limits` | `Limits::default()`, `Limits::with_output(bytes) -> Limits` | The output limit is capped at `DATA_SIZE_MAX`, and the cap is stated in its docs. Private fields. The constructor can't fail because it caps, so it returns `Self` and not a `Result` (M-BUILD-RESULT applies only to fallible builds). |
| `Page`, `Batch` | public fields, as the spec; `#[non_exhaustive]` | `Page` has `records` and `errors` from Task 3; Task 4 adds `more_older`, `more_newer` and `end`. The flags count an entry the page chose but couldn't show (unreadable, or left out by a limit); with no record shown they are relative to the position (Task 4). |
| `PageEnd` | opaque; `Clone`, `PartialEq`, `Eq`, `Default` | Used only to start a follow: each file's (device, inode) and header entry count, and the newest entry, unfiltered, with its directory. Equality tells two pages of an unchanged journal (Task 4). |
| `ReadErrorKind::OutputLimitNewer`, `HeldLimitNewer` | variants | A forward page's limits, which leave out its newer end: it reads oldest first, so the next `After` from its last record has no gap (Task 4). |
| `ReadErrorKind::Symlink`, `NotRegular` | variants | A journal file that is a symlink (ELOOP under `O_NOFOLLOW`, confirmed by `lstat`), or not a regular file (`fstat`): open errors, one per file (L2, Difference 4; Task 4). |
| `testing::Host`, `open_on`, `Counts`, `counts` | under `testing` | `Source::Local`'s machine-id file and roots injected; a per-`Journal` count of opens, closes and preads, kept on its opener (Task 4). |
| `ReadError` | private `path` and `kind: ReadErrorKind`, with accessors; `path()` is `Option<&Path>`, `None` for page-level notices (the output, held and list-changing limits) (Task 3); `is_over_limit()`, `is_known_bad()`, `is_io()`, `is_too_large()`, `is_symlink()` to branch on (M-ERRORS-CANONICAL-STRUCTS; `kind()` stays public for the fields, and `ReadErrorKind` `#[non_exhaustive]`; maintainer, 2026-11-11, Checkpoint A); plus `ReadErrors { shown: Vec<ReadError>, omitted: usize }` | `Display` escapes the path and field names (L5). Ten are shown and the rest counted. Open errors take one entry per file, outside the cap. |
| `OpenError`, `QueryError`, `ReleaseError` | thiserror enums | |
| `Journal`, `Follower` | structs; `Journal: Send + Sync`, `Follower: Send` | `Journal::open(Source, &Release) -> Result<_, OpenError>`: the release whose journalctl the pages match, for every source (its boot order, decision 11). `Follower::drain(&mut self, &Limits) -> Batch`, `wake_fd(&self) -> io::Result<OwnedFd>` (CLOEXEC, non-blocking). |
| `DisplayText` (sb-term-ui) | `DisplayText::from_bytes(&[u8], colour: bool) -> DisplayText`; `lines()`, `as_str()` | As the spec. Callers pass term-ui's `keeps_codes(..)`. Sanitized text, held as one `String` plus line ranges. |

## Traceability

The prototype has 112 tests. They are assigned as follows; each commit
records its exact count. All 112 are ported as of Task 6: 1 + 31 + 26 +
25 + 21 + 3 + 1 + 4.

| Tests | Count | Task |
|---|---|---|
| hash.rs; file.rs header, objects, hash table, codecs and limits | 1 + 31 | 1 |
| file.rs entry arrays, chains, repeats, `all_entries`, `entries_since` | 26 | 2 |
| query.rs tail, the merge across files (`newest`) and rules | 25 | 3 |
| query.rs follow, plus `merge` and `is_old`, whose only caller is the follow | 21 + 3 | 6 |
| query.rs `printable`, used only by the JSON form | 1 | 5 |
| main.rs `classify` | 4 | 6 |

| Spec item (journal.md) | Task |
|---|---|
| Difference 1: the coredump rule (decisions 1, 4) | 3 (crafted), 5 (matrix row) |
| Difference 2: the follow loses and doubles nothing across a flush | 6 (crafted), 7 (follow.sh) |
| Difference 3: no JSON, no 4096-byte `null` (a 5 KiB field read back whole) | 3 |
| Difference 4: a symlinked journal file is an open error; directories followed | 4 |
| Difference 5: an entry over the limit, a zstd window over 128 MiB | 1 (codec), 3 (page skips and reports) |
| Difference 6: xz's 64 MiB decoder bound | 1 |
| Difference 7: unit-name mangling, other forms refused | 3 |
| Differences 8, 10: a zero item skipped; a stale item ends its array | 2 |
| Difference 9: a bad data hash table refuses lookups, not the file | 1 |
| Difference 11: the file list read again after opening | 4 |
| Difference 12: the follow shows both copies; the tail shows one | 3 (tail), 6 (follow) |
| Differences 13, 16: re-shown flush copies, a failing lookup reported once | 6 |
| Difference 14: record field names aren't refused (raw bytes, sanitized for display) | 3 |
| Difference 15: no second forward walk | 3 |
| Difference 17: an out-of-order item stops a followed file; the tail walks past it | 3 (tail), 6 (follow) |
| Difference 18: nothing after a vanished or non-matching cursor dropped (26.04's `--after-cursor`) | 4 (crafted), 5 (matrix) |
| Difference 19: a forward page on a corrupt file goes on where journalctl stops | 4 |
| Difference 20: forward pages over overlapping boots follow sb's order (24.04, 26.04) | after 4 (crafted: `boots_order_as_each_releases_journalctl`, `forward_pages_show_every_entry_of_another_boot`, `forward_back_and_tail_pages_agree`, `each_boot_of_a_file_starts_on_its_own`), 5 (matrix) |
| New: `After` and exact `more_*` (journalctl 259's seek emulated, decision 10) | 4 |
| New: boot order per release (`compare_boot_ids` on 24.04 and 26.04, decision 11) | after 4 (crafted: tail, `Before` and `After` on each row, a boot missing from the table, two machines, every file in the table, the newest readable entry for the table and the end, the per-boot seek and its fallback, the pread costs), 5 (matrix), 6 (the follow keeps the table current) |
| New: safe opening (L2), `MachineId` (L1) | 4 |
| New: typed `ReadError` (L5), `ReadErrors` | 3 (read errors), 4 (open errors) |
| New: `DisplayText` | 9 |
| New: the follow API (start from `PageEnd`, `Batch::more`, wake protocol) | 7 |
| New: the coredump rule for every source | 3 |
| New: the fuzz harness enforcing its bounds | 8 |
| Success criterion 1 | 10 |
| Success criterion 2: 112 tests, plus the Differences and new items above; `Send` on the types and the wake fd | 3, 7 (`Send`); counted at Checkpoint C |
| Success criterion 2: `RELEASES` matches `SUPPORTED_RUNTIME_RELEASES` | 10 |
| Success criterion 3: the matrix | 5 (pages, flags, coredump row), 7 (follow.sh, stress.sh) |
| Success criterion 4: the fuzz harness | 8 |
| Success criterion 5: the pread bound (figures below) | 4 (crafted), 5 (matrix fixture) |
| Success criterion 6: `check.sh full` | every checkpoint |
| Success criterion 7: documents | 1 (ADR-013, map row, deny.toml), 9 (term-ui spec), 12 (the rest) |
| Success criterion 8: the security audit | 11 |

**The pread bound (success criterion 5).** Each page counts its preads
on its journal's opener (`testing::counts`).
- Crafted fixture (Task 4): `crafted::journal(200)` matched on `U=x`
  (every entry), pages of ten, in
  `the_preads_of_a_tail_a_page_back_and_a_page_forward_are_bounded`:
  - the tail: 42 preads;
  - the page before it: 52;
  - the page after that page's first entry: 88 (a bisect; 101 for a page
    after the 10th or the 1981st entry of `crafted::journal(2000)`, so the
    same at any depth, and 104 after a vanished cursor, which also reads
    each file's first match: `a_deep_forward_page_costs_a_bisect`).
  - The tests assert these figures exactly.
  - The boot table (decision 11) adds nothing on any row while the
    header's tail is the newest entry, and one pread per file and page
    on 24.04 and 26.04 when it names another (an archived file journald
    before 254 wrote, as an upgraded host keeps): 43, 53 and 89
    (`the_boot_table_reads_at_most_one_entry_per_file`).
  - On 26.04, when the table holds two boots or more, it also reads
    each boot's newest file's tail entry's items up to its
    `_MACHINE_ID=`, one pread each, all of them without one (Checkpoint
    A, issue #34169; `BootOrder::name_machines`). One boot costs
    nothing, so the figures above hold on 26.04. The decision-11
    fixture's tail costs ten preads more with `_MACHINE_ID=` last of
    five items, eight without it
    (`the_tail_entrys_machine_id_costs_a_pread_per_item_read`).
  - sb's per-boot forward seek (decision 11, Difference 20), for a file
    whose boots the table orders against the cursor's: the `_BOOT_ID`
    field lookup (3 preads for one boot: the bucket, the FIELD object,
    the value) and a bisect per boot started by time. On the
    decision-11 fixture a page after a2 costs 50 on 24.04 against 46 on
    22.04, showing one entry more, and 58 on 26.04 with the tail entries'
    items (`the_boot_seek_reads_the_files_boots`).
- The matrix fixture's figures (Task 5): gen-export's 3000 entries
  written by each release's systemd-journal-remote (the `default`
  variant), `-u demo.service`, pages of 500:
  - the tail: 1694 preads on 22.04 and 24.04, 1711 on 26.04;
  - the page before it: 2361, and 2393;
  - the first page forward: 1726, and 1747.
  - The figures were the same in every run (four per release).
    compare.sh's bounds (`PREADS_*_MAX`) are 26.04's figures plus 1%:
    1728, 2417 and 1765, between three and five preads per entry shown.
  - Measured before 26.04 read the tail entry's `_MACHINE_ID=`
    (Checkpoint A). That costs nothing where the files' tails are of one
    boot; where they name two or more, each boot's newest file adds a
    read per item of its tail entry (eight for entry 2999, which has no
    `_MACHINE_ID=`), close to the 1%. Re-measured in Task 6 on 26.04 with
    the tail-entry reading in place: 1711, 2393 and 1747 again (the
    fixture's file is of one boot), so the bounds stand.

`Before` walks back from the newest entry to the cursor, so its cost
grows with the depth paged (accepted by success criterion 5). `After` is
journalctl 259's `--after-cursor`, its seek's bisects emulated (spec,
decision 10), so it costs O(log n) preads per value of the query at any
depth, plus a read of each file's first match when the landing isn't the
cursor's own entry (for `more_older`), and one read per entry shown. On a
damaged list (a zero slot where the walk's start falls) the walk reads
that list from its head instead: a pread per 512 items.

## Dependency graph

```
T0 ─ T1 ─ T2 ─ T3 ─ T4 ─ T5 ─ T6 ─ T7 ─┬─ T8 ──┐
                                       └─ T10 ─┤
T9 term-ui sanitizer (independent) ────────────┴─ T11 ─ T12
```

T10 (sb-host) needs T7 and does not need T9: sb-host cannot depend on
sb-term-ui. T11's audit waits for T8 (Checkpoint B) and covers the
sanitizer, so it waits for T9 too.

## Task list

### Phase 1: The reader
- [x] Task 0: time one mutant (85 s all-crate, 2 s own-crate); ADR-012
- [x] Task 1: crate skeleton, ADR-013, `hash.rs`, file objects and codecs
- [x] Task 2: entry arrays, chains and `EntriesRev`
- [x] Task 3: walks, rules and `page` (Tail, Before)
- [x] Task 4: `After`, exact `more_*`, `PageEnd`, safe opening, `MachineId`, the pread bound
- [x] Task 5: the comparison tools and container scripts on the crate

### Checkpoint A: the reader against journalctl
- [ ] compare.sh and compat.sh pass on 22.04, 24.04 and 26.04, pinning
      the forward page, the `more_*` flags and the coredump row
- [x] The API names are reviewed; the ratchet is re-measured
- [x] Review with the maintainer (2026-11-11)

### Phase 2: Following
- [x] Task 6: the follow ported, with the prototype's start
- [x] Task 7: the page-anchor start, the wake fd and `Batch::more`
- [x] Task 8: the fuzz harness enforcing its bounds

### Checkpoint B: follow and robustness
- [x] follow.sh (with the between-page-and-follow scenario) and
      stress.sh pass on the three releases; the fuzz harness passes; the
      ratchet is re-measured

### Phase 3: Consumers and close-out
- [x] Task 9: term-ui's shared sanitizer (`DisplayText`)
- [x] Task 10: sb-host's release, uid lookup, re-export and glue
- [x] Task 11: the security audit and its fixes
- [ ] Task 12: docs, the map's stack rows, the prototype removed

### Checkpoint C: complete
- [ ] The spec's success criteria 1-8 met; `scripts/check.sh full`
- [ ] Review with the maintainer; the plan archived to `tasks/journal/`

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Mutation testing runs over a whole new crate (about 640 mutants in the prototype). Task 0 measured 85 s per mutant judged by every crate's tests, and 2 s by the crate's own. | High: a `task` run lasting hours | ADR-012: sb-journal is listed in check.sh's `MUTATION_TESTERS` from Task 1, so its mutants are judged by its own tests (sb-host's from Task 10). The bar is unchanged: a narrower scope can't turn a missed mutant into a caught one (ADR-012 has the two edge cases). |
| Pedantic lints and coverage/mutation bars on about 3,800 ported lines | High: slow slices | Each slice adds at most two source modules, plus lib.rs and testing.rs. Run `check.sh task` per slice. If project figures move, a ratchet ADR, as ADR-011 did. |
| The ported tests lose meaning in adaptation | High | In each slice, port the tests first and see them pass on the moved code before adapting it. Floor guard on assertions; per-slice test counts against the traceability table. |
| `PageEnd` start or forward paging diverges from journalctl | Med | Crafted tests first. Before Tasks 6-7 build on them, matrix checks for the forward page, the flags, and writes between page and follow. |
| A matrix mismatch that isn't a listed Difference | Med | Stop and bring it to the maintainer (journal.md, "Boundaries"). It is never pinned as a new difference silently. |
| anstyle-parse doesn't cover a class the policy needs | Med | Task 9 starts by testing each class against the parser. vte has the same 7-bit model. Any gap is ours to fill, recorded the way the C1 pre-pass is. |
| anstyle-parse hands over parsed `Params`, not raw bytes | Med | A kept SGR is emitted from the raw bytes the sanitizer captures from ESC to the final byte, never rebuilt from `Params`. A byte-exactness test covers `38:2::r:g:b`. A CSI past the parameter limit (`ignore` set) is dropped. |
| The term-ui sanitizer change alters the runner's output | Med | The runner's existing tests stay, and new cases only drop what the policy drops. The run log moves from `console::strip_ansi_codes` to `DisplayText` with colour off, so the log loses APC/PM/SOS payloads too. The term-ui spec is amended in the same task. |
| Container scripts are slow, or flaky on image pulls | Low | A unique `SBJ_PREFIX` per run. Per task, scripts run only for the releases the change touches; all three run at checkpoints. |
| Disk use by `target/` | Low | `du -sh target` after each task; `cargo clean` above 10 GB. |

## Open questions

None. The log-source interface shared with sb-docker belongs to the tui
spec (spec, decision 8). Pushing the CI workflow, and afl.rs fuzzing
after it, wait for the maintainer's go-ahead (decision 7, tasks/todo.md
follow-ups).
