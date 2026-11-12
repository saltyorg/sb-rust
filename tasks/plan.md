# Implementation Plan: hardening after the re-review (approved 2026-11-11)

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

The work is 52 small tasks: six on the gates and records, then three
lanes (text, process and display; the journal reader; config), then the
build, then a final review.

## Maintainer decisions (2026-11-11)

1. **Fix all High and Medium before the viewer spec.**
2. **A terminal hangup cancels cleanly.** SIGHUP is handled like
   SIGTERM: the children are stopped with the usual grace, the run log
   gets its ending, then sb ends by SIGHUP. This replaces the cli-core
   spec's "sb doesn't handle SIGHUP".
3. **sb accepts the YAML Ansible accepts.** The options are researched
   first, with pros and cons (Task 42), and the recommendation is
   followed as decision 14 says. A residual case the chosen fix can't
   cover goes to the maintainer. A standing differential test follows.
4. **Lows are not ignored.** Every Low in tasks/findings.md, and every
   unrated item (Info, test gaps, notes), is one of:
   - an acceptance criterion in the task that touches its code;
   - an entry in a per-crate Low sweep at the end of that crate's lane
     (the journal sweeps come before the reader checkpoint).

   Each sweep lists its ids, each marked fix or proposed-decline with a
   reason. A Low that can happen on a real Saltbox server is fixed. The
   candidates for declining are the ones that need crafted or imported
   files, or that match sb-go on purpose. The proposed declines go on
   one running list, confirmed or overruled in one sitting, and the
   sweep's fixes are committed without waiting (decision 15).

   The sweeps as drafted also propose declines on grounds the decision
   doesn't name. Each says which, and each needs its confirmation like
   any other:
   - a dependency's own behaviour that sb could change only by
     re-implementing the library;
   - a case sb's current-thread runtime can't reach, pinned by a test;
   - an API change with no consumer yet;
   - a reviewer's note that asks for no change;
   - a decision already recorded in a spec or a follow-up.

Answers at the plan's approval (2026-11-11):

5. **The plan is approved.** Work starts with Tasks 1-6.
6. **Task 2's ADR is accepted.** The Floor guard row becomes "zero
   unacknowledged floor violations".
7. **Bidi and zero-width characters are escaped in table cells**
   (san-I2). The term-ui spec gets the table exception.
8. **SIGHUP stays ignored when it was ignored at startup** (`nohup`).
9. **The repeat window applies to SIGTERM and SIGHUP only.** Its value,
   250 ms, is confirmed by the probe. If the probe shows SIGINT doubled
   under sudo, that comes back to the maintainer.
10. **API names:** `Page::oldest_chosen` and `Page::newest_chosen`;
    `Journal::unit_query(&UnitName, Option<Uid>)` is added and
    `Query::unit` kept; `PageEnd` is documented, not renamed.
11. **The forward loop:** `boot_starts` changes, on all three releases.
    22.04 gets a listed Difference only if journalctl 249 shows
    something else on the fixture.
12. **A page matches journalctl on a journal being written.** It keeps
    reading the live lists, and there is no new Difference. The
    follow's start moves instead, so that a page plus its follow still
    gives 0 lost and 0 doubled. (The plan had recommended a snapshot
    page; the maintainer chose otherwise.)
    - What "matches" is checked against, since a journalctl run at the
      same moment races the page: every entry a page shows is in
      journalctl's final view, with no gap; and the page plus its
      follow is that final view from the page's oldest entry on, each
      entry once (Task 35).
    - A Tail page already matches: `journalctl -n N` seeks to the
      tail once and shows at most N lines (systemd v255 journalctl.c
      2510-2514, 1958).
    - A forward page that reaches the end differs: journalctl reads
      the live count at each step and goes on to entries appended
      meanwhile, while sb's page stops at the count its header gave.
      Those entries come with the follow, not the page. Task 32's
      recommendation reads that as within this decision, and it is
      followed with the handover answer, as reversible (decision 14).

### How the plan runs unattended (2026-11-11)

13. **Checkpoints T, D, B, Y and C are reports, not stops.** Each one's
    "Review with the maintainer" becomes a written checkpoint report
    (what landed, the gate results, the reviews, the open risks), and
    work goes on. The maintainer reviews the commits afterwards.
    Nothing is pushed. Screenshots of looks (Task 21, Checkpoint D)
    still wait for the maintainer, and only that lane pauses; the
    screenshots Tasks 19 and 20 promise go into the checkpoint report
    instead. Each report is committed as
    `tasks/checkpoints/<checkpoint>.md`, with results summarised, no raw
    output and no host details. At Checkpoint C, setting CONSTRAINTS
    "Last reviewed" and confirming the collected declines stay open for
    the maintainer; the plan is archived after that sitting. (Added in
    Task 5, a tightening: every checkpoint, A included, also runs
    `node scripts/check-plan-ids.mjs tasks/findings.md tasks/plan.md
    tasks/todo.md` and CONSTRAINTS.md's two by-hand journal rows, and a
    failure blocks the checkpoint.)
