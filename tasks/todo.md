# sb tasks

In progress: hardening after the re-review. Plan:
[tasks/plan.md](plan.md) (decisions, lanes, the traceability tables,
risks, open questions). Findings, by id:
[tasks/findings.md](findings.md). Finished plans:
- [tasks/journal/](journal/plan.md) (the sb-journal crate);
- [tasks/docker-control/](docker-control/plan.md) (docker start, stop
  and restart);
- [tasks/exec/](exec/plan.md);
- [tasks/config/](config/plan.md);
- [tasks/docker/](docker/plan.md) (`ps`);
- [tasks/term-ui/](term-ui/plan.md);
- [tasks/cli-core/](cli-core/plan.md);
- [tasks/prototypes/](prototypes/plan.md).

Every task follows CLAUDE.md:
- the skills;
- TDD: each finding's probe becomes a failing test before its fix;
- `scripts/check.sh fast` after edits, `task` before handing back,
  `full` before committing;
- the code-reviewer on each commit, plus the library-reviewer where I/O,
  process or terminal code changes, and the security-auditor where
  untrusted input is handled;
- `Library-check:` and `Review:` lines.

Standing rules:
- sb runs as root only with `SB_LOG_DIR` in `target/` or a scratch
  directory outside the repository. Every brief that may run sb says so.
- Heavy builds, the matrix, the fuzz harness and every probe run capped:
  `systemd-run --scope -p MemoryMax=... -p CPUQuota=... -p
  OOMPolicy=continue`.
- Waits poll a PID or a done-file, never `pgrep -f`.
- Container scripts run with a unique `SBJ_PREFIX`. A matrix mismatch
  that isn't a listed Difference stops for the maintainer.
- A named expected failure in a script or the fuzz harness (the plan,
  "Named expected failures") is one line in the script itself, at the
  start of a line after any indentation:
  `expected_failure <id> <task> <always|racy> <name...>`,
  with the finding's id (`[a-z]+-[A-Z][0-9]+`). No data file beside
  the script holds entries. The task that fixes a finding removes that
  finding's entries wherever they are.
- Probe logs and screenshots stay outside the repository. A platform
  note holds versions and the method, never host details.
- Nothing is pushed.

A Low sweep lists its ids from tasks/findings.md, each marked **fix** or
**proposed-decline** with a reason. The proposed declines go to the
maintainer as a list before the sweep is committed; an overruled decline
is fixed in the same sweep. A fix that needs files beyond the sweep's
own, or that changes a design (a protocol, a public API), becomes a new
task.

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
- [ ] code-reviewer on the commit.

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
- [ ] `check.sh full <base>` runs `scripts/check-commit-messages.sh
      <base> HEAD` when the base isn't HEAD.
- [ ] The commit-message script skips only true merge commits (by
      parent count), not any subject starting "Merge ", and checks the
      whole description for lowercase, not only its first character.
- [ ] `fast` fails when a crate under `crates/` has no `deny.toml`
      `bans.deny` entry, or doesn't inherit the workspace lints.
- [ ] `check.sh` compares each tool's version with the CONSTRAINTS Tools
      table and exits 2 naming a tool that differs: the cargo-installed
      and pinned tools exactly; the system packages (node, `script`,
      Docker) by major version or minimum, so a host package update
      doesn't fail every run. The table says which rule each tool has.
- [ ] `git diff` calls use `--no-ext-diff --no-color` and fixed
      prefixes; cargo calls use `--locked`; an inherited
      `CARGO_TARGET_DIR` can't redirect a stage; a run on an
      architecture other than x86_64 says what it skips; the dead "no
      crates yet" branches go.
- [ ] CONSTRAINTS.md gets rows for the commit-message check and the
      `Review:` line.

**Verification:**
- [ ] A stubbed run for each new branch (a crate without an entry, a
      wrong tool version, a "Merge " subject); `scripts/check.sh full`.
- [ ] code-reviewer on the commit.

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
- [ ] CONSTRAINTS.md says the matrix and the fuzz harness run by hand,
      at each checkpoint and after any task that touches reading, until
      CI is authorised.
- [ ] The journal plan's Checkpoint A agrees with its todo's record.
- [ ] The journal plan's "decisions 1-9" and "API sketch" references
      match the spec.
- [ ] `scripts/check-plan-ids.mjs` checks that every id in
      tasks/findings.md is in the plan's traceability tables and in a
      task here, and that no task cites an unknown id. It takes the
      three paths and is run at each checkpoint.
- [ ] This file: done items leave the open lists; a follow-up is added
      for `MUTATION_TESTERS` when sb reaches sb-journal; host.md has a
      plan entry or a note that it has none.

**Verification:**
- [ ] `scripts/check.sh fast`; the links resolve; the id check passes.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 4
**Files:** `CONSTRAINTS.md`, `tasks/journal/plan.md`, `tasks/journal/todo.md`, `tasks/todo.md`, `scripts/check-plan-ids.mjs`
**Scope:** S

## Task 6: Low sweep: documents

**Description:** The stale statements the docs review listed, and the
capability map's missing rows (docs-M6). It runs before the lanes, so
the journal lane starts from a corrected spec.

**Acceptance criteria:**
- [ ] docs-M6: the capability map's stack table has the nix row host.md
      requires, and the rustix row names sb-host's `event`.
