# Implementation Plan: hardening after the re-review (draft, 2026-11-11)

Findings: [tasks/findings.md](findings.md) (every item, with its id).
Specs: each crate's spec in `docs/specs/`. Quality bar:
[CONSTRAINTS.md](../CONSTRAINTS.md). Tasks: [tasks/todo.md](todo.md).
Earlier plans are in `tasks/<module>/`.

## Overview

Thirteen fresh-context reviewers re-read every crate at 96e940a. They
found 8 distinct High and about 35 distinct Medium defects, most of them
confirmed with a probe, and about 120 Low and unrated items. This plan
fixes every High and Medium, and gives every Low a place: a criterion in
the task that touches its code, or a per-crate Low sweep. The TUI log
viewer spec waits for it: the viewer builds on paging, the follow,
tables and the error display, where most of the findings are.

The work is 50 small tasks: six on the gates and records, then three
lanes (text, process and display; the journal reader; config), then the
build, then a final review.

## Maintainer decisions (2026-11-11)

1. **Fix all High and Medium before the viewer spec.**
2. **A terminal hangup cancels cleanly.** SIGHUP is handled like
   SIGTERM: the children are stopped with the usual grace, the run log
   gets its ending, then sb ends by SIGHUP. This replaces the cli-core
   spec's "sb doesn't handle SIGHUP".
3. **sb accepts the YAML Ansible accepts.** The options are researched
   first, with pros and cons, and brought to the maintainer (Task 40). A
   standing differential test follows.
4. **Lows are not ignored.** Every Low in tasks/findings.md, and every
   unrated item (Info, test gaps, notes), is one of:
   - an acceptance criterion in the task that touches its code;
   - an entry in a per-crate Low sweep at the end of that crate's lane
     (the journal sweeps come before the reader checkpoint).

   Each sweep lists its ids, each marked fix or proposed-decline with a
   reason. A Low that can happen on a real Saltbox server is fixed. The
   candidates for declining are the ones that need crafted or imported
   files, or that match sb-go on purpose. The proposed declines go to the
   maintainer as a list, to confirm or overrule, before the sweep is
   committed.

   The sweeps as drafted also propose declines on grounds the decision
   doesn't name. Each says which, and each needs its confirmation like
   any other:
   - a dependency's own behaviour that sb could change only by
     re-implementing the library;
   - a case sb's current-thread runtime can't reach, pinned by a test;
   - an API change with no consumer yet;
   - a reviewer's note that asks for no change;
   - a decision already recorded in a spec or a follow-up.

## Architecture decisions

- **Gates first.** The floor guard and `check.sh` are fixed before any
  code. Every later task then runs under a guard that sees non-Rust
  files, and `check.sh full` can pass with a finding acknowledged in the
  open, with no step skipped.
- **Acknowledged floor findings (Task 3, an ADR).** One tracked file,
  `scripts/floor-acknowledged.txt`. Each entry is one line: the rule,
  the file, the flagged line's text and a reason.
  - An entry covers exactly one finding with that rule, file and line
    text. Two findings need two entries.
  - An entry counts only in a diff that also adds the entry. So it is
    committed with the change it acknowledges, and an entry from an
    earlier commit acknowledges nothing. A range run over several
    commits sees each entry and its finding together.
  - Entries are only ever added. The file is a record; nothing empties
    it, since a removal inside a checked range would cancel the entry
    the range needs.
  - `check.sh` prints each entry the diff adds and the finding it
    covers. Adding an entry is not itself a finding. It fails on any
    unacknowledged finding, and on an added entry that matched nothing.
  - This rewords the Floor guard row from "zero floor violations" to
    "zero unacknowledged floor violations", which is a loosening, so it
    has an ADR.
- **One choke point per sink.** Untrusted text is made safe where it is
  written, so callers don't each remember to escape:
  - table cells in `Table`;
  - error text in cli-core's error block;
  - log values in the run log's formatter layer (M-LOG-STRUCTURED),
    which escapes control characters, `\n` and `\r` in every field of
    every record, bridged dependency records included. A source scan of
    `%` Display fields is the backstop.
- **A page is a snapshot.** A page shows only what its files' headers
  counted when it opened them. That makes the page, its `end` and the
  follow's start agree.
- **Test the format the releases write.** The crafted-file builder gains
  compact and keyed layouts, and the reader's suites run over both. This
  and the script hardening come before the journal fixes, so each fix is
  tested in both layouts and checked by a matrix that can't pass
  vacuously.