14. **A research task's question takes the recommendation.** When the
    research for Tasks 7/14, 32, 34 and 42/43 is done, the task's
    recommendation is followed if a fresh-context review on Fable
    agrees with it. It is recorded as reversible, in the task and in
    its checkpoint report, for the maintainer to overrule on return,
    which may mean rework. The handover recommendation includes decision
    12's point on a forward page bounded by its header's count. A
    residual YAML difference the chosen fix can't cover still waits for
    the maintainer (decision 3).
    If the reviewer disagrees, or the research finds no option that
    meets the decisions above, the task and the tasks that depend on
    it wait for the maintainer, and other work goes on.
15. **Low sweeps commit their fixes and collect their declines.** Each
    sweep commits its fixes without waiting. Its proposed declines go on
    one running list in tasks/todo.md
    ("Proposed declines for the maintainer"), to be confirmed or
    overruled in one sitting. An overruled decline becomes a fix task.
16. **Task 6's proposed decline is confirmed:** the exception E1 half of
    docs-L12 stays as the E1 decision records.
17. **Answers given ahead of their tasks:**
    - Task 18: the "controller may still be working" notice on every
      ending without a final job status, and a few failed polls in a
      row tolerated before the notice (listed differences from sb-go).
    - Task 19: ECHO is switched off too during a live run, and the
      term-ui spec's ECHOCTL wording is amended.
    - Task 25 (san-L2): a reset is appended to any drawn line that kept
      an SGR, and font-select parameters 10 to 20 are dropped; other
      kept codes stay byte-exact. The three spec passages change.
    - Task 20: settled lines of a tall live tree collapse to
      "… N finished".
    - Task 45: the lowercase proxy variables are read too, as Go does.
    - Task 47: a `sb/<version>` User-Agent; `\d`, `\w` and `\s` in a
      schema pattern take Go's ASCII meaning.
    - Task 48: startup is timed on the musl production binary, with an
      ADR recording both figures.
    - Task 50: overflow checks on in the release and production
      profiles, unless the measurement shows a cost past a ratchet's
      tolerance.
    - Task 52 (sb-L1): the root's `--color=never` with no subcommand
      (exit 2) is recorded as a difference in the cli-core spec.
    If research at a task contradicts the premise of one of these
    answers, that task goes back to the maintainer.

Decided unattended, reversible
(2026-11-12; listed in Checkpoint T's report, the first one committed, and
kept or overruled at Checkpoint C's sitting). Neither had a Fable
review of its own before it was made; both were reviewed in the Fable
review of this alignment, which found them accurately stated and
checked the 26-of-290 count and the `go version -m` fact:

- **The commit-message lowercase rule (Task 4, docs-L14).** The
  description's first letter, after any leading non-letters (a
  backtick, a quote, a digit, punctuation), is lowercase; proper nouns
  and identifiers later in the description stay allowed. The literal
  reading, no uppercase anywhere, would reject 26 of the 290 existing
  commits, which the maintainer accepted, and would need history
  rewritten. It declines the stricter half of docs-L14, so it is also
  on "Proposed declines for the maintainer" in tasks/todo.md.
- **How tool versions are read (Task 4, docs-M4).** No cargo-installed
  tool is run for its version, since cross would sync a toolchain. The cargo-installed
  tools' versions come from `cargo install --list`; gitleaks' from
  `go version -m` on its binary, since its `go install` build can't
  report one; Go joins the CONSTRAINTS Tools table as a minimum-version
  row.

## Architecture decisions

- **Gates first.** The floor guard and `check.sh` are fixed before any
  code. Every later task then runs under a guard that sees non-Rust
  files, and `check.sh full` can pass with a finding acknowledged in the
  open, with no step skipped.
