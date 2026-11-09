# Implementation Plan: `sb-journal` (draft, 2026-11-09)

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
    - ADR-012, so every `Library-check:` line can cite the decision to
      own the reader.
  - Binary size is unaffected until a consumer calls the crate, because
    the linker drops unused code. Startup is likewise unaffected.
- **Code with no user yet.** Some ported items only get a library user
  in a later slice; file.rs's walk helpers, for example, are first used
  by Task 3.
  - Under `testing` those items are `pub`, as the spec intends for the
    tools.
  - A self dev-dependency (`sb-journal = { path = ".", features =
    ["testing"] }`) turns `testing` on for `--all-targets` builds.
  - For the plain build, the module carries
    `#![cfg_attr(not(any(test, feature = "testing")), expect(dead_code, reason = "..."))]`.
    It has a temporary row in the CONSTRAINTS Exceptions table, which is
    deleted in the slice that adds the user.
  - Every no-user-yet item is reachable from an item that is `pub`
    under `testing`, since a `pub(crate)` helper with no caller would
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
  - UNVERIFIED until Task 1 confirms the unification with `cargo tree -e
    features -p sb-journal` (reasoned from the resolver docs, not
    built).
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
| `Release` | struct, from `Release::parse("24.04")` | A release in `RELEASES`. Parsing any other release fails with `ReleaseError::Unsupported`. |
| `UnitName` | `UnitName::new(&str) -> Result<_, QueryError>` | A name with no dot gets `.service`; a `.service` name passes unchanged; anything else (other suffixes, dotted names, globs) is refused (Difference 7). |
| `FieldName` | `FieldName::new(&[u8]) -> Result<_, QueryError>` | journald's field-name rule, for queries and `Record::field`. `Record::fields()` yields raw byte pairs, unvalidated (Difference 14). |
| `MachineId` | `MachineId::parse(&str) -> Result<_, OpenError>` | 32 lowercase hex digits, not all zero (L1). |
| `BootId` | `struct BootId([u8; 16])`, `Display` as hex | A boot id from the entry header. |
| `Cursor` | private fields; `Display` and `FromStr` (`QueryError::BadCursor`) | sd-journal's cursor text (`s=…;i=…;b=…;m=…;t=…;x=…`), documented and round-trip tested. It parses journalctl's `__CURSOR`, which the matrix pages from. |
| `Source` | `enum { Local, Directory(PathBuf), Files(Vec<PathBuf>) }` | |
| `Query` | `Query::unit(&Release, &UnitName, Option<Uid>, &Source)`, `Query::field(&FieldName, &[u8])`, `and(self, other) -> Result<Self, QueryError>` (at most 64 groups), `or(self, other) -> Self` | As the spec. |
| `Position` | `enum { Tail, Before(Cursor), After(Cursor) }` | |
| `Limits` | `Limits::default()`, `Limits::with_output(bytes) -> Limits` | The output limit is capped at `DATA_SIZE_MAX`, and the cap is stated in its docs. Private fields. The constructor can't fail because it caps, so it returns `Self` and not a `Result` (M-BUILD-RESULT applies only to fallible builds). |
| `Page`, `Batch` | public fields, as the spec | |
| `PageEnd` | opaque | Used only to start a follow. |
| `ReadError` | private `path` and `kind: ReadErrorKind`, with accessors, plus `ReadErrors { shown: Vec<ReadError>, omitted: usize }` | `Display` escapes the path and field names (L5). Ten are shown and the rest counted. Open errors take one entry per file, outside the cap. |
| `OpenError`, `QueryError`, `ReleaseError` | thiserror enums | |
| `Journal`, `Follower` | structs; `Journal: Send + Sync`, `Follower: Send` | `Follower::drain(&mut self, &Limits) -> Batch`, `wake_fd(&self) -> io::Result<OwnedFd>` (CLOEXEC, non-blocking). |
| `DisplayText` (sb-term-ui) | `DisplayText::from_bytes(&[u8], colour: bool) -> DisplayText`; `lines()`, `as_str()` | As the spec. Callers pass term-ui's `keeps_codes(..)`. Sanitized text, held as one `String` plus line ranges. |

## Traceability

The prototype has 112 tests. They are assigned as follows; each commit
records its exact count.

| Tests | Count | Task |
|---|---|---|
| hash.rs; file.rs header, objects, hash table, codecs and limits | 1 + about 35 | 1 |
| file.rs entry arrays, chains, repeats, `all_entries`, `entries_since` | about 22 | 2 |
| query.rs tail, merge and rules | about 29 | 3 |
| query.rs follow | about 21 | 6 |
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
| New: `After` and exact `more_*` | 4 |
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
| Success criterion 5: the pread bound | 4 (crafted), 5 (matrix fixture) |
| Success criterion 6: `check.sh full` | every checkpoint |
| Success criterion 7: documents | 1 (ADR-012, map row, deny.toml), 9 (term-ui spec), 12 (the rest) |
| Success criterion 8: the security audit | 11 |

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
- [ ] Task 0: time one mutant; record count × time; split slices if needed
- [ ] Task 1: crate skeleton, ADR-012, `hash.rs`, file objects and codecs
- [ ] Task 2: entry arrays, chains and `EntriesRev`
- [ ] Task 3: walks, rules and `page` (Tail, Before)
- [ ] Task 4: `After`, exact `more_*`, `PageEnd`, safe opening, `MachineId`, the pread bound
- [ ] Task 5: the comparison tools and container scripts on the crate

### Checkpoint A: the reader against journalctl
- [ ] compare.sh and compat.sh pass on 22.04, 24.04 and 26.04, pinning
      the forward page, the `more_*` flags and the coredump row
- [ ] The API names are reviewed; the ratchet is re-measured
- [ ] Review with the maintainer

### Phase 2: Following
- [ ] Task 6: the follow ported, with the prototype's start
- [ ] Task 7: the page-anchor start, the wake fd and `Batch::more`
- [ ] Task 8: the fuzz harness enforcing its bounds

### Checkpoint B: follow and robustness
- [ ] follow.sh (with the between-page-and-follow scenario) and
      stress.sh pass on the three releases; the fuzz harness passes; the
      ratchet is re-measured

### Phase 3: Consumers and close-out
- [ ] Task 9: term-ui's shared sanitizer (`DisplayText`)
- [ ] Task 10: sb-host's release, uid lookup, re-export and glue
- [ ] Task 11: the security audit and its fixes
- [ ] Task 12: docs, the map's stack rows, the prototype removed

### Checkpoint C: complete
- [ ] The spec's success criteria 1-8 met; `scripts/check.sh full`
- [ ] Review with the maintainer; the plan archived to `tasks/journal/`

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Mutation testing runs over a whole new crate. `cargo mutants --list` on the prototype gives 349 mutants in file.rs, 43 in hash.rs and 249 in query.rs, and every crate's tests judge each one. | High: a `task` run lasting hours | Before Task 1, time one mutant on the workspace and record count × time here. file.rs is already split across Tasks 1 and 2. If a slice still exceeds about an hour, split it further. Don't lower the bar. |
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
