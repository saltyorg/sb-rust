# Implementation Plan: hardening after the re-review (approved 2026-11-11, cut 2026-11-12)

Findings: [tasks/findings.md](findings.md) (every item, with its id).
The cut: [tasks/triage.md](triage.md) (every open id's verdict and
reason). Specs: each crate's spec in `docs/specs/`. Quality bar:
[CONSTRAINTS.md](../CONSTRAINTS.md). Tasks: [tasks/todo.md](todo.md).
Earlier plans are in `tasks/<module>/`.

## Overview

Thirteen fresh-context reviewers re-read every crate at 96e940a. The
plan approved on 2026-11-11 had 52 tasks and gave every finding a
place. Tasks 1-6 (the gates and the records) are done.

On 2026-11-12 the maintainer cut the plan to the findings that can
happen when sb runs on a Saltbox server (decision 18). 29 ids are
left, about 21 distinct defects, in 21 tasks (Task 35 was merged into
Task 33, decision 21). The surviving tasks keep
their original numbers, so the numbers have gaps. The TUI log viewer
spec waits for this plan: the viewer builds on paging, the follow,
tables and the error display.

The work runs in three lanes: text, process and display; the journal
reader; config.

## Maintainer decisions (2026-11-11)

1. *Superseded by decision 18.* (Was: fix all High and Medium before
   the viewer spec.)
2. **A terminal hangup cancels cleanly.** SIGHUP is handled like
   SIGTERM: the children are stopped with the usual grace, the run log
   gets its ending, then sb ends by SIGHUP. This replaces the cli-core
   spec's "sb doesn't handle SIGHUP".
3. **sb accepts the YAML Ansible accepts.** The options are researched
   first, with pros and cons (Task 42), and the maintainer chooses
   (decision 20). A residual case the chosen fix can't cover goes to
   the maintainer. A standing differential test follows.