- **Named expected failures.** A hardened script, or the fuzz harness,
  may expose a defect that a later task fixes (follow.sh's new count
  check and follow-H1, for one). Such a check is listed as an expected
  failure, by name, with the finding's id, the task that clears it and
  a mark.
  - `always`: the defect fails every run. The entry fails the script
    when its check passes, so it can't outlive its fix.
  - `racy`: the defect needs a race to show (follow-H1 does). The entry
    is printed whether its check fails or passes, and fails nothing.
  - Any failure with no entry still fails the run. A failure with no
    finding behind it isn't given an entry: it stops for the
    maintainer.
  - The task that fixes a finding removes that finding's entries,
    wherever they are. Checkpoint B allows none, which is the backstop
    for a `racy` entry.
  - The floor guard reports an added entry (Task 1), so each is
    acknowledged in the open. The guard watches `crates/*/tests/*.sh`,
    so the fuzz harness keeps its entries in `seeds.sh`, not in
    `examples/fuzz.rs`.
- **Named mutants are run by name.** `check.sh task` runs
  `cargo mutants --in-diff`, which generates no mutant in unchanged
  code. Where a task's criterion is "this surviving mutant is caught",
  its verification runs cargo-mutants on that file with `--file` and a
  `--re` filter for the site, and records caught or missed. The site is
  found with `cargo mutants --list` at the task's start, since line
  numbers move.
- **Research before a stop.** Two tasks end at a maintainer's choice:
  Task 7 (the prompting child) and Task 40 (YAML). Neither commits a
  failing test: Task 7's reproduction is a probe described in its
  write-up, and the test lands with the fix.
  - Task 7 opens lane 1 and is done when its options are with the
    maintainer. The lane goes on; only Task 14 needs the answer, and
    records it in the spec.
  - Lane 3 does wait: Task 41 comes straight after Task 40 and needs
    the answer. So Task 40, which is research only, runs early, while
    lanes 1 and 2 hold the two worktrees. The wait then costs nothing,
    and the answer is in before lane 3's code starts.
- **Test-first, as CLAUDE.md requires.** Each fix starts with a failing
  test that reproduces its finding as tasks/findings.md describes it.
- **Reviews per commit.** Each commit gets the code-reviewer, plus the
  library-reviewer where I/O, process or terminal code changes and the
  security-auditor where untrusted input is handled.
- **Specs, platform notes and ADRs change with the code.** A task that
  changes specified behaviour names its spec in Files. A new dependency,
  a loosened rule or a changed ratchet basis has an ADR (the next free
  number in `docs/decisions/`).

## Lanes and shared files

After Checkpoint A the work runs in three lanes, at most two at a time,
each in its own git worktree with its own target directory.

| Lane | Tasks | Edits |
|---|---|---|
| 1: text, process and display | 7-26 | `crates/{term-ui,exec,docker}`; cli-core's `exit.rs`, `log.rs`, `cancel.rs`, `root.rs`; `crates/sb/src/main.rs` and sb's signal and log tests; the cli-core, exec, term-ui and docker specs |
| 2: the journal reader | 27-39 | `crates/journal`, `crates/host/src/journal.rs` and its tests, the journal spec, the matrix workflow |
| 3: config | 40-45 | `crates/config`, cli-core's `config.rs`, new sb test files, the config spec |

A lane is sequential: its tasks share files (exec's `run.rs` and
`pty.rs`, term-ui's `runner.rs` and `tree.rs`, docker's `connect.rs`),
so each waits for the one before it. Lanes 1 and 2 start first, because
the viewer needs both. Lane 3's code starts when one of them frees a
worktree. Its first task, Task 40, is research only and runs early,
beside them, so the YAML answer is in by then.

Shared files have one owner at a time:
- **`Cargo.toml`, `Cargo.lock`, `deny.toml`:** the main checkout owns
  them. A lane task that needs a manifest change lands it on main, as
  its own commit, while no other open task touches those files; the
  other worktree rebases after it. That covers Task 41 (a parser), and
  Task 10 or Task 31 if one needs a workspace feature
  (tracing-subscriber, rustix). The build tasks (46-50) run on the main
  checkout, one at a time, after lanes 1 and 2 are merged.
- **`scripts/floor-acknowledged.txt`:** any lane may add entries. Lines
  are only added, so a merge takes the union of both sides' lines. No
  line is edited or removed.
- **The CONSTRAINTS ratchet rows** ("Measured, not yet enforced"):
  changed only on the main checkout, at Checkpoints B and C and in
  Tasks 46 and 48. A lane worktree never edits them. An Exceptions row a
  lane needs goes in that lane's own commit.