- **Acknowledged floor findings (Task 2, an ADR).** One tracked file,
  `scripts/floor-acknowledged.txt`. Each entry is one line: the rule,
  the file, the flagged line's text and a reason.
  - An entry covers exactly one finding with that rule, file and line
    text. Two findings need two entries.
  - No entry covers a file. Task 1's guard flags every changed or
    removed code line of its own scripts (`gate-line-removed`), and
    each such line needs its own entry. A commit that changes many
    lines alike carries many entries; each names its line, so a
    reviewer sees exactly what left the gate, and a removal nobody
    looked at can't ride along.
  - It lands second, before the guard's own patterns are extended
    (Task 3) and before Task 4: both edit existing lines of the guard
    scripts, which only passes once a finding can be acknowledged.
    Each acknowledges its own edits in its own commit.
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
- **A page and its follow give every entry once** (decision 12; Tasks
  32 to 35). The defect, the facts and the options are in Task 32 and
  in the handover question below; this is the frame.
  - **The invariant.** The follow's start, its placing of new files
    and its flush rule were all built on one statement: every entry
    below the count in `PageEnd` was examined by the page. A filtered
    page breaks it today from both sides. It can show an entry past
    the count (the double), and it can miss the last counted entry,
    whose value lists journald hadn't linked yet (the mirror case).
  - **The proposal** restores the invariant in the page: after its
    lookups the page reads the header once more and examines the
    main-list entries from just below the first count to the second
    by their items. `PageEnd` holds the second count. `PageEnd`, the
    follow's start and the flush and rotation rules then stay as they
    are. The other shape moves the follow's start and keeps a list of
    what the page showed past the count.
  - **Unfiltered queries stay out of the change.** They read no value
    list, so neither defect exists for them.
  - **What is established** from systemd's sources is listed in Task
    32 with its citations: the order journald links an entry in, that
    only the last counted entry can be half linked, that offsets rise
    with the index, what a flush copy keeps, and that journalctl reads
    live counts.
  - **What the probe still has to do** (Task 32): the flush code of
    249 and 259; the distribution's patch levels against the upstream
    tags; a run under writes that tries to refute the link order. One
    assumption can't be settled by reading at all: that a reader sees
    journald's stores in program order. It holds on x86-64.
  - **A limit that remains under every option:** the headers are read
    at different instants, so `PageEnd`'s newest entry can be newer
    than an entry of another file that the page didn't read. The
    entry is lost only if its file is gone before the follow opens it,
    which takes a flush in between, once per boot. Task 34 lists it in
    the spec, and Checkpoint B's report names it for the maintainer.
- **Test the format the releases write.** The crafted-file builder gains
  compact and keyed layouts, and the reader's suites run over both. This
  and the script hardening come before the journal fixes, so each fix is
  tested in both layouts and checked by a matrix that can't pass
  vacuously.
- **Named expected failures.** A hardened script, or the fuzz harness,
  may expose a defect that a later task fixes (follow.sh's new count
  check and follow-H1, for one). Such a check is listed as an expected
  failure in one line of the script itself, at the start of a line
  after any indentation:
  `expected_failure <id> <task> <always|racy> <name...>`, where the id
  is the finding's (`[a-z]+-[A-Z][0-9]+`). The guard matches that
  form. Entries aren't kept in a data file beside the script; the
  harness's are lines in seeds.sh.
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
- **Research tasks take their recommendation** (decision 14). Four
  questions are settled by research: Task 7 (the prompting child), Task
  32 (the handover), Task 34's flush question and Task 42 (YAML). None
  commits a failing test: a reproduction is a probe described in the
  write-up, and the test lands with the fix.
  - Each recommendation is checked by a fresh-context review on Fable.
    If it agrees, the recommendation is followed and recorded as
    reversible, in the task and in its checkpoint report, for the
    maintainer to overrule on return, which may mean rework.
  - If the review disagrees, or the research finds no option that meets
    the decisions, that task and the tasks that depend on it wait for
    the maintainer, and other work goes on.
  - Task 7 opens lane 1; Task 14 is coded to its recommendation. Task 32
    comes after the builder, the scripts and the harness its fix is
    tested with; Task 33 is the fix. Task 42 opens lane 3 and runs
    early, beside lanes 1 and 2, so its review is done before lane 3's
    code starts.
  - A residual YAML difference the chosen fix can't cover still waits
    for the maintainer (decision 3).
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
| 2: the journal reader | 27-41 | `crates/journal`, `crates/host/src/journal.rs` and its tests, the journal spec, the matrix workflow |
| 3: config | 42-47 | `crates/config`, cli-core's `config.rs`, new sb test files, the config spec |

A lane is sequential: its tasks share files (exec's `run.rs` and
`pty.rs`, term-ui's `runner.rs` and `tree.rs`, docker's `connect.rs`),
so each waits for the one before it. Lanes 1 and 2 start first, because
the viewer needs both. Lane 3's code starts when one of them frees a
worktree. Its first task, Task 42, is research only and runs early,
beside them, so its recommendation is reviewed by then.

Shared files have one owner at a time:
- **`Cargo.toml`, `Cargo.lock`, `deny.toml`:** the main checkout owns
  them. A lane task that needs a manifest change lands it on main, as
  its own commit, while no other open task touches those files; the
  other worktree rebases after it. That covers Task 43 (a parser), and
  Task 10 or Task 31 if one needs a workspace feature
  (tracing-subscriber, rustix). The build tasks (48-52) run on the main
  checkout, one at a time, after lanes 1 and 2 are merged.
- **`scripts/floor-acknowledged.txt`:** any lane may add entries. Lines
  are only added, so a merge takes the union of both sides' lines. No
  line is edited or removed.
- **The CONSTRAINTS ratchet rows** ("Measured, not yet enforced"):
  changed only on the main checkout, at Checkpoints B and C and in
  Tasks 48 and 50. A lane worktree never edits them. An Exceptions row a
  lane needs goes in that lane's own commit.