- [ ] Each id below is fixed, or its decline is confirmed:
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
        tightening); **proposed-decline** for exception E1's ignores:
        the recorded decision keeps them until E1's expiry (this file,
        "Follow-ups from the journal reader").
      - docs-L15 **maintainer's**: CLAUDE.md's two stale lines (the
        plan's open questions).
      - docs-L16 **fix at Checkpoint C**: CONSTRAINTS "Last reviewed"
        is set when the maintainer reviews.

**Verification:**
- [ ] `scripts/check.sh fast`; `cargo deny --manifest-path
      crates/sb/Cargo.toml check` after the deny.toml edit.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 5
**Files:** `docs/specs/capability-map.md`, `docs/specs/journal.md`, `docs/specs/host.md`, `deny.toml`, `docs/decisions/ADR-013-own-journal-reader.md`
**Scope:** S

## Checkpoint A: gates
- [ ] `check.sh full` passes on its own change, every step run, with
      any expected finding acknowledged in the open.
- [x] Six [start] questions were answered on 2026-11-11 (the plan's
      decisions 6 to 12): Task 2's ADR, table cells in Task 8, `nohup`
      in Task 12, the window in Task 13, the API names, the forward
      loop, and the page against journalctl. Publication and docs-L15
      were deferred; no task waits on either.
- [ ] Task 6's proposed decline (the exception E1 half of docs-L12) is
      confirmed or overruled.
- [ ] `scripts/check-plan-ids.mjs` passes.

## Task 7: a child that prompts on the terminal: research and decision

**Description:** A piped child that touches the controlling terminal
(an ssh passphrase or host-key prompt under git) is stopped by the
kernel, and sb waits silently for ever (exec-M1; confirmed). This task
finds what can be done without `unsafe` and ends at the maintainer's
choice. It opens lane 1 and is done when the options are with the
maintainer, so the lane goes on while the answer is waited for; Task 14
records the decision. It commits no test: a failing test can't pass
`check.sh full`, so the reproduction is a probe described in the
write-up, and the test lands with the fix in Task 14.

**Acceptance criteria:**
- [ ] A probe reproduces the hang with a child that opens the terminal,
      under a timeout. The write-up gives its method and the versions,
      with no host details, exactly enough for Task 14 to turn it into
      the test.
- [ ] The research is written up with sources:
      - whether a safe `setsid` for a child exists on Rust 1.99.1 (std,
        tokio, rustix), given that `pre_exec` is `unsafe`;
      - detecting the stopped child from the wait, as exec's PTY path
        already asks for stops;
      - ssh's batch mode for children that aren't attached
        (`GIT_SSH_COMMAND` with `BatchMode=yes`,
        `SSH_ASKPASS_REQUIRE=never`): what it covers and what it
        doesn't;
      - what each option does to an ssh prompt under git.
- [ ] The options and a recommendation go to the maintainer (the plan's
      open question): fail fast by detection, by batch mode, by a new
      session, or keep the terminal reachable as sb-go did, with that
      option's cost to the process-group design stated.

**Verification:**
- [ ] The probe hangs for the stated reason and ends at its timeout;
      `scripts/check.sh fast`.
- [ ] code-reviewer on the write-up.

**Dependencies:** Checkpoint A
**Files:** `docs/platform-notes/exec-processes.md`
**Scope:** S

## Task 8: table cells are sanitized

**Description:** `sb docker ps` prints container label values raw: an
image's `LABEL` reaches root's terminal (san-H1, docker-M1, ui-M4;
confirmed with OSC 52, a title change and a screen clear). Sanitize in
`Table`, the one place every cell passes.

**Acceptance criteria:**
- [ ] A cell holding OSC, CSI, C1, a tab or a CR renders with none of
      them; `\n` still separates a cell's lines. Colour the table adds
      itself is kept.
- [ ] A `docker ps` test with such a Traefik label and such a name
      shows no escape and no forged row. `--json` is unchanged.
- [ ] `docs/specs/term-ui.md` "Escape handling" names tables.
- [ ] The table half of san-T1: a test over the sanitizer's alphabet in
      a cell.
- [ ] san-I2 (table cells): bidi overrides and zero-width characters in
      a cell are escaped (decision 7), with a test; the term-ui spec's
      "pass unmarked" sentence gets the table exception.

**Verification:**
- [ ] `cargo test -p sb-term-ui -p sb-docker`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 7 (its write-up; not the maintainer's answer)
**Files:** `crates/term-ui/src/table.rs`, `crates/term-ui/tests/table.rs`, `crates/docker/src/ps.rs` (tests), `docs/specs/term-ui.md`
**Scope:** S

## Task 9: the error block escapes error text

**Description:** cli-core's error block prints error text raw (san-M1,
cli-M3 on screen). cli-core can't use term-ui's `DisplayText`, so it
escapes control characters itself, per line.

**Acceptance criteria:**
- [ ] `shown_chain` and `render_block` escape control characters in
      every line; a test with an OSC, a CR and a newline in a cause shows
      none raw, and the block keeps its shape.
- [ ] It calls cli-core's `escape_controls` as it is. That function
      lives in `config.rs`, lane 3's file, which this task doesn't edit.
- [ ] `docs/specs/cli-core.md`'s error-block text says so.
- [ ] Closes the first half of the follow-up "cli-core's `error_block`
      prints error text raw" (Follow-ups from the journal reader).

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb`; `scripts/check.sh task`.
- [ ] security-auditor and code-reviewer on the commit.

**Dependencies:** Task 8
**Files:** `crates/cli-core/src/exit.rs`, `crates/sb/tests/errors.rs`, `docs/specs/cli-core.md`
**Scope:** S

## Task 10: no logged value can forge a run-log record

**Description:** The run log takes `\n` and `\r` unescaped, and `%`
Display fields with nothing escaped, so an error can forge a "run
finished" record (san-M2, cli-M3 in the log). The fix is at one choke
point, the log's formatter layer (M-LOG-STRUCTURED), not at each call.

**Acceptance criteria:**
- [ ] The formatter escapes control characters, `\n` and `\r` in every
      field of every record: message, error, Debug and Display fields,
      and records bridged from dependencies. A record is always one
      line.
- [ ] A test logs a value holding a newline and a forged "run finished"
      through each field kind and through a bridged record, and reads
      back one record each (the newline part of cli-T1).
- [ ] The backstop: a test scans `crates/` for `%` Display fields in
      tracing macros and fails on one that isn't in a checked-in
      allowlist of fields holding sb's own text.
- [ ] docker-L1 and docker-L2: daemon error text reaches the log
      escaped, and `DOCKER_HOST` is shown and logged with its userinfo
      removed and control characters escaped.
- [ ] san-L4: the stale comment in cli-core's log.rs is corrected.
      Closes the second half of the follow-up "cli-core's `error_block`
      prints error text raw".
- [ ] The specs agree: cli-core.md's log format, and term-ui.md's "a
      step's error record is logged as is".
- [ ] Takes up the backlog entry "sb: `warn!("{warning}")` as a field".

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb-docker -p sb-exec -p sb`;
      `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 9. A tracing-subscriber feature, if one is
needed, lands on main first (the plan, "Lanes and shared files").
**Files:** `crates/cli-core/src/log.rs`, `crates/cli-core/tests/` (the scan and its allowlist), `crates/docker/src/connect.rs`, `crates/sb/src/main.rs`, `docs/specs/cli-core.md`, `docs/specs/term-ui.md`
**Scope:** M

## Task 11: secrets are masked after escapes are stripped

**Description:** exec masks raw bytes, then term-ui drops escapes, so
`sec<SGR>ret` reaches the screen and the log as `secret` (exec-M2,
san-L3; confirmed). Sanitize first, then mask.

**Acceptance criteria:**
- [ ] A secret split by an escape or by a dropped control character is
      masked on screen and in the log, for `stream` and `pty`.
- [ ] The same holds for a failure block's last lines and for a child's
      error text.
- [ ] The exec spec's success criterion 6 names this case.

**Verification:**
- [ ] `cargo test -p sb-exec -p sb-term-ui`; `scripts/check.sh task`.
- [ ] security-auditor and code-reviewer on the commit.

**Dependencies:** Task 10
**Files:** `crates/exec/src/{run.rs,pty.rs,mask.rs}`, `crates/term-ui/src/runner.rs`, `docs/specs/exec.md`
**Scope:** M

## Task 12: a hangup cancels cleanly

**Description:** Decision 2. Today a dropped SSH session kills sb and
orphans its piped children (exec-H1, cli-M1; confirmed). SIGHUP is
handled like SIGTERM.

**Acceptance criteria:**
- [ ] On SIGHUP the token is cancelled, children are stopped with the
      usual grace, the run log gets its ending, and sb ends by SIGHUP
      (`$?` reads 129).
- [ ] A real hangup: a PTY test closes the master, so writes to stderr
      and the termios restore fail with EIO; sb still logs its ending,
      leaves no child, and dies by SIGHUP. A `kill -HUP` test covers
      the plain case.
- [ ] While an attached child runs, a SIGHUP is treated as a SIGTERM is
      (exec decision 12).
- [ ] Under `nohup` (SIGHUP ignored at startup) sb leaves it ignored
      (decision 8): a hangup then doesn't cancel the run. A test starts
      sb that way.
- [ ] The specs agree: cli-core.md's cancellation text, exit codes,
      testing strategy, success criterion 4 and the logrotate
      paragraph; exec.md's attached children.
- [ ] `docs/platform-notes/logrotate.md` no longer says sb doesn't
      handle SIGHUP, and still says no `postrotate` signal is wanted.

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb-exec -p sb`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 11
**Files:** `crates/cli-core/src/cancel.rs`, `crates/sb/tests/` (a hangup test file), `crates/exec/tests/attached_signals.rs`, `docs/specs/cli-core.md`, `docs/specs/exec.md`, `docs/platform-notes/logrotate.md`
**Scope:** M

## Task 13: a relayed signal isn't a second signal; forced exits are logged

**Description:** Under sudo a group-wide signal arrives twice, and the
copy counts as the second, forcing signal, so cleanup is skipped
(cli-M2; live once Task 12 lands). A forced exit and the hard escape
leave no log record (cli-L3).

**Acceptance criteria:**
- [ ] The same signal repeated within the window doesn't force the
      exit. The window is one documented constant
      (M-DOCUMENTED-MAGIC): 250 ms, unless the probe shows another
      value is needed. It applies to SIGTERM and SIGHUP only (decision
      9); two SIGINTs still force the exit.
- [ ] A unit test drives the window with an injected clock
      (M-MOCKABLE-SYSCALLS): inside it, at its edge and past it.
- [ ] A probe under a real session scope and sudo confirms the relayed
      copy's timing. Its method and versions go in the platform note,
      with no host details. If it shows SIGINT doubled under sudo too,
      that goes back to the maintainer before the task ends.
- [ ] A forced exit and the hard escape each leave one log record with
      the run id.
- [ ] `decide(Some(SIGINT), true, SIGTERM)` has its test (cli-T1).
- [ ] cli-core.md's "a second signal ends sb at once" and its "Decided
      in review" entry are amended.

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb`; the probe; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 12
**Files:** `crates/cli-core/src/cancel.rs`, `crates/sb/tests/` (signals), `docs/specs/cli-core.md`, `docs/platform-notes/exec-processes.md`
**Scope:** M

## Task 14: a child that prompts on the terminal: the fix

**Description:** Implement the decision Task 7 brought to the
maintainer (exec-M1). The reproduction becomes the failing test here.

**Acceptance criteria:**
- [ ] The maintainer's decision is recorded in the exec spec, with a
      Differences entry if it differs from sb-go.
- [ ] Task 7's probe is a test: a piped child that opens the terminal,
      under a timeout. It fails before the fix.
- [ ] A piped child that prompts on the terminal never leaves sb
      waiting silently: the step fails with an error that says the
      child asked for input on the terminal, or the prompt works, as
      decided.
- [ ] If batch mode was chosen: every child that isn't attached gets
      it, beside `GIT_TERMINAL_PROMPT=0`; the exec spec's success
      criterion 7 names it; a caller's own `GIT_SSH_COMMAND` isn't
      overwritten silently.
- [ ] The child and its group are gone afterwards; nothing is left
      stopped. A `stream` and a `capture` case are covered.

**Verification:**
- [ ] `cargo test -p sb-exec`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 13, and the maintainer's answer to Task 7
**Files:** `crates/exec/src/{run.rs,group.rs,lib.rs}`, `crates/exec/tests/` (the test), `docs/specs/exec.md`
**Scope:** M

## Task 15: exec's smaller defects

**Description:** Four confirmed defects in exec's launch and wait
paths (exec-M3, exec-L1, exec-L2, exec-L6).

**Acceptance criteria:**
- [ ] `pty` with a cleared environment finds a bare program name as
      the piped ways do, and a missing program fails with `NotFound`.
- [ ] `timeout` and `grace` of `Duration::MAX` don't panic.
- [ ] `stream` keeps a final unterminated line when a grandchild holds
      the pipe and the drain gives up.
- [ ] `attached` checks the token again after the hold.

**Verification:**
- [ ] `cargo test -p sb-exec`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 14
**Files:** `crates/exec/src/{run.rs,pty.rs}`, `crates/exec/tests/{pty.rs,stream.rs,attached.rs}`
**Scope:** M

## Task 16: exec's timing-bound tests assert order

**Description:** Several exec tests have tight wall-clock upper bounds
that fail on a loaded machine (exec-M4). Closes the backlog entry
"exec: tight wall-clock bounds in the group tests".

**Acceptance criteria:**
- [ ] The tests the finding names (stream.rs:201, pty.rs:183, 215 and
      247, groups.rs:163-164) assert the order of events, with wide
      bounds only as a guard against hanging.
- [ ] No assertion is dropped: each still fails when the behaviour it
      names is broken (shown once per test by breaking it).

**Verification:**
- [ ] `cargo test -p sb-exec`, three times in a row under a CPU cap;
      `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 15
**Files:** `crates/exec/tests/{stream.rs,pty.rs,groups.rs}`, `scripts/floor-acknowledged.txt` (the re-targeted bounds)
**Scope:** S

## Task 17: panics reach the run log and the screen

**Description:** A panic leaves "run started" with no ending in the
log (sb-M2), and a panic in a concurrent step is overdrawn by the live
tree (ui-M3; confirmed).

**Acceptance criteria:**
- [ ] A panic hook, installed once the log is open, records the message
      and location with the run id, then calls the previous hook. The
      message goes through Task 10's escaping; a test panics with a
      newline in the message.
- [ ] A group sibling's panic stops the redraw unconditionally, so its
      message stays on screen; a PTY test asserts it.
- [ ] ui-L2: the terminal is restored when the runner's setup itself
      fails (`Saved` restores on drop).

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb-term-ui -p sb`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 10 (cli-core's log), Task 16
**Files:** `crates/cli-core/src/log.rs`, `crates/sb/src/main.rs`, `crates/term-ui/src/{runner.rs,tty.rs}`, `crates/sb/tests/run_log.rs`
**Scope:** M

## Checkpoint T: text and processes
- [ ] `check.sh full`; the signal probes pass under sudo.
- [ ] The plan's questions marked [checkpoint T] are answered (Tasks
      18, 19 and 25).
- [ ] Review with the maintainer.

## Task 18: `docker restart` never ends silently short of a start

**Description:** A wait that ends without a final job status (the 295 s
wait against the controller's 300 s stop, one failed poll, a timed-out
request) prints no notice, and `restart` then never starts the
containers (docker-M2). How far to differ from sb-go is the
maintainer's (the plan's open question); the floor below isn't.

**Acceptance criteria:**
- [ ] The floor: `restart` never ends without either the start
      requested or a line saying the start wasn't requested and why.
      A test covers each non-final ending: sb's timeout, a failed poll,
      a timed-out request.
- [ ] The notice "the controller may still be working" and the
      tolerated poll failures are as the maintainer chose, each with a
      test.
- [ ] The docker spec records the decision, its Differences and the
      amended Known limit "sb can give up before the controller does".
      Closes that half of the follow-up "sb's wait can give up before
      the controller's own job timeout".

**Verification:**
- [ ] `cargo test -p sb-docker -p sb`; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Checkpoint T, and the maintainer's answer to this
task's question
**Files:** `crates/docker/src/control/{commands.rs,client.rs}`, `crates/docker/src/testing/`, `crates/sb/tests/docker_control.rs`, `docs/specs/docker.md`
**Scope:** M

## Task 19: echo is off during a live run

**Description:** Typing or pressing Enter during a live run leaves
stale duplicate lines, because only `ECHOCTL` is cleared (ui-M2;
confirmed in tmux).

**Acceptance criteria:**
- [ ] ECHO is off while the runner is live and restored after: on a
      normal end, on an interruption, on the forced exit's restore hook
      and on a failed setup.
- [ ] A PTY test types and presses Enter during a run and finds no
      stale line.
- [ ] term-ui.md's `ECHOCTL` wording (the terminal restore, the
      `Runner` rustdoc, success criterion 6, decision 5) is amended,
      as the maintainer accepted.

**Verification:**
- [ ] `cargo test -p sb-term-ui -p sb`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 11, Task 17 (`runner.rs`, `tty.rs`)
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
      lines stay visible, by the way the maintainer chose (collapsed
      settled lines, or settled children printed for good).
- [ ] An in-memory test has fewer rows than the tree; a second covers
      a terminal resized while live.
- [ ] The finished tree is complete either way: nothing collapsed is
      lost from the final output or the log.
- [ ] term-ui.md records the decision.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; a tmux probe at 80x12, with
      screenshots of before and after for the maintainer;
      `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 19
**Files:** `crates/term-ui/src/{tree.rs,runner.rs}`, `crates/term-ui/tests/live.rs`, `docs/specs/term-ui.md`
**Scope:** M

## Task 21: table wrap, uncut lines and one-line labels

**Description:** The display Lows that change a reviewed look, brought
to the maintainer with snapshots (ui-L1 and san-I1, ui-L3, ui-L4,
ui-L5).

**Acceptance criteria:**
- [ ] ui-L3: a wrapped table has no blank line at exact multiples of
      the width, a padded binding stays whole, and a terminal below the
      minimum width says so instead of emptying cells.
- [ ] ui-L4: notices and kept output lines in the finished tree are
      shown as the maintainer chose (whole, or cut).
- [ ] ui-L1, san-I1: a step label and a title are cleaned to one line.
- [ ] ui-L5: a stored output line is capped, and isn't measured again
      on every frame.
- [ ] The snapshots change only where these criteria say; term-ui.md
      records the looks.

**Verification:**
- [ ] `cargo test -p sb-term-ui -p sb-docker`; a tmux probe at 36
      columns; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 20
**Files:** `crates/term-ui/src/{table.rs,tree.rs,runner.rs}`, `crates/term-ui/tests/table.rs`, `docs/specs/term-ui.md`, `scripts/floor-acknowledged.txt` (the changed snapshots' assertions)
**Scope:** M

## Task 22: Low sweep: cli-core

**Description:** cli-core's remaining Lows. All can happen on a real
server, so all are fixes.

**Acceptance criteria:**
- [ ] Each id is fixed with a test:
      - cli-L1 **fix**: the log directory check doesn't follow
        symlinks (`symlink_metadata`, and the file opened without
        following one).
      - cli-L2 **fix**: an empty root-owned `SB_LOG_DIR` isn't
        silently changed to 0700 with its sticky bit dropped.
      - cli-L4 **fix** (with the `SB_LOG_DIR=""` part of sb-L4): an
        empty `SB_LOG_DIR` is treated as unset.
      - cli-L5 **fix**: a requested exit code doesn't swallow a real
        error.
      - cli-L6 **fix**: no details pointer is printed when the log
        stopped; the spec line agrees. cli-core has no "log stopped"
        state today, so the fix includes noting a failed write.
      - cli-L7 **fix**: sudo's argv has `--` before the executable.
      - cli-T1 **fix**: tests for a symlinked and an empty log
        directory.
- [ ] No proposed declines.

**Verification:**
- [ ] `cargo test -p sb-cli-core -p sb`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 21
**Files:** `crates/cli-core/src/{log.rs,exit.rs,root.rs}` (with their unit tests), `crates/sb/src/main.rs`, `crates/sb/tests/run_log.rs`, `docs/specs/cli-core.md`
**Scope:** M

## Task 23: Low sweep: exec

**Description:** exec's remaining Lows.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - exec-L3 **fix**: a group id isn't tested again once it was
        seen empty.
      - exec-L4 **fix** (document): the spec says a PTY child that
        reads twice hangs, and why.
      - exec-L5 **fix**: a secret sent on a child's stdin can be
        registered for masking. The API name goes to the maintainer.
      - exec-L7 **fix**: the forced-exit hook doesn't log between
        kills under the lock.
      - exec-L8 **proposed-decline**: the leak needs a spawn on
        another thread while a PTY opens, and sb's runtime is
        current-thread, so no spawn runs in that window. A test in sb
        pins the runtime kind where it is built
        (`crates/sb/src/main.rs`), and the exec spec's known limits
        say that parallel spawns need a spawn lock in exec first.
      - exec-L9, in three parts:
        - **fix**: a secret with a leading newline and a short body is
          masked in line-split output;
        - **proposed-decline**: a 1 or 2 byte secret masks every
          occurrence. It is the spec's rule, and no Saltbox secret is
          that short; the doc comment says so;
        - **proposed-decline**: encoded forms aren't matched. Matching
          is literal by spec, and a caller registers each form it
          passes.

**Verification:**
- [ ] `cargo test -p sb-exec`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 22
**Files:** `crates/exec/src/{group.rs,lib.rs,mask.rs}`, `crates/exec/tests/`, `crates/sb/src/main.rs` (the runtime test), `docs/specs/exec.md`
**Scope:** M

## Task 24: Low sweep: docker

**Description:** docker's remaining Lows.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - docker-L3 **fix**: `tcp://host` with no port and a bare
        `unix://` are refused before any request, with an error that
        names the missing port or path, instead of reaching bollard as
        port 80 and an empty path. docker.md's Daemon paragraph and its
        differences list say so; a test each.
      - docker-L4 **fix if confirmed**: a probe against the
        controller's idle timeout decides whether a reused keep-alive
        connection can fail a poll. If it can, idle connections aren't
        pooled; if not, **proposed-decline** with the measurement.
      - docker-L5 **fix**: a list entry without an id is a failure,
        not a silent skip.
      - docker-L6 **fix**: one full-shape synthetic inspect and list
        fixture.
      - docker-L7 **proposed-decline**: the fake's refusal of
        unversioned paths is a guess, and its code says so; the test
        claims only "an error, not a panic".
      - docker-L8 **proposed-decline**: the first of several names is
        the display name, as in sb-go; only legacy links give a second
        name, and Saltbox doesn't use them.
      - docker-L9 **proposed-decline**: two guideline notes with no
        bug (`ControlError`'s shape; `Daemon`'s public bollard field),
        in an internal crate whose tests match the variants.

**Verification:**
- [ ] `cargo test -p sb-docker -p sb`; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 23
**Files:** `crates/docker/src/connect.rs`, `crates/docker/src/control/{client.rs,mod.rs}`, `crates/docker/src/{testing/,fixtures/}`, `docs/specs/docker.md`
**Scope:** S

## Task 25: Low sweep: the sanitizer

**Description:** The sanitizer's remaining Lows and unrated items.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - san-L1 **fix**: the live tree's `cut` and its width agree with
        the sanitizer on what an SGR is (colon forms, long parameters),
        with a test that runs sanitized colon-form lines through `cut`.
      - san-L2, as the maintainer chose (the plan's open question): a
        reset appended to any drawn line that kept an SGR, and
        font-select parameters dropped; or the reset only; or a
        confirmed decline. The term-ui spec's "Escape handling" and
        success criterion 2, and the journal spec's display decision 2,
        are amended to match.
      - san-I2 (log text) **proposed-decline**: bidi and zero-width
        characters pass in a step's output and in log text, as the
        journal spec's display decision 2 accepts; right-to-left text
        must display. Table cells are Task 8's.
      - san-I3 **proposed-decline**: a sequence split across lines
        leaves only printable text on the second line, never a control
        byte; joining lines would need state across exec's line split.
      - san-I4 **proposed-decline** (carried): the viewer's own size
        cap is already a recorded requirement for the tui spec (this
        file, "The tui spec's viewer").
      - san-T1 **fix**: tests feed sanitized output through `cut`, and
        the short-input test's alphabet gains CAN, SUB, DEL, `:` and
        the 8-bit DCS and APC introducers.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 24, and the answer to the san-L2 question
**Files:** `crates/term-ui/src/{text.rs,tree.rs}`, `crates/term-ui/src/runner.rs` (only if san-L2's reset is written there, for Plain mode), `docs/specs/term-ui.md`, `docs/specs/journal.md` (one line of display decision 2, if san-L2 is accepted)
**Scope:** M

## Task 26: Low sweep: term-ui's runner

**Description:** The runner's remaining Lows. All are fixes.

**Acceptance criteria:**
- [ ] Each id is fixed:
      - ui-L6 **fix**: a step record is logged before the drawing lock
        is taken.
      - ui-L7 **fix**: `tests/runner.rs` runs `script` under a
        timeout; `tests/live.rs` can't go live on a real terminal; the
        `--colour` case removes `NO_COLOR`. The two stale ignored
        `.pending-snap` files are listed for the maintainer to delete.
        (The tests for ui-M1 to ui-M4 and ui-L1 come with Tasks 8, 17
        and 19-21.)
      - ui-L8 **fix**: `begin` checks that the parent is still running
        under the same lock.
- [ ] No proposed declines.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; `scripts/check.sh task`.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 25
**Files:** `crates/term-ui/src/{runner.rs,tree.rs}`, `crates/term-ui/tests/{runner.rs,live.rs}`
**Scope:** S

## Checkpoint D: docker and display
- [ ] `check.sh full`; the tmux probes at 80x12 and 36 columns.
- [ ] The declines of Tasks 22-25 are confirmed or overruled (Task 26
      proposes none).
- [ ] Review with the maintainer (the looks, from screenshots).

## Task 27: the crafted builder writes compact and keyed files

**Description:** The crafted builder writes only regular, unkeyed
files, so five mutants in the compact branches survive every unit test
(tests-H1). 24.04 and 26.04 write compact files by default. The builder
comes first, so the fixes of Tasks 33-38 are tested in both layouts.

**Acceptance criteria:**
- [ ] The builder has compact and keyed modes, with tests of its own
      that read its files back.
- [ ] The five named mutants (file.rs:1417, 1547, 1746, 1813, 1857)
      are caught by file-layer tests on a compact file.
- [ ] file-L5's file-layer mutants are caught: the arena cap on `n`
      (file.rs:1108, 1721), the alignment check (:1023), `left_items`
      (:1533) and `ENTRY_SIZE_MAX` (:631). The dead code at :1789 goes.

**Verification:**
- [ ] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] The named mutants, run by name: `cargo mutants -p sb-journal
      --file crates/journal/src/file.rs --re <site>` for each site found
      with `--list`; caught or missed recorded in the commit body.
- [ ] code-reviewer on the commit.

**Dependencies:** Checkpoint A
**Files:** `crates/journal/src/testing.rs`, `crates/journal/src/file.rs` (tests, the dead code)
**Scope:** M

## Task 28: the reader's suites run over both layouts

**Description:** The second half of tests-H1: the suites above the
file layer run on regular and compact files. The weak tests the reviews
confirmed by mutation go with them (file-L5's hash vectors, page-L8).

**Acceptance criteria:**
- [ ] The walk, forward, boot and follow suites run over both layouts
      (one parameter, not copied tests).
- [ ] lookup3 has the two vectors from systemd's own `lookup3.c` at
      lengths that are multiples of 12; the `hash.rs:40` mutant is
      caught.
- [ ] The weak walk tests (walk.rs:1190-1203, 961-972, 1442-1456, 1184)
      assert what their names say.
- [ ] A suite that fails on compact files for a reason no finding
      covers isn't committed failing: it stops for the maintainer and
      becomes a finding with an id and a task.

**Verification:**
- [ ] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] `cargo mutants -p sb-journal --file crates/journal/src/hash.rs`,
      and the walk tests' mutants by name; results in the commit body.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 27
**Files:** `crates/journal/src/{walk.rs,forward.rs,boots.rs,follow.rs,hash.rs}` (tests), `scripts/floor-acknowledged.txt` (the re-targeted weak tests)
**Scope:** M

## Task 29: the page scripts can't pass vacuously

**Description:** stress.sh passes when every snapshot fails; compare.sh
labels variants it never checks; the page scripts ignore the read
errors sb-journal reports (tests-H2, tests-M2, tests-M6). The scripts
are hardened before the fixes they must check.

**Acceptance criteria:**
- [ ] stress.sh requires snapshots taken, entries compared and clean
      reads, each above a stated floor, and fails on a noisy snapshot
      whose stderr isn't a listed transient pattern. Its ready loop has
      a failure branch (tests-L13), and its fixed sleep is replaced by
      a wait on each writer's last number (tests-L3).
- [ ] compare.sh asserts each variant's header flags per release, with
      a printed note where a release can't write one, and asserts that
      all seven variants ran (tests-L7).
- [ ] `compare` fails on zero expected entries (tests-L6);
      `stepback_checks` fails for a 22.04 writer instead of returning
      with a note (tests-L5). If a release's writer can never keep the
      stepped-back clock, that release gets an explicit skip that the
      script asserts and prints, not a silent return.
- [ ] compare.sh, compat.sh and stress.sh fail on stderr from
      sb-journal that isn't its stats line.
- [ ] The workflow runs compat.sh after a compare failure (the first
      half of tests-L11).
- [ ] The three scripts pass on 22.04, 24.04 and 26.04. A check that
      fails on a defect a later task fixes is a named expected failure
      (the plan, "Named expected failures"): one line in the script,
      `expected_failure <id> <task> <always|racy> <name...>`. Each is
      printed on every run. An `always` entry fails the script when
      its check passes; a `racy` one (a defect that needs a
      race to show) fails nothing. Any other failure stops for the
      maintainer.

**Verification:**
- [ ] compare.sh, compat.sh and stress.sh on three releases, capped,
      logs kept outside the repository; each new check shown to fail
      once on a broken input.
- [ ] shellcheck; actionlint; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 28
**Files:** `crates/journal/tests/{stress.sh,compare.sh,checks.sh}`, `crates/journal/src/testing/tools.rs`, `.github/workflows/journal-matrix.yml`, `scripts/floor-acknowledged.txt` (an expected-failure entry, if any)
**Scope:** M

## Task 30: follow.sh checks order, count and overflow

**Description:** follow.sh compares sets, never order and never the
written count, and its overflow scenario passes without an overflow
(tests-M7, tests-M8). Its smaller defects go with them (tests-L1,
tests-L2, tests-L14).

**Acceptance criteria:**
- [ ] Each single-writer scenario asserts the numbers in order, and the
      written count against the expected one; the between-page writes
      are checked for position.
- [ ] The overflow scenario waits, after the followers continue, for
      the entries written before it, so only the overflow's recovery
      can deliver them; it asserts that an overflow happened.
- [ ] Follower PIDs come from `$!`, and an empty one fails (tests-L1).
- [ ] The restart scenario waits until journald has read the last line
      before it kills journald (tests-L2).
- [ ] The vacuum scenario also deletes a file not yet read to the end
      (tests-L14).
- [ ] follow.sh fails on stderr from sb-journal that isn't its stats
      line.
- [ ] It passes on 22.04, 24.04 and 26.04, with named expected failures
      only for defects a later task fixes, as in Task 29. follow-H1
      through the count check is one, and it is a race, so its entry is
      `racy`.

**Verification:**
- [ ] follow.sh on three releases, capped; each new check shown to fail
      once on a broken input.
- [ ] shellcheck; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 29
**Files:** `crates/journal/tests/follow.sh`, `crates/journal/tests/checks.sh`, `scripts/floor-acknowledged.txt` (an expected-failure entry, if any)
**Scope:** M

## Task 31: the fuzz seeds and harness check what they read

**Description:** The seeds are six formats, not eight; the harness
never checks that anything was read, misses several read paths, and
bounds only resident memory (tests-M1, tests-M3, tests-M4, tests-M5).

**Acceptance criteria:**
- [ ] Seeds are distinct by header size, flags and writer, not by
      bytes, and include 240-byte headers, non-compact 255, a 26.04
      file and a journald-written file.
- [ ] Each unmutated seed must read clean with its expected count, and
      a run keeps a floor of records read.
- [ ] The harness reads each input through: the realtime and monotonic
      bisects; the per-boot seek; a vanished cursor; the match path on
      drains; a small `Limits` (`Batch::more`); a merge of several
      files; and all three positions on the grown file.
- [ ] Address space is bounded from the base (`RLIMIT_AS` at the base
      size plus a margin), not only resident pages: in the harness if
      rustix's feature is already on, otherwise through `prlimit --as`
      from seeds.sh with the base taken from a dry run. A manifest
      change lands on main first (the plan, "Lanes and shared files").
- [ ] The workflow runs the seeds and the harness (the second half of
      tests-L11).
- [ ] A harness failure on a defect that Tasks 33-38 fix is a named
      expected failure, marked `always` or `racy`, as in Task 29: an
      `expected_failure` line in seeds.sh, where the floor guard sees
      it. The fixing task removes it. A harness failure with no finding
      behind it stops for the maintainer.

**Verification:**
- [ ] seeds.sh and the fuzz harness on three releases, capped;
      `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] code-reviewer and security-auditor on the commit.

**Dependencies:** Task 30
**Files:** `crates/journal/tests/seeds.sh`, `crates/journal/src/testing/fuzz.rs`, `crates/journal/examples/fuzz.rs`, `.github/workflows/journal-matrix.yml`, `scripts/floor-acknowledged.txt` (an expected-failure entry, if any)
**Scope:** M

## Task 32: the page-to-follow handover: facts, probe and options

**Description:** A page and the follow after it disagree about where
the page ended (follow-H1, page-H1, fwd-M2; confirmed three times; the
mirror case is follow-N1). This task is research: it records what is
established, probes what isn't, and brings the options to the
maintainer. The fix is Task 33. It commits no test.

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
- [ ] `docs/platform-notes/systemd-journal.md` records the facts above
      with their citations, the systemd versions and the method, and no
      host details.
- [ ] The probe settles what reading didn't, and the note says so for
      each:
      - the flush code of 249 and 259 (journald-server.c): that /run
        is removed only when every entry was copied, as on 255;
      - that the three releases' packaged systemd matches the upstream
        tags for the functions cited (the distribution's patch level);
      - a run under writes, on each release, that tries to refute the
        link order: a reader that never finds an entry in a value's
        list before the header counts it.
- [ ] The note states the one assumption no reading can settle: a
      reader's `pread` sees journald's mmap stores in program order.
      It holds on x86-64, the only architecture sb ships for.
- [ ] A design note shows, for each option in the plan's handover
      question, how a Tail page, a forward page, a Before page and a
      merged page hand over. For the recommended option it shows
      whether the forward page takes the re-read cleanly: an entry
      before the landing the seek found must be taken by the cursor's
      order.
- [ ] The options and a recommendation go to the maintainer. Task 33
      records the answer and gets its option-specific criteria then.

**Verification:**
- [ ] The probe's runs, capped, in the matrix's containers, logs kept
      outside the repository; `scripts/check.sh fast`.
- [ ] code-reviewer on the note.

**Dependencies:** Task 31
**Files:** `docs/platform-notes/systemd-journal.md`, `tasks/plan.md` (the design note, under the handover question; it lands on the main checkout, as the plan's shared-file rule requires)
**Scope:** S

## Task 33: a page and its follow give every entry once

**Description:** The fix for follow-H1, page-H1, fwd-M2 and follow-N1,
in the shape the maintainer chose after Task 32. The criteria below
are behaviour. They hold under any option, except that the first two
fixtures are set up differently for each option, as each one says.
The criteria particular to
the chosen option (what `PageEnd` holds, what the page re-reads) are
added to this task with the answer, before any code. Unfiltered
queries read no value list, so neither defect exists for them, and
they stay out of the change.

**Acceptance criteria:**
- [ ] The maintainer's decision is recorded in journal.md, and its
      "Starting a follow" matches the code. "0 lost, 0 doubled" holds
      as stated, with no new Difference.
- [ ] An entry appended and linked during the page. The fixture is
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
- [ ] Two lists read at different moments, set up the same way: a
      query of two values. At the first value's lookup an entry isn't
      linked into that list yet; at the second's, a later entry is.
      The header counts both only after the page's first header read:
      raised through the seam under (a), between the page and the
      follow under (b). Each entry is delivered once, though one is
      older than an entry the page showed.
- [ ] The mirror case (follow-N1): a header that counts 3 while a
      value's list holds 2. The third entry is delivered once, by the
      page or by the follow.
- [ ] A page stopped by a limit, and a Before page: the follow starts
      at the journal's end as today and repeats nothing the page
      showed.
- [ ] A Tail page over an archived file and an active one, unfiltered
      and filtered, then a follow: its first batch is empty. An
      archived file's last entry that journald was cut off from
      linking (Difference 12) doesn't come again.
- [ ] A first batch cut by a small `Limits` (`Batch::more`): the later
      drains repeat nothing the page showed and lose nothing.
- [ ] A page that read nothing from a file: what is written to that
      file afterwards is delivered.
- [ ] An unfiltered page and its follow are unchanged: the existing
      tests pass as they are.
- [ ] `PageEnd` stays opaque and keeps `Debug`, `Clone`, `PartialEq`,
      `Eq`, `Default` and `Send`. Its rustdoc says what it carries.
- [ ] The expected-failure entries for follow-H1, page-H1 and fwd-M2
      are removed from follow.sh and seeds.sh (the standing rule). The
      script edits here are entry removals only.

**Verification:**
- [ ] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh task`.
- [ ] follow.sh on at least one release, capped.
- [ ] code-reviewer and library-reviewer on the commit.

**Dependencies:** Task 32, and the maintainer's answer to the handover
question
**Files:** `crates/journal/src/{walk.rs,forward.rs,follow.rs,lib.rs}` (which of them depends on the answer), `crates/journal/src/testing.rs` (the test seam), `docs/specs/journal.md`, `crates/journal/tests/{follow.sh,seeds.sh}` (entry lines only)
**Scope:** M

## Task 34: the handover across files

**Description:** The other half of the handover (follow-H1, page-H1,
fwd-M2): files that change between the page and the follow. Most of
it holds today and is pinned here; two points are new.

**Acceptance criteria:**
- [ ] A rotation between the page and the follow: the archived file
      keeps its position by inode and file id, and every entry of the
      new file is delivered. Nothing lost or doubled.
- [ ] An archived file rewritten as a copy under a new inode before
      the follow starts keeps its position. This is a code change:
      today the start needs the inode and the file id to match, and
      only a later rescan matches by file id alone.
- [ ] A flush between the page and the follow, where the page read
      the /var file: every flush copy is shown again, as Difference 13
      says of a followed file. A test pins it.
- [ ] A flush between the page and the follow, where the /var file is
      new: as the maintainer chose (the plan's question). Either
      Difference 13's wording is extended to a page and a follow on
      either side of a flush, or each shown entry's identity is
      carried and one copy of each is dropped. Under both, no entry is
      lost, and `PageEnd`'s newest entry is not raised past what the
      headers gave.
- [ ] A file the page couldn't open: the follow reads it as a new
      file, and its open error is reported once.
- [ ] A merged page over several files hands each file over on its
      own.
- [ ] journal.md lists the limit that remains under every option:
      the headers are read at different instants, so `PageEnd`'s newest
      entry can be newer than an entry of another file that the page
      didn't read. That entry is lost only if its file is gone before
      the follow opens it, which takes a flush between the two, once
      per boot.
- [ ] follow.sh gains a rotation and a flush between the page and the
      follow, next to its two "between" scenarios.

**Verification:**
- [ ] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh task`.
- [ ] follow.sh on three releases, capped.
- [ ] code-reviewer and library-reviewer on the commit.

**Dependencies:** Task 33, and the maintainer's answer to the flush
question
**Files:** `crates/journal/src/{follow.rs,lib.rs}`, `docs/specs/journal.md`, `crates/journal/tests/follow.sh`
**Scope:** M

## Task 35: stress.sh checks the handover under writes

**Description:** The crafted tests of Tasks 33 and 34 place each race
by hand. stress.sh is where the races happen for real, and today it
checks only that each page taken during the run is a gap-free part of
journalctl's final view. This adds the handover, as the matrix
evidence for follow-H1 (the same defect as page-H1 and fwd-M2). A
journalctl run at the same moment would race the page, so the oracle
is journalctl's final view.

**Acceptance criteria:**
- [ ] The oracle, per writer: every entry a page shows is in
      journalctl's final view, with no gap; and the page plus its
      follow is the final view from the page's oldest entry on, each
      entry once and in order. Only the listed Differences are allowed
      for (2, 12 and 13).
- [ ] While the writers and the rotations run, stress.sh repeatedly
      takes a filtered page and follows from its end, and checks each
      against that oracle.
- [ ] The run prints two counts: the handovers checked, and the
      handovers in which the file's entry count moved while the page
      was read. The second comes from a `testing` accessor the tool
      prints with its stats, since `PageEnd` is opaque.
- [ ] Both counts have a floor, so a run can't pass without meeting
      the race: the second is above zero on each release. So that a
      quiet run doesn't fail with no defect, stress.sh repeats
      handovers, up to a stated cap, until the count has moved in one;
      a run that reaches the cap without one fails.
- [ ] The check is shown to fail once on a broken input: a follow
      started one entry early, and one started one entry late.
- [ ] No expected-failure entry for follow-H1, page-H1 or fwd-M2 is
      left in stress.sh.

**Verification:**
- [ ] stress.sh on three releases, capped; shellcheck;
      `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 34
**Files:** `crates/journal/tests/{stress.sh,checks.sh}`, `crates/journal/src/testing/tools.rs`, `crates/journal/examples/journal.rs`, `docs/specs/journal.md` (the testing strategy's stress line)
**Scope:** M

## Task 36: a page can always be paged past

**Description:** A page whose chosen entries all fail to decode has no
cursor to continue from, and `more_newer` stays set at a tail whose
newest entry can't be shown, so a viewer never reaches the follow
(page-M1, page-M2; confirmed).

**Acceptance criteria:**
- [ ] `Page` exposes the cursors of the oldest and newest entries it
      chose, shown or not: `Page::oldest_chosen` and
      `Page::newest_chosen`, each an `Option<Cursor>` (decision 10).
      `Before` and `After` from them step past, in both layouts.
- [ ] `After(newest_chosen)` at a tail whose newest entry can't be
      shown returns `more_newer == false`.
- [ ] The tests at walk.rs:2111-2125 pin the old behaviour. They are
      re-targeted to the new one, and the change is acknowledged
      through Task 2's file.
- [ ] journal.md's API section has the new fields; sb-host re-exports
      them.
- [ ] Any expected-failure entry for page-M1 or page-M2 is removed (the
      standing rule).

**Verification:**
- [ ] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh task`.
- [ ] compare.sh on at least one release, capped.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 35
**Files:** `crates/journal/src/{lib.rs,walk.rs}`, `crates/host/src/journal.rs`, `docs/specs/journal.md`, `scripts/floor-acknowledged.txt`
**Scope:** M

## Task 37: a forward page on the cursor's own boot

**Description:** A single-boot file of the cursor's own boot falls back
to a realtime seek that can land before the cursor, so `After` returns
entries before the cursor and the cursor's own entry, and paging one at
a time loops for ever (fwd-M1; confirmed on all three releases).

**Acceptance criteria:**
- [ ] The task first records what journalctl shows for the reviewer's
      two-file fixture on 22.04, 24.04 and 26.04.
- [ ] `boot_starts` changes (decision 11): a file that holds the
      cursor's boot seeks per boot. On 22.04, 24.04 and 26.04,
      `After(e)` on that fixture returns only entries after the cursor,
      and paging one at a time ends. In both layouts.
- [ ] 22.04 gets a listed Difference only if journalctl 249 shows
      something else on the fixture. A difference on 24.04 or 26.04
      stops for the maintainer.
- [ ] The property probe's sixth mode is a test and passes, with the
      five that passed before.
- [ ] fwd-L3: the `_BOOT_ID` value list read per page is capped, with
      a fall back to the plain seek.
- [ ] Any expected-failure entry for fwd-M1 is removed (the standing
      rule).
- [ ] The docs are corrected: forward.rs:28-32 and 81-86; journal.md's
      "sb doesn't emulate that step" passage and its "259's seek is
      kept ... one of a single boot, the cursor's" sentence.

**Verification:**
- [ ] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] compare.sh on 22.04 and on one of 24.04 and 26.04, capped.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 36
**Files:** `crates/journal/src/{forward.rs,file.rs}`, `crates/journal/tests/checks.sh` (the fixture's row), `docs/specs/journal.md`
**Scope:** M

## Task 38: forward walks over damaged entry arrays

**Description:** A forward walk loses every array after one bogus item,
where the backward walk goes on (file-M1; confirmed). The file layer's
smaller defects go with it (file-L2, file-L3, file-L4, page-N1).

**Acceptance criteria:**
- [ ] A forward walk goes on after an out-of-order array, as the
      backward walk does; `After` and the tail agree. Each direction
      has a test, in both layouts.
- [ ] Deleting the backward walk's reset now fails a test.
- [ ] file-L2: an entry array with no capacity is refused, as systemd's
      `check_object` does; the bisect and the walk agree. In both
      layouts: a compact array computes its capacity differently.
- [ ] file-L3: an I/O error in the arena re-check is reported as I/O,
      not as corruption, in both layouts.
- [ ] file-L4: the header read retries `EINTR`.
- [ ] Any expected-failure entry for file-M1 is removed (the standing
      rule).
- [ ] page-N1: the entry-array chain walk at `EntriesRev::new` is
      shown bounded by a test, or bounded.

**Verification:**
- [ ] `cargo test -p sb-journal`; `scripts/check.sh task`.
- [ ] The two reset mutants (the forward and the backward walk's), run
      by name with `cargo mutants --file crates/journal/src/file.rs
      --re <site>`; both caught, recorded in the commit body.
- [ ] compare.sh on at least one release, capped.
- [ ] code-reviewer and security-auditor on the commit.

**Dependencies:** Task 37
**Files:** `crates/journal/src/file.rs`, `crates/journal/src/testing.rs`
**Scope:** M

## Task 39: Low sweep: journal paging and API

**Description:** The remaining Lows of paging, the public API and
forward paging.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - page-L1 **fix**: limit notices sit outside the ten-error cap.
      - page-L2 **fix**: `omitted` counts distinct errors.
      - page-L3 **fix**: `decode` doesn't reserve by the candidate
        count.
      - page-L4 **fix** (document): `PageEnd` is the journal's end,
        and a follow from a `Before` page skips entries. It is
        documented, not renamed (decision 10).
      - page-L5 **fix**: a helper builds a unit query from the
        journal's own release and source:
        `Journal::unit_query(&UnitName, Option<Uid>)`. `Query::unit`
        stays (decision 10).
      - page-L6 **proposed-decline**: the subdirectory name rule only
        matters for an imported directory; record it in the spec.
      - page-L7 **fix**: the spec's `Record` directory text, "0
        doubled" and "ten distinct".
      - page-L9 **fix** for the additive parts (`PartialEq` on
        `Record`, a cursor with one trailing newline parsed);
        **proposed-decline** for the rest (`Query::and` consuming on
        error, `BadCursor`'s place, constructors outside `testing`):
        API changes with no consumer yet, for the tui spec.
      - fwd-L4 **fix**: per-boot starts are strictly after by
        `Cursor::order`, with a tie test.
      - fwd-L5 **fix** (document): the repeat across a flush is listed
        for `After` too; the behaviour stays journalctl's.
      - docs-L5 **fix**: `UnitRule::Pid1`'s rustdoc agrees with the
        platform note.

**Verification:**
- [ ] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh task`.
- [ ] compare.sh on at least one release, capped.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 38
**Files:** `crates/journal/src/{lib.rs,walk.rs,forward.rs,rules.rs}`, `docs/specs/journal.md`
**Scope:** M

## Task 40: Low sweep: the journal follow

**Description:** The follow's remaining Lows. Each can happen on a
server with a busy or rotating journal, so all are fixes.

**Acceptance criteria:**
- [ ] Each id is fixed with a test:
      - follow-L1 **fix**: a waited read is retried without waiting
        for the next inotify event. A timed retry changes the wake
        protocol and sb-host's `FollowWait`; if the fix needs that, it
        leaves the sweep as its own task before Checkpoint B, with the
        spec's wake protocol amended.
      - follow-L2 **fix**: a file opened mid-follow doesn't lose its
        leading entries. If the fix is more than a bounded re-read,
        it leaves the sweep as its own task before Checkpoint B, and
        its cost goes to the maintainer.
      - follow-L3 **fix**: a follow that lost every watch reports it.
      - follow-L4 **fix** (document): inotify exhaustion fails the
        follow with an error that names the limit; the viewer's
        wording is carried to the tui spec.
      - follow-L5 **fix** (document): the first batch's memory when
        resuming from an old or default `PageEnd`.
      - follow-L6 **fix**: one directory listing per rescan; removed
        watches leave the map.
      - follow-L7 **fix**: the 1 s deadlines are widened and assert
        order; a host test covers `FollowWait` across a rescan.
- [ ] No proposed declines.

**Verification:**
- [ ] `cargo test -p sb-journal -p sb-host`; `scripts/check.sh task`.
- [ ] follow.sh on at least one release, capped.
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 39
**Files:** `crates/journal/src/{follow.rs,lib.rs}`, `crates/host/src/journal.rs`, `crates/host/tests/journal.rs`, `docs/specs/journal.md`
**Scope:** M

## Task 41: Low sweep: journal test evidence

**Description:** The remaining Lows of the test-evidence review.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - tests-L4 **fix**: compare.sh's live row stops journald before
        its two reads, as compat.sh does, so a write between them can't
        fail it.
      - tests-L8 **fix**: Difference 18's fixtures name a cursor that
        has vanished, and a gap case with the same seqnum id is added.
      - tests-L9 **fix**: Difference 15 has a test where the two walks
        would diverge.
      - tests-L10 **fix** for non-zero xor hashes, two boot ids and a
        realistic bucket count in the crafted builder;
        **proposed-decline** for TAG objects and sealing: a sealed
        journal exists on a server only after an admin ran
        `journalctl --setup-keys`, which Saltbox never does, and the
        spec lists sealed files as unverified. No sealed seed is
        added.
      - tests-L12 **fix**: compat.sh leaves no root-owned files in
        `target/journal-compat`.

**Verification:**
- [ ] `cargo test -p sb-journal`; compare.sh and compat.sh on at least
      one release, capped; shellcheck; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 40
**Files:** `crates/journal/tests/{compare.sh,compat.sh,checks.sh}`, `crates/journal/src/testing.rs`, `crates/journal/src/walk.rs` (tests), `scripts/floor-acknowledged.txt` (the re-targeted fixtures)
**Scope:** M

## Checkpoint B: reader
- [ ] The matrix (compare, compat, follow, stress) is green on 22.04,
      24.04 and 26.04 with no expected failure left, and the fuzz
      harness passes, on the final tree.
- [ ] stress.sh's handover check ran above both its floors on each
      release (Task 35): every page and its follow gave journalctl's
      entries once.
- [ ] Size, startup and coverage are re-measured as `check.sh` runs,
      and the ratchet rows updated on the main checkout, with a ratchet
      ADR if a value passes its tolerance.
- [ ] The new API is reviewed: decision 10's names in place, and what
      the handover changed in rustdoc or types (Tasks 33 and 34).
- [ ] The declines of Tasks 39-41 are confirmed or overruled.
- [ ] Review with the maintainer.

## Task 42: YAML: the corpus and the options

**Description:** Decision 3. saphyr-parser refuses multi-line flow
layouts and quoted scalars that libyaml, PyYAML and yaml.v3 accept, and
accepts two tab layouts Ansible rejects (config-H1, config-L1;
confirmed with about 200 snippets). This task builds the standing
differential corpus and the options, and ends at the maintainer's
choice. Lane 3 then waits for the answer: Task 43 is next and needs it.
So this task runs early, while lanes 1 and 2 hold the two worktrees,
and the answer is in before lane 3's code starts.

**Acceptance criteria:**
- [ ] A checked-in corpus of layouts, each with what libyaml accepts
      and, for an accepted case, the resolved value of each scalar, so
      a parser change can't alter scalar resolution unnoticed (the
      acceptance and parity-table parts of config-T1):
      the review's table, the tab-only line, the tab-indented top
      level, and a UTF-16 file (config-L10).
- [ ] The expected results are generated once from libyaml through
      PyYAML, by a checked-in generator. Its versions and method go in
      a platform note. The generator passes ruff, `ruff format
      --check`, mypy and Bandit.
- [ ] A test runs sb's parser over the corpus and asserts the exact
      list of mismatches, checked in beside it. Today's list is the
      finding.
- [ ] The options are written up with pros and cons, each run against
      the corpus where it can be: a fix upstream, a pure-Rust
      libyaml-compatible parser used for acceptance, or a pre-pass.
      Listing a difference isn't an option beside them (decision 3):
      it is only for a residual case the chosen fix can't cover.
      Maintenance, adoption, licence and size
      are stated for any new crate.
- [ ] The maintainer chooses.

**Verification:**
- [ ] `cargo test -p sb-config`; the Python checks on the generator;
      `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Checkpoint A
**Files:** `crates/config/tests/` (the corpus, its mismatch list and its test), `scripts/` (the generator), `docs/platform-notes/` (a YAML acceptance note)
**Scope:** M

## Task 43: sb accepts the YAML Ansible accepts

**Description:** Implement the option chosen in Task 42 (config-H1,
config-L1).

**Acceptance criteria:**
- [ ] The corpus's mismatch list is empty, or holds only entries the
      maintainer accepted, each in the config spec's Differences list.
- [ ] validate-config and cli-core's inventory read agree on every
      corpus case; `appdata_path` no longer falls back to `/opt` on a
      file Ansible reads.
- [ ] An ADR records the choice. If a dependency is added or changed:
      `Cargo.toml`, `deny.toml`, the config spec's Tech Stack and the
      capability map's stack row are updated, and the production
      binary's size is measured against its ratchet (ADR-004 and
      ADR-009 are the precedents).
- [ ] Positions, duplicate-key reports and secret hiding are unchanged
      on the existing tests.
- [ ] The corpus's resolved values are unchanged for every case that
      was accepted before (the parity-table part of config-T1).

**Verification:**
- [ ] `cargo test -p sb-config -p sb-cli-core -p sb`; the corpus;
      `cargo deny --manifest-path crates/sb/Cargo.toml check`;
      `scripts/check.sh full` (the size).
- [ ] library-reviewer, security-auditor and code-reviewer on the commit.

**Dependencies:** Task 42 and the maintainer's answer. Lands alone on
`Cargo.toml`, `Cargo.lock` and `deny.toml` (the plan, "Lanes and shared
files").
**Files:** `crates/config/src/parse.rs`, `crates/cli-core/src/config.rs`, `Cargo.toml` and `deny.toml` (if a dependency changes), `docs/specs/config.md`, `docs/specs/capability-map.md`, `docs/decisions/` (a new ADR)
**Scope:** M

## Task 44: config files can't exhaust memory or the stack

**Description:** A file under 1 MiB can OOM-kill or crash sb three
ways, one of them at startup for every command (config-M1; confirmed).
The files are group-writable and sb runs as root.

**Acceptance criteria:**
- [ ] Alias-expanded size is capped; the review's 870 KB file is
      refused with a clear issue. The cap refuses a file Ansible would
      accept, so the config spec's Differences list gets an entry with
      the cap's value.
- [ ] A duplicate key lists a few earlier positions, then a count; the
      150 KB case stays small.
- [ ] cli-core's inventory read has the same depth and size guards as
      validate-config; 20,000 nesting levels don't overflow the stack.
- [ ] Each case (the memory part of config-T1) runs as a test of the
      `sb` binary under an
      address-space limit, set by starting it through `prlimit --as`
      (util-linux, already a required tool), not through `pre_exec`.
- [ ] `sb version` with the alias file as the inventory ends normally
      under that limit: the startup read doesn't take sb down.

**Verification:**
- [ ] `cargo test -p sb-config -p sb-cli-core -p sb`; `scripts/check.sh task`.
- [ ] security-auditor and code-reviewer on the commit.

**Dependencies:** Task 43
**Files:** `crates/config/src/parse.rs`, `crates/cli-core/src/config.rs`, `crates/sb/tests/` (a limits test file), `docs/specs/config.md`, `docs/specs/cli-core.md` (the inventory-read paragraph only)
**Scope:** M

## Task 45: proxies as Go reads them

**Description:** reqwest's system proxy honours `ALL_PROXY`, which Go
ignores: a socks proxy breaks both credential checks, and an http one
routes credentials through it silently (config-M2; confirmed).

**Acceptance criteria:**
- [ ] The client reads `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`, and
      their lowercase forms as the maintainer chose, in Go's order.
      `ALL_PROXY` and `all_proxy` are ignored. Each has a behavioural test
      of the system-proxy path (the proxy part of config-T1).
- [ ] config.md's HTTP paragraph and its proxy Difference say exactly
      which variables are read.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 44
**Files:** `crates/config/src/http.rs`, `docs/specs/config.md`
**Scope:** S

## Task 46: secrets are hidden wherever their key sits, and in `Debug`

**Description:** A value under a secret key name is echoed when the key
sits in a misplaced structure, hiding covers only verbatim echoes, and
derived `Debug` prints secrets (config-L2, config-L3, config-L4).
Closes the backlog entry "config: `Node` and `Value` with a redacting
`Debug`".

**Acceptance criteria:**
- [ ] A value under a secret key name is hidden wherever the key sits:
      the review's `user:` list case has a test.
- [ ] A secret a service echoes percent-encoded or JSON-escaped is
      hidden too (config-L3).
- [ ] `tree::Node`, `Data` and `scalar::Value` don't print secret
      values in `Debug`, and `cloudflare::Check`'s `Debug` leaves out
      its HTTP client; a known-secret test formats each.

**Verification:**
- [ ] `cargo test -p sb-config`; `scripts/check.sh task`.
- [ ] security-auditor and code-reviewer on the commit.

**Dependencies:** Task 45
**Files:** `crates/config/src/{rules.rs,secret.rs,tree.rs,scalar.rs,cloudflare.rs}`
**Scope:** M

## Task 47: Low sweep: config

**Description:** config's remaining Lows. config-L11's fix is in sb's
`main.rs`, so it is in Task 52.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - config-L5 **fix**: a User-Agent is sent, as the maintainer
        chose (new on the wire).
      - config-L6 **fix**: a schema pattern's `\d`, `\w` and `\s`
        mean what they mean in Go, or such a pattern is refused at
        load, as the maintainer chose. One of the two, with a test on
        Arabic digits.
      - config-L7 **fix**: `Retry-After` is read when
        `Retry-After-Ms` can't be parsed.
      - config-L8 **fix**: a schema value of the wrong type
        (`required: "true"`, `min_length: "1"`, `description: 5`,
        `pattern: 5`) is refused at load.
      - config-L9 **fix**: an empty key is shown as `""` in an
        issue's path, by the same join the parser uses.
      - config-L10 **fix**: UTF-16 as Task 42's corpus says libyaml
        reads it. If Task 43's parser can't, **proposed-decline** as a
        listed Difference (no Saltbox file is UTF-16).

**Verification:**
- [ ] `cargo test -p sb-config -p sb`; `scripts/check.sh task`.
- [ ] security-auditor, library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 46
**Files:** `crates/config/src/{http.rs,schema.rs,cloudflare.rs,rules.rs,parse.rs}`, `docs/specs/config.md` (config-L7 and config-L9 are a few lines each)
**Scope:** M

## Checkpoint Y: config
- [ ] `check.sh full`; the differential corpus passes.
- [ ] The declines of Task 47 are confirmed or overruled.
- [ ] Review with the maintainer.

## Task 48: the production binary is run by the gate

**Description:** No gate runs the production musl binary, and a cross
build drops the version variables (sb-M1, sb-M4). The cross image is
named by a mutable tag (sb-L8).

**Acceptance criteria:**
- [ ] `check.sh full` runs a smoke set against the musl production
      binary through `SB_TEST_BINARY`, set for that stage only:
      `version`, `--help`, and `docker ps` and `validate-config`
      against the fakes.
- [ ] sb-host's `systemd-coredump` user lookup test is built and run
      for the static musl target (`cross test -p sb-host`, that test
      only; the other half of sb-L7). It isn't in the smoke set: no sb
      command reaches the lookup yet. The first command that does adds
      it there.
- [ ] Startup is timed on that binary, if the maintainer agrees: the
      CONSTRAINTS Startup row names it, the `sb --help` ratchet is
      re-based, and an ADR records the old and new figures. This is
      needed whether or not a limit moves.
- [ ] `Cross.toml` passes `SB_VERSION`, `SB_GIT_COMMIT` and
      `SB_DISABLE_SELF_UPDATE` through, and names its image by digest.
      A test asserts `sb version --json` of the cross-built binary
      carries the values.

**Verification:**
- [ ] `scripts/check.sh full`; the figures recorded.
- [ ] code-reviewer on the commit.

**Dependencies:** Checkpoints D and B (Task 17 for `main.rs`, Task 31
for the workflow), and the answer to the startup question
**Files:** `scripts/check.sh`, `Cross.toml`, `crates/sb/tests/version.rs`, `crates/sb/tests/` (the smoke cases), `CONSTRAINTS.md`, `docs/decisions/` (a new ADR)
**Scope:** M

## Task 49: skipped tests are counted, and the gate runs as root

**Description:** Half of sb's integration tests return early as `ok`
without `SB_TEST_ROOT=1`, and a non-root `check.sh` run skips them
silently (sb-M3; docs-L13's "non-root skips silently"). The tests that
need a supported Ubuntu host fail elsewhere, and one host test asserts
nothing off such a host (sb-L10).

**Acceptance criteria:**
- [ ] `check.sh task` and `full` set `SB_TEST_ROOT=1` and fail when not
      run as root, unless a named opt-out variable is given.
- [ ] The suite prints how many tests skipped, for the root gate and
      for the host gate; with both open the count is zero.
- [ ] The tests that need a supported Ubuntu host say so and skip,
      counted, on another host.
- [ ] End-to-end tests cover a missing `/etc/os-release` and a missing
      `VERSION_ID`.
- [ ] `ubuntu_release_reads_the_version_id_check_support_reads` asserts
      on a fixture, so it checks something on any host.

**Verification:**
- [ ] `scripts/check.sh full` as root, and once as a normal user with
      and without the opt-out; the skip counts recorded.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 48
**Files:** `scripts/check.sh`, `crates/sb/tests/root_gate/mod.rs`, `crates/sb/tests/host_check.rs`, `crates/host/src/lib.rs`
**Scope:** M

## Task 50: overflow checks in the shipped profiles

**Description:** The production profile has overflow checks off while
the tests have them on, and sb-journal parses untrusted binary files
(sb-M5). The panic-strategy comment goes with the profiles (sb-L11).

**Acceptance criteria:**
- [ ] `overflow-checks = true` in the release and production profiles,
      as the maintainer decides.
- [ ] Size, startup and the pread-bound tests' time are measured
      before and after. The ratchet rows are updated, with a ratchet
      ADR.
- [ ] The fuzz harness is run once on a build with overflow checks on,
      and finds no panic.
- [ ] A comment on the profiles says the panic strategy must stay
      `unwind`, and why (Task 17's hook and the terminal restore).

**Verification:**
- [ ] `scripts/check.sh full`; the fuzz run, capped; the figures in the
      ADR.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 49, Task 31
**Files:** `Cargo.toml`, `CONSTRAINTS.md`, `docs/decisions/` (a new ADR)
**Scope:** S

## Task 51: startup parses once

**Description:** Each run builds the command tree twice, parses up to
three times and renders help for every command (sb-M6). The unused
`colorchoice-clap` dependency goes (sb-L5).

**Acceptance criteria:**
- [ ] Startup parses plainly first, and builds help only on the help
      and error paths. Help, errors and `--color` behave as before on
      every existing test.
- [ ] `sb --help` and `sb version` medians don't grow; the figures are
      recorded.
- [ ] `colorchoice-clap` leaves sb's manifest and the lock file.

**Verification:**
- [ ] `cargo test -p sb -p sb-cli-core`; `scripts/check.sh full` (the
      startup time).
- [ ] library-reviewer and code-reviewer on the commit.

**Dependencies:** Task 50
**Files:** `crates/sb/src/main.rs`, `crates/cli-core/src/help.rs`, `crates/sb/Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task 52: Low sweep: the sb binary

**Description:** The binary's remaining Lows, and config-L11, whose
fix is in `main.rs`.

**Acceptance criteria:**
- [ ] Each id is fixed, or its decline is confirmed:
      - sb-L1, as the maintainer chose (the plan's open question):
        the root's case recorded as a difference in the cli-core spec
        (recommended), or a missing subcommand treated as help at the
        root and for `sb docker`, with the docker spec amended.
      - sb-L2 **fix**: help to a closed or full stdout ends as the
        cli-core spec says a closed pipe does.
      - sb-L3 **fix**: a service URL override in effect shows a
        notice on screen, not only a log record.
      - sb-L4 **fix**: non-UTF-8 override values are handled one way
        (the `SB_LOG_DIR=""` part is Task 22's).
      - sb-L6 **proposed-decline**: exception E1's ignores stay until
        its expiry, as the recorded decision says (with docs-L12).
      - sb-L7 **fix**: the `getpwnam_r` rationale comment is corrected
        for static musl (Task 48 runs the lookup's test on musl).
      - sb-L9 **fix**: the version test asserts the output, not only
        the failure.
      - config-L11 **fix**: Ctrl-C ends `validate-config` promptly
        when DNS is dead; the runtime's drop doesn't wait for the
        resolver's blocking thread.
      - sb-L12 **proposed-decline**: mimalloc is a new dependency
        with C code; taken up only if Task 50's measurement shows a
        need.

**Verification:**
- [ ] `cargo test -p sb -p sb-cli-core -p sb-host`; `scripts/check.sh task`.
- [ ] code-reviewer on the commit.

**Dependencies:** Task 51
**Files:** `crates/sb/src/main.rs`, `crates/cli-core/src/help.rs`, `crates/sb/tests/{help.rs,version.rs}`, `crates/host/src/journal.rs` (one comment), `docs/specs/cli-core.md` (one line, if sb-L1 is recorded)
**Scope:** M

## Checkpoint C: complete
- [ ] `check.sh full` passes; the matrix is green on three releases and
      the fuzz harness passes.
- [ ] Size, startup and coverage are re-measured, the ratchet rows
      updated, with a ratchet ADR.
- [ ] A fresh review of the plan's whole range, one reviewer per area,
      finds no High or Medium. One it does find becomes a new numbered
      task in this plan (test first, reviewed), and that area is
      reviewed again until a pass finds none. New Lows get ids in
      tasks/findings.md and a fix or a confirmed decline.
- [ ] Every id in tasks/findings.md is fixed or has a confirmed
      decline; the counts restated by Task 6 (docs-L4) are current.
- [ ] `scripts/check-plan-ids.mjs` passes.
- [ ] Review with the maintainer, who sets CONSTRAINTS "Last reviewed"
      (docs-L16); the plan archived to `tasks/hardening/`.

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

- cli-core's `error_block` prints error text raw, and cli-core can't use
  term-ui's `DisplayText` (term-ui depends on cli-core). Check whether
  any error text can carry untrusted bytes (a child's stderr, a file
  name), and if so move the sanitizer's core below cli-core or sanitize
  at the source. Also fix the stale comment in cli-core/src/log.rs
  (about lines 184-186) that says child output is stripped with
  `console::strip_ansi_codes`; the runner uses `DisplayText` now.

- cargo-deny reports advisory-not-detected for exception E1's two
  ignores (RUSTSEC-2025-0141, RUSTSEC-2024-0320), although bincode,
  yaml-rust and syntect are still in Cargo.lock. Found in Task 11:
  syntect is in the lock only through `prototypes/proto-markdown`
  (tui-markdown), not in crates/sb's graph, which the gate checks
  (`cargo tree -p sb -i syntect` matches nothing). The ignores stay
  until the announcements renderer brings tui-markdown into sb's graph,
  or E1 is decided again at its expiry (2027-01-28).

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

- Decided in Task 6: the follow reads every header first on each
  drain (`refresh()`), which caps the guessed `object_head` reads at the
  file's length then, and an `Io` from a cut inside one drain is
  waited for once (`Wait::Entry`), like any read, before the next
  drain reads the header again
  (`a_file_cut_while_followed_reads_up_to_the_cut`).

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