- **`tasks/todo.md` and `tasks/plan.md`:** ticked on the main checkout
  when a task merges.
- A worktree's commits are rebased onto main, and `check.sh full` runs
  again on the rebased commit before the merge.

Three tasks touch another lane's file, each in one place only:
- **Task 9** (lane 1) calls `escape_controls`, which lives in cli-core's
  `config.rs`, lane 3's file. It uses the function as it is, and
  neither moves nor changes it.
- **Task 42** (lane 3) changes cli-core's inventory read, which the
  cli-core spec states in one paragraph ("Config and inventory are read
  up to 1 MiB ..."). Task 42 edits that paragraph only; lane 1 leaves it
  alone.
- **Task 25** (lane 1), if the maintainer accepts the SGR change, edits
  one line of the journal spec's display decision 2. Lane 2 leaves that
  decision alone.

## Traceability

Every id in tasks/findings.md is in one of the tables below. Where
several reviewers found one defect, the ids share a row. Task 5 adds a
script that checks this.

### High and Medium

| Ids | Finding | Task |
|---|---|---|
| docs-H1 | The floor guard is blind outside `.rs` and CONSTRAINTS.md | 1 |
| docs-M2 | The guard's false negatives in Rust files | 2 |
| docs-H2 | `check.sh full` has no acknowledgement path | 3 |
| docs-M1, docs-M4 | The commit-message check is unwired; no crate-entry or tool-version check | 4 |
| docs-M3, docs-M5 | The matrix and fuzz are in no gate; the Checkpoint A record contradicts itself | 5 |
| docs-M6 | The capability map lacks the nix row and rustix's `event` | 6 |
| exec-M1 | A piped child prompting on the terminal hangs sb | 7, 14 |
| san-H1, docker-M1, ui-M4 | Table cells printed raw (container labels) | 8 |
| san-M1, cli-M3 (screen) | Error text raw in the error block | 9 |
| san-M2, cli-M3 (log) | Run-log injection by newline and Display fields | 10 |
| exec-M2 | A secret split by an escape evades the mask | 11 |
| exec-H1, cli-M1 | SIGHUP kills sb and orphans piped children | 12 |
| cli-M2 | A relayed SIGTERM counts twice and forces the exit | 13 |
| exec-M3 | `pty` with a cleared environment can't find a bare name | 15 |
| exec-M4 | Tight wall-clock bounds in exec's tests | 16 |
| sb-M2, ui-M3 | A panic leaves nothing in the log; a group panic loses its message | 17 |
| docker-M2 | `docker restart` can leave containers down silently | 18 |
| ui-M2 | Typing during a live run leaves stale lines | 19 |
| ui-M1 | A tall live tree hides the running step | 20 |
| tests-H1 | Compact files untested above object decoding | 27, 28 |
| tests-H2, tests-M2, tests-M6 | stress.sh passes vacuously; unchecked variants; read errors ignored | 29 |
| tests-M7, tests-M8 | follow.sh's overflow scenario, order and count | 30 |
| tests-M1, tests-M3, tests-M4, tests-M5 | Fuzz seeds, clean reads, missing reads and the memory bound | 31 |
| follow-H1, page-H1, fwd-M2 | A page shows an entry its `end` doesn't count; the follow repeats it | 32, 33 |
| page-M1, page-M2 | A page of unreadable entries can't be paged past; `more_newer` stuck | 34 |
| fwd-M1 | A forward page loops on the cursor's own boot | 35 |
| file-M1 | A forward walk loses every array after a bogus item | 36 |
| config-H1 | Valid YAML refused | 40, 41 |
| config-M1 | A small config file exhausts memory or the stack | 42 |
| config-M2 | `ALL_PROXY` honoured | 43 |
| sb-M1, sb-M4 | No gate runs the production binary; cross drops the version | 46 |
| sb-M3 | Root tests skip silently | 47 |
| sb-M5 | Overflow checks off in production | 48 |
| sb-M6 | Startup parses and renders more than it needs | 49 |

### Low and unrated

Lows that are acceptance criteria of a task:

| Ids | Task |
|---|---|
| docs-L13, docs-L14 | 4 |
| docs-L6, docs-L17 | 5 |
| san-T1 (the table part), san-I2 (table cells) | 8 |
| docker-L1, docker-L2, san-L4, cli-T1 (the newline in the log) | 10 |
| san-L3 | 11 |
| cli-L3, cli-T1 (the `decide` case) | 13 |
| exec-L1, exec-L2, exec-L6 | 15 |
| ui-L2 | 17 |
| ui-L1, san-I1, ui-L3, ui-L4, ui-L5 | 21 |
| file-L5 (the file-layer mutants) | 27 |
| file-L5 (the hash vectors), page-L8 | 28 |
| tests-L3, tests-L5, tests-L6, tests-L7, tests-L13, tests-L11 (the compat step) | 29 |
| tests-L1, tests-L2, tests-L14 | 30 |
| tests-L11 (the seeds and fuzz in the workflow) | 31 |
| follow-N1 | 32 |
| fwd-L3 | 35 |
| file-L2, file-L3, file-L4, page-N1 | 36 |
| config-L1, config-T1 (acceptance, the parity tables) | 40, 41 |
| config-T1 (memory) | 42 |
| config-T1 (the proxy test) | 43 |
| config-L2, config-L3, config-L4 | 44 |
| sb-L8, sb-L7 (the lookup's test run on musl) | 46 |
| sb-L10, docs-L13 (the silent non-root skip) | 47 |
| sb-L11 | 48 |
| sb-L5 | 49 |

Lows in a sweep (each sweep's task lists fix or proposed-decline):

| Sweep | Ids | Task |
|---|---|---|
| Documents | docs-L1, docs-L2, docs-L3, docs-L4, docs-L7, docs-L8, docs-L9, docs-L10, docs-L11, docs-L12, docs-L15, docs-L16 | 6 |
| cli-core | cli-L1, cli-L2, cli-L4, cli-L5, cli-L6, cli-L7, cli-T1 (the log directory cases) | 22 |
| exec | exec-L3, exec-L4, exec-L5, exec-L7, exec-L8, exec-L9 | 23 |
| docker | docker-L3, docker-L4, docker-L5, docker-L6, docker-L7, docker-L8, docker-L9 | 24 |
| The sanitizer | san-L1, san-L2, san-I2 (log text), san-I3, san-I4, san-T1 (cut, alphabet) | 25 |
| term-ui's runner | ui-L6, ui-L7, ui-L8 | 26 |
| Journal: paging and API | page-L1, page-L2, page-L3, page-L4, page-L5, page-L6, page-L7, page-L9, fwd-L4, fwd-L5, docs-L5 | 37 |
| Journal: the follow | follow-L1, follow-L2, follow-L3, follow-L4, follow-L5, follow-L6, follow-L7 | 38 |
| Journal: test evidence | tests-L4, tests-L8, tests-L9, tests-L10, tests-L12 | 39 |
| config | config-L5, config-L6, config-L7, config-L8, config-L9, config-L10 | 45 |
| The sb binary | sb-L1, sb-L2, sb-L3, sb-L4, sb-L6, sb-L7, sb-L9, sb-L12, config-L11 | 50 |

## Dependency graph

```
T1 ─ T2 ─ T3 ─ T4 ─ T5 ─ T6 ─ [A]

[A] ─┬─ lane 1: T7 ─ T8 ─ T9 ─ T10 ─ T11 ─ T12 ─ T13 ─ T14 ─ T15 ─ T16 ─ T17 ─ [T]
     │          [T] ─ T18 ─ T19 ─ T20 ─ T21 ─ T22 ─ T23 ─ T24 ─ T25 ─ T26 ─ [D]
     │
     ├─ lane 2: T27 ─ T28 ─ T29 ─ T30 ─ T31 ─ T32 ─ T33 ─ T34 ─ T35 ─ T36
     │          T36 ─ T37 ─ T38 ─ T39 ─ [B]
     │
     └─ lane 3: T40 ─ (the YAML answer) ─ T41 ─ T42 ─ T43 ─ T44 ─ T45 ─ [Y]

[D] + [B] ─ T46 ─ T47 ─ T48 ─ T49 ─ T50 ─┐
[Y] ─────────────────────────────────────┴─ [C]
```

Each lane runs in the order shown. The dependencies that aren't just
"the task before":
- T14 needs the maintainer's answer to T7; the tasks between don't.
- T41 needs the answer to T40, and lane 3 waits for it. T40 runs early,
  beside lanes 1 and 2.
- T10 after T9: both edit cli-core's log and exit code.
- T14 and T15 after T11: exec's `run.rs` and `pty.rs`.
- T17 after T10 (cli-core's `log.rs`; the panic message goes through its
  escaping) and after T16.
- T19, T20 and T21 after T11 and T17 (term-ui's `runner.rs` and
  `tree.rs`).
- T24 after T10: docker's `connect.rs`.
- T27 to T31 (the builder, the scripts and the harness) before T32 to
  T36 (the fixes).
- T46 after T17 (sb's `main.rs`) and T31 (the workflow); T48 after T31
  (the fuzz harness runs once with overflow checks on).

## Task list

### Phase 0: the gates and the records
- [ ] Task 1: the floor guard sees every gate file
- [ ] Task 2: the floor guard's false negatives in Rust files
- [ ] Task 3: `check.sh` accepts acknowledged floor findings
- [ ] Task 4: `check.sh` runs the checks that exist only on paper
- [ ] Task 5: the gate records are put right
- [ ] Task 6: Low sweep: documents

### Checkpoint A: gates
- [ ] `check.sh full` passes on its own change, every step run, with
      any expected finding acknowledged in the open
- [ ] The open questions marked [start] are answered
- [ ] Task 6's proposed decline (the exception E1 half of docs-L12) is
      confirmed or overruled
- [ ] `scripts/check-plan-ids.mjs` passes

### Phase 1 (lane 1): untrusted text, signals, processes and panics
- [ ] Task 7: a child that prompts on the terminal: research and decision
- [ ] Task 8: table cells are sanitized
- [ ] Task 9: the error block escapes error text
- [ ] Task 10: no logged value can forge a run-log record
- [ ] Task 11: secrets are masked after escapes are stripped
- [ ] Task 12: a hangup cancels cleanly
- [ ] Task 13: a relayed signal isn't a second signal; forced exits are logged
- [ ] Task 14: a child that prompts on the terminal: the fix
- [ ] Task 15: exec's smaller defects
- [ ] Task 16: exec's timing-bound tests assert order
- [ ] Task 17: panics reach the run log and the screen

### Checkpoint T: text and processes
- [ ] `check.sh full`; the signal probes pass under sudo
- [ ] The open questions marked [checkpoint T] are answered
- [ ] Review with the maintainer

### Phase 2 (lane 1): docker and the live display
- [ ] Task 18: `docker restart` never ends silently short of a start
- [ ] Task 19: echo is off during a live run
- [ ] Task 20: a live tree taller than the terminal
- [ ] Task 21: table wrap, uncut lines and one-line labels
- [ ] Task 22: Low sweep: cli-core
- [ ] Task 23: Low sweep: exec
- [ ] Task 24: Low sweep: docker
- [ ] Task 25: Low sweep: the sanitizer
- [ ] Task 26: Low sweep: term-ui's runner

### Checkpoint D: docker and display
- [ ] `check.sh full`; the tmux probes at 80x12 and 36 columns
- [ ] The sweeps' declines are confirmed or overruled
- [ ] Review with the maintainer (the looks, from screenshots)

### Phase 3 (lane 2): the journal reader
- [ ] Task 27: the crafted builder writes compact and keyed files
- [ ] Task 28: the reader's suites run over both layouts
- [ ] Task 29: the page scripts can't pass vacuously
- [ ] Task 30: follow.sh checks order, count and overflow
- [ ] Task 31: the fuzz seeds and harness check what they read
- [ ] Task 32: a page is a snapshot of its headers
- [ ] Task 33: stress.sh compares a snapshot page
- [ ] Task 34: a page can always be paged past
- [ ] Task 35: a forward page on the cursor's own boot
- [ ] Task 36: forward walks over damaged entry arrays
- [ ] Task 37: Low sweep: journal paging and API
- [ ] Task 38: Low sweep: the journal follow
- [ ] Task 39: Low sweep: journal test evidence

### Checkpoint B: reader
- [ ] The matrix (compare, compat, follow, stress) is green on 22.04,
      24.04 and 26.04 with no expected failure left, and the fuzz
      harness passes, on the final tree
- [ ] Size, startup and coverage are re-measured; the ratchet rows are
      updated, with a ratchet ADR if a value passes its tolerance
- [ ] The new API names are reviewed; the sweeps' declines are
      confirmed or overruled
- [ ] Review with the maintainer

### Phase 4 (lane 3): config
- [ ] Task 40: YAML: the corpus and the options
- [ ] Task 41: sb accepts the YAML Ansible accepts
- [ ] Task 42: config files can't exhaust memory or the stack
- [ ] Task 43: proxies as Go reads them
- [ ] Task 44: secrets are hidden wherever their key sits, and in `Debug`
- [ ] Task 45: Low sweep: config

### Checkpoint Y: config
- [ ] `check.sh full`; the differential corpus passes
- [ ] The sweep's declines are confirmed or overruled
- [ ] Review with the maintainer

### Phase 5: the build
- [ ] Task 46: the production binary is run by the gate
- [ ] Task 47: skipped tests are counted, and the gate runs as root
- [ ] Task 48: overflow checks in the shipped profiles
- [ ] Task 49: startup parses once
- [ ] Task 50: Low sweep: the sb binary

### Checkpoint C: complete
- [ ] `check.sh full` passes; the matrix is green on three releases
- [ ] Size, startup and coverage are re-measured, with a ratchet ADR
- [ ] A fresh review of the plan's whole range, one reviewer per area,
      finds no High or Medium. One it does find becomes a new numbered
      task here (test first, reviewed), and that area is reviewed again
      until a pass finds none. New Lows get ids in tasks/findings.md and
      a fix or a confirmed decline
- [ ] Every id in tasks/findings.md is fixed or has a confirmed decline;
      `scripts/check-plan-ids.mjs` passes
- [ ] Review with the maintainer (CONSTRAINTS "Last reviewed",
      docs-L16); the plan archived to `tasks/hardening/`

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A fix changes behaviour the matrix pinned | High | Each journal fix runs the matrix script that covers it on one release (Tasks 32-36); all three run at Checkpoint B. A mismatch that isn't a listed Difference stops for the maintainer. |
| The YAML fix needs a second parser or a patched dependency | High | Task 40 builds the corpus and the options first and stops for the maintainer. Task 41 has an ADR and a size check. |
| Two worktrees edit one file | High | Three sequential lanes with disjoint files; one owner for the shared files and three named cross-lane touches ("Lanes and shared files"); rebase and `check.sh full` before each merge. |
| A hardened script fails on a defect not yet fixed | Med | A named expected failure, marked `always` or `racy`, with the finding's id; the fixing task removes it, and none is left at Checkpoint B. |
| SIGHUP handling and the relayed-signal window interact | Med | Task 13 follows Task 12 at once. The window is a documented constant with an injected clock, probed under a real session scope and sudo. |
| No safe `setsid` exists without `unsafe` | Med | Task 7 researches it first. Detecting the stopped child and ssh's batch mode need no new session. |
| Overflow checks grow the binary or slow paging | Med | Task 48 measures size, startup and the pread-bound tests' time, with a ratchet ADR. |
| Timing startup on the musl binary moves the ratchet's basis | Med | Task 46 changes the CONSTRAINTS row and records both figures in an ADR. |
| A display fix changes a reviewed look | Med | Tasks 19-21 bring each change to the maintainer with screenshots of both. |
| A mutant named in a criterion is never generated | Med | The verification runs cargo-mutants on the named site and records the result. |
| A Low sweep grows into a large change | Med | A sweep fixes only its listed ids. A fix that needs more than the sweep's files, or that changes a design (a protocol, a public API), becomes its own task. |
| Parallel worktrees fill the disk | Med | Two at a time; `cargo clean` above 10 GB each; `df` before heavy runs; a worktree is removed when its lane ends. |
| Fixes drift from the findings | Low | Each task names its ids, and Task 5's script checks every id in tasks/findings.md against this plan and the task list at each checkpoint. |

## Open questions

These are the maintainer's decisions, grouped by the task where each
comes up, with the options and a recommendation. Each is marked by when
its answer is needed, so Checkpoint A waits only for what the gates and
the first tasks of lanes 1 and 2 need:
- **[start]** before work starts (asked at the plan's approval);
- **[checkpoint T]** by Checkpoint T, before lane 1's second phase;
- **[lane 3]** with the YAML answer, before lane 3's code (Task 41);
- **[build]** before the build phase (Task 46);
- **[task]** at its task, because research, a probe or a screenshot
  comes first.

**Publication [start]**
- tasks/findings.md lists security defects that aren't fixed yet.
  Nothing is pushed today. May the three task files be pushed before
  the fixes?
  - (a) Not until Checkpoint C. Recommended.
  - (b) Once the High items are fixed (after Checkpoints T and B).
  - (c) Any time.

**CLAUDE.md (docs-L15) [start]**
- Its two stale lines are the maintainer's to change: "Create these
  paths when first needed; they do not exist yet", and the `systemd-run`
  example without `OOMPolicy=continue`. Recommended: drop the first,
  add the option to the second.

**Task 3: acknowledged floor findings [start]**
- The Floor guard row becomes "zero unacknowledged floor violations",
  with the entry rules under "Architecture decisions". Accept the ADR,
  or keep the row and require an ADR for every expected finding?
  Recommended: accept.

**Tasks 7 and 14: a child that prompts on the terminal [task]**
- Task 7 researches first: whether a safe `setsid` for a child exists
  on Rust 1.99.1 without `unsafe` (`pre_exec` is `unsafe`, which the
  workspace forbids).
- (a) Fail fast by detection: sb notices the stopped child (the wait
  reports it, as exec's PTY path already asks) and fails the step with
  an error that names the prompt. No new session needed.
- (b) Fail fast by ssh's batch mode, for children that aren't attached:
  `GIT_SSH_COMMAND` with `BatchMode=yes` (and
  `SSH_ASKPASS_REQUIRE=never`). ssh then fails at once instead of
  prompting, with no stop and no `setsid`. It covers ssh only.
- (c) A new session for piped children, if a safe `setsid` exists: no
  child can reach the terminal, so every tool fails fast.
- (d) Keep the terminal reachable, as sb-go did (an ssh passphrase
  prompt under git worked there). The cost: the child must share sb's
  process group, which undoes exec's group-kill design and the reason a
  hangup can be handled per group, and the prompt lands inside a live
  tree.
- Recommended: (a) and (b) together, a listed difference from sb-go.
  sb already sets `GIT_TERMINAL_PROMPT=0`, which covers git's own
  prompt and not ssh's.

**Task 8: bidi and zero-width characters in table cells (san-I2) [start]**
- After Task 8 a cell goes through `DisplayText`, which passes bidi
  overrides and zero-width characters (term-ui spec, "Escape handling":
  "Bidi and zero-width characters pass unmarked"). An image label can
  then reorder a `docker ps` row.
  - (a) Escape them in table cells, as cli-core's `escape_controls`
    does for error text; the term-ui spec's sentence gets the table
    exception. Recommended.
  - (b) Pass them, as for a step's output.
- For log text shown by the runner and the viewer, the journal spec's
  display decision 2 already accepts them; Task 25 proposes declining
  that half.

**Task 12: SIGHUP under `nohup` [start]**
- `nohup sb ...` starts sb with SIGHUP ignored. Installing a handler
  overrides that.
  - (a) Keep it ignored when it was ignored at startup. Recommended:
    it is what `nohup` asks for.
  - (b) Always handle it, so a hangup cancels even under `nohup`.

**Task 13: the repeat window [start, the value at the task]**
- "A second signal ends sb at once" gets an exception for the copy sudo
  relays.
  - The value: 250 ms (the finding's figure), confirmed or changed by
    the probe. Recommended.
  - Whether it applies to SIGINT: (a) SIGTERM and SIGHUP only, so two
    quick Ctrl-C presses still force the exit; recommended, unless the
    probe shows SIGINT doubled too. (b) every signal.

**Task 18: `docker restart` against sb-go [checkpoint T]**
- The floor isn't a question: `restart` never ends without either the
  start requested or a line saying it wasn't.
- The notice "the controller may still be working": (a) on every
  ending without a final job status; recommended. (b) only on sb's own
  timeout, as sb-go.
- A failed poll: (a) tolerate a few in a row, then end with the notice;
  recommended. (b) end on the first, as sb-go.

**Task 19: ECHO off [checkpoint T]**
- The term-ui spec says only `ECHOCTL` is switched off. Turning ECHO
  off too hides typed characters during a live run. Accept and amend
  the spec's wording? Recommended: yes.

**Task 20: the tall live tree [task]**
- (a) Collapse settled lines to "… N finished" while live. Recommended:
  it keeps decision 3.
- (b) Print settled children for good as they settle (changes decision
  3).

**Task 21: reviewed looks [task]**
- Notices and kept output lines in the finished tree: (a) printed
  whole, wrapped by the terminal; recommended. (b) cut with `…`, as
  now.
- Table wrap: accept the three fixes (no blank line at exact multiples,
  padded bindings whole, a too-narrow terminal says so) after seeing
  the new snapshots? Recommended: yes.

**Task 25: kept SGR codes (san-L2) [checkpoint T]**
- Today every SGR is kept byte for byte and nothing closes it. The fix
  changes three passages: the term-ui spec's "Escape handling" (an SGR
  is "written from the input's own bytes ... byte-exact"), its success
  criterion 2 ("a child's own codes pass through unchanged"), and the
  journal spec's display decision 2 ("every SGR passes through when
  colour is on").
  - (a) Append a reset to any drawn line that kept an SGR, and drop
    font-select parameters (10 to 20), which garble the Linux console
    past sb's exit. Kept codes stay byte-exact. Recommended.
  - (b) The reset only; every SGR still passes.
  - (c) Decline: latent today apart from the Linux console.

**Task 32: a snapshot page against journalctl [start]**
- On a journal being written, a snapshot page can show fewer entries
  than journalctl, which reads live lists. Is that a listed Difference?
  - (a) Yes: list it, and stress.sh accepts journalctl's extra entries
    only at the tail and only when the follow then delivers them
    (Task 33). Recommended.
  - (b) No: the page must match journalctl, and the follow's start
    moves instead (a larger change).

**Task 32: the mirror case (follow-N1) [task]**
- A header read after the main-list link but before the item links
  makes the page miss an entry and the follow start past it: a lost
  entry on a live journal, against the spec's "0 lost, 0 doubled".
  - (a) Fix it in Task 32 if the same bound or a bounded re-read of the
    header closes it; a larger fix becomes its own task. Recommended:
    it can happen on a real server.
  - (b) Decline: list it in the spec's known limits with its window,
    and reword "0 lost".
- The task reports the fix's cost before the choice.

**Tasks 34 and 37: API names [start]**
- `Page::oldest_chosen` and `Page::newest_chosen`
  (`Option<Cursor>`): the finding's names. Recommended.
- `Journal::unit_query(&UnitName, Option<Uid>)`, with `Query::unit`
  kept or removed. Recommended: add the helper, keep `Query::unit`.
- `PageEnd` (page-L4): document that it is the journal's end, or
  rename it. Recommended: document.

**Task 35: the forward loop [start, the 22.04 row at the task]**
- (a) Change `boot_starts`, so a file that holds the cursor's boot
  always seeks per boot. Recommended: the finding's smaller fix.
- (b) Emulate journalctl 259.9's exact-match first step.
- 22.04: (a) fixed there too, with a Difference if journalctl 249 shows
  something else on the fixture; recommended, since the finding loops
  on all three releases. (b) left as journalctl 249 behaves, a listed
  Difference, with the viewer's cursor guard as the stop.
- The task records what journalctl shows on each release first.

**Tasks 40 and 41: YAML [task]**
- Which fix, after the research: a fix upstream, a pure-Rust
  libyaml-compatible parser for acceptance, or a pre-pass.
- Listing a difference isn't an option beside them (decision 3). It is
  only for a residual case the chosen fix can't cover, and each such
  case is the maintainer's to accept.

**Task 43: proxy variables [lane 3]**
- Go reads `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`, and the lowercase
  forms when the uppercase one is unset. (a) Read both, as Go;
  recommended. (b) Uppercase only.

**Task 45: on the wire, and patterns [lane 3]**
- A User-Agent: (a) `sb/<version>`; recommended. (b) none, as now.
  sb-go sends Go's default.
- Unicode classes in a schema `pattern`: (a) translate `\d`, `\w` and
  `\s` to Go's ASCII meaning; recommended, since the schemas are
  written for Go. (b) refuse such a pattern at load.

**Task 46: startup's basis [build]**
- Time startup on the musl production binary instead of the glibc
  release build? It changes the basis of the CONSTRAINTS Startup row and
  of the `sb --help` ratchet. Recommended: yes, with an ADR that records
  both figures.

**Task 48: overflow checks [task]**
- On in the release and production profiles? Recommended: on, unless
  the measurement shows a cost past a ratchet's tolerance.

**Task 50: `sb --color=never` with no subcommand (sb-L1) [build]**
- It exits 2 where bare `sb` prints help and exits 0; sb-go prints help
  for both. The docker spec records the same case for `sb docker` as a
  difference ("Docker group" and its differences list), and says the
  root behaves the same; the cli-core spec doesn't record the root's.
  - (a) Record the root's case as a difference in the cli-core spec,
    as the docker group's is. Recommended: it keeps the reviewed
    behaviour.
  - (b) Treat a missing subcommand as help, exit 0, at the root and for
    `sb docker`, and amend both docker spec passages.

**The Low sweeps (Tasks 6, 22-26, 37-39, 45, 50) [task]**
- Each sweep's proposed declines, listed in its task, to confirm or
  overrule before the sweep is committed.
- Task 23: the name of the API that masks a secret sent on stdin
  (exec-L5), and whether the mask's rules for very short secrets change
  (exec-L9; the exec spec defines today's).
- Task 50: mimalloc (sb-L12) is a new dependency; proposed-decline
  unless Task 48's measurement shows a need.