- **`tasks/todo.md` and `tasks/plan.md`:** ticked on the main checkout
  when a task merges.
- A worktree's commits are rebased onto main, and `check.sh full` runs
  again on the rebased commit before the merge.

Three tasks touch another lane's file, each in one place only:
- **Task 9** (lane 1) calls `escape_controls`, which lives in cli-core's
  `config.rs`, lane 3's file. It uses the function as it is, and
  neither moves nor changes it.
- **Task 44** (lane 3) changes cli-core's inventory read, which the
  cli-core spec states in one paragraph ("Config and inventory are read
  up to 1 MiB ..."). Task 44 edits that paragraph only; lane 1 leaves it
  alone.
- **Task 25** (lane 1) edits one line of the journal spec's display
  decision 2 for the SGR change (decision 17). Lane 2 leaves that
  decision alone.

## Traceability

Every id in tasks/findings.md is in one of the tables below. Where
several reviewers found one defect, the ids share a row. Task 5 adds a
script that checks this.

### High and Medium

| Ids | Finding | Task |
|---|---|---|
| docs-H1 | The floor guard is blind outside `.rs` and CONSTRAINTS.md | 1 |
| docs-H2 | `check.sh full` has no acknowledgement path | 2 |
| docs-M2 | The guard's false negatives in Rust files | 3 |
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
| follow-H1, page-H1, fwd-M2 | A page shows an entry its `end` doesn't count; the follow repeats it | 32, 33, 34, 35 |
| page-M1, page-M2 | A page of unreadable entries can't be paged past; `more_newer` stuck | 36 |
| fwd-M1 | A forward page loops on the cursor's own boot | 37 |
| file-M1 | A forward walk loses every array after a bogus item | 38 |
| config-H1 | Valid YAML refused | 42, 43 |
| config-M1 | A small config file exhausts memory or the stack | 44 |
| config-M2 | `ALL_PROXY` honoured | 45 |
| sb-M1, sb-M4 | No gate runs the production binary; cross drops the version | 48 |
| sb-M3 | Root tests skip silently | 49 |
| sb-M5 | Overflow checks off in production | 50 |
| sb-M6 | Startup parses and renders more than it needs | 51 |

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
| follow-N1 | 32, 33 |
| fwd-L3 | 37 |
| file-L2, file-L3, file-L4, page-N1 | 38 |
| config-L1, config-T1 (acceptance, the parity tables) | 42, 43 |
| config-T1 (memory) | 44 |
| config-T1 (the proxy test) | 45 |
| config-L2, config-L3, config-L4 | 46 |
| sb-L8, sb-L7 (the lookup's test run on musl) | 48 |
| sb-L10, docs-L13 (the silent non-root skip) | 49 |
| sb-L11 | 50 |
| sb-L5 | 51 |

Lows in a sweep (each sweep's task lists fix or proposed-decline):

| Sweep | Ids | Task |
|---|---|---|
| Documents | docs-L1, docs-L2, docs-L3, docs-L4, docs-L7, docs-L8, docs-L9, docs-L10, docs-L11, docs-L12, docs-L15, docs-L16 | 6 |
| cli-core | cli-L1, cli-L2, cli-L4, cli-L5, cli-L6, cli-L7, cli-T1 (the log directory cases) | 22 |
| exec | exec-L3, exec-L4, exec-L5, exec-L7, exec-L8, exec-L9 | 23 |
| docker | docker-L3, docker-L4, docker-L5, docker-L6, docker-L7, docker-L8, docker-L9 | 24 |
| The sanitizer | san-L1, san-L2, san-I2 (log text), san-I3, san-I4, san-T1 (cut, alphabet) | 25 |
| term-ui's runner | ui-L6, ui-L7, ui-L8 | 26 |
| Journal: paging and API | page-L1, page-L2, page-L3, page-L4, page-L5, page-L6, page-L7, page-L9, fwd-L4, fwd-L5, docs-L5 | 39 |
| Journal: the follow | follow-L1, follow-L2, follow-L3, follow-L4, follow-L5, follow-L6, follow-L7 | 40 |
| Journal: test evidence | tests-L4, tests-L8, tests-L9, tests-L10, tests-L12 | 41 |
| config | config-L5, config-L6, config-L7, config-L8, config-L9, config-L10 | 47 |
| The sb binary | sb-L1, sb-L2, sb-L3, sb-L4, sb-L6, sb-L7, sb-L9, sb-L12, config-L11 | 52 |

## Dependency graph

```
T1 ─ T2 ─ T3 ─ T4 ─ T5 ─ T6 ─ [A]

[A] ─┬─ lane 1: T7 ─ T8 ─ T9 ─ T10 ─ T11 ─ T12 ─ T13 ─ T14 ─ T15 ─ T16 ─ T17 ─ [T]
     │          [T] ─ T18 ─ T19 ─ T20 ─ T21 ─ T22 ─ T23 ─ T24 ─ T25 ─ T26 ─ [D]
     │
     ├─ lane 2: T27 ─ T28 ─ T29 ─ T30 ─ T31 ─ T32 ─ (its review)
     │          T33 ─ T34 ─ T35 ─ T36 ─ T37 ─ T38 ─ T39 ─ T40 ─ T41 ─ [B]
     │
     └─ lane 3: T42 ─ (its review) ─ T43 ─ T44 ─ T45 ─ T46 ─ T47 ─ [Y]

[D] + [B] ─ T48 ─ T49 ─ T50 ─ T51 ─ T52 ─┐
[Y] ─────────────────────────────────────┴─ [C]
```

Each lane runs in the order shown. The dependencies that aren't just
"the task before":
- T14 follows T7's recommendation once a fresh-context review agrees
  (decision 14); the tasks between don't need it.
- T33 follows T32's recommendation, and T34 the flush recommendation,
  each once reviewed. If a review disagrees, lane 2 waits there for the
  maintainer.
- T43 follows T42's recommendation once reviewed; a residual YAML
  difference waits for the maintainer. T42 runs early, beside lanes 1
  and 2.
- T22 waits until the maintainer has seen T21's looks (decision 13);
  only lane 1 pauses for them.
- T10 after T9: both edit cli-core's log and exit code.
- T14 and T15 after T11: exec's `run.rs` and `pty.rs`.
- T17 after T10 (cli-core's `log.rs`; the panic message goes through its
  escaping) and after T16.
- T19, T20 and T21 after T11 and T17 (term-ui's `runner.rs` and
  `tree.rs`).
- T24 after T10: docker's `connect.rs`.
- T27 to T31 (the builder, the scripts and the harness) before T33 to
  T38 (the fixes).
- T48 after T17 (sb's `main.rs`) and T31 (the workflow); T50 after T31
  (the fuzz harness runs once with overflow checks on).

## Task list

### Phase 0: the gates and the records
- [x] Task 1: the floor guard sees every gate file
- [x] Task 2: `check.sh` accepts acknowledged floor findings
- [x] Task 3: the floor guard's false negatives in Rust files
- [x] Task 4: `check.sh` runs the checks that exist only on paper
- [x] Task 5: the gate records are put right
- [x] Task 6: Low sweep: documents

### Checkpoint A: gates
- [ ] `check.sh full` passes on its own change, every step run, with
      any expected finding acknowledged in the open
- [x] Six [start] questions were answered on 2026-11-11 (decisions 6
      to 12); Publication and docs-L15 were deferred, and no task
      waits on either
- [x] Task 6's proposed decline (the exception E1 half of docs-L12) is
      confirmed (decision 16)
- [x] Decisions 13 to 17 set how the plan runs unattended and answered
      nine questions ahead of their tasks
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
- [ ] A checkpoint report is committed as `tasks/checkpoints/T.md`
      (decision 13)

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
- [ ] The sweeps' proposed declines are on the collected list
- [ ] The maintainer has seen Task 21's looks, from screenshots
- [ ] A checkpoint report is committed as `tasks/checkpoints/D.md`,
      with Tasks 19 and 20's screenshots described

### Phase 3 (lane 2): the journal reader
- [ ] Task 27: the crafted builder writes compact and keyed files
- [ ] Task 28: the reader's suites run over both layouts
- [ ] Task 29: the page scripts can't pass vacuously
- [ ] Task 30: follow.sh checks order, count and overflow
- [ ] Task 31: the fuzz seeds and harness check what they read
- [ ] Task 32: the page-to-follow handover: facts, probe and options
- [ ] Task 33: a page and its follow give every entry once
- [ ] Task 34: the handover across files
- [ ] Task 35: stress.sh checks the handover under writes
- [ ] Task 36: a page can always be paged past
- [ ] Task 37: a forward page on the cursor's own boot
- [ ] Task 38: forward walks over damaged entry arrays
- [ ] Task 39: Low sweep: journal paging and API
- [ ] Task 40: Low sweep: the journal follow
- [ ] Task 41: Low sweep: journal test evidence

### Checkpoint B: reader
- [ ] The matrix (compare, compat, follow, stress) is green on 22.04,
      24.04 and 26.04 with no expected failure left, and the fuzz
      harness passes, on the final tree
- [ ] stress.sh's handover check ran above both its floors on each
      release: every page and its follow gave journalctl's entries once
- [ ] Size, startup and coverage are re-measured; the ratchet rows are
      updated, with a ratchet ADR if a value passes its tolerance
- [ ] The new API is reviewed (decision 10's names; what the handover
      changed); the sweeps' proposed declines are on the collected list
- [ ] A checkpoint report is committed as `tasks/checkpoints/B.md`

### Phase 4 (lane 3): config
- [ ] Task 42: YAML: the corpus and the options
- [ ] Task 43: sb accepts the YAML Ansible accepts
- [ ] Task 44: config files can't exhaust memory or the stack
- [ ] Task 45: proxies as Go reads them
- [ ] Task 46: secrets are hidden wherever their key sits, and in `Debug`
- [ ] Task 47: Low sweep: config

### Checkpoint Y: config
- [ ] `check.sh full`; the differential corpus passes
- [ ] The sweep's proposed declines are on the collected list
- [ ] A checkpoint report is committed as `tasks/checkpoints/Y.md`

### Phase 5: the build
- [ ] Task 48: the production binary is run by the gate
- [ ] Task 49: skipped tests are counted, and the gate runs as root
- [ ] Task 50: overflow checks in the shipped profiles
- [ ] Task 51: startup parses once
- [ ] Task 52: Low sweep: the sb binary

### Checkpoint C: complete
- [ ] `check.sh full` passes; the matrix is green on three releases
- [ ] Size, startup and coverage are re-measured, with a ratchet ADR
- [ ] A fresh review of the plan's whole range, one reviewer per area,
      finds no High or Medium. One it does find becomes a new numbered
      task here (test first, reviewed), and that area is reviewed again
      until a pass finds none. New Lows get ids in tasks/findings.md and
      a fix or a confirmed decline
- [ ] A checkpoint report is committed as `tasks/checkpoints/C.md`
- [ ] The maintainer's sitting: the collected declines confirmed or
      overruled (an overruled one becomes a fix task first), the
      followed recommendations and the unattended calls kept or
      overruled, CONSTRAINTS "Last reviewed" set (docs-L16)
- [ ] Every id in tasks/findings.md is fixed or has a confirmed decline;
      `scripts/check-plan-ids.mjs` passes; then the plan is archived to
      `tasks/hardening/`

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| The page-to-follow handover is a design change, with races that tests must place by hand | High | Task 32 records what the sources establish, probes the rest and recommends an option; a fresh-context review on Fable checks it before any code, and if it disagrees, lane 2 waits for the maintainer (decision 14). Crafted tests place each race (Tasks 33, 34), and the doubling test raises the header and drains again, so it can't pass vacuously. stress.sh checks the handover under real writes on three releases (Task 35), with floors at Checkpoint B. |
| A file is gone between the page and the follow with an entry nobody showed | Med | It takes a flush between the two, once per boot. It remains under every option; Task 34 lists it in the spec as a limit, and Checkpoint B's report names it for the maintainer. |
| A fix changes behaviour the matrix pinned | High | Each journal fix runs the matrix script that covers it on one release (Tasks 33-38); all three run at Checkpoint B. A mismatch that isn't a listed Difference stops for the maintainer. |
| The YAML fix needs a second parser or a patched dependency | High | Task 42 builds the corpus and the options first; its recommendation is followed once a fresh-context review agrees, and a residual difference waits for the maintainer. Task 43 has an ADR and a size check. |
| Two worktrees edit one file | High | Three sequential lanes with disjoint files; one owner for the shared files and three named cross-lane touches ("Lanes and shared files"); rebase and `check.sh full` before each merge. |
| A hardened script fails on a defect not yet fixed | Med | A named expected failure, marked `always` or `racy`, with the finding's id; the fixing task removes it, and none is left at Checkpoint B. |
| SIGHUP handling and the relayed-signal window interact | Med | Task 13 follows Task 12 at once. The window is a documented constant with an injected clock, probed under a real session scope and sudo. |
| No safe `setsid` exists without `unsafe` | Med | Task 7 researches it first. Detecting the stopped child and ssh's batch mode need no new session. |
| Overflow checks grow the binary or slow paging | Med | Task 50 measures size, startup and the pread-bound tests' time, with a ratchet ADR. |
| Timing startup on the musl binary moves the ratchet's basis | Med | Task 48 changes the CONSTRAINTS row and records both figures in an ADR. |
| A display fix changes a reviewed look | Med | Tasks 19 and 20 follow decision 17, with screenshots in Checkpoint D's report; Task 21's looks wait for the maintainer, and only lane 1 pauses (decision 13). |
| A followed recommendation is overruled on return | Med | Each is recorded as reversible in its task and its checkpoint report, with the tasks built on it, so the rework is known and scoped (decision 14). |
| Proposed declines wait until Checkpoint C | Low | One running list in tasks/todo.md, each line with its id, task, reason and date; an overruled decline becomes a fix task before the archive (decision 15). |
| A mutant named in a criterion is never generated | Med | The verification runs cargo-mutants on the named site and records the result. |
| A Low sweep grows into a large change | Med | A sweep fixes only its listed ids. A fix that needs more than the sweep's files, or that changes a design (a protocol, a public API), becomes its own task. |
| Parallel worktrees fill the disk | Med | Two at a time; `cargo clean` above 10 GB each; `df` before heavy runs; a worktree is removed when its lane ends. |
| Fixes drift from the findings | Low | Each task names its ids, and Task 5's script checks every id in tasks/findings.md against this plan and the task list at each checkpoint. |

## Open questions

Of the eight questions asked at approval, six were answered (decisions
6 to 12) and two were deferred: Publication and CLAUDE.md's two lines.
Decision 17 answered nine more ahead of their tasks, and decision 14
settles the research questions by their recommendation. What is left is
marked:
- **[review]** a research question: its recommendation is followed once
  a fresh-context review on Fable agrees, and recorded as reversible;
  only if the review disagrees does it wait for the maintainer;
- **[task]** a real stop at its task, for the maintainer;
- **[deferred]** put off at approval; no task waits for it.

**Publication [deferred]**
- tasks/findings.md lists security defects that aren't fixed yet.
  Nothing is pushed without the maintainer's explicit authorization, so
  no task waits for this. When a push is considered: (a) not until
  Checkpoint C, recommended; (b) once the High items are fixed; (c) any
  time.

**CLAUDE.md (docs-L15) [deferred]**
- Its two stale lines are the maintainer's own to change: "Create these
  paths when first needed; they do not exist yet", and the `systemd-run`
  example without `OOMPolicy=continue`.

**Answered ahead of their tasks (decision 17)**
- Task 18: the notice on every ending without a final job status; a
  few failed polls in a row tolerated.
- Task 19: ECHO off during a live run; the ECHOCTL wording amended.
- Task 20: settled lines collapse to "… N finished".
- Task 25 (san-L2): a reset after a kept SGR; font-select parameters
  10 to 20 dropped.
- Task 45: the lowercase proxy variables read too.
- Task 47: a `sb/<version>` User-Agent; Go's ASCII `\d`, `\w`, `\s`.
- Task 48: startup timed on the musl production binary, with an ADR.
- Task 50: overflow checks on, unless a cost passes a ratchet's
  tolerance.
- Task 52 (sb-L1): the root's case recorded as a difference.

**Tasks 7 and 14: a child that prompts on the terminal [review]**
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

**Task 13: SIGINT doubled under sudo [task, only if the probe shows it]**
- Decision 9 keeps the window to SIGTERM and SIGHUP. If the probe
  shows a terminal's SIGINT relayed twice as well, the maintainer
  decides whether the window covers it.

**Task 21: reviewed looks [task]**
- Notices and kept output lines in the finished tree: (a) printed
  whole, wrapped by the terminal; recommended. (b) cut with `…`, as
  now.
- Table wrap: accept the three fixes (no blank line at exact multiples,
  padded bindings whole, a too-narrow terminal says so) after seeing
  the new snapshots? Recommended: yes.

**Task 32: the shape of the handover [review]**
- Decision 12 fixes the outcome: a page plus its follow gives every
  entry once, with no new Difference. How is a design choice. Task 32's
  probe and design note come first; Task 33 is coded to the answer.
  - (a) **One re-read in the page.** For a filtered query, per file:
    the header is read (n0), every value is looked up, the header is
    read once more (n1), and the main-list entries from n0-1 to n1 are
    examined by their items, as the follow examines entries, and
    merged into the file's stream by offset. `PageEnd` holds n1.
    - It changes the page's reads, not the follow's code: the follow
      starts as it does today, only from a later count. That differs
      from decision 12's wording ("the follow's start moves instead"),
      though not from its outcome. Decision 14 covers that: the
      recommendation is followed as reversible, and the maintainer may
      overrule it on return.
    - Why it is exact: an entry found in a looked-up list was counted
      before it was linked there, so its index is below n1 and nothing
      shown is past the count. Every entry below n0-1 was fully linked
      before the first header read. The entries from n0-1 to n1 are
      matched by their items, which covers two lists read at different
      moments and the mirror case.
    - For: `PageEnd`, its equality and size, the follow's start, and
      the flush and rotation rules stay as they are. It keeps the
      invariant they were built on.
    - Costs:
      - The page's read path changes in walk.rs and forward.rs. The
        forward page is the hard part: an extra entry can lie before
        the landing the seek found, so the stream must take it by the
        cursor's order, not from the landing.
      - A value first written during the page has no DATA object at
        lookup, so the re-read must look it up again, as the follow
        does.
      - The page reads the entries appended while it ran: usually
        none.
      - The re-read starts at n0-1 only for a file that isn't sealed.
        Otherwise the tail shows a cut-off entry of an archived file
        (Difference 12).
      - `PageEnd`'s newest entry is kept from the first headers, or
        the directory is listed again after the second read.
        Otherwise a file created during the page loses its first
        entries.
      - It hasn't been built.
    - Recommended, provided Task 32's design note shows the forward
      page takes it cleanly.
  - (b) **An index and the entries shown past it.** `PageEnd` holds,
    per file, the header's count and the entries the page delivered
    at or past it, by offset. The follow reads from the count and
    drops those entries.
    - The last counted entry is read again only where the mirror case
      can exist and was seen: a filtered query, a file that isn't
      sealed, and an entry that matches by its items while the page's
      lists didn't hold its offset. Reading it again for every file
      would redeliver an old entry from each archived file.
    - The drop lasts until the file's position passes the last shown
      entry, not for the first batch only: a first batch can stop at
      the output limit.
    - `PageEnd`'s newest entry is not raised to the newest record
      shown. Raising it would make a new file's start pass over the
      copy of an older entry the page didn't show.
    - The flush cases are as the flush question states them, against
      Difference 13.
    - For: the page's reads don't change.
    - Against: `PageEnd` grows with the entries shown past the count
      and its equality changes; the follow's start gains a filter that
      outlives the first batch; more rules, each with its own race.
    - The fallback if (a) doesn't fit the forward page.
  - (c) **A high-water mark only:** per file, the position past the
    newest entry the page showed. Not recommended: it loses an entry
    that is older than one the page showed but was linked later (two
    lists, and the mirror case), which breaks "0 lost".
- Two weaker shapes, not proposed:
  - One call that pages and then follows, keeping the page's files
    open. It is the only shape that closes the file-gone limit, but it
    costs a new public entry point, and `follow(PageEnd)` would still
    need a fix or removal.
  - `PageEnd` holding every shown record's cursor. It removes (b)'s
    boundary logic, but `PageEnd` is then always as large as the page,
    and equality compares the whole page.
- With it, one point of decision 12, which the followed recommendation
  includes (decision 14): is a page bounded by its header's count within
  "a page matches journalctl"?
  - A Tail page already matches: `journalctl -n N` seeks to the tail
    once and shows at most N lines (systemd v255 journalctl.c
    2510-2514, 1958).
  - Only a forward page that reaches the end differs. journalctl
    reads the live count at each step and goes on to entries appended
    meanwhile; sb's unfiltered page stops at the count its header
    gave, and under (a) a filtered page stops at its second header
    read. The entries written after that come with the follow.
  - The plan reads that as a yes: what is checked is journalctl's
    final view, not a run at the same moment.
  - What a "no" would take: a forward page that reaches its count
    reads the header again and goes on, up to the new count, and
    `PageEnd` is built after the walk instead of before it. That
    touches forward.rs and the page's order of reads, for filtered
    pages on top of (a)'s re-read. The gain for a viewer is nil: the
    follow delivers the same entries.

**Task 34: a flush between the page and the follow [review]**
- A flush copies /run's entries into /var and removes /run. It
  happens once per boot, early.
- Where the page read the /var file, every flush copy is shown again
  today, as Difference 13 says of a followed file. That stays.
- Where the /var file is new to the follow, a copy of an entry the
  page showed from /run can come again:
  - (a) Extend Difference 13's wording to a page and a follow on
    either side of a flush. No code. It widens a listed Difference.
    Recommended: a repeat once per boot is the safer failure than a
    loss.
  - (b) Carry, for each entry the page showed that is newer than
    `PageEnd`'s newest entry, its boot id, monotonic and realtime
    stamps and xor hash, and drop one copy of each. Only those entries
    need carrying: under handover (a), the ones the re-read added. The
    cost: `PageEnd` grows by them and its equality changes, for a case
    that happens once per boot.
- A limit remains under every option, named in Checkpoint B's report
  for the maintainer to see: the headers are read at different
  instants, so `PageEnd`'s newest entry can be newer than an entry of
  another file that the page didn't read. That entry is lost only if
  its file is gone before the follow opens it, which takes a flush in
  between. Task 34 lists it in the spec. Closing it needs the one-call
  shape above.

**Task 37: 22.04 on the forward-loop fixture [task, only if it differs]**
- Decision 11 allows a listed Difference on 22.04 if journalctl 249
  shows something else there. Its wording is the maintainer's to
  approve; a difference on 24.04 or 26.04 stops for the maintainer.

**Tasks 42 and 43: YAML [review]**
- Which fix, after the research: a fix upstream, a pure-Rust
  libyaml-compatible parser for acceptance, or a pre-pass.
- Listing a difference isn't an option beside them (decision 3). It is
  only for a residual case the chosen fix can't cover, and each such
  case is the maintainer's to accept: that part stays a stop.

**The Low sweeps (Tasks 6, 22-26, 39-41, 47, 52)**
- Each sweep commits its fixes and adds its proposed declines to the
  collected list in tasks/todo.md, confirmed or overruled in one
  sitting at Checkpoint C (decision 15). An overruled decline becomes a
  fix task.
- **[task]** Task 23: the name of the API that masks a secret sent on
  stdin (exec-L5), and whether the mask's rules for very short secrets
  change (exec-L9; the exec spec defines today's).
- Task 52: mimalloc (sb-L12) is a new dependency; proposed for the
  collected list unless Task 50's measurement shows a need.
