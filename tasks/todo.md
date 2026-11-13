# sb tasks

In progress: hardening after the re-review, cut on 2026-11-12 (the
plan's decision 18). Plan: [tasks/plan.md](plan.md) (decisions, lanes,
the traceability table, risks, open questions). Findings, by id:
[tasks/findings.md](findings.md). The cut, id by id:
[tasks/triage.md](triage.md). Finished plans:
- [tasks/journal/](journal/plan.md) (the sb-journal crate);
- [tasks/docker-control/](docker-control/plan.md) (docker start, stop
  and restart);
- [tasks/exec/](exec/plan.md);
- [tasks/config/](config/plan.md);
- [tasks/docker/](docker/plan.md) (`ps`);
- [tasks/term-ui/](term-ui/plan.md);
- [tasks/cli-core/](cli-core/plan.md);
- [tasks/prototypes/](prototypes/plan.md).

[docs/specs/host.md](../docs/specs/host.md) (approved 2026-11-08) has
no plan of its own yet: sb-host holds the supported-release check (the
cli-core plan's Task 4) and the journal reader's wrapper (the sb-journal
plan's Task 10), and the rest of the spec waits for its plan.

Every task follows CLAUDE.md:
- the skills;
- TDD: each finding's probe becomes a failing test before its fix;
- `scripts/check.sh fast` after edits, `task` before handing back,
  `full` before committing;
- the review its tier asks for ("Fresh-context review, by tier";
  ADR-016): none for a commit of `docs/` and `tasks/` only; otherwise
  one code-reviewer pass for the task, on its whole diff, on one
  model, plus the library-reviewer and the security-auditor where the
  task's Verification names them;
- a fix made after the review is reviewed again only when it changes
  behaviour beyond what the finding spelled out;
- a `Library-check:` line on a commit that changes Rust, and a
  `Review:` line on a commit that changes a file outside `docs/` and
  `tasks/`.

Each open task's verification is its tests, `scripts/check.sh full
<base>` (the base is the commit the task started from) and that
review.

The task numbers are the original plan's and have gaps: Tasks 7, 11,
14-16, 22-26, 29-31, 39-41, 44, 45 and 47-52 were removed by decision
18, and Task 35 was merged into Task 33 by decision 21. Tasks 1-6 and
Checkpoint A are kept as they were written. Where they point at a
later task or at Checkpoint C for docs-L13, docs-L15 or docs-L16, the
cut closed those ids (tasks/triage.md).

Standing rules:
- sb runs as root only with `SB_LOG_DIR` in `target/` or a scratch
  directory outside the repository. Every brief that may run sb says so.
- Heavy builds, the matrix, the fuzz harness and every probe run capped:
  `systemd-run --scope -p MemoryMax=... -p CPUQuota=... -p
  OOMPolicy=continue`.
- Waits poll a PID or a done-file, never `pgrep -f`.
- Container scripts run with a unique `SBJ_PREFIX`. A matrix mismatch
  that isn't a listed Difference stops for the maintainer.
- Probe logs and screenshots stay outside the repository. A platform
  note holds versions and the method, never host details. Screenshots
  and captures for the maintainer come from fixture-driven runs (a
  real `sb docker` run would carry container names). They are kept
  outside the repository and shown to the maintainer directly
  (decision 22).
- Nothing is pushed.
- A research task (Tasks 32 and 42) ends by
  putting its options, their pros and cons and a recommendation to the
  maintainer, who chooses (decision 20). The tasks that build on the
  answer (33 and 43) wait for that choice, and other work goes on.
  A question the recorded decisions already settle is decided, not
  asked, and named among the plan's unattended calls (Task 34's
  flush question was).
- If research at a task contradicts the premise of a decision 17
  answer, that task goes back to the maintainer (decision 17).
- A checkpoint is its ticked criteria here plus a short message to the
  maintainer; no report file is committed (decision 22). Checkpoints
  T, D, B, Y and C don't stop the work, except for the looks at
  Checkpoint D (decision 13); the maintainer reviews the commits
  afterwards.
- Every checkpoint also runs CONSTRAINTS.md's two by-hand journal rows
  (the matrix and the fuzz harness), and a failure blocks the
  checkpoint (Task 5; the plan, decision 13). The plan id check that
  ran with them was retired on 2026-11-12.
- At a checkpoint the `docs/` and `tasks/` commits since the last one
  are read (ADR-016).
- A KEEP id that turns out to need no fix isn't closed unattended. It
  goes on "Proposed declines for the maintainer" below (decision 15),
  and is closed with the maintainer's confirmation (decision 18).

## Task 1: the floor guard sees every gate file

**Description:** `scripts/floor-guard.mjs` checks only `.rs` files and
CONSTRAINTS.md (docs-H1). Extend it to the other files that hold the
bar, the guard's own scripts included.

**Acceptance criteria:**
- [x] It flags a weakening in each of:
      - `scripts/check.sh`: a lowered threshold constant, a dropped
        `-D warnings`, a new `MUTATION_TESTERS` entry, a removed step;
      - `deny.toml`: a new advisory ignore, a widened `wrappers` list, an
        added licence;
      - the root and crate `Cargo.toml` lint tables;
      - `rust-toolchain.toml`;
      - a new tool config: `.cargo/mutants.toml`, `clippy.toml`,
        `.gitleaksignore`, `.cargo/config.toml`;
      - `crates/*/tests/*.sh`: a raised bound (`PREADS_*_MAX`), a deleted
        script, an added `expected_failure` line (acknowledged through
        Task 2's file once it exists);
      - `scripts/floor-guard.mjs` and `scripts/check-commit-messages.sh`
        themselves: a removed rule or pattern.
      (Task 2's acknowledgement file isn't in this list: its additions
      are printed, not flagged.)
- [x] The `deny.toml` and `clippy::all` alternatives the finding says
      can never fire now fire, each with a test.
- [x] Each case has a test in `scripts/floor-guard.test.mjs`, with a
      clean case beside it. `check.sh fast` runs the file with
      `node --test`.

**Verification:**
- [x] `node --test scripts/floor-guard.test.mjs`; `scripts/check.sh fast`.
- [x] code-reviewer on the commit.

**Dependencies:** None
**Files:** `scripts/floor-guard.mjs`, `scripts/floor-guard.test.mjs`, `scripts/check.sh`
**Scope:** M

## Task 2: `check.sh` accepts acknowledged floor findings

**Description:** `check.sh full` can't pass when a floor finding is
expected and recorded (a re-targeted assertion, an accepted exception),
so it has been run with the guard skipped (docs-H2). Give it an
explicit, logged acknowledgement, as the plan's architecture decision
defines it.

**Acceptance criteria:**
- [x] `scripts/floor-acknowledged.txt` holds entries of rule, file, the
      flagged line's text and a reason. The guard reads it.
- [x] An entry covers exactly one finding with its rule, file and line
      text; a second identical finding needs a second entry.
- [x] That holds for `gate-line-removed` too: each changed or removed
      line of a guard script needs its own entry, and no entry covers
      a file. A test removes two lines with one entry and fails. (A
      commit that changes many lines alike, as Task 4's `--locked`
      does, carries one entry per line; each names the line, so the
      review sees exactly what left the gate.)
- [x] An entry counts only when the checked diff adds it. An entry
      from an earlier commit acknowledges nothing; a test shows a later
      weakening in the same file still fails.
- [x] `check.sh` prints each acknowledged finding with its reason. It
      fails on any other finding, and on an added entry that matched
      nothing.
- [x] `check.sh` prints each entry the diff adds; adding one is not
      itself a finding. A range run over several commits passes when
      each finding and its entry are both in the range (a test).
- [x] An ADR (the maintainer accepted the change, decision 6) records
      the change and the file's lifetime: an entry is
      committed with the change it acknowledges; entries are only ever
      added, never edited or removed; a merge takes the union of lines.
- [x] CONSTRAINTS.md's Floor guard row reads "zero unacknowledged floor
      violations", with a paragraph that names the file and the ADR.

**Verification:**
- [x] The guard's tests for each rule above; `scripts/check.sh full`
      with every step run.
- [x] code-reviewer on the commit.

**Dependencies:** Task 1 (the guard's finding format; nothing from Task 3)
**Files:** `scripts/floor-guard.mjs`, `scripts/floor-guard.test.mjs`, `scripts/check.sh`, `scripts/floor-acknowledged.txt`, `CONSTRAINTS.md`, `docs/decisions/` (a new ADR)
**Scope:** M

## Task 3: the floor guard's false negatives in Rust files

**Description:** The guard misses several ways to silence a check or
drop an assertion in Rust and in CONSTRAINTS.md (docs-M2). It comes
after the acknowledgement file (Task 2): Task 1's guard flags any
changed or removed code line of the guard's own scripts, so this
task's edits to existing pattern lines are acknowledged in its own
commit, one entry per line.

**Acceptance criteria:**
- [x] It flags:
      - `cfg_attr(.., mutants::skip)`, `cfg_attr(.., coverage(off))` and
        a `cfg_attr` expect split over lines;
      - `#[cfg(any())]` on a test, and a nested `cfg_attr(.., ignore)`;
      - a removed assert_cmd chain step (`.assert()`, `.success()`,
        `.code(..)`, `.stdout(..)`, `.stderr(..)`);
      - a deleted `src/*.rs` file that held tests;
      - a changed continuation line of a Floor bullet, and a looser
        alternative threshold appended to a row.
- [x] Each case has a test in `scripts/floor-guard.test.mjs`, with a
      clean case beside it.

**Verification:**
- [x] `node --test scripts/floor-guard.test.mjs`; `scripts/check.sh fast`.
- [x] code-reviewer on the commit.

**Dependencies:** Task 2
**Files:** `scripts/floor-guard.mjs`, `scripts/floor-guard.test.mjs`, `scripts/floor-acknowledged.txt`
**Scope:** S

## Task 4: `check.sh` runs the checks that exist only on paper

**Description:** The commit-message check is wired to nothing, a new
crate can miss its deny.toml entry and its lints, and tool versions
aren't checked (docs-M1, docs-M4). The script's smaller defects go with
them (docs-L13, docs-L14). Its edits to existing lines of the guard
scripts are flagged by Task 1's rule and acknowledged in the same
commit, one entry per line.

**Acceptance criteria:**
- [x] `check.sh full <base>` runs `scripts/check-commit-messages.sh
      <base> HEAD` when the base isn't HEAD.
- [x] The commit-message script skips only true merge commits (by
      parent count), not any subject starting "Merge ". The
      description's first letter, after any leading non-letters (a
      backtick, a quote, a digit, punctuation), is lowercase; proper
      nouns and identifiers later in the description stay allowed.
      (Decided unattended, reversible: the plan, after decision 17.)
- [x] `fast` fails when a crate under `crates/` has no `deny.toml`
      `bans.deny` entry, or doesn't inherit the workspace lints.
- [x] `check.sh` compares each tool's version with the CONSTRAINTS Tools
      table and exits 2 naming a tool that differs: the cargo-installed
      and pinned tools exactly; the system packages (node, `script`,
      Docker) by major version or minimum, so a host package update
      doesn't fail every run. The table says which rule each tool has.
      No cargo-installed tool is run for its version (cross would sync
      a toolchain):
      the cargo-installed tools' versions come from
      `cargo install --list`, and gitleaks' from `go version -m` on its
      binary, since its `go install` build can't report one. Go joins
      the Tools table as a minimum-version row. (Decided unattended,
      reversible: the plan, after decision 17.)
- [x] `git diff` calls use `--no-ext-diff --no-color` and fixed
      prefixes; cargo calls use `--locked`; an inherited
      `CARGO_TARGET_DIR` can't redirect a stage; a run on an
      architecture other than x86_64 says what it skips; the dead "no
      crates yet" branches go.
- [x] CONSTRAINTS.md gets rows for the commit-message check and the
      `Review:` line.

**Verification:**
- [x] A stubbed run for each new branch (a crate without an entry, a
      wrong tool version, a "Merge " subject); `scripts/check.sh full`.
- [x] code-reviewer on the commit.

**Dependencies:** Task 3
**Files:** `scripts/check.sh`, `scripts/check-commit-messages.sh`, `scripts/floor-guard.test.mjs` (the new gate lines), `scripts/floor-acknowledged.txt`, `CONSTRAINTS.md`
**Scope:** M

## Task 5: the gate records are put right

**Description:** The records of what gates what are wrong in places:
the matrix and the fuzz harness are in no gate, and the journal plan's
Checkpoint A record contradicts itself (docs-M3, docs-M5). The plan
references and this file's housekeeping go with them (docs-L6,
docs-L17).

**Acceptance criteria:**
- [x] CONSTRAINTS.md says the matrix and the fuzz harness run by hand,
      at each checkpoint and after any task that touches reading, until
      CI is authorised.
- [x] The journal plan's Checkpoint A agrees with its todo's record.
- [x] The journal plan's "decisions 1-9" and "API sketch" references
      match the spec.
- [x] `scripts/check-plan-ids.mjs` checks that every id in
      tasks/findings.md is in the plan's traceability tables and in a
      task here, and that no task cites an unknown id. It takes the
      three paths and is run at each checkpoint. (The script was
      retired on 2026-11-12 and is no longer run.)
- [x] This file: done items leave the open lists; a follow-up is added
      for `MUTATION_TESTERS` when sb reaches sb-journal; host.md has a
      plan entry or a note that it has none.

**Verification:**
- [x] `scripts/check.sh fast`; the links resolve; the id check passes.
- [x] code-reviewer on the commit.

**Dependencies:** Task 4
**Files:** `CONSTRAINTS.md`, `tasks/journal/plan.md`, `tasks/journal/todo.md`, `tasks/todo.md`, `scripts/check-plan-ids.mjs`, `scripts/check-plan-ids.test.mjs`, `scripts/check.sh` (runs the tests), `scripts/floor-guard.mjs`, `scripts/floor-guard.test.mjs`, `scripts/floor-acknowledged.txt`
**Scope:** S

## Task 6: Low sweep: documents

**Description:** The stale statements the docs review listed, and the
capability map's missing rows (docs-M6). It runs before the lanes, so
the journal lane starts from a corrected spec.

**Acceptance criteria:**
- [x] docs-M6: the capability map's stack table has the nix row host.md
      requires, and the rustix row names sb-host's `event`.
- [x] Each id below is fixed, or it is on the collected declines list
      (docs-L12's E1 half is confirmed already, decision 16):
      - docs-L1 **fix**: journal.md's Project Structure lists boots.rs.
      - docs-L2 **fix**: the Tech Stack describes rustix as used.
      - docs-L3 **fix**: the scenario count.
      - docs-L4 **fix**: the line count (re-counted at Checkpoint C);
        an amendment note on ADR-013's upkeep names `BootRule`.
      - docs-L7 **fix**: journal.md no longer says `clean`.
      - docs-L8 **fix**: decision 2's C0 wording agrees with the CR
        rule.
      - docs-L9 **fix**: the capability map's term-ui row.
      - docs-L10 **fix**: host.md agrees with deny.toml.
      - docs-L11 **fix**: the deny.toml comment.
      - docs-L12 **fix** for the 0BSD allowance that never applies (a
        tightening); **declined, confirmed** (decision 16) for exception
        E1's ignores: the recorded decision keeps them until E1's expiry
        (this file, "Follow-ups from the journal reader"). It isn't on
        the collected list.
      - docs-L15 **maintainer's**: CLAUDE.md's two stale lines (the
        plan's open questions).
      - docs-L16 **fix at Checkpoint C**: CONSTRAINTS "Last reviewed"
        is set when the maintainer reviews.

**Verification:**
- [x] `scripts/check.sh fast`; `cargo deny --manifest-path
      crates/sb/Cargo.toml check` after the deny.toml edit.
- [x] code-reviewer on the commit.

**Dependencies:** Task 5
**Files:** `docs/specs/capability-map.md`, `docs/specs/journal.md`, `docs/specs/host.md`, `deny.toml`, `docs/decisions/ADR-013-own-journal-reader.md`
**Scope:** S

## Checkpoint A: gates
- [x] `check.sh full` passes on its own change, every step run, with
      any expected finding acknowledged in the open. Met on 2026-11-12
      as the maintainer chose: `check.sh full HEAD` on the clean tree at
      8935668 (every step runs when nothing changed since the base),
      each earlier commit having passed against its parent. Over the
      whole Phase 0 range the floor guard refuses entries for lines
      added and changed again inside the range (ADR-015's earlier-base
      limit).
- [x] Six [start] questions were answered on 2026-11-11 (the plan's
      decisions 6 to 12): Task 2's ADR, table cells in Task 8, `nohup`
      in Task 12, the window in Task 13, the API names, the forward
      loop, and the page against journalctl. Publication and docs-L15
      were deferred; no task waits on either.
- [x] Task 6's proposed decline (the exception E1 half of docs-L12) is
      confirmed (decision 16, 2026-11-11).
- [x] Decisions 13 to 17 (2026-11-11) set how the plan runs unattended
      and answered the questions of Tasks 18, 19, 20, 25, 45, 47, 48,
      50 and 52 ahead of their tasks.

(The line "`scripts/check-plan-ids.mjs` passes" was removed: the script
was retired on 2026-11-12.)

## Task 8: table cells are sanitized

**Description:** `sb docker ps` prints container label values raw: an
image's `LABEL` reaches root's terminal (san-H1, docker-M1, ui-M4;
confirmed with OSC 52, a title change and a screen clear). Sanitize in
`Table`, the one place every cell passes.

**Acceptance criteria:**
- [x] A cell holding OSC, CSI, C1, a tab or a CR renders with none of
      them; `\n` still separates a cell's lines. Colour the table adds
      itself is kept.
- [x] A `docker ps` test with such a Traefik label and such a name
      shows no escape and no forged row. `--json` is unchanged.
- [x] `docs/specs/term-ui.md` "Escape handling" names tables.

**Verification:**
- [x] `cargo test -p sb-term-ui -p sb-docker`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer and the security-auditor.

**Dependencies:** Checkpoint A
**Files:** `crates/term-ui/src/table.rs`, `crates/term-ui/tests/table.rs`, `crates/docker/src/ps.rs` (tests), `docs/specs/term-ui.md`
**Scope:** S

## Task 9: the error block escapes error text

**Description:** cli-core's error block prints error text raw (san-M1,
cli-M3 on screen). cli-core can't use term-ui's `DisplayText`, so it
escapes control characters itself, per line.

**Acceptance criteria:**
- [x] `shown_chain` and `render_block` escape control characters in
      every line; a test with an OSC, a CR and a newline in a cause shows
      none raw, and the block keeps its shape.
- [x] It calls cli-core's `escape_controls` as it is. That function
      lives in `config.rs`, lane 3's file, which this task doesn't edit.
- [x] `docs/specs/cli-core.md`'s error-block text says so.
- [x] Closes the first half of the follow-up "cli-core's `error_block`
      prints error text raw" (Follow-ups from the journal reader).

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      security-auditor.

**Dependencies:** Task 8
**Files:** `crates/cli-core/src/exit.rs`, `crates/sb/tests/errors.rs`, `docs/specs/cli-core.md`
**Scope:** S

## Task 10: no logged value can forge a run-log record

**Description:** The run log takes `\n` and `\r` unescaped, and `%`
Display fields with nothing escaped, so an error can forge a "run
finished" record (san-M2, cli-M3 in the log, docker-L1). The fix is at
one choke point, the log's formatter layer (M-LOG-STRUCTURED), not at
each call.

**Acceptance criteria:**
- [x] The formatter escapes control characters, `\n` and `\r` in every
      field of every record: message, error, Debug and Display fields,
      and records bridged from dependencies. A record is always one
      line.
- [x] A test logs a value holding a newline and a forged "run finished"
      through each field kind and through a bridged record, and reads
      back one record each.
- [x] docker-L1: daemon error text reaches the log escaped, through
      the formatter. It needs no change of its own.
- [x] The stale comment in cli-core's log.rs is corrected in passing.
      Closes the second half of the follow-up "cli-core's `error_block`
      prints error text raw".
- [x] The specs agree: cli-core.md's log format, and term-ui.md's "a
      step's error record is logged as is".

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb-docker -p sb-exec -p sb`;
      `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer and the security-auditor.

**Dependencies:** Task 9. A tracing-subscriber feature, if one is
needed, lands on main first (the plan, "Lanes and shared files").
**Files:** `crates/cli-core/src/log.rs`, `crates/sb/src/main.rs`, `docs/specs/cli-core.md`, `docs/specs/term-ui.md`
**Scope:** M

## Task 12: a hangup cancels cleanly

**Description:** Decision 2. Today a dropped SSH session kills sb and
orphans its piped children (exec-H1, cli-M1; confirmed). SIGHUP is
handled like SIGTERM.

**Acceptance criteria:**
- [x] On SIGHUP the token is cancelled, children are stopped with the
      usual grace, the run log gets its ending, and sb ends by SIGHUP
      (`$?` reads 129).
- [x] A real hangup: a PTY test closes the master, so writes to stderr
      and the termios restore fail with EIO; sb still logs its ending,
      leaves no child, and dies by SIGHUP. A `kill -HUP` test covers
      the plain case.
- [x] While an attached child runs, a SIGHUP is treated as a SIGTERM is
      (exec decision 12).
- [x] Under `nohup` (SIGHUP ignored at startup) sb leaves it ignored
      (decision 8): a hangup then doesn't cancel the run. A test starts
      sb that way.
- [x] The specs agree: cli-core.md's cancellation text, exit codes,
      testing strategy, success criterion 4 and the logrotate
      paragraph; exec.md's attached children.
- [x] `docs/platform-notes/logrotate.md` no longer says sb doesn't
      handle SIGHUP, and still says no `postrotate` signal is wanted.

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb-exec -p sb`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 10
**Files:** `crates/cli-core/src/cancel.rs`, `crates/sb/tests/` (a hangup test file), `crates/exec/tests/attached_signals.rs`, `docs/specs/cli-core.md`, `docs/specs/exec.md`, `docs/platform-notes/logrotate.md`
**Scope:** M

## Task 13: a relayed signal isn't a second signal

**Description:** Under sudo a group-wide signal arrives twice, and the
copy counts as the second, forcing signal, so cleanup is skipped
(cli-M2; live once Task 12 lands).

**Acceptance criteria:**
- [x] The same signal repeated within the window doesn't force the
      exit. The window is one documented constant
      (M-DOCUMENTED-MAGIC): 250 ms, unless the probe shows another
      value is needed. It applies to SIGTERM and SIGHUP only (decision
      9); two SIGINTs still force the exit.
- [x] A unit test drives the window with an injected clock
      (M-MOCKABLE-SYSCALLS): inside it, at its edge and past it.
- [x] A hangup under an interactive bash login shell comes twice too,
      without sudo: bash re-sends SIGHUP to its jobs, then the kernel
      hangs up the foreground group, about 0.13 to 0.24 ms apart (Task 12's
      review; the platform note's "Doubled signals"). A test with
      `bash -i` as the session leader asserts the grace is taken and the
      run log's ending is written.
- [x] A probe under a real session scope and sudo confirms the relayed
      copy's timing. Its method and versions go in the platform note,
      with no host details. If it shows SIGINT doubled under sudo too,
      that goes back to the maintainer before the task ends.
- [x] cli-core.md's "a second signal ends sb at once" and its "Decided
      in review" entry are amended.

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb`; the probe; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 12
**Files:** `crates/cli-core/src/cancel.rs`, `crates/sb/tests/` (signals), `docs/specs/cli-core.md`, `docs/platform-notes/exec-processes.md`
**Scope:** M

## Task 17: panics reach the run log and the screen

**Description:** A panic leaves "run started" with no ending in the
log (sb-M2), and a panic in a concurrent step is overdrawn by the live
tree (ui-M3; confirmed).

**Acceptance criteria:**
- [x] A panic hook, installed once the log is open, records the message
      and location with the run id, then calls the previous hook. The
      message goes through Task 10's escaping; a test panics with a
      newline in the message.
- [x] A group sibling's panic stops the redraw unconditionally, so its
      message stays on screen; a PTY test asserts it.

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb-term-ui -p sb`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 10 (cli-core's log), Task 13
**Files:** `crates/cli-core/src/log.rs`, `crates/sb/src/main.rs`, `crates/term-ui/src/runner.rs`, `crates/sb/tests/run_log.rs` (left as is: sb has no way to panic on demand; the hook is tested in cli-core's own test binary)
**Scope:** M

## Checkpoint T: text and processes
- [x] `check.sh full`; the signal probes pass under sudo. 2026-11-12:
      `check.sh full HEAD` passed with every step at c05c9fd; the probes
      under sudo found every copy within 0.41 ms of the first, and the
      hangup tests took the grace. The matrix passed on 22.04, 24.04
      and 26.04 after compat.sh's rotation wait was raised (5482678)
      and the pread bounds re-based (ADR-019); the fuzz harness passed.

## Task 18: `docker restart` never ends silently short of a start

**Description:** A wait that ends without a final job status (the 295 s
wait against the controller's 300 s stop, one failed poll, a timed-out
request) prints no notice, and `restart` then never starts the
containers (docker-M2). How far it differs from sb-go is decision 17;
the floor below holds in any case.

**Acceptance criteria:**
- [x] The floor: `restart` never ends without either the start
      requested or a line saying the start wasn't requested and why.
      A test covers each non-final ending: sb's timeout, a failed poll,
      a timed-out request.
- [x] The notice "the controller may still be working" is shown on
      every ending without a final job status, and a few failed polls
      in a row are tolerated before it (decision 17), each with a
      test.
- [x] The docker spec records decision 17, its Differences from sb-go
      and the amended Known limit "sb can give up before the controller
      does".
      Closes that half of the follow-up "sb's wait can give up before
      the controller's own job timeout".

**Verification:**
- [x] `cargo test -p sb-docker -p sb`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Checkpoint T
**Files:** `crates/docker/src/control/{commands.rs,client.rs}`, `crates/docker/src/testing/`, `crates/sb/tests/docker_control.rs`, `docs/specs/docker.md`
**Scope:** M

## Task 19: echo is off during a live run

**Description:** Typing or pressing Enter during a live run leaves
stale duplicate lines, because only `ECHOCTL` is cleared (ui-M2;
confirmed in tmux).

**Acceptance criteria:**
- [x] ECHO is off while the runner is live and restored after: on a
      normal end, on an interruption, on the forced exit's restore hook
      and on a failed setup.
- [x] A PTY test types and presses Enter during a run and finds no
      stale line.
- [x] term-ui.md's `ECHOCTL` wording (the terminal restore, the
      `Runner` rustdoc, success criterion 6, decision 5) is amended
      (decision 17).

**Verification:**
- [x] `cargo test -p sb-term-ui -p sb`; `scripts/check.sh full <base>`.
- [x] A before and after capture of a fixture-driven live run while
      typing, shown to the maintainer at Checkpoint D (decision 22).
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 18; Task 17 (`runner.rs`, `tty.rs`)
**Files:** `crates/term-ui/src/{tty.rs,runner.rs}`, `crates/term-ui/tests/signals.rs`, `docs/specs/term-ui.md`
**Scope:** S

## Task 20: a live tree taller than the terminal

**Description:** A live subtree taller than the terminal hides the
running step (ui-M1; confirmed with 30 sub-steps on 80x12, and with
validate-config's failure blocks on 80x24). Takes up the backlog entry
"term-ui: the live area's total height capped for ended-but-live
steps".

**Acceptance criteria:**
- [ ] With more live lines than rows, the running step and its output
      lines stay visible: settled lines collapse to "… N finished"
      (decision 17).
- [ ] An in-memory test has fewer rows than the tree; a second covers
      a terminal resized while live.
- [ ] The finished tree is complete: nothing collapsed is lost from
      the final output or the log.
- [ ] term-ui.md records the decision.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; a tmux probe at 80x12, with
      screenshots of before and after, shown to the maintainer at
      Checkpoint D (decision 22);
      `scripts/check.sh full <base>`.
- [ ] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 19
**Files:** `crates/term-ui/src/{tree.rs,runner.rs}`, `crates/term-ui/tests/live.rs`, `docs/specs/term-ui.md`
**Scope:** M

## Task 21: table wrap and uncut lines

**Description:** The display Lows that change a reviewed look, brought
to the maintainer with snapshots (ui-L3, ui-L4). The looks still wait
for the maintainer (decision 13): lane 1 pauses here until the
maintainer has seen the screenshots, and the other lanes go on.

**Acceptance criteria:**
- [ ] ui-L3: a wrapped table has no blank line at exact multiples of
      the width, a padded binding stays whole, and a terminal below the
      minimum width says so instead of emptying cells.
- [ ] ui-L4: notices and kept output lines in the finished tree are
      shown as the maintainer chose (whole, or cut).
- [ ] The snapshots change only where these criteria say; term-ui.md
      records the looks.

**Verification:**
- [ ] `cargo test -p sb-term-ui -p sb-docker`; a tmux probe at 36
      columns; `scripts/check.sh full <base>`.
- [ ] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 20
**Files:** `crates/term-ui/src/{table.rs,tree.rs,runner.rs}`, `crates/term-ui/tests/table.rs`, `docs/specs/term-ui.md`, `scripts/floor-acknowledged.txt` (the changed snapshots' assertions)
**Scope:** M

## Checkpoint D: docker and display
- [ ] `check.sh full`; the tmux probes at 80x12 and 36 columns.
- [ ] The maintainer has seen Task 21's looks, from screenshots
      (decision 13).
- [ ] The maintainer has been shown Tasks 19 and 20's screenshots
      (decision 22).

## Task 27: the crafted builder writes compact files

**Description:** The crafted builder writes only regular, unkeyed
files, so the compact branches are untested above object decoding
(tests-H1). 24.04 and 26.04 write compact files by default. The builder
comes first, so the fixes of Tasks 33-38 are tested in both layouts.
The cut keeps the builder mode only: no mutant is hunted here.

**Acceptance criteria:**
- [x] The builder has a compact mode, with tests of its own that read
      its files back.
- [x] A keyed mode is added only if a fix in Tasks 33-38 needs it.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Checkpoint A
**Files:** `crates/journal/src/testing.rs`
**Scope:** S

## Task 28: the suites the fixes touch run over both layouts

**Description:** The second half of tests-H1: the suites above the
file layer that the kept fixes touch (Tasks 33-38) run on regular and
compact files.

**Acceptance criteria:**
- [x] Each suite that Tasks 33-38 touch runs over both layouts (one
      parameter, not copied tests). A suite none of them touches stays
      as it is.
- [x] A suite that fails on compact files for a reason no finding
      covers isn't committed failing: it stops for the maintainer and
      becomes a finding with an id and a task.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Task 27
**Files:** `crates/journal/src/{walk.rs,forward.rs,boots.rs,follow.rs}` (tests; only the suites the fixes touch)
**Scope:** M

## Task 32: the page-to-follow handover: facts, probe and options

**Description:** A page and the follow after it disagree about where
the page ended (follow-H1, page-H1, fwd-M2; confirmed three times; the
mirror case is follow-N1). This task is research: it records what is
established, probes what isn't, and ends at its options and a
recommendation, put to the maintainer, who chooses (decision 20). The
fix is Task 33. It commits no test.

What the code does today:
- A page reads each file's header once (n0), then each file's newest
  entry, then builds `PageEnd`, then walks.
- An unfiltered query walks the main entry list up to the header's
  count, for Tail, Before and After. It reads no value list.
- A filtered query walks each value's entry list. The list's count is
  read once per value, at lookup; the items are read lazily below it.
  So a filtered page can hold an entry past the header's count.
- `PageEnd` holds, per file, the inode, the file id and the header's
  count, and across files the newest entry the headers gave, with its
  directory.
- The follow reads a file the page read from that count (same inode
  and file id, no fewer entries). A file the page didn't read starts
  after its newest entry at or below `PageEnd`'s newest entry.
- Why it doubles: an entry appended after the header read and linked
  into a value's list before that lookup is shown by the page, at an
  index at or past the count, and the follow reads it again.
- Why the mirror case loses: an entry counted in the header but not
  yet linked into its value's list is missed by the page, and the
  follow starts past it.

What is established from systemd's sources (journal-file.c unless
named), for the platform note to be written from:
- The main-list item is written, then `n_entries` raised: v249
  1827-1828 and 1857-1872; v255 2100-2101 and 2132-2150; v259 the same
  function at 2092.
- An entry is linked into the main list and counted, then the header's
  tail fields are set, then each value's list is linked: v249
  1944-1966; v255 2232-2266; v259 2253-2286.
- A value's item is written, then that list's count raised: v249
  1892-1906; v255 2175-2189.
- An entry's items are complete before it is linked: v259 2390-2399.
- So, with one writer appending entry by entry, only the last counted
  entry can be half linked for a reader (by reasoning; not probed).
- The exception: a link that fails stays partly linked for good: v249
  returns at the first failed item (1962-1965); v255 and v259 go on
  past `-E2BIG` and return other errors (2257-2265; 2277-2285). That
  is Difference 12.
- Entry offsets rise with the main-list index in files journald
  writes, since objects are appended at the tail: v249 1069; v255
  1242-1259; v259 1250-1267.
- journalctl without `--follow` reads the live count at each step
  (v249 2902; v255 3532; v259 3551) and live value counts (v249 2975;
  v255 3234; v259 3253), so it shows entries written after it opened
  the file.
- With `-n N` journalctl seeks to the tail once and shows at most N
  lines (v255 journalctl.c 2510-2514 and 1958).
- A flush copy keeps the boot id, both times and the xor hash: v249
  3882-3955; v255 4436-4497; v259 4453-4515.
- After a flush the whole /run directory is removed, and only if every
  entry iterated was copied: v255 journald-server.c 1289-1354.
- The compact and regular layouts link in the same order (v255
  2089-2103).

**Acceptance criteria:**
- [x] `docs/platform-notes/systemd-journal.md` records the facts above
      with their citations, the systemd versions and the method, and no
      host details.
- [x] The probe settles what reading didn't, and the note says so for
      each:
      - the flush code of 249 and 259 (journald-server.c): that /run
        is removed only when every entry was copied, as on 255;
      - that the three releases' packaged systemd matches the upstream
        tags for the functions cited (the distribution's patch level);
      - a run under writes, on each release, that tries to refute the
        link order: a reader that never finds an entry in a value's
        list before the header counts it.
- [x] The note states the one assumption no reading can settle: a
      reader's `pread` sees journald's mmap stores in program order.
      It holds on x86-64, the only architecture sb ships for.
- [x] A design note shows, for each option in the plan's handover
      question, how a Tail page, a forward page, a Before page and a
      merged page hand over. For the recommended option it shows
      whether the forward page takes the re-read cleanly: an entry
      before the landing the seek found must be taken by the cursor's
      order.
- [x] The options and a recommendation are written up. The
      recommendation includes decision 12's point on a forward page
      bounded by its header's count.
- [x] The options, their pros and cons and the recommendation are put
      to the maintainer, who chooses (decision 20). The choice is
      recorded here (2026-11-12: option (a), the plan's decisions 23 and
      24), and Task 33 gets its option-specific criteria
      from it. The rest of lane 2 waits for the choice, and the other
      lanes go on.

**Verification:**
- [x] The probe's runs, capped, in the matrix's containers, logs kept
      outside the repository; `scripts/check.sh full <base>`.
- [x] No commit review: the task commits documents only (ADR-016).

**Dependencies:** Task 28
**Files:** `docs/platform-notes/systemd-journal.md`, `tasks/plan.md` (the design note, under the handover question; it lands on the main checkout, as the plan's shared-file rule requires)
**Scope:** S

## Task 33: a page and its follow give every entry once

**Description:** The fix for follow-H1, page-H1 and fwd-M2, in the
shape the maintainer chose from Task 32's options (decision 20).
The criteria below
are behaviour. They hold under any option, except that the first two
fixtures are set up differently for each option, as each one says.
The criteria particular to the chosen option (what `PageEnd` holds,
what the page re-reads) are added to this task from the maintainer's
choice, before any code. Unfiltered queries read no value
list, so neither defect exists for them, and they stay out of the
change.

**Acceptance criteria:**
- [x] The maintainer's choice is recorded in journal.md (decision 20),
      and its "Starting a follow" matches the code. "0 lost, 0 doubled" holds
      as stated, with no new Difference.
- [x] An entry appended and linked during the page. The fixture is
      what the file holds at each of the page's reads, not one static
      file: at the first header read the header counts 2; at the
      lookup a value's list holds 3; after the lookup the header
      counts 3. Under option (a) the test raises the header between
      the page's first and second header read, through a test seam.
      Under (b) it raises it between the page and the follow, as the
      follow's own tests grow a file. The follow then drains again.
      Over the page and every batch, each of the three is delivered
      once. For Tail and for an After page that reaches the tail; in
      both layouts.
- [x] Two lists read at different moments, set up the same way: a
      query of two values. At the first value's lookup an entry isn't
      linked into that list yet; at the second's, a later entry is.
      The header counts both only after the page's first header read:
      raised through the seam under (a), between the page and the
      follow under (b). Each entry is delivered once, though one is
      older than an entry the page showed.
- [x] The mirror case (follow-N1, dropped): a header that counts 3
      while a value's list holds 2. The third entry is delivered once,
      by the page or by the follow, if the followed option gives that
      for free (the re-read shape does). It gets no work of its own; if
      it doesn't come free, this criterion is struck.
- [x] A page stopped by a limit, and a Before page: the follow starts
      at the journal's end as today and repeats nothing the page
      showed.
- [x] A Tail page over an archived file and an active one, unfiltered
      and filtered, then a follow: its first batch is empty. An
      archived file's last entry that journald was cut off from
      linking (Difference 12) doesn't come again.
- [x] A first batch cut by a small `Limits` (`Batch::more`): the later
      drains repeat nothing the page showed and lose nothing.
- [x] A page that read nothing from a file: what is written to that
      file afterwards is delivered.
- [x] An unfiltered page and its follow are unchanged: the existing
      tests pass as they are.
- [x] Option (a), chosen 2026-11-12 (the plan's decisions 23 and 24;
      Task 32's design note under the handover question):
      - for each file that isn't sealed, a filtered page reads the
        header again after its lookups (n1), matches the main-list
        entries from n0-1 to n1-1 by their items, and merges them into
        the file's stream by offset, an offset met twice taken once;
        a sealed file isn't re-read (Difference 12);
      - `PageEnd` holds n1 for such a file; the follow's start, the
        flush and the rotation rules are unchanged;
      - a value first written during the page, with no DATA object at
        lookup, is looked up again for the re-read;
      - a forward page takes a re-read entry by the cursor's order:
        a crafted test places one before the landing the seek found,
        and one where the landing is itself among the re-read entries;
      - a realtime clock stepping back among the entries written during
        a page is listed in forward.rs's departures from journalctl.
- [x] `PageEnd` stays opaque and keeps `Debug`, `Clone`, `PartialEq`,
      `Eq`, `Default` and `Send`. Its rustdoc says what it carries, and
      that it is the journal's end, not the page's: a follow from a
      `Before` page skips entries (decision 10).
- [x] follow.sh has one scenario on real journald (this was Task 35,
      decision 21): a filtered page is taken while a writer runs, a
      follow starts from its end, and the page plus its follow give
      every entry once.

**Verification:**
- [x] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh full <base>`.
- [x] follow.sh, with that scenario, on at least one release, capped.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 32, and the maintainer's choice from its
options (decision 20)
**Files:** `crates/journal/src/{walk.rs,forward.rs,follow.rs,lib.rs}` (which of them depends on the answer), `crates/journal/src/testing.rs` (the test seam), `docs/specs/journal.md`, `crates/journal/tests/follow.sh` (the scenario)
**Scope:** M

## Task 34: the handover across files

**Description:** The other half of the handover (follow-H1, page-H1,
fwd-M2): files that change between the page and the follow. Most of
it holds today and is pinned here. The flush question was settled by
decision 12 (the plan's unattended list).

**Acceptance criteria:**
- [x] The flush question is settled by decision 12 (0 lost, 0
      doubled), so it isn't put to the maintainer again (the plan's
      unattended list): option (b). Extending Difference 13 would allow a
      doubled entry once per boot, against decision 12.
- [x] A rotation between the page and the follow: the archived file
      keeps its position by inode and file id, and every entry of the
      new file is delivered. Nothing lost or doubled.
- [x] An archived file rewritten as a copy under a new inode before
      the follow starts keeps its position, but only if Task 32 shows
      that journald rewrites an archived file that way. It would be a
      code change: today the start needs the inode and the file id to
      match, and only a later rescan matches by file id alone. If Task
      32 doesn't show it, nothing changes here.
- [x] A flush between the page and the follow, where the page read
      the /var file: every flush copy is shown again, as Difference 13
      says of a followed file. A test pins it.
- [x] A flush between the page and the follow, where the /var file is
      new: `PageEnd` carries, for each entry the page showed that is
      newer than its newest entry (under handover (a), the ones the
      re-read added), its boot id, monotonic and realtime stamps and
      xor hash, and the follow drops one flush copy of each. Its
      equality includes them. No entry is lost or doubled, and
      `PageEnd`'s newest entry is not raised past what the headers
      gave.
- [x] A file the page couldn't open: the follow reads it as a new
      file, and its open error is reported once.
- [x] A merged page over several files hands each file over on its
      own.
- [x] journal.md lists the limit that remains under every option:
      the headers are read at different instants, so `PageEnd`'s newest
      entry can be newer than an entry of another file that the page
      didn't read. That entry is lost only if its file is gone before
      the follow opens it, which takes a flush between the two, once
      per boot. Checkpoint B's message names it for the maintainer.
- [x] follow.sh gains a rotation and a flush between the page and the
      follow, next to its two "between" scenarios.

**Verification:**
- [x] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh full <base>`.
- [x] follow.sh on three releases, capped.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 33, and the maintainer's choice on the flush
question (decision 20)
**Files:** `crates/journal/src/{follow.rs,lib.rs}`, `docs/specs/journal.md`, `crates/journal/tests/follow.sh`
**Scope:** M

## Task 35: merged into Task 33

Merged on 2026-11-12 (the plan's decision 21). The stress.sh handover
check and its floors are gone. What is left is one follow.sh scenario
on real journald, in Task 33's criteria and verification.

## Task 39: a follow doesn't double a burst across a flush

**Description:** follow-M1, found by Task 34's real-journald run and
older than it: a follow running through a flush shows the last burst
before the flush twice, because journald gives every entry of one pass
the same timestamps and the follow matches a copy only against the very
last entry it showed. The maintainer chose a fix (the plan's decision
26). sb keeps its reader (decision 27).

**Acceptance criteria:**
- [x] The follow keeps the identities (boot id, monotonic and realtime
      stamps, xor hash) of the entries it showed at its last entry's
      stamp, at most one burst, reset when the stamp moves on; an entry
      at that stamp is a copy if its identity is among them.
- [x] A crafted test (both layouts): /run holds four entries with one
      stamp, all shown; the flush writes their copies and a new entry
      to /var; the follow shows only the new one.
- [x] follow.sh's `during` gets its flush round back (saved from Task
      34) and passes on 22.04, 24.04 and 26.04: nothing lost or doubled.
- [x] The flush round also runs with `/run/log` on tmpfs, as on a host,
      where journald removes the /run file after the flush (the matrix
      containers' overlayfs keeps it): a follow started before
      `/var/log/journal` existed loses nothing (the libsystemd probe saw
      readers lose entries there; the platform note).
- [x] Difference 2 holds as written.

**Verification:**
- [x] `cargo test -p sb-journal -p sb-host`; follow.sh on three releases;
      `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer.

**Dependencies:** Task 34
**Files:** `crates/journal/src/follow.rs`, `crates/journal/tests/follow.sh`
**Scope:** S

## Task 36: a page can always be paged past

**Description:** A page whose chosen entries all fail to decode has no
cursor to continue from, and `more_newer` stays set at a tail whose
newest entry can't be shown, so a viewer never reaches the follow
(page-M1, page-M2; confirmed).

**Acceptance criteria:**
- [x] `Page` exposes the cursors of the oldest and newest entries it
      chose, shown or not: `Page::oldest_chosen` and
      `Page::newest_chosen`, each an `Option<Cursor>` (decision 10).
      `Before` and `After` from them step past, in both layouts.
- [x] `After(newest_chosen)` at a tail whose newest entry can't be
      shown returns `more_newer == false`.
- [x] The tests at walk.rs:2111-2125 pin the old behaviour. They are
      re-targeted to the new one, and the change is acknowledged
      through Task 2's file.
- [x] journal.md's API section has the new fields; sb-host re-exports
      them.

**Verification:**
- [x] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh full <base>`.
- [x] compare.sh on at least one release, capped.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Task 34
**Files:** `crates/journal/src/{lib.rs,walk.rs}`, `crates/host/src/journal.rs`, `docs/specs/journal.md`, `scripts/floor-acknowledged.txt`
**Scope:** M

## Task 37: a forward page on the cursor's own boot

**Description:** A single-boot file of the cursor's own boot falls back
to a realtime seek that can land before the cursor, so `After` returns
entries before the cursor and the cursor's own entry, and paging one at
a time loops for ever (fwd-M1; confirmed on all three releases).

**Acceptance criteria:**
- [x] The task first records what journalctl shows for the reviewer's
      two-file fixture on 22.04, 24.04 and 26.04.
- [x] `boot_starts` changes (decision 11): a file that holds the
      cursor's boot seeks per boot. On 22.04, 24.04 and 26.04,
      `After(e)` on that fixture returns only entries after the cursor,
      and paging one at a time ends. In both layouts.
- [x] 22.04 gets a listed Difference only if journalctl 249 shows
      something else on the fixture. A difference on 24.04 or 26.04
      stops for the maintainer.
- [x] The property probe's sixth mode is a test and passes, with the
      five that passed before.
- [x] The docs are corrected: forward.rs:28-32 and 81-86; journal.md's
      "sb doesn't emulate that step" passage and its "259's seek is
      kept ... one of a single boot, the cursor's" sentence.

**Verification:**
- [x] `cargo test -p sb-journal`; `scripts/check.sh full <base>`.
- [x] compare.sh on 22.04 and on one of 24.04 and 26.04, capped.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Task 36
**Files:** `crates/journal/src/{forward.rs,file.rs}`, `crates/journal/tests/checks.sh` (the fixture's row), `docs/specs/journal.md`
**Scope:** M

## Task 38: forward walks over damaged entry arrays

**Description:** A forward walk loses every array after one bogus item,
where the backward walk goes on (file-M1; confirmed).

**Acceptance criteria:**
- [ ] A forward walk goes on after an out-of-order array, as the
      backward walk does; `After` and the tail agree. Each direction
      has a test, in both layouts.
- [ ] Deleting the backward walk's reset now fails a test (shown once
      by breaking it).

**Verification:**
- [ ] `cargo test -p sb-journal`; `scripts/check.sh full <base>`.
- [ ] compare.sh on at least one release, capped.
- [ ] Review: one code-reviewer pass on the task's diff, with the
      security-auditor.

**Dependencies:** Task 37
**Files:** `crates/journal/src/file.rs`, `crates/journal/src/testing.rs`
**Scope:** M

## Checkpoint B: reader
- [ ] The matrix (compare, compat, follow, stress) is green on 22.04,
      24.04 and 26.04, and the fuzz harness passes, on the final tree.
- [ ] follow.sh's handover scenario passed on each release (Task 33):
      a page and its follow gave every entry once.
- [ ] Size, startup and coverage are re-measured as `check.sh` runs,
      and the ratchet rows updated on the main checkout, with a ratchet
      ADR if a value passes its tolerance.
- [ ] The new API is reviewed: `Page::oldest_chosen` and
      `Page::newest_chosen` in place (decision 10), and what the
      handover changed in rustdoc or types (Tasks 33 and 34).
- [ ] The message to the maintainer names the limit Task 34 lists.

## Task 42: YAML: the corpus and the options

**Description:** Decision 3. saphyr-parser refuses multi-line flow
layouts and quoted scalars that libyaml, PyYAML and yaml.v3 accept, and
accepts two tab layouts Ansible rejects (config-H1, config-L1;
confirmed with about 200 snippets). This task builds the standing
differential corpus and the options, and ends at a recommendation,
put to the maintainer, who chooses (decision 20). Task 43 is next
and is coded to the choice. So this task runs early, while lanes 1 and
2 hold the two worktrees, and the choice can be made before lane 3's
code starts.

**Acceptance criteria:**
- [x] A checked-in corpus of layouts, each with what libyaml accepts
      and, for an accepted case, the resolved value of each scalar, so
      a parser change can't alter scalar resolution unnoticed:
      the review's table, the tab-only line and the tab-indented top
      level.
- [x] The expected results are generated once from libyaml through
      PyYAML, by a checked-in generator. Its versions and method go in
      a platform note. The generator passes ruff, `ruff format
      --check`, mypy and Bandit.
- [x] A test runs sb's parser over the corpus and asserts the exact
      list of mismatches, checked in beside it. Today's list is the
      finding.
- [x] The options are written up with pros and cons, each run against
      the corpus where it can be: a fix upstream, a pure-Rust
      libyaml-compatible parser used for acceptance, or a pre-pass.
      Listing a difference isn't an option beside them (decision 3):
      it is only for a residual case the chosen fix can't cover.
      Maintenance, adoption, licence and size
      are stated for any new crate.
- [x] The options, their pros and cons and the recommendation are put
      to the maintainer, who chooses (decision 20). The choice is
      recorded here (2026-11-12: a pre-pass in sb, the plan's decision
      28), and Task 43 follows it. Lane 3 waits for the
      choice, and the other lanes go on.

**Verification:**
- [x] `cargo test -p sb-config`; the Python checks on the generator;
      `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Checkpoint A
**Files:** `crates/config/tests/` (the corpus, its mismatch list and its test), `scripts/` (the generator), `docs/platform-notes/` (a YAML acceptance note)
**Scope:** M

## Task 43: sb accepts the YAML Ansible accepts

**Description:** Implement the option the maintainer chose from Task
42's options (decision 20; config-H1, config-L1): a pre-pass in sb
before saphyr-parser (the plan's decision 28).

**Acceptance criteria:**
- [ ] The corpus's mismatch list is empty, or holds only entries the
      maintainer accepted, each in the config spec's Differences list.
      A residual case the fix can't cover waits for the maintainer
      (decision 3); the rest of the task goes on.
- [ ] validate-config and cli-core's inventory read agree on every
      corpus case; `appdata_path` no longer falls back to `/opt` on a
      file Ansible reads.
- [ ] An ADR records the choice (decision 20). If
      a dependency is added or changed:
      `Cargo.toml`, `deny.toml`, the config spec's Tech Stack and the
      capability map's stack row are updated, and the production
      binary's size is measured against its ratchet (ADR-004 and
      ADR-009 are the precedents).
- [ ] Positions, duplicate-key reports and secret hiding are unchanged
      on the existing tests.
- [ ] The pre-pass pads continuation lines inside flow collections and
      quoted scalars and turns a tab after a colon into a space; it
      tracks quotes, comments, flow depth and block scalars, and leaves
      block scalars' contents alone. A code comment says why not a
      libyaml port (the maintainer's decision 28; Task 42's measured
      options in the platform note).
- [ ] The tab layouts libyaml refuses (a tab-only line, a tab before a
      top-level key, `-\tvalue`) are refused with a message naming the
      line and column.
- [ ] An error's line and column point into the original file, not the
      padded text.
- [ ] The corpus's resolved values are unchanged for every case that
      was accepted before.

**Verification:**
- [ ] `cargo test -p sb-config -p sb-cli-core -p sb`; the corpus;
      `cargo deny --manifest-path crates/sb/Cargo.toml check`;
      `scripts/check.sh full <base>` (the size).
- [ ] Review: one code-reviewer pass on the task's diff, with the
      library-reviewer and the security-auditor.

**Dependencies:** Task 42, and the maintainer's choice from its
options (decision 20). Lands alone on
`Cargo.toml`, `Cargo.lock` and `deny.toml` (the plan, "Lanes and shared
files").
**Files:** `crates/config/src/parse.rs`, `crates/cli-core/src/config.rs`, `Cargo.toml` and `deny.toml` (if a dependency changes), `docs/specs/config.md`, `docs/specs/capability-map.md`, `docs/decisions/` (a new ADR)
**Scope:** M

## Task 46: secrets are hidden wherever their key sits

**Description:** A value under a secret key name is echoed when the key
sits in a misplaced structure (config-L2). config-L11 is attached here
from the removed Task 52 (the plan, "Traceability"): after Ctrl-C,
`validate-config` may lag when DNS is dead, because the runtime's drop
in sb's `main.rs` waits for the resolver's blocking thread. That
finding is untested.

**Acceptance criteria:**
- [ ] A value under a secret key name is hidden wherever the key sits:
      the review's `user:` list case has a test.
- [ ] config-L11, as its own commit: Ctrl-C ends `validate-config`
      promptly when DNS is dead; the runtime's drop doesn't wait for
      the resolver's blocking thread. Its test reproduces the lag
      first. If the lag can't be reproduced, the id goes on "Proposed
      declines for the maintainer" instead.

**Verification:**
- [ ] `cargo test -p sb-config -p sb`; `scripts/check.sh full <base>`.
- [ ] Review: one code-reviewer pass on the task's diff, with the
      security-auditor, and the library-reviewer for the `main.rs`
      change.

**Dependencies:** Task 43
**Files:** `crates/config/src/{rules.rs,secret.rs}`, `crates/sb/src/main.rs` (the runtime's teardown only; lane 1's file), `crates/sb/tests/` (a Ctrl-C test)
**Scope:** M

## Checkpoint Y: config
- [ ] `check.sh full`; the differential corpus passes.
- [ ] The message to the maintainer names any residual YAML difference
      that waits for them.

## Task 53: the production build links glibc, built on 22.04

**Description:** Replace the static musl build with a glibc build made
in an `ubuntu:22.04` image (plan decision 29,
[ADR-020](../docs/decisions/ADR-020-glibc-builds-on-the-oldest-release.md)).
Under "Library first", first check whether `cross` 0.2.5 runs the build
in a custom 22.04 image (`Cross.toml`'s `image` or `dockerfile`, with
`pre-build` for the C compiler). If it doesn't, use a `docker run` of
the repository's own image. Record which, and why, in ADR-020.

**Acceptance criteria:**
- [x] `check.sh full` builds the production `x86_64-unknown-linux-gnu`
      binary in the 22.04 image, in its own target directory, gates its
      size, and runs `sb version` in a container of each release on the
      host spec's runtime list (22.04, 24.04, 26.04); a binary one of
      them can't load fails the gate.
- [x] The arm64 build (`aarch64-unknown-linux-gnu`) is made once in the
      same image with Ubuntu's aarch64 GCC, and runs `sb version` under
      qemu or on an arm64 host. ADR-020's UNVERIFIED mark is removed if
      it works, and the result goes to the maintainer if it doesn't.
- [x] No musl target is left: `rust-toolchain.toml`'s targets, `deny.toml`'s
      targets (on main, alone), CONSTRAINTS.md's binary-size and Tools
      rows (`cross` stays only if it is the route), check.sh's step name and
      `SIZE_MAX_BYTES` comment, the floor guard's tests, the cli-core
      spec's build command, `crates/host/src/lib.rs`'s "static" comment.
- [x] The toolchain change is acknowledged by the floor guard
      (ADR-015) where it flags it, citing ADR-020.
- [x] ADR-020 records the route chosen, and the size and startup
      measured on this build.

**Verification:**
- [x] `scripts/check.sh full <base>`; the glibc binary's highest
      `GLIBC_` symbol version is at most 2.35 (`objdump -T`), recorded
      in ADR-020.
- [x] Review: one code-reviewer pass on the task's diff (it changes the
      gate), with the library-reviewer for the build route.

**Dependencies:** none; lane 4, beside the others (plan decision 31).
It lands alone on `deny.toml` and `rust-toolchain.toml` (the plan, "Lanes
and shared files"), and its merge re-runs every open lane's full check
(decision 30, a gate file).
**Files:** `rust-toolchain.toml`, `deny.toml`, `CONSTRAINTS.md`,
`scripts/check.sh`, `scripts/floor-guard.test.mjs`, a build image (or
`Cross.toml`), `docs/specs/cli-core.md`, `docs/decisions/ADR-020-*.md`,
`docs/decisions/ADR-008-*.md`, `crates/host/src/lib.rs`
**Scope:** M (many files, each a line or two; the image is the work)

## Task 54: the journal matrix builds in the same image

**Description:** The journal scripts and their CI workflow build the
`journal` example in Task 53's image, not for musl, so the binary under
test is built the way sb ships, and runs in the 22.04 container from any
build host (ADR-020, decision 5).

**Acceptance criteria:**
- [x] compare.sh, compat.sh, follow.sh, seeds.sh and stress.sh build the
      example through one shared helper in the image (or take
      `SBJ_BIN`, as now), and their "Needs ... the musl target" lines say
      what they need instead.
- [x] `.github/workflows/journal-matrix.yml` builds the same way; its
      step names and artifact path change with it. Local only, no push
      or workflow run (global rules).
- [x] The matrix is green on 22.04, 24.04 and 26.04 with the glibc
      example, with the same pread bounds.

**Verification:**
- [x] compare.sh, compat.sh and follow.sh on the three releases;
      `scripts/check.sh full <base>`.
- [x] Review: one code-reviewer pass on the task's diff.

**Dependencies:** Task 53.
**Files:** `crates/journal/tests/{compare,compat,follow,seeds,stress}.sh`,
`.github/workflows/journal-matrix.yml`, the journal spec's Commands if it
names the musl build
**Scope:** S

## Task 56: the mutation pass times out per test

**Description:** cargo-mutants runs each mutant's tests under
cargo-nextest with a per-test timeout, so a test that hangs on a mutant
is ended and counted as a failure, and the mutant as caught (plan
decision 34). Task 38 found the case: replacing `EntriesFwd::next`'s
body with an endless `Some(Ok(0))` fails its bounded unit test at once,
but about 30 other sb-journal tests loop until the 300 s mutant timeout,
which the gate counts as not caught (ADR-012). Also takes the follow-up
"check.sh's production `cross build` runs uncapped".

**Acceptance criteria:**
- [ ] check.sh's mutation passes run cargo-mutants with nextest as its
      test tool (https://mutants.rs/nextest.html), with a nextest
      profile whose `slow-timeout` ends a test after a set time
      (https://nexte.st/docs/features/slow-tests/). The time is set from
      the slowest test's measured duration with a stated margin, so a
      test that passes on the unmutated tree is never ended. The
      whole-mutant timeout stays as the backstop.
- [ ] Nothing the old runner caught is lost: doc tests and anything
      nextest doesn't run are named, and either still run or shown not
      to catch mutants today. Tests that share process state (for
      example a `static` lock across tests) are checked under nextest's
      process-per-test model.
- [ ] Task 38's two mutants (its diff is uncommitted in lane 2's
      worktree) are caught under the new runner. Shown by applying that
      diff in a scratch copy, not by committing it.
- [ ] cargo-nextest joins CONSTRAINTS.md's Tools table at an exact pin,
      installed on this host. An ADR (the next free number) records why
      this doesn't weaken the bar, the alternatives (redundant caps in
      code, an exception, counting timeouts as caught) and the timeout
      chosen. ADR-012's timeout paragraph points to it.
- [ ] check.sh's production `cross build` is capped through
      `CROSS_CONTAINER_OPTS` (`--memory`, `--cpus`), as
      `crates/journal/tests/example.sh` does; the follow-up is ticked.
- [ ] The floor guard's acknowledgements, where it flags the change
      (ADR-015).

**Verification:**
- [ ] `scripts/check.sh full <base>`, its mutation step run on a crate
      change (a scratch edit, reverted) to show the new runner works;
      the floor guard's own tests.
- [ ] Review: one code-reviewer pass on the task's diff.

**Dependencies:** none (lane 4). Its merge re-runs every open lane's full
check (decision 30, a gate file).
**Files:** `scripts/check.sh`, `scripts/floor-guard.test.mjs`,
`scripts/floor-acknowledged.txt`, `.config/nextest.toml`,
`CONSTRAINTS.md`, `docs/decisions/` (a new ADR, and ADR-012's pointer),
`tasks/todo.md` (the follow-up's tick)
**Scope:** M

## Task 55: the toolchain pin moves to 1.100

**Description:** Move `rust-toolchain.toml` and `rust-version` from 1.99.1
to the newest 1.100.x (plan decision 32), after every lane's current work
has merged. A trial on 67fcdd2 measured what breaks (the review's logs outside
the repo, not tracked).

**Acceptance criteria:**
- [ ] The escaping change decided first, test-first: std's
      `str::escape_debug` now escapes a grapheme extender anywhere
      (https://github.com/rust-lang/rust/pull/158303), so
      `exit::tests::combining_marks_mid_line_show_unchanged`
      (crates/cli-core/src/exit.rs) fails and an NFD name prints with
      `\u{301}`. The options (an escaper of sb's own that keeps marks
      after a base character, as the cli-core contract at exit.rs's
      `escaped` documents; or the new output, with the spec and test
      changed) go to the maintainer with a recommendation (decision 20).
      The journal's path messages (crates/journal/src/lib.rs) follow the
      same answer.
- [ ] clippy `-D warnings` clean: the `map_unwrap_or` findings (now also
      `map(..).unwrap_or_default()`), `needless_range_loop` in
      walk.rs's tests and `unused_must_use` in the journal example.
- [ ] Cargo's new `unused_dependencies` warning on sb-journal's optional
      `clap` (used only by its examples under `testing`) is resolved by
      structure where one fits, or a `[lints.cargo]` entry with its reason.
- [ ] ADR-001's pins (ratatui =0.30.2, clap_complete =4.6.11) and the
      production build still pass; the doc and comment references to
      1.99.1 are updated.

**Verification:**
- [ ] `scripts/check.sh full <base>`, including the production build and
      cargo-llvm-cov and cargo-mutants on the new toolchain.
- [ ] Review: one code-reviewer pass on the task's diff; the
      library-reviewer if the escaper becomes sb's own.

**Dependencies:** every open lane task merged (it touches six crates and
the gate). Lands alone on `rust-toolchain.toml` and `Cargo.toml`.
**Files:** `rust-toolchain.toml`, `Cargo.toml`, `crates/cli-core/src/{exit.rs,config.rs,help.rs}`, `crates/journal/`, `crates/{config,docker,sb,term-ui}/src/` (the lint sites), the cli-core spec
**Scope:** M

## Checkpoint C: complete
- [ ] `check.sh full` passes; the matrix is green on three releases and
      the fuzz harness passes.
- [ ] Size, startup and coverage are re-measured, the ratchet rows
      updated, with a ratchet ADR.
- [ ] The maintainer's sitting: the proposed declines confirmed or
      overruled (an overruled one becomes a fix task here, before the
      archive), and the unattended calls kept or overruled.
- [ ] Every KEEP id in tasks/triage.md is fixed, or declined with the
      maintainer's confirmation (decision 18). Then the plan is
      archived to `tasks/hardening/`.

## Proposed declines for the maintainer

The running list of decision 15. A task that proposes declining a KEEP
id, or part of one, adds it here, one line each: the finding's id, the
task, the reason in a sentence and the date it was added. Work doesn't
wait for it. The maintainer confirms or overrules the whole list in one
sitting at Checkpoint C; a confirmed decline is ticked, and an
overruled one becomes a fix task in this plan before the plan is
archived. An unattended call that declines part of a finding is
listed here too. The ids the cut dropped aren't listed: they are closed
(tasks/triage.md). (docs-L12's E1 half, the one decline proposed before
decision 15, was confirmed by decision 16 and is recorded in Task 6,
not here.)

- [ ] docs-L14, its stricter half (Task 4, 2026-11-12): "lowercase
      first char only" read as no uppercase anywhere in the description
      is declined; only the first letter, after leading non-letters,
      must be lowercase. The stricter reading would reject 26 of the
      290 existing commits, which the maintainer accepted, and would
      need history rewritten. Agent-decided and reversible (the plan,
      after decision 17).

## Deferred by the cut (2026-11-12)

- The DEFER table in [tasks/triage.md](triage.md) lists 21 findings
  that wait for a consumer. The tui spec takes up the journal items and
  the ones on exec's streaming, PTY, attached and mask paths; release
  preparation takes up sb-M1, sb-M4 and sb-L8.

## Follow-ups from the glibc build (Tasks 53 and 54)

- [x] check.sh's production `cross build` runs uncapped: a
      `systemd-run` scope around check.sh doesn't reach cross's
      container. Pass `CROSS_CONTAINER_OPTS` (`--memory`, `--cpus`), as
      `crates/journal/tests/example.sh` does (Task 54). A gate change:
      take it up with the next task that touches check.sh.
- [ ] `crates/journal/src/file.rs:13` still says "static musl binary"
      (ADR-013's reasoning, from before ADR-020): reword when the file
      is next touched.

## Follow-ups from docker start, stop and restart

- docker: a failed job's containers aren't named (the controller's
  `failed`, `outcomes` and `error` would name them; a listed difference
  from sb-go to decide); sb's wait can give up before the controller's own
  job timeout (docker spec, Known limits).
- sb tests: the terminal helpers (`KillOnDrop`, `last_row`,
  `exit_code`, the restore checks) exist in validate_config_signals.rs
  and docker_control_signals.rs; a third copy moves them to
  `tests/common`. Both kill only `timeout` when a test fails before sb's
  pid is known (a process group would reach `script`).
- Contract test: the command tree's dropped flags (`-v` on docker start,
  stop and restart, as on validate-config) aren't recorded; a
  full-equality contract test records them (docker spec, Known limits).
- The controller's richer feedback (live per-container progress, queue
  position, cancel, a version and features check) is for the
  controller's Rust rewrite, the maintainer's (docker decision 9).

## Follow-ups from the journal reader (proto-journal, sb-journal spec)

- The matrix scripts' other fixed waits on journald can time out on a
  loaded host as compat.sh's rotation wait did (raised to 30 s at
  Checkpoint T): compat.sh's `sleep 1` before it counts the lines in
  /var, the `sleep 2` after journald starts (compat.sh, compare.sh,
  stress.sh), and follow.sh's 5 s `start_jd` and 10 s `in_journal` and
  `finish` polls. A shared `wait_until <seconds> <cmd>` helper would
  keep them in step. Raise one when it fails, not before.

- ~~cli-core's `error_block` prints error text raw~~: done in hardening
  Task 9 (014f0c6), which escapes the block and sb's warnings in
  cli-core itself; the stale log.rs comment is fixed in Task 10.

- cargo-deny reports advisory-not-detected for exception E1's two
  ignores (RUSTSEC-2025-0141, RUSTSEC-2024-0320), although bincode,
  yaml-rust and syntect are still in Cargo.lock. Found in Task 11:
  syntect is in the lock only through `prototypes/proto-markdown`
  (tui-markdown), not in crates/sb's graph, which the gate checks
  (`cargo tree -p sb -i syntect` matches nothing). The ignores stay
  until the announcements renderer brings tui-markdown into sb's graph,
  or E1 is decided again at its expiry (2027-01-28).

- The tui spec's viewer (the first consumer): the release's entry order
  can form a cycle when a boot is in no file's tail (Difference 20,
  `forward_paging_cycles_where_the_order_does`), so paging forward one
  entry at a time can loop. A page knows only its cursor, so the
  viewer (or sb-host) must stop at a cursor it has already shown.
  Decide where the guard lives with the tui spec; raise at Checkpoint A.
  The security audit (Task 11) adds a requirement the tui spec must
  record: the viewer passes a much smaller `Limits` than the 256 MiB
  default (a few MiB), and caps the size of one record it displays
  (`DisplayText` expands tabs up to 8 times). The audit accepted a page's
  per-file memory on an imported directory of crafted files (about a
  third of the limit per file) on that condition.

- A mutant can make a walk allocate without bound (`advance ->
  Some(Ok(0))`). Under a `systemd-run` MemoryMax scope, systemd's default
  OOMPolicy then stops the whole scope, cargo-mutants included. Run
  `check.sh task` with `-p OOMPolicy=continue` added, so only the test
  process dies and the mutant counts as caught. Consider a per-test
  memory cap inside check.sh's mutation pass, so an uncapped run can't
  exhaust the host.

- A page holds a descriptor for every listed file at once (Task 3's
  design, kept in Task 4), and a follow holds one per followed file for
  as long as it runs (Task 6), plus its inotify descriptor, and each
  waiter (`journal::FollowWait`) two more: the wake fd's duplicate and
  its stop eventfd, so one eventfd more per follow than the `AsyncFd`
  design. Past the soft `RLIMIT_NOFILE` (1024 by default)
  the extra opens fail with EMFILE, reported one per file. journald's
  defaults keep a journal far below that; decide whether sb raises the
  soft limit to the hard one at startup (rustix `setrlimit`, beside
  cli-core's `RLIMIT_CORE`) or caps the files per page (security audit of
  Task 4, Low; Task 11's audit kept it Low and open for the maintainer:
  the opens past the limit are reported, never dropped).

- CI: a workflow running `scripts/check.sh` (CONSTRAINTS.md asks for
  `full` in CI once CI exists; the runner needs cargo-deny, cargo-mutants,
  cargo-llvm-cov and cross). `.github/workflows/journal-matrix.yml` runs
  only the journal matrix (sb-journal spec, decision 7).
- CI: pushing and running the journal matrix, then afl.rs fuzzing, each
  on the maintainer's go-ahead.

- check.sh's `MUTATION_TESTERS[sb-journal]` names sb-host only, the one
  crate whose tests reach sb-journal today (ADR-012). The task in which
  an `sb` command first uses sb-journal (the tui picker and viewer) adds
  `sb` to the entry, or accepts the slower pass with every crate's
  tests, as ADR-012 says; a tester missing from the entry lowers the
  score.

## Closed follow-ups

Moved here from the lists above once done, for the record (hardening
Task 5, docs-L17). The task numbers are the sb-journal plan's
([tasks/journal/](journal/plan.md)).

- Done: the consumer's wake wait. tokio 1.54.1 deprecates `AsyncFd::new`
  as unsound (tokio PR #8587), and its replacement `AsyncFd::register` is
  `unsafe`, which the workspace forbids. Maintainer's decision
  (2026-11-11): sb-host's `journal::FollowWait` waits with rustix poll on
  the wake fd and a stop eventfd, inside spawn_blocking; its
  `FollowStop` handle, or dropping it, writes the stop fd. The journal
  spec's wake protocol is amended (decision 12), and the follow test in
  crates/host/tests/journal.rs waits through it.

- Closed in Task 11: crates/cli-core/src/exit.rs's anstream `strip_str`
  (which passes C1 code points through) is only in its test module,
  over the test's own fixed error, so no child or file text reaches it.

- Done in Task 7: the local CI workflow (journal-matrix.yml) runs the
  crate's follow.sh and stress.sh after the page checks; still unpushed.

- Done in Task 6: the two mutation survivors at the `e.is_io()` guards
  in file.rs's `test_object` and forward.rs's `all_matches_from` are
  killed by `the_systems_error_reading_a_probed_entry_stops_the_bisect`
  and `the_systems_error_in_the_resumed_list_is_the_walks`.

- Decided in Task 6: the follow reads every header first on each
  drain (`refresh()`), which caps the guessed `object_head` reads at the
  file's length then, and an `Io` from a cut inside one drain is
  waited for once (`Wait::Entry`), like any read, before the next
  drain reads the header again
  (`a_file_cut_while_followed_reads_up_to_the_cut`).

## Review backlog (taken up when each crate is next touched)

From the whole-codebase review (2026-11-05), moved here from
[tasks/exec/todo.md](exec/todo.md); Optional findings and nits, by
crate (the full list, by reviewer finding, is in the commit body named
in the exec plan's Task 9).
Headlines:
- cli-core: `listen_for_signals` once per process (guard it); restore
  hooks `FnOnce`; `Shutdown` as one `Arc<Inner>`; `snapshot()` returns a
  small ending value; `ConfigError` with its path (standalone messages);
  the theme passed to
  `finish_run`/`report`, `install` reporting a second call; `ensure_root`
  given its exe and args (its test is tautological); `config.rs` `Read`
  error's source shown twice; `build_info` `unwrap_or_default` →
  `expect`; `signal_exit_code` overflow on a non-signal; `RelaunchError`
  a struct; the re-exports' reasons; `Uid`/`Signal` types, `AsRef<Path>`,
  named log events (only if the run log wants them).
- host: the supported-releases test restates the constant.
- config: secret redaction typed, not by searching the error text; secret
  paths from the rule walk, not a re-parsed path string (a key with `.`
  or `[`); `Validator` split into sync and credential kinds;
  `max_length` `Option<NonZeroUsize>`; typed network-check errors; a
  `CLOUDFLARE_BASE_URL` path that can't be used is an error, not
  dropped; URLs parsed once; `Node` and `Value` with a redacting `Debug`
  (no production path formats them today: only tests); internal modules
  `pub(crate)` (saphyr,
  regex and reqwest types leak); the `Validators` trait `pub(crate)` or
  concrete; the clock and `fs::metadata` injectable; silent fallbacks →
  `expect`.
- term-ui: the live area's total height capped for ended-but-live steps;
  one lock per output line (`colour`/`terminal` out of the shared
  state); `clean` per-character width; tree helpers' full scans;
  `Target` `#[non_exhaustive]`; the runner's
  clock through `tokio::time::Instant` (times testable); a plain
  in-memory target (an API change: ask); rustdoc without design
  history; `table_colour` `pub(crate)`; `Send + Sync` assertions;
  `write_all` retrying `EINTR`.
- exec: a failed wait marks the leader reaped (the drop guard is off)
  and reads as a spawn failure; `pty_error` keeps `io::ErrorKind`; one
  line splitter for both ways; `value()`/`secret()` take `OsString`;
  `SpawnCause` `#[non_exhaustive]` or accessors; `Send` assertions on the
  futures; the forced-exit hook's registration checked; a missing signal
  read as signal 0 and the PTY wait thread's error → `expect`; tight
  wall-clock bounds in the group tests.
- docker: `json()` and the fixtures fail loudly; bollard types
  `pub(crate)`; typed errors instead of `anyhow` in
  `ps`'s public API (start/stop/restart's are typed: the docker-control plan).
- sb: `warn!("{warning}")` as a field; one runtime builder.