4. *Superseded by decision 18.* (Was: Lows are not ignored; each is a
   task's criterion or in a per-crate Low sweep.)

Answers at the plan's approval (2026-11-11):

5. **The plan is approved.** Work starts with Tasks 1-6.
6. **Task 2's ADR is accepted.** The Floor guard row becomes "zero
   unacknowledged floor violations".
7. *Superseded by decision 18: san-I2 is dropped; confirmed by
   decision 19.* (Was: bidi and zero-width characters are escaped in
   table cells.)
8. **SIGHUP stays ignored when it was ignored at startup** (`nohup`).
9. **The repeat window applies to SIGTERM and SIGHUP only.** Its value,
   250 ms, is confirmed by the probe. If the probe shows SIGINT doubled
   under sudo, that comes back to the maintainer.
10. **API names:** `Page::oldest_chosen` and `Page::newest_chosen`;
    `PageEnd` is documented, not renamed. (`Journal::unit_query` was
    part of this decision; it is deferred with page-L5, decision 18.)
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
      entry once (Task 33).
    - A Tail page already matches: `journalctl -n N` seeks to the
      tail once and shows at most N lines (systemd v255 journalctl.c
      2510-2514, 1958).
    - A forward page that reaches the end differs: journalctl reads
      the live count at each step and goes on to entries appended
      meanwhile, while sb's page stops at the count its header gave.
      Those entries come with the follow, not the page. Task 32's
      recommendation reads that as within this decision; the
      maintainer decides it with the handover answer (decision 20).

### How the plan runs (2026-11-11; narrowed by decisions 18 to 22)

13. **Checkpoints T, D, B, Y and C are not stops.** Work goes on after
    each. The maintainer reviews the commits afterwards. Nothing is
    pushed. Screenshots of looks (Task 21, Checkpoint D) still wait for
    the maintainer, and only that lane pauses. At Checkpoint C,
    confirming the proposed declines stays open for the maintainer; the
    plan is archived after that sitting. Every checkpoint also runs
    CONSTRAINTS.md's two by-hand journal rows, and a failure blocks the
    checkpoint.
    - Narrowed: the plan id check that ran at every checkpoint was
      retired on 2026-11-12. Setting CONSTRAINTS "Last reviewed" at
      Checkpoint C went with docs-L16. The committed report files are
      gone (decision 22).
14. *Superseded by decision 20.* (Was: a research task's
    recommendation is followed once a fresh-context review agrees.)
15. **Proposed declines are collected.** A proposed decline goes on
    one running list in tasks/todo.md ("Proposed declines for the
    maintainer"), to be confirmed or overruled in one sitting. An
    overruled decline becomes a fix task.
    - Narrowed: no Low sweep remains. The DROP table in tasks/triage.md
      replaces the sweeps' declines, and those ids are closed.
16. **Task 6's proposed decline is confirmed:** the exception E1 half of
    docs-L12 stays as the E1 decision records.
17. **Answers given ahead of their tasks:**
    - Task 18: the "controller may still be working" notice on every
      ending without a final job status, and a few failed polls in a
      row tolerated before the notice (listed differences from sb-go).
    - Task 19: ECHO is switched off too during a live run, and the
      term-ui spec's ECHOCTL wording is amended.
    - Task 20: settled lines of a tall live tree collapse to
      "… N finished".
    If research at a task contradicts the premise of one of these
    answers, that task goes back to the maintainer.
    - Narrowed: the answers for Tasks 25, 45, 47, 48, 50 and 52 went
      with those tasks.

### The cut (2026-11-12)

18. **The plan is cut to what can happen when sb runs on a Saltbox
    server.** The maintainer adopted the triage in
    [tasks/triage.md](triage.md).
    - The 29 KEEP ids stay, the unsure ones included. The 21 DEFER ids
      and the 102 DROP ids leave the plan.
    - 22 tasks survive, 9 whole and 13 reduced, under their original
      numbers. 24 tasks are gone: 7, 11, 14-16, 22-26, 29-31, 39-41,
      44, 45 and 47-52.
    - A DEFER id comes back when its feature is specified: the TUI log
      viewer spec for the journal and exec streaming items, release
      preparation for the release items. A DROP id is closed.
    - The drop of config-M1 is confirmed explicitly: it needs a file
      the server owner would have to write themselves.
    - This replaces decision 1 and decision 4, and supersedes decision
      7. It narrows decisions 13 to 17, as each says above.
    - Checkpoint C's criteria change to: every KEEP id is fixed, or
      declined with the maintainer's confirmation. The fresh review of
      the whole range is gone.
    - `scripts/check-plan-ids.mjs` is retired. Nothing runs it.
    - Reviews follow CLAUDE.md's "Fresh-context review, by tier"
      (ADR-016). A commit of `docs/` and `tasks/` only gets no
      review. A task that changes Rust or the gate gets one
      `agent-skills:code-reviewer` pass on one model, plus the
      library-reviewer and the security-auditor where its task says so.
      A fix is reviewed again only when it changes behaviour beyond the
      finding. No gate, plan or record work is done without a defect
      that affects sb.

### Answers after the cut (2026-11-12)

19. **Decision 7's end is confirmed.** Bidi and zero-width characters
    in table cells are not escaped; san-I2 stays dropped.
20. **The maintainer chooses after research.** A research task (Tasks
    32, 34's flush question and 42) ends by putting its options, their
    pros and cons and a recommendation to the maintainer, who chooses.
    The tasks that build on the answer (33, 34 and 43) wait for that
    choice; other work goes on. This replaces decision 14.
21. **Task 35 is merged into Task 33.** What is left of it is one
    follow.sh scenario on real journald, run as Task 33's
    verification, that shows a page plus its follow gives every entry
    once. The stress.sh check and its floors are gone.
22. **Checkpoints have no report files.** A checkpoint is its ticked
    criteria in tasks/todo.md plus a short message to the maintainer.
    The screenshots of Tasks 19, 20 and 21 are shown to the maintainer
    directly.
23. **The handover is option (a), one re-read in the page** (Task 32's
    question, 2026-11-12). Per unsealed file a filtered page reads the
    header again after its lookups (n1), matches the main-list entries
    from n0-1 to n1-1 by their items and merges them by offset;
    `PageEnd` holds n1, and the follow's start, the flush and the
    rotation rules stay as they are. This keeps decision 12's outcome
    while departing from its wording ("the follow's start moves
    instead"), which the maintainer accepted with the choice. Task 32's
    probe backs it: on 22.04, 24.04 and 26.04 no entry was found in a
    value's list before the header counted it, and the newest counted
    entry was often not listed yet (the mirror case). Task 32's design
    note is below the handover question.
24. **A forward page at the journal's end is within decision 12**
    (2026-11-12). It stops at the count it read (n0 unfiltered, n1
    filtered), and the follow delivers the entries appended meanwhile.
    "Matches journalctl" means every entry the page shows is in
    journalctl's view, and the page plus its follow is the whole view.

Decided unattended, reversible
(2026-11-12; named in Checkpoint T's message, and kept or overruled at
Checkpoint C's sitting). Neither had a Fable
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

- **Gates first (done, Tasks 1-6).** The floor guard and `check.sh`
  were fixed before any code. Acknowledged floor findings are recorded
  in ADR-015: one tracked file, `scripts/floor-acknowledged.txt`, one
  line per finding, added in the commit it acknowledges and never
  edited or removed.
- **One choke point per sink.** Untrusted text is made safe where it is
  written, so callers don't each remember to escape:
  - table cells in `Table`;
  - error text in cli-core's error block;
  - log values in the run log's formatter layer (M-LOG-STRUCTURED),
    which escapes control characters, `\n` and `\r` in every field of
    every record, bridged dependency records included.
- **A page and its follow give every entry once** (decision 12; Tasks
  32 to 34). The defect, the facts and the options are in Task 32 and
  in the handover question below; this is the frame.
  - **The invariant.** The follow's start, its placing of new files
    and its flush rule were all built on one statement: every entry
    below the count in `PageEnd` was examined by the page. A filtered
    page breaks it today from both sides. It can show an entry past
    the count (the double), and it can miss the last counted entry,
    whose value lists journald hadn't linked yet (the mirror case).
  - **Unfiltered queries stay out of the change.** They read no value
    list, so neither defect exists for them.
  - **A limit remains under every option:** an entry can be lost if
    its file is gone before the follow opens it, which takes a flush
    in between, once per boot (the flush question below). Task 34
    lists it in the spec, and Checkpoint B's message names it for the
    maintainer.
- **Test the format the releases write.** The crafted-file builder
  gains a compact layout, and the suites the kept journal fixes touch
  run over both (Tasks 27 and 28). This comes before the journal
  fixes, so each fix is tested in both layouts.
- **Research first, then the maintainer chooses** (decision 20). Three
  questions are researched: Task 32 (the handover), Task 34's flush
  question and Task 42 (YAML). None commits a failing test: a
  reproduction is a probe described in the write-up, and the test lands
  with the fix.
  - Each ends with its options, their pros and cons and a
    recommendation, put to the maintainer.
  - The tasks that build on the answer wait for the maintainer's
    choice, and other work goes on.
  - Task 32 comes after the builder its fix is tested with; Task 33 is
    the fix. Task 42 opens lane 3 and runs early, beside lanes 1 and 2,
    so the choice can be made before lane 3's code starts.
  - A residual YAML difference the chosen fix can't cover still waits
    for the maintainer (decision 3).
- **Test-first, as CLAUDE.md requires.** Each fix starts with a failing
  test that reproduces its finding as tasks/findings.md describes it.
- **Reviews by tier** (CLAUDE.md, ADR-016). One code-reviewer pass per
  task, on the task's whole diff, with the library-reviewer or the
  security-auditor where the task names them. No review for a commit of
  documents only.
- **Specs, platform notes and ADRs change with the code.** A task that
  changes specified behaviour names its spec in Files. A new dependency,
  a loosened rule or a changed ratchet basis has an ADR (the next free
  number in `docs/decisions/`).

## Lanes and shared files

After Checkpoint A the work runs in three lanes, at most two at a time,
each in its own git worktree with its own target directory.

| Lane | Tasks | Edits |
|---|---|---|
| 1: text, process and display | 8-10, 12, 13, 17-21 | `crates/{term-ui,docker}`; cli-core's `exit.rs`, `log.rs`, `cancel.rs`; `crates/sb/src/main.rs` and sb's signal and log tests; exec's attached-signals test; the cli-core, exec, term-ui and docker specs |
| 2: the journal reader | 27, 28, 32-34, 36-38 | `crates/journal`, `crates/host/src/journal.rs` and its tests, the journal spec |
| 3: config | 42, 43, 46 | `crates/config`, cli-core's `config.rs`, new sb test files, the config spec |

A lane is sequential: its tasks share files (cli-core's `log.rs` and
`cancel.rs`, term-ui's `runner.rs` and `tree.rs`, the journal's
`walk.rs` and `forward.rs`), so each waits for the one before it. Lanes
1 and 2 start first, because the viewer needs both. Lane 3's code
starts when one of them frees a worktree. Its first task, Task 42, is
research and runs early, beside them, so the maintainer can choose by
then.

Shared files have one owner at a time:
- **`Cargo.toml`, `Cargo.lock`, `deny.toml`:** the main checkout owns
  them. A lane task that needs a manifest change lands it on main, as
  its own commit, while no other open task touches those files; the
  other worktree rebases after it. That covers Task 43 (a parser), and
  Task 10 if it needs a workspace feature (tracing-subscriber).
- **`scripts/floor-acknowledged.txt`:** any lane may add entries. Lines
  are only added, so a merge takes the union of both sides' lines. No
  line is edited or removed.
- **The CONSTRAINTS ratchet rows** ("Measured, not yet enforced"):
  changed only on the main checkout, at Checkpoints B and C. A lane
  worktree never edits them. An Exceptions row a lane needs goes in
  that lane's own commit.
- **`tasks/todo.md` and `tasks/plan.md`:** ticked on the main checkout
  when a task merges.
- A worktree's commits are rebased onto main, and `check.sh full` runs
  again on the rebased commit before the merge.

Two tasks touch another lane's file, each in one place only:
- **Task 9** (lane 1) calls `escape_controls`, which lives in cli-core's
  `config.rs`, lane 3's file. It uses the function as it is, and
  neither moves nor changes it.
- **Task 46** (lane 3) changes the runtime's teardown in
  `crates/sb/src/main.rs`, lane 1's file, for config-L11. It edits
  that one place; lane 1's tasks leave it alone.

## Traceability

Every KEEP id is in the table below, with the task that holds it.
Where several reviewers found one defect, the ids share a row. Every
other id is in [tasks/triage.md](triage.md): closed before the cut,
deferred or dropped.

| Ids | Finding | Task |
|---|---|---|
| san-H1, docker-M1, ui-M4 | Table cells printed raw (container labels) | 8 |
| san-M1, cli-M3 (screen) | Error text raw in the error block | 9 |
| san-M2, cli-M3 (log), docker-L1 | Run-log injection by newline and Display fields | 10 |
| exec-H1, cli-M1 | SIGHUP kills sb and orphans piped children | 12 |
| cli-M2 | A relayed SIGTERM counts twice and forces the exit | 13 |
| sb-M2, ui-M3 | A panic leaves nothing in the log; a group panic loses its message | 17 |
| docker-M2 | `docker restart` can leave containers down silently | 18 |
| ui-M2 | Typing during a live run leaves stale lines | 19 |
| ui-M1 | A tall live tree hides the running step | 20 |
| ui-L3, ui-L4 | Table wrap artefacts; notices and kept lines cut in the finished tree | 21 |
| tests-H1 | Compact files untested above object decoding | 27, 28 |
| follow-H1, page-H1, fwd-M2 | A page shows an entry its `end` doesn't count; the follow repeats it | 32, 33, 34 |
| page-M1, page-M2 | A page of unreadable entries can't be paged past; `more_newer` stuck | 36 |
| fwd-M1 | A forward page loops on the cursor's own boot | 37 |
| file-M1 | A forward walk loses every array after a bogus item | 38 |
| config-H1, config-L1 | Valid YAML refused; two tab layouts passed that Ansible rejects | 42, 43 |
| config-L2 | A secret under a misplaced structure is echoed | 46 |
| config-L11 | Ctrl-C may lag when DNS is dead | 46 |

config-L11 was in Task 52, which is gone. It is attached to Task 46:
the lag is in `validate-config`'s credential checks, which are Task
46's side of the config crate, and Task 46 ends the lane. Its one
edit to `main.rs` is listed under "Lanes and shared files".

Task 33 also covers the mirror case of the handover (follow-N1,
dropped) if the followed option gives it for free.

## Dependency graph

```
T1 ─ T2 ─ T3 ─ T4 ─ T5 ─ T6 ─ [A]

[A] ─┬─ lane 1: T8 ─ T9 ─ T10 ─ T12 ─ T13 ─ T17 ─ [T]
     │          [T] ─ T18 ─ T19 ─ T20 ─ T21 ─ [D]
     │
     ├─ lane 2: T27 ─ T28 ─ T32 ─ (the maintainer's choice)
     │          T33 ─ T34 ─ T36 ─ T37 ─ T38 ─ [B]
     │
     └─ lane 3: T42 ─ (the maintainer's choice) ─ T43 ─ T46 ─ [Y]

[D] + [B] + [Y] ─ [C]
```

Each lane runs in the order shown. The dependencies that aren't just
"the task before":
- T33 waits for the maintainer's choice on T32's options, and T34 for
  the choice on the flush question (decision 20). Lane 2 waits there.
- T43 waits for the maintainer's choice on T42's options; a residual
  YAML difference waits for the maintainer too. T42 runs early, beside
  lanes 1 and 2.
- Checkpoint D waits until the maintainer has seen T21's looks
  (decision 13); only lane 1 pauses for them.
- T10 after T9: both edit cli-core's log and exit code.
- T17 after T10 (cli-core's `log.rs`; the panic message goes through its
  escaping).
- T19, T20 and T21 after T17 (term-ui's `runner.rs` and `tree.rs`).
- T27 and T28 (the builder and the suites) before T33 to T38 (the
  fixes).

## Task list

The numbers are the original plan's. Tasks 7, 11, 14-16, 22-26, 29-31,
39-41, 44, 45 and 47-52 were removed by decision 18, and Task 35 was
merged into Task 33 by decision 21. Each task's
criteria, and each checkpoint's, are in [tasks/todo.md](todo.md).

### Phase 0: the gates and the records
- [x] Task 1: the floor guard sees every gate file
- [x] Task 2: `check.sh` accepts acknowledged floor findings
- [x] Task 3: the floor guard's false negatives in Rust files
- [x] Task 4: `check.sh` runs the checks that exist only on paper
- [x] Task 5: the gate records are put right
- [x] Task 6: Low sweep: documents
- [x] Checkpoint A: gates

### Phase 1 (lane 1): untrusted text, signals and panics
- [x] Task 8: table cells are sanitized
- [x] Task 9: the error block escapes error text
- [x] Task 10: no logged value can forge a run-log record
- [ ] Task 12: a hangup cancels cleanly
- [ ] Task 13: a relayed signal isn't a second signal
- [ ] Task 17: panics reach the run log and the screen
- [ ] Checkpoint T: text and processes

### Phase 2 (lane 1): docker and the live display
- [ ] Task 18: `docker restart` never ends silently short of a start
- [ ] Task 19: echo is off during a live run
- [ ] Task 20: a live tree taller than the terminal
- [ ] Task 21: table wrap and uncut lines
- [ ] Checkpoint D: docker and display

### Phase 3 (lane 2): the journal reader
- [x] Task 27: the crafted builder writes compact files
- [x] Task 28: the suites the fixes touch run over both layouts
- [x] Task 32: the page-to-follow handover: facts, probe and options
- [ ] Task 33: a page and its follow give every entry once
- [ ] Task 34: the handover across files
- [ ] Task 36: a page can always be paged past
- [ ] Task 37: a forward page on the cursor's own boot
- [ ] Task 38: forward walks over damaged entry arrays
- [ ] Checkpoint B: reader

### Phase 4 (lane 3): config
- [ ] Task 42: YAML: the corpus and the options
- [ ] Task 43: sb accepts the YAML Ansible accepts
- [ ] Task 46: secrets are hidden wherever their key sits
- [ ] Checkpoint Y: config

### Checkpoint C: complete
- [ ] `check.sh full` passes; the matrix is green on three releases and
      the fuzz harness passes
- [ ] Size, startup and coverage are re-measured, with a ratchet ADR
- [ ] The maintainer's sitting: the proposed declines confirmed or
      overruled (an overruled one becomes a fix task first), and the
      unattended calls kept or overruled
- [ ] Every KEEP id in tasks/triage.md is fixed, or declined with the
      maintainer's confirmation (decision 18); then the plan is
      archived to `tasks/hardening/`

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| The page-to-follow handover is a design change, with races that tests must place by hand | High | Task 32 records what the sources establish, probes the rest and puts its options and a recommendation to the maintainer, who chooses before any code (decision 20). Crafted tests place each race (Tasks 33, 34), and the doubling test raises the header and drains again, so it can't pass vacuously. One follow.sh scenario shows the handover on real journald (Task 33). |
| A file is gone between the page and the follow with an entry nobody showed | Med | It takes a flush between the two, once per boot. It remains under every option; Task 34 lists it in the spec as a limit, and Checkpoint B's message names it for the maintainer. |
| A fix changes behaviour the matrix pinned | High | Each journal fix runs the matrix script that covers it on one release (Tasks 33-38); all three run at Checkpoint B. A mismatch that isn't a listed Difference stops for the maintainer. |
| The matrix scripts and the fuzz harness keep the weaknesses the test-evidence review found (dropped, decision 18) | Med | The crafted tests of Tasks 33 to 38 don't depend on the scripts. Task 33's follow.sh scenario shows the handover on real journald. |
| The YAML fix needs a second parser or a patched dependency | High | Task 42 builds the corpus and the options first; the maintainer chooses (decision 20), and a residual difference waits for the maintainer too. Task 43 has an ADR and a size check. |
| Two worktrees edit one file | High | Three sequential lanes with disjoint files; one owner for the shared files and two named cross-lane touches ("Lanes and shared files"); rebase and `check.sh full` before each merge. |
| SIGHUP handling and the relayed-signal window interact | Med | Task 13 follows Task 12 at once. The window is a documented constant with an injected clock, probed under a real session scope and sudo. |
| A display fix changes a reviewed look | Med | Tasks 19 and 20 follow decision 17, with screenshots shown to the maintainer at Checkpoint D; Task 21's looks wait for the maintainer, and only lane 1 pauses (decision 13). |
| A lane waits for the maintainer's choice after research | Med | Tasks 32 and 42 come early in their lanes, and Task 42 runs beside lanes 1 and 2; the other lanes go on meanwhile (decision 20). |
| A deferred finding is forgotten | Med | tasks/triage.md names what each DEFER id waits for; the viewer spec and release preparation start from that table. |
| Parallel worktrees fill the disk | Med | Two at a time; `cargo clean` above 10 GB each; `df` before heavy runs; a worktree is removed when its lane ends. |
| Fixes drift from the findings | Low | Each task names its ids, and the traceability table lists every KEEP id with its task. |

## Open questions

What is left is marked:
- **[research]** a research question: the task puts its options, their
  pros and cons and a recommendation to the maintainer, who chooses
  (decision 20);
- **[task]** a real stop at its task, for the maintainer;
- **[deferred]** put off at approval; no task waits for it.

**Publication [deferred]**
- tasks/findings.md lists security defects that aren't fixed yet.
  Nothing is pushed without the maintainer's explicit authorization, so
  no task waits for this. When a push is considered: (a) not until
  Checkpoint C, recommended; (b) once the High items are fixed; (c) any
  time.

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

**Task 32: the shape of the handover [research]**
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
      though not from its outcome. The maintainer decides that with
      the choice (decision 20).
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
- With it, one point of decision 12, which the maintainer decides
  with the choice (decision 20): is a page bounded by its header's
  count within "a page matches journalctl"?
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

**Task 32's design note** (answered: option (a), decision 23):

- **Design note (Task 32, 2026-11-12):** how each option hands over, per
  page kind. The facts it rests on are in
  docs/platform-notes/systemd-journal.md, "The order journald links an
  entry in". On 22.04, 24.04 and 26.04 a run under writes found:
  - no entry in a value's list that a header read after the lookup
    didn't count;
  - no half-linked entry before the newest counted one;
  - the newest counted entry not yet in its value's list in 0.01% to
    0.27% of rounds (the mirror case is real under load, not only
    reasoned).

  Terms used below: n0 is a file's count at the page's first header
  read. n1 is the count at a header read after every lookup. "Extras"
  are the main-list entries from index n0-1 to n1-1, matched by their
  items.
  Unfiltered pages read no value list and change under no option.
  - (a) **One re-read in the page.**
    - *Tail:*
      - The extras have the file's highest offsets, so newest first they
        come before every listed entry. Merged by offset, the file's
        stream is its listed entries plus the matching extras, each once.
      - Nothing listed is at or past n1 (the run's first fact), so
        `PageEnd`'s n1 covers everything shown.
      - The follow reads from n1, as it reads from n0 today.
    - *Before:* the same stream, with the cursor's step applied to each
      candidate as now. The follow from a Before page starts at the
      journal's end, as today (decision 10).
    - *Merged page:*
      - Each file gets its own extras and its own n1. The merge across
        files doesn't change.
      - `PageEnd`'s newest entry stays from the first headers (`lasts`).
        So a file created during the page is walked back from an entry
        no newer than what every file's first header counted.
    - *Forward page:* the seek lands by bisecting the value lists, which
      the extras may be missing from. There are two cases.
      1. The landing L lies below main[n0-1]. Every extra is after L in
         file order, so the forward walk takes it by offset, as it takes
         a listed entry. This is clean.
      2. L is itself at or past main[n0-1] (an entry listed during the
         page). Then up to the few extras between the last listed entry
         below L and L may be missing from the list. A bisect over the
         complete list would have tested them as the last step between
         two adjacent items, one tested left and one right. So such an
         extra E is taken exactly when the seek's own test (seqnum,
         monotonic time in the cursor's boot, or realtime, as
         `test_object_*` compares) puts E at or after the cursor. That
         is the cursor's order, not the landing's position.
      - On a file sought per boot (`boot_starts`), the walk already
        takes only entries after the cursor in the release's order. An
        extra is taken by that same order, which is clean.
      - The one place this isn't exact: a bisect by realtime over extras
        whose clock stepped back among the few entries written during
        the page. There the full-list bisect's landing isn't a simple
        predicate. The entries involved are the ones journalctl itself
        lands on differently run to run. Recommendation: accept it, and
        state it in forward.rs's departures.
      - So the forward page takes the re-read cleanly. It is one rule,
        "an extra is taken by the seek's test against the cursor", and
        it holds on both landings.
    - *Sealed files:* no extras. An archived file's count is final, and
      re-reading from n0-1 would show a cut-off entry again
      (Difference 12).
    - *A value first written during the page:* it has no DATA object at
      lookup. The extras are matched by looking the value up again after
      n1, as the follow does.
  - (b) **An index and the entries shown past it.**
    - *Tail and Before:* `PageEnd` holds n0 per file and the offsets the
      page showed at or past main[n0-1]'s offset. The follow reads from
      n0 and drops those offsets until its position passes the last one.
      That needs a filter that outlives the first batch, since a batch
      can stop at the output limit.
    - *The mirror case:* the page reads main[n0-1] by its items, for a
      filtered query on a file that isn't sealed. If it matches and no
      list held it, the follow starts at n0-1 for that file. The run
      shows this branch is taken under ordinary load, so it is a tested
      path, not a corner.
    - *Merged page:* per file, as above. `PageEnd`'s newest entry is not
      raised to the newest record shown.
    - *Forward page:* the same set, filled where the walk reaches the
      end. Its seek is unchanged.
    - *`PageEnd`:* grows with the entries shown past n0, and its
      equality compares them.
  - (c) **A high-water mark only.**
    - *All kinds:* per file, the position past the newest entry shown.
      It loses the newest counted entry when that was half linked (the
      mirror case, which the run sees under load). It also loses an
      entry of one list read before another list gained a later one.
    - Either way "0 lost" breaks, for Tail and forward pages alike. Not
      recommended.
  - **Decision 12's bounded forward page under (a):**
    - An unfiltered forward page stops at n0, a filtered one at n1. The
      follow delivers what came later.
    - Going on to the live count would mean reading the header again at
      the end of the walk and building `PageEnd` after it. That changes
      forward.rs and the page's order of reads, for no entry the follow
      doesn't deliver.
    - Recommendation: read it as within decision 12.


**Task 34: a flush between the page and the follow [research]**
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
- A limit remains under every option, named in Checkpoint B's message
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

**Tasks 42 and 43: YAML [research]**
- Which fix, after the research: a fix upstream, a pure-Rust
  libyaml-compatible parser for acceptance, or a pre-pass.
- Listing a difference isn't an option beside them (decision 3). It is
  only for a residual case the chosen fix can't cover, and each such
  case is the maintainer's to accept: that part stays a stop.
