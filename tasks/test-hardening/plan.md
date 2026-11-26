# Implementation Plan: test hardening (approved 2026-11-26)

Decisions: MD1-MD4 and Q1-Q9 below (the maintainer, 2026-11-26). Tasks:
[todo.md](todo.md). Quality bar: [CONSTRAINTS.md](../../CONSTRAINTS.md).

Base: main at 48f367ea. The reviews read 07367e47. Between the two, only
`crates/sb/tests/logs.rs` changed (48f367ea), so the reviews' line numbers
hold everywhere else. In `logs.rs` they are a few lines off.

Sources:
- the four test reviews, A to D;
- the closed-reader research;
- the maintainer's decisions of 2026-11-26 (below).

## Overview

The plan fixes every finding of the four test reviews in five themed
batches, plus two decided changes:
- **The closed-reader redesign.** Piped `sb logs` now ends with the quiet 141 as soon as its reader goes away, with no new line needed.
- **Go leaves the gate.** gitleaks comes from its release binary instead of `go install`.

Each batch ends at a checkpoint: one amd64 CI rehearsal and one
`check.sh full` based on the batch's start commit (Q8, Q9).

Finding ids are the review's letter plus a number, as in the reports: A1 to A13, B1 to B12,
C1 to C12 (the reports' F1 to F12), and D1 to D16.

## Decisions applied

The maintainer, 2026-11-26:
- **MD1.** Fix every finding of the four reviews, in themed batches. A style finding is fixed, or declined with a stated reason.
- **MD2.** Piped `sb logs` ends at once with cli-core's quiet 141 when its reader goes away, even when no new line comes.
  - It watches stdout for POLLERR/POLLHUP alongside the follow, as `journalctl -f` and GNU `tail -f` do.
  - This is a spec change to `docs/specs/tui.md` (the inline fallback) and to `docs/specs/cli-core.md` (the quiet exit), attributed "(the maintainer, 2026-11-26)".
- **MD3.** The end-to-end closed-reader test owns its pipe:
  - The PTY is the terminal through `setsid --ctty`.
  - stdout is a test-owned `std::io::pipe()`.
  - The test asserts sb's own exit code.
  - It is joined by an in-process `streamed` test with a BrokenPipe writer, and by a separate "a new entry arrives" test that waits for `journal follow started`.
  - There is also a case that appends before the follow starts.
  - No `| head` in any of these tests. They replace `a_closed_reader_ends_the_stream_with_141`.
- **MD4.** Drop Go from the gate:
  - gitleaks 8.30.1 comes from its GitHub release's prebuilt binary, checked against the release's `gitleaks_8.30.1_checksums.txt`.
  - `actions/setup-go`, CONSTRAINTS.md's Go row and check.sh's `go version -m` read all go.
  - The version is read from `gitleaks version`.
  - Renovate tracks the release.

### Choices this plan makes from those decisions

These follow from the decisions and the repository's existing patterns, so
they are not open questions. Each one is weighed in its task.

1. **One skip mechanism, in sb-cli-core's `testing` module.**
   - `testing::root_tests()` keeps `root_gate::requested()`'s contract: it returns false when `SB_TEST_ROOT` is unset, and panics on any value but `1`, or on `1` without root.
   - `testing::skip(reason)` prints `skipped: <test>: <reason>` to stderr and, when the gate gives `SB_TEST_SKIPS`, appends the same line to that file. The test's name is the libtest thread's name, the same under `cargo test` and nextest.
   - `crates/sb/tests/root_gate/mod.rs` becomes a re-export, so its 311 call sites stay as they are.
   - How the gate shows skips: this helper plus check.sh's per-run summary (Q1).
2. **The gate asks for root out loud.**
   - ci.yml sets `SB_TEST_ROOT=1` before its `sudo`.
   - check.sh refuses with exit 2 when a caller set `SB_TEST_ROOT=1` and it isn't root.
   - So a CI run that lost its root fails, instead of skipping 330 tests. The gate panics when it asked for root and didn't get it. A non-root developer's run still works and lists its skips.
3. **sb-repo's isolation is one change.**
   - B2's half (the host's git config reaching `Repo`'s own git calls, and the "outside a repository" tests' assumption about what sits above `TMPDIR`) uses the same wrapper as B1.
   - So B1 and B2 are fixed together in batch 1, not split between batches 1 and 2.
4. **"Appends before the follow starts" is tested at sb-tui's journal source.**
   - Its tests send `Request::Tail`, wait for the page, append, then send `Request::Follow(true)`.
   - That pins `Follower::start`'s first batch deterministically, with no seam in sb.
   - It runs under both releases' journal rules (24.04's `NewestTail`/`Pid1` and 26.04's `NewestTailEntryMachine`/`InitScope`), because the follow miss seen on 26.04 is suspected in exactly that path.
   - The end-to-end "a new entry arrives" test then pins the inotify path.
5. **Mid-stream at sb's level is the new behaviour.**
   - The end-to-end test's main case drops the reader after the tail, once the follow has started, with no append at all, and asserts 141.
   - A second case drops the reader before Enter (bat's and uutils' pattern).
6. **The reader-gone watch lives in sb-cli-core**, beside the quiet exit it ends with.
   - It follows `FollowWait`'s pattern: rustix `poll` plus a stop `eventfd` on cli-core's `spawn::blocking`.
   - The pattern is in `crates/host/src/journal.rs:264-281`, which also explains why not tokio's `AsyncFd`.
   - It watches fd 1 only when that is a pipe, FIFO or socket, as GNU tail does (`tail.c:2527-2535`). poll reports no error on a regular file or `/dev/null`, but the check keeps the blocking thread out of runs that can't need it.
7. **gitleaks' checksum is handled as the repo's other release downloads are.**
   - The checksums file is fetched from the same release at install time, as `provision.sh` does for Node and `rootfs.sh` for the Ubuntu base. Renovate then bumps only the version.
   - The limit: this proves the archive matches its release's list, not who published it., which is what MD4 asks.

### Batch adjustments (dependencies), and why

- **B2 moves to batch 1**, with B1: same helper, same files, one review (choice 3). Batch 2's TMPDIR item is then only the check.sh comment that records the assumption (`check.sh:1048-1051`). It is updated in the same task.
- **The skip helper comes first in batch 1** (Task 1.1). The logs root gate (1.4) and the other skip sites (1.3) use it.
- **D6 joins D7 in batch 3** (Task 3.7). D6 is a timing finding, but its fix (a hung listing on a channel the test releases) is D7's fix.
- **D11 joins D5 in batch 4** (Task 4.5). It is a blind-failure finding in the same `ctrl_c` helper that D5 changes.
- **C7's PTY margins come after C2** (batch 3). They read the child's own `millis=` from the run log, which Task 3.9 adds.
- **D9 is split.** First the LEAK status is made visible (batch 3, 3.15). Then leaks fail, once A13's deliberate orphans close their output (batch 4, 4.8). Making them fail first would fail the gate on known orphans.
- **The gitleaks task (2.9) comes after 1.8**, because both edit ci.yml's install and `check.sh task` steps.

## Conventions for every task

- **Skills.** Load each phase's skill:
  - `agent-skills:test-driven-development` for each behaviour change or bug fix;
  - `agent-skills:incremental-implementation` for implementing;
  - `agent-skills:source-driven-development` for crate and tool usage (nextest, cargo-llvm-cov, portable-pty, rustix, std, tokio);
  - `agent-skills:api-and-interface-design` for Task 3.2;
  - `agent-skills:ci-cd-and-automation` for the workflow tasks;
  - `agent-skills:security-and-hardening` for Task 2.9;
  - `agent-skills:git-workflow-and-versioning` for commits.
- **Test-first.**
  - Where behaviour changes, a test fails first, and the failure is recorded in the task's notes.
  - For an oracle fix, the new oracle is shown to fail against a deliberately broken variant (a local revert or a planted mutant, never committed). This is the "can fail" proof each batch-3 task names.
- **Commits.** One commit per task, unless the task says otherwise.
  - Subjects are Conventional Commits (`test(...)`, `fix(...)`, `ci:`, `build:`, `docs(...)`).
  - The body has a `Library-check:` line when Rust changes and a `Review:` line when anything outside `docs/` and `tasks/` changes.
  - Each removed assertion or test, and each changed gate-script line, has its reason in the body (the floor guard reports them for the reviewer).
- **Reviews (CLAUDE.md tiers).**
  - Every task outside `docs/` and `tasks/` gets one `agent-skills:code-reviewer` run on its diff, in parallel with `scripts/check.sh task` (Q8).
  - Tasks touching terminal, UI, input, process or I/O code also get the `library-reviewer`. That includes the test harnesses for PTYs, pipes and child processes, in sb, exec, cli-core, term-ui, tui, docker, host, motd and release.
  - Task 2.9 (a downloaded binary in the gate) also gets `agent-skills:security-auditor`.
  - Each brief gives the range and the goal, says to edit nothing, says to run sb only with `SB_LOG_DIR` in a scratch directory, and carries the Public repository rules.
- **Floor guard.** Each task names the blocking rules it may trigger. Approvals are asked for (AskUserQuestion) when the finding appears, never written ahead.
- **Public repository rules.**
  - Fixtures and expected outputs are invented. That covers the configparser reference in 2.4 (an invented corpus), the gitleaks test archive in 2.9, and the matrix scripts' output.
  - No host values, and no session narration in commits or comments.
- **Running things.**
  - Check `uptime` first.
  - Every build or test run goes in the capped scope (`CAP` below), with `SB_LOG_DIR` in a scratch directory for any run of sb.
  - No sysctl, `/proc/sys` or other host-wide change.
  - Stop runs by exact PID.
  - Never read `/srv`. Saltbox facts (its pinned Python) come from public GitHub.
- **Python.** Tasks that change Python (`scripts/ci-rehearsal/job.py` and its tests in 1.8, the configparser generator in 2.4) run these on every changed file:
  - `ruff check`, `ruff format --check`, `mypy --check-untyped-defs` and `bandit`, from an isolated tool environment outside the repository;
  - the file's own tests (`python3 -m unittest scripts/ci-rehearsal/test_job.py`).

  The repository has no Python linter configuration of its own.

```sh
# The capped scope: one run at a time, 2400%; two at once, 1200% each.
CAP='systemd-run --scope -q -p AllowedCPUs=0-47 -p CPUWeight=1 -p CPUQuota=2400%
     -p MemoryMax=32G -p MemorySwapMax=0 --setenv=PATH="$PATH" nice -n 10 ionice -c3'
S=<a scratch directory outside the repository>
# Focused tests while iterating (as root, the gate's way):
$CAP env SB_LOG_DIR=$S/sb-logs SB_TEST_ROOT=1 cargo test -p <crate> --test <binary> [<filter>]
# Before each commit (Q8):
$CAP scripts/check.sh task <task base>
# At each batch's end (Q8, Q9), with the rehearsal:
$CAP scripts/check.sh full <batch start>
```

### Checkpoint at the end of each batch

- **The rehearsal:** `$CAP scripts/ci-rehearsal.sh --arch amd64`, once, on the batch's last commit (all three amd64 releases).
- **The gate:** `$CAP scripts/check.sh full <batch start>`, once. It passes only if its log ends `check full: all passed`.
  - Its base is the batch's start commit, so the batch's whole diff is judged once: with base HEAD on a clean tree, changed-line coverage, mutation and the commit-message check would have nothing to check (D1's own finding; Q9).
- **The journal matrix and fuzz harness**, by hand and capped, after a batch that changed `crates/journal/`: batches 3 (Task 3.8) and 4 (Task 4.9).
- **Before saying "done":** walk the standing agreements. Clean up worktrees and images, read every added fixture and snapshot against the Public repository rules, and stop any orphaned processes.

---

## Batch 1: fix first

### Task 1.1: one skip mechanism, and the gate's report of skips

**Findings:**
- A3 (the mechanism; `root_gate/mod.rs:8-26`);
- C5 (the mechanism);
- D8 (the mechanism; `check.sh` `sandbox_argv`);
- B11 (the mechanism).

**What:**
- Add `root_tests()` and `skip(reason)` to `crates/cli-core/src/testing.rs` (choice 1).
- `crates/sb/tests/root_gate/mod.rs` re-exports `root_tests()` as `requested()`.
- `check.sh` gives each test run (coverage, doc tests, release build, mutation baseline) `SB_TEST_SKIPS` under the run's `TMPDIR`. After each run it prints the skipped tests, one line each, with a count.

**Acceptance:**
- [ ] `root_tests()`:
  - returns false with `SB_TEST_ROOT` unset;
  - panics on `SB_TEST_ROOT=yes`;
  - panics on `SB_TEST_ROOT=1` when not root;
  - returns true as root.
  - Each is proven by a child re-run of the test binary with its environment set, as cli-core's env tests do (no `set_var`).
- [ ] `skip()` writes `skipped: <test name>: <reason>`:
  - to stderr;
  - to `$SB_TEST_SKIPS` when set, from parallel threads without losing a line. Proven with two threads, under `cargo test` and nextest.
- [ ] check.sh prints `skipped: N` and the lines after each test step. With no skip it prints `skipped: none`. A stubbed `checkRun` case in `floor-guard.test.mjs` proves both.

**Test-first:** the helper's tests fail before the helper exists, and the `checkRun` case fails before check.sh prints the summary.

**Verify:**
- `cargo test -p sb-cli-core --lib testing` and `node --test scripts/floor-guard.test.mjs` (focused);
- `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (cli-core; child processes in the tests).

**Floor guard:** none blocking expected. The check.sh lines changed are reported.

**Files:**
- `crates/cli-core/src/testing.rs`
- `crates/sb/tests/root_gate/mod.rs`
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`

**Scope:** M.

**Dependencies:** none.

### Task 1.2: the gate refuses a root run it asked for and didn't get

**Findings:**
- D8 (`check.sh` sets `SB_TEST_ROOT` only when root, so a CI run that lost its root skips silently);
- C5 ("a gate run that lost its root shows the same green").

**What:**
- ci.yml's `check.sh task` step sets `SB_TEST_ROOT=1` and keeps it through `sudo` (`--preserve-env`).
- check.sh exits 2 with `check: SB_TEST_ROOT=1 asks for the root tests, but this run isn't root` when the caller set it without root. As root it exports `SB_TEST_ROOT=1` as now.
- The CI rehearsal follows, because it runs ci.yml's step as written.

**Acceptance:**
- [ ] A `checkRun` case: non-root with `SB_TEST_ROOT=1` gives exit 2 and the message, before any stage.
- [ ] A `checkRun` case: non-root without it runs, and the skip summary lists the root tests.
- [ ] The rehearsal's `task` job shows `SB_TEST_ROOT=1` reaching check.sh. Checked by its log line at this batch's checkpoint.
- [ ] ci.yml's comment and CONSTRAINTS.md's Tests row say CI asks for the root tests explicitly.

**Test-first:** the first `checkRun` case fails against today's check.sh.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>` (it runs the workflow linters, because `.github/` changed).

**Reviews:** code-reviewer.

**Floor guard:**
- **Possible:** a `threshold-changed` on the Tests row of `CONSTRAINTS.md`, if the reworded row changes a number in it. Otherwise the row change is clean.
- **Reported only:** the changed gate lines.

**Files:**
- `.github/workflows/ci.yml`
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`
- `CONSTRAINTS.md`

**Scope:** S.

**Dependencies:** 1.1.

### Task 1.3: every other skip goes through the helper, and the floor guard reports a new hand-rolled skip

**Findings:**
- C5:
  - `crates/cli-core/src/log.rs:724-728`;
  - `crates/cli-core/src/run_lock.rs:707-709`.
- D8:
  - `crates/config/tests/providers.rs:354`;
  - `crates/config/src/facts/apply/tests.rs:151`;
  - `crates/python-env/tests/reconcile.rs:161` and `:837`;
  - `crates/python-env/tests/run_log.rs:85`;
  - `crates/python-env/src/verify.rs:1326`.
- B11:
  - `crates/repo/tests/run_log.rs:56-58`;
  - `crates/docker/tests/live.rs:13-16`.
- A3: `crates/sb/tests/hangup.rs:306-317` (the systemd skip goes through `skip()`).
- D8's floor-guard half: "the floor guard blocks a new `#[ignore]` but not a new early `return`".
- The live opt-ins (`python-env/tests/live.rs:25`) use `skip()` too.

**What:**
- Each root check becomes `if !sb_cli_core::testing::root_tests() { return; }`.
- Each skip for a missing condition becomes `testing::skip("…"); return;`.
- config and python-env gain `sb-cli-core` with `testing` as a dev-dependency where they lack it.
- The floor guard reports, without blocking, an added early `return` in Rust test code whose condition reads `SB_TEST_ROOT`, `effective_uid`, `geteuid` or `is_root`, outside `testing.rs`. A new rule `skip-added` joins the `REPORTED` list, with its test cases.
- CONSTRAINTS.md's "No test made easier" bullet names runtime skips: they go through the helper, and the guard reports any other.

**Acceptance:**
- [ ] `grep` finds no uid or `SB_TEST_ROOT` early return outside the helper.
- [ ] A non-root `cargo test` of each changed crate lists its skips through the helper. As root with `SB_TEST_ROOT=1`, none are listed but the systemd test (Q2) and the opt-in live tests.
- [ ] floor-guard tests:
  - `skip-added` is reported, and doesn't fail the run, for a planted `if geteuid() != 0 { return; }`;
  - the helper's own form is clean.

**Test-first:** the guard's new cases fail before the rule exists.

**Verify:**
- focused `cargo test` of cli-core, config, python-env, repo and docker, as root and as `nobody` (the `setpriv` way the sb tests use; output to a scratch directory);
- `node --test scripts/floor-guard.test.mjs`;
- `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (cli-core, docker).

**Floor guard:**
- **Blocking:** `rule-changed` on CONSTRAINTS.md's Floor bullet "No test made easier". It is a tightening, but the guard flags every word change in a Floor bullet. Ask then.
- **Reported:** the changed `floor-guard.mjs` lines.
- If the task is too large for one review, split it: 1.3a moves the crates' skip sites to the helper, 1.3b adds the guard rule and the CONSTRAINTS.md bullet.

**Files:**
- the nine test files above;
- two `Cargo.toml` (config and python-env dev-dependencies);
- `scripts/floor-guard.mjs`
- `scripts/floor-guard.test.mjs`
- `CONSTRAINTS.md`

**Scope:** L, so split into 1.3a and 1.3b as above.

**Dependencies:** 1.1.

### Task 1.4: `sb logs` tests are root-gated, and isolation.rs catches the next one

**Findings:**
- A1: `crates/sb/tests/logs.rs` (19 tests, no `root_gate`), `crates/sb/tests/logs_containers/mod.rs` (6 tests), and the harness at `logs_pty/mod.rs:95-110`;
- A1's second fix: extend `isolation.rs`.

**What:**
- Each test that runs a root-needing command starts with `if !root_gate::requested() { return; }`. That covers `logs.rs` and `logs_containers`, but not `help_is_pinned` if `--help` doesn't relaunch.
- `isolation.rs` gains a check: a test file under `crates/sb/tests/` that runs one of sb's root-needing subcommands calls `root_gate::requested()`.
  - The list of root-needing subcommands is read from `Command::needs_root`. Through a `testing` export if sb has one; otherwise a list in the test with its source line cited, which a unit test in sb keeps equal to `needs_root`.

**Acceptance:**
- [ ] As a non-root user, `cargo test -p sb --test logs` runs no sb as root. The tests list their skips, and no `sudo` is started.
  - Proven by a stand-in `sudo` first on `PATH` that records any call and fails. It is never the host's sudo.
- [ ] isolation.rs fails on a planted test file that runs `sb logs` with no root gate, and passes on the tree.

**Test-first:** with the stand-in sudo, today's `logs` binary records sudo calls (red). After the fix, none.

**Verify:**
- `cargo test -p sb --test logs --test isolation`, as root with `SB_TEST_ROOT=1`, and as `nobody` with the stand-in sudo;
- `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (PTY harness).

**Floor guard:** none expected.

**Files:**
- `crates/sb/tests/logs.rs`
- `crates/sb/tests/logs_containers/mod.rs`
- `crates/sb/tests/isolation.rs`
- possibly `crates/sb/src/main.rs` (a `testing` export of the root list)

**Scope:** M.

**Dependencies:** 1.1.

### Task 1.5: the systemd hangup test runs without being remembered

**Findings:** A3, last bullet (`hangup.rs:306-317`, `a_session_scope_stop_takes_the_grace`: never run in the gate or CI; only by hand through `crates/sb/tests/hangup-systemd.sh`).

**What (Q2: at `full`):** `check.sh full` runs `crates/sb/tests/hangup-systemd.sh` when `crates/` or `scripts/` changed. The script already needs root and Docker, which `full` has, and caps its container (`--cpus 2 --memory 2g`).

**Acceptance:**
- [ ] `check.sh full` runs it, and fails on its failure. The last line of its output is in the gate's log.
- [ ] `check.sh full` skips it with a printed line on a host without Docker or another architecture, like the binary-size checks.
- [ ] CONSTRAINTS.md's Tests row says where it runs.

**Test-first:** a `checkRun` case with a stub that fails the script makes `full` fail (red before the step exists).

**Verify:** a real `check.sh full <base>` (it runs the container); `node --test scripts/floor-guard.test.mjs`.

**Reviews:** code-reviewer.

**Floor guard:** possibly `threshold-changed` on the Tests row, if its numbers change. Gate lines are reported.

**Files:**
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`
- `CONSTRAINTS.md`

**Scope:** S.

**Dependencies:** 1.1, Q2.

### Task 1.6: sb-repo's tests ignore an inherited `GIT_DIR` and the host's git configuration

**Findings:**
- B1: `crates/repo/tests/update.rs:15-49`, `run_log.rs:19-45`, `file_at.rs:12-30`; the code under test is `crates/repo/src/git.rs:24`.
- B2, moved here (choice 3):
  - every `Repo` call in `update.rs` (`:470`, `:643`, `:1256`);
  - the "outside a repository" tests `:303`, `:1089`, `:1636`;
  - the assumption recorded at `check.sh:1048-1051`.

**What:**
- The tests' git helpers remove `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR` and `GIT_NAMESPACE`. They set `GIT_CEILING_DIRECTORIES` to the fixture's temp directory's parent.
- `Repo` runs in these tests through a stand-in `git` first on `PATH`, the `recording_git` pattern. The stand-in does the same, and sets `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` to an empty file in the test's temp directory, then `exec`s the real git.
- Exactly one test keeps a config value on purpose: `a_checkout_git_leaves_without_an_upstream_is_refused_after_the_reset`, `:665`.
- `check.sh:1048-1051`'s comment is reworded. The tests now set their own ceiling, and no longer depend on what sits above `TMPDIR`.

**Acceptance:**
- [ ] With `GIT_DIR` pointing at a scratch repository outside the tree, all 60 `update` tests pass, and the scratch repository gains no commit and no config.
- [ ] With `GIT_CONFIG_GLOBAL` holding `branch.autoSetupMerge = false`, the three tests B2 names pass.
- [ ] With `TMPDIR` inside a scratch git repository, the "outside a repository" tests pass.

**Test-first:** B1's and B2's reproductions fail today: 55 of 60, and 3 tests. Rerun them first and record the result.

**Verify:** `cargo test -p sb-repo` under each of the three environments above, plus plain; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (process code in the tests).

**Floor guard:** none expected. The check.sh comment is reported if the guard sees it as a changed gate line.

**Files:**
- `crates/repo/tests/update.rs`
- `crates/repo/tests/run_log.rs`
- `crates/repo/tests/file_at.rs`
- possibly a shared `crates/repo/tests/common/mod.rs`
- `scripts/check.sh` (the comment)

**Scope:** M.

**Dependencies:** none.

### Task 1.7: "nothing reaches the shell" reads what the shell would read

**Findings:** C1: `crates/tui/tests/term.rs:394`, `:439`, `:461`, `:488`, `:516`, `:539` (only echo is checked; raw-mode input stays queued; `_slave` is held at `:158` and never read).

**What:**
- `Pty::assert_restored` (`term.rs:336`), after `exit()`, asks the kernel how much is queued on the slave (rustix `ioctl_fionread`, from rustix's documentation) and reads it non-blocking.
- It asserts nothing is queued, and prints the bytes if something is.

**Acceptance:**
- [ ] The six tests assert an empty slave queue.
- [ ] Each one shows the screen and the queued bytes on failure.

**Test-first ("can fail" proof):**
- Locally, without committing it, plant C1's regression in `Tui::leave` (`crates/tui/src/term.rs:468-504`): abort the event task before the 150 ms wait.
- The old oracle passes against it, and the new one fails. Record both results.

**Verify:** `cargo test -p sb-tui --test term`, then 3 copies on 1 core (C's contention run); `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (PTY, terminal).

**Floor guard:** none expected.

**Files:** `crates/tui/tests/term.rs`

**Scope:** S.

**Dependencies:** none.

### Task 1.8: CI and the rehearsal check a push's own diff

**Findings:**
- D1:
  - `.github/workflows/ci.yml:118-128` (the base is `HEAD` on a push, and the comment says "checks everything");
  - `check.sh` `mutation_check` (an empty diff gives "no changed crate code");
  - the `diff-cover --compare-branch` step;
  - `scripts/ci-rehearsal.sh:31-35` (base HEAD).

**What:**
- **ci.yml, on a push:** the base is `github.event.before` when it isn't all zeros and is an ancestor of HEAD (`git merge-base --is-ancestor`; fetch-depth is already 0). Otherwise it is HEAD, with a printed line saying why. Pull requests keep their merge base.
  - Whether the mutation passes run on every runner on a push is Q3: yes, if the batch-1 measurement fits the 120-minute job limit; otherwise mutation stays local, and ci.yml and CONSTRAINTS.md say so.
- **The rehearsal:**
  - it gives the job the clone's pre-patch commit as `event.before`, or a `--base <rev>` it is given;
  - `job.py` supplies `github.event.before` in its event payload and context;
  - its tests cover the all-zeros and non-ancestor cases.
- **check.sh:** with an empty diff, changed-line coverage and mutation print `skipped: no changed lines since <base>`, instead of passing silently.
- ci.yml's comment and CONSTRAINTS.md's sentence "In CI, ci.yml runs `task`" say what a push now checks.

**Acceptance:**
- [ ] `checkRun` cases: an empty diff prints both skip lines, and a one-line Rust change runs diff-cover and a mutation pass.
- [ ] `job.py`'s tests:
  - `github.event.before` comes from the payload;
  - a push with the all-zeros sha gets HEAD with the printed line;
  - ci.yml's step reads it as GitHub does.
- [ ] The batch-1 checkpoint rehearsal shows diff-cover and a mutation pass running on each amd64 image, over the batch's diff. Its duration is recorded for Q3.

**Test-first:** the `checkRun` empty-diff case fails today (no skip line), and `job.py`'s new test fails before `event.before` exists.

**Verify:**
- `node --test scripts/floor-guard.test.mjs`;
- `python3 -m unittest scripts/ci-rehearsal/test_job.py`;
- ruff, mypy and bandit on `job.py` and `test_job.py`;
- `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none blocking expected. The changed check.sh lines are reported.

**Files:**
- `.github/workflows/ci.yml`
- `scripts/check.sh`
- `scripts/ci-rehearsal.sh`
- `scripts/ci-rehearsal/job.py`
- `scripts/ci-rehearsal/test_job.py`
- `CONSTRAINTS.md` (the CI sentence)
- `scripts/floor-guard.test.mjs`

**Scope:** L, so split it:
- 1.8a: check.sh's skip lines and ci.yml's base;
- 1.8b: the rehearsal's base and `job.py`.

**Dependencies:** none (Q3 for the mutation part).

### Task 1.9: the gate's tests run the real `node`, so the verdicts that compute have tests

**Findings:** D2:
- `scripts/floor-guard.test.mjs:1355`: the `node` stub;
- `:1440`, `:1483`, `:1528`, `:1547`: always `{"caught":1,"missed":0,"timeout":0}`;
- the untested paths: the mutation sum and `MUTATION_MIN`, an outcomes file with no counts, the hyperfine median against `STARTUP_MAX_MS`, and `mutation_pass`'s mapping of exits 4 and 125, and of a missing `outcomes.json`, to exit 2.

**What:**
- Link the real `node` into `sys-bin`, as `git` and `bash` are. The stub forwards everything but `--version`.
- The mutants stub writes the outcomes a case asks for. A hyperfine stub writes a `startup.json` with a given median.

**Acceptance:** each case passes:
- [ ] 79.9% over two passes fails, and exactly 80% passes;
- [ ] passes are summed, not overwritten: 2 of 2 plus 0 of 1 gives 66.7%, which fails;
- [ ] an outcomes file without counts gives exit 2;
- [ ] a mutants exit of 4, a mutants exit of 125, and no `outcomes.json`, each give exit 2;
- [ ] a median of 10.01 ms fails, 10.00 ms passes, and a missing median key gives "no median".

**Test-first:** with the real node, the "every stage passes" case fails until it gets a `startup.json`. That is the stub's blind spot made visible.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none. `floor-guard.test.mjs` is not a gate script.

**Files:** `scripts/floor-guard.test.mjs`

**Scope:** M.

**Dependencies:** none.

### Task 1.10: the floor guard's rules are tested against the real check.sh and CONSTRAINTS.md

**Findings:** D10: `scripts/floor-guard.test.mjs:113-133` (`checkSh` is a hand-written copy; the same holds for `CONSTRAINTS.md`, `deny.toml` and `Cargo.toml`).

**What:** one table-driven test. In a scratch copy of the real tree, loosen in turn:
- each `*_MIN` and `*_MAX` constant of the real `scripts/check.sh`;
- each `MUTATION_TESTERS` entry;
- each numbered row of the real `CONSTRAINTS.md`;
- deny.toml's settings;
- the workspace lint levels.

For each, assert the guard reports the matching rule.

**Acceptance:**
- [ ] Every threshold found in the real files is covered. The test fails if a new constant appears that no case loosens: it counts them.
- [ ] A reshaped threshold (`readonly -A`, an array) fails the test. Shown by a local change that is not committed.

**Test-first:** the "reshaped" check fails against a planted reshaping before the count exists.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none.

**Files:** `scripts/floor-guard.test.mjs`

**Scope:** M.

**Dependencies:** none. It goes after 1.9 to avoid edit conflicts in the same file.

### Checkpoint: batch 1

- [ ] amd64 rehearsal passes; it shows `SB_TEST_ROOT=1`, the skip summary, and diff-cover and mutation over the batch's diff.
- [ ] `check.sh full <batch start>` log ends `check full: all passed` (Q9).
- [ ] Every new skip is visible; no root test skips as root.
- [ ] Q3's measurement recorded, and Q3's outcome applied.

---

## Batch 2: environment and tooling

### Task 2.1: colour variables are scrubbed in one place

**Findings:**
- A2: `crates/sb/tests/common/mod.rs:136-246` (`apply_sb_env` keeps `CLICOLOR_FORCE` and `CLICOLOR`; 21 tests fail with `CLICOLOR_FORCE=1`). The per-file copies are in `motd.rs:26-30`, `docker.rs:107`, `colour.rs:24-27` and `docker_engine.rs`. `validate_config.rs` `a_valid_set_passes` has the same pattern.
- C12, third bullet: `crates/term-ui/tests/runner.rs:23-37` and `table.rs:21-35` inherit `CLICOLOR` (`runner.rs:248` fails with `CLICOLOR=0`).

**What:**
- `apply_sb_env` removes `CLICOLOR_FORCE`, `CLICOLOR` and `NO_COLOR`.
- Tests that want them set them. The per-file copies go.
- term-ui's two helpers also remove `CLICOLOR`.

**Acceptance:**
- [ ] A2's nine binaries, plus `validate_config` and term-ui's `runner` and `table`, pass:
  - with `CLICOLOR_FORCE=1`, with and without `SB_TEST_ROOT=1`;
  - with `CLICOLOR=0`.

**Test-first:** A2's reproduction (21 failures) and C12's (`runner.rs:248`), recorded before the change.

**Verify:** those binaries under each variable; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (terminal colour in the harness).

**Floor guard:** none expected. A removed `env_remove` is not an assertion.

**Files:**
- `crates/sb/tests/common/mod.rs`
- `motd.rs`
- `docker.rs`
- `colour.rs`
- `docker_engine.rs`
- `crates/term-ui/tests/runner.rs`
- `crates/term-ui/tests/table.rs`

**Scope:** M. Seven files, but each changes by a line or two.

**Dependencies:** none.

### Task 2.2: `SHELL` and the PTY's working directory are the test's

**Findings:** A8:
- `script -c` runs `$SHELL`: `self_update.rs:389-397`, and the 11 `Command::new("script")` sites;
- portable-pty uses `$HOME` as the working directory: `raw_pty/mod.rs:37-45`, `edit.rs:806-825`, `restore.rs:552-570`, `announcements.rs:409-420`, and fact's `Pty`.

**What:**
- `apply_sb_env` sets `SHELL=/bin/sh`, and the per-site copies at `edit.rs:1154` and `hangup.rs:181` go.
- Each PTY harness calls `builder.cwd(<test dir>)` (portable-pty 0.9.0's `CommandBuilder::cwd`, cited).
- A8's "consider one shared PTY harness" is declined:
  - the five harnesses wait differently on purpose (20 s and 60 s), and each feeds a different screen check;
  - merging them changes no outcome, and costs a cross-file refactor of test code (CLAUDE.md: keep the process in proportion);
  - the reason goes in the commit body.

**Acceptance:**
- [ ] With `SHELL=` set to a scratch stand-in that refuses POSIX syntax, `self_update` and every `script` test pass.
- [ ] With `HOME` set to a scratch directory that a stand-in program records as its working directory, every PTY test's child starts in the test's directory.

**Test-first:** both stand-in runs fail today.

**Verify:** the affected sb test binaries under both stand-ins; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (PTY, process).

**Floor guard:** none expected.

**Files:**
- `crates/sb/tests/common/mod.rs`
- `raw_pty/mod.rs`
- `edit.rs`
- `restore.rs`
- `announcements.rs`
- the fact harness
- `self_update.rs`

**Scope:** M.

**Dependencies:** 2.1 (same `apply_sb_env`).

### Task 2.3: sb's own git reads no host configuration, in sb's tests and in the gate's sandbox

**Findings:**
- A8, third bullet: `branch.rs:43` and `branch_sandbox.rs:42` (`Tools::sb`). `setup.rs:122-123` already does it right.
- A8, fourth bullet: `announcements.rs:1158-1162` and `:1197-1204` run `git rev-parse HEAD` unchecked and unisolated.
- D12: `crates/repo/src/git.rs:21-25`, used from `ops/tests/update.rs`, `setup.rs` and the ansible checkouts.

**What:**
- `Tools::sb` sets `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` to an empty file in the test's directory.
- announcements' git goes through the file's isolated `git()` helper, and asserts the exit status, with its stderr.
- `check.sh` `test_sandbox` adds both variables, pointing at an empty file it makes in the run's `TMPDIR`, so every test run of the gate sees no host git configuration.

**Acceptance:**
- [ ] With a `GIT_CONFIG_GLOBAL` holding `url.x.insteadOf` and `pull.rebase=true`, these pass: the branch tests, the ops `update`/`setup` tests, and the ansible checkout tests.
- [ ] A failing `git rev-parse` in announcements fails with git's message. Proven by a local plant.

**Test-first:** run the config-planted environment against today's branch tests first. If they pass today, the finding is latent: record that, and keep the fix as a guard.

**Verify:** the binaries above under the planted config; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (process code).

**Floor guard:** none blocking expected. The changed check.sh lines are reported.

**Files:**
- `crates/sb/tests/branch.rs`
- `branch_sandbox.rs`
- `announcements.rs`
- `scripts/check.sh`

**Scope:** S.

**Dependencies:** 1.6.

### Task 2.4: the configparser reference is frozen, from Saltbox's pinned Python

**Findings:** D4: `crates/config/src/facts/document/tests.rs:737-771` (`python_reads_all`), used by `:704` and five other tests.

**What:**
- Generate the expected sections once, as `yaml_corpus.rs` does for YAML.
- A checked-in generator script (in the style of `scripts/yaml-corpus.py`) runs in a container, on the Python minor that Saltbox's master pins. That pin is read from Saltbox's public GitHub `.python-version`, never from `/srv`. The generator and the Python version go in the fixture's header.
- The tests compare against the frozen file and no longer start `python3`.
- The corpus is the tests' own invented input.

**Acceptance:**
- [ ] No test in sb-config starts `python3`. Checked by running them with a `python3` stand-in that fails.
- [ ] The frozen file matches a fresh run of the generator in its container. A test documents the command, and the generator has a `--check` mode.
- [ ] The generator passes ruff, `ruff format --check`, mypy and bandit.

**Test-first:** with the failing `python3` stand-in, today's six tests fail.

**Verify:** `cargo test -p sb-config`, with and without the stand-in; the generator's `--check` in its container (container downloads allowed); `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:**
- **Reported:** assertions in `python_reads_all`'s callers may be rewritten (`assertion-removed`); the reason goes in the commit.
- **Not expected:** `test-removed`.

**Files:**
- `crates/config/src/facts/document/tests.rs`
- a fixture under `crates/config/tests/fixtures/`
- `scripts/configparser-corpus.py`

**Scope:** M.

**Dependencies:** none.

### Task 2.5: the disk log test uses invented filesystem figures

**Findings:** B6: `crates/host/tests/disk_log.rs:52-71` (the real `statvfs` on `TMPDIR`; the duplicate's check is skipped under 2 GiB free; `lines[0]` panics blind).

**What:**
- Use `Disk::new().with_statvfs(...)` (`crates/host/src/disk.rs:85`) with invented figures, through sb-host's `testing` feature as a self dev-dependency, as sb-repo has.
- Assert `lines.len() == 2`, with the text, before indexing.

**Acceptance:**
- [ ] The test checks both records whatever the host's free space.
- [ ] A missing record fails with the captured text.

**Test-first:** with invented figures under 2 GiB, today's shape skips the check. Show that by asserting the branch is taken, then fix.

**Verify:** `cargo test -p sb-host --test disk_log`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (I/O).

**Floor guard:** none. The manifest change touches no lints.

**Files:**
- `crates/host/tests/disk_log.rs`
- `crates/host/Cargo.toml`

**Scope:** S.

**Dependencies:** none.

### Task 2.6: the pci.ids comparison reads a pinned upstream copy, never the host's

**Findings:**
- C5: `crates/motd/src/gpu.rs:914-918`. It reads the host's `/usr/share/misc/pci.ids` and returns silently without it.
- The third CI run's failure (run 45746849992). Its `all.devices.len() > 20_000` and `every.len() > 2000` were one host's figures: the runners' copy has 18,685 devices.

**Decision** (the maintainer, 2026-11-26): no test is built on a host's setup.

**What:**
- **A pinned copy:** the public upstream `pci.ids`, copied unchanged from a named pciutils/pci.ids release, goes into `crates/motd/tests/data/` (or the crate's existing fixture directory), with a `PROVENANCE.md` giving the source URL, version and date. This is CLAUDE.md's "Public upstream files may be copied unchanged with a `PROVENANCE.md`".
- **The test reads only that copy.** It keeps the comparison of `parse` with the reference parser, for all IDs and per vendor.
  - The host-derived thresholds go.
  - A sanity bound, if one is kept at all, comes from the pinned file itself (its known counts), never a host.
  - It never reads `/usr/share/misc/pci.ids` and never skips.
- **The production code** still reads the host's file at runtime; only the test changes.

**Acceptance:**
- [ ] The test passes identically on the gate host, in the three amd64 rehearsal containers (with and without pciutils installed) and on CI.
- [ ] No test under `crates/motd` opens a host path (task 2.10's check).

**Test-first:** the current test fails in a container whose pci.ids has fewer than 20,000 devices, which reproduces CI. The new one passes there.

**Verify:** `cargo test -p sb-motd gpu`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (motd).

**Floor guard:** the removed thresholds are reported, as an assertion change, with the reason in the commit.

**Files:**
- `crates/motd/src/gpu.rs`
- the pinned `pci.ids` and its `PROVENANCE.md`

**Scope:** S.

**Dependencies:** none. **Done first, as the fix for CI run 45746849992,** ahead of the batches.

### Task 2.10: no test depends on the host's data, and the gate says so

**Findings:**
- The maintainer's decision of 2026-11-26.
- The class behind A (git config, `SHELL`, `HOME`), B2 (git config), B6 (`statvfs`), C5 (pci.ids), C12 (colour variables), D4 (system `python3`) and D6's git config.

**What:**
1. **An audit.** List every test, in unit tests, `tests/` and the scripts' tests, that reads host files or state:
   - `/usr/share`, `/etc`, `/proc`, `/sys`, `/var`, `/run`, `$HOME`;
   - tools found on `PATH` whose output the test compares, such as `python3` and `git` with host config;
   - environment variables it doesn't set itself.

   Each is moved onto pinned upstream data (with PROVENANCE.md) or invented data, or a seam. Or it's kept and listed, with the reason it must touch the host (sb's root tests are that kind), and it fails visibly when the condition isn't met.
2. **The rule:** CONSTRAINTS.md gets a Floor bullet: "a test never depends on the host's data files, configuration or environment; a test that must touch the host says why next to it, and fails visibly when it can't".
3. **The gate check:** a floor-guard rule, with tests, that flags a new test-code reference to a host path (`/usr/share/`, `/etc/`, `/proc/`, `/sys/`, `/var/`, `/run/`, `$HOME`, `dirs::home_dir`) or a new read of an unset environment variable, unless a comment beside it gives the reason. This is the same shape as the `output-discarded` rule.

**Acceptance:**
- [ ] The audit list is in the commit body.
- [ ] Every entry is moved or justified in place.
- [ ] The rule has fails-before tests.
- [ ] The rehearsal passes on all three amd64 OSes.

**Floor guard:** adding a rule is a tightening; the Floor bullet may report `rule-changed`, for the maintainer's approval then.

**Scope:** M. It is split per crate if large.

**Dependencies:** 2.1 to 2.6.

### Task 2.7: no shared `no-*` paths between tests in one process

**Findings:** A11: `crates/sb/tests/common/mod.rs:138-214` (`test_log_dir()`'s per-process `no-*` paths).

**What:** `apply_sb_env` gives each call its own directory for the `no-*` paths, counted per call as `run_lock_dir()` does. `remove_ended` asserts they were never created.

**Acceptance:**
- [ ] A test that makes sb write to one of these paths fails, naming the path. Proven by a local plant: a test that omits one override.

**Test-first:** the plant passes today (red: the leak is unseen), and fails after.

**Verify:** `cargo test -p sb` (threads) and `cargo nextest run -p sb`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none.

**Files:** `crates/sb/tests/common/mod.rs`

**Scope:** S.

**Dependencies:** 2.1 and 2.2 (same function).

### Task 2.8: the rehearsal runs for what changes a runner's verdict

**Findings:** D13: `scripts/check.sh:141-151` (`rehearsal_changed` misses `Cargo.lock`, the manifests, `build.rs`, and unit tests under `src/`).

**What (Q5: the list):** add `Cargo.lock`, `Cargo.toml`, `crates/*/Cargo.toml`, `crates/*/build.rs`, `crates/*/src/**/tests.rs`, `crates/*/src/**/tests/*`, `crates/*/src/testing*` and `crates/*/src/testing/*` to the patterns. The comment at `:118-123` is updated to match.

**Acceptance:**
- [ ] `checkRun` cases:
  - a `Cargo.lock` change sets the rehearsal at `full`;
  - a `src/**/tests.rs` change sets it;
  - a `docs/` change doesn't.

**Test-first:** the `Cargo.lock` case fails today.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>`; the batch's `full` rehearses it, since `scripts/` changed.

**Reviews:** code-reviewer.

**Floor guard:** none blocking. The changed `case` line is reported. Widening is a tightening.

**Files:**
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`
- `CONSTRAINTS.md` (the CI rehearsal row's "Runs at")

**Scope:** S.

**Dependencies:** Q5.

### Task 2.9: gitleaks from its release binary; Go leaves the gate

**Findings:** the maintainer's decision MD4.

**What:**
- **A new `scripts/install-gitleaks.sh BIN_DIR`**, in the style of `install-check-jsonschema.sh`. Its download-and-verify steps follow `provision.sh:89-100` (Node) and `rootfs.sh:57-67`. It:
  1. downloads `gitleaks_8.30.1_linux_{x64,arm64}.tar.gz` (by `uname -m`) and `gitleaks_8.30.1_checksums.txt` from the v8.30.1 release, with `curl -fsSL --retry 3`;
  2. checks the archive with `sha256sum -c` against its line in the checksums file, refusing a missing line or a mismatch;
  3. extracts only the `gitleaks` binary into `BIN_DIR`;
  4. prints `gitleaks version`.

  The version is one variable, `GITLEAKS_VERSION`. The release URL base can be overridden for its test (`GITLEAKS_BASE_URL`, `file://` allowed).
- **ci.yml's `task` job and release.yml's two jobs:**
  - `actions/setup-go` goes;
  - `go install …gitleaks…` and the `GOPATH` line become `scripts/install-gitleaks.sh "$RUNNER_TEMP/bin"`.
- **check.sh:**
  - `tool_version gitleaks` reads `gitleaks version`, whose output is the bare `8.30.1` in a release build;
  - the `go` case and `go` in `tools=(node go gitleaks)` go;
  - the comment at `:177-181` goes;
  - `install-gitleaks.test.sh` runs at fast when `scripts/` changed, as `release.test.sh` does.
- **`scripts/install-gitleaks.test.sh`:** an invented archive and checksums file in a temp directory, served by `file://`. It shows:
  - the right sum installs;
  - a wrong sum, a missing line and a missing checksums file each refuse with a message and leave `BIN_DIR` empty.
- **CONSTRAINTS.md:**
  - the gitleaks Tools row's Install column becomes `scripts/install-gitleaks.sh <bin-dir>`;
  - the Go row goes;
  - the Tools prose ("gitleaks from `go version -m`") becomes "gitleaks from `gitleaks version`".
- **floor-guard.test.mjs:** the `go` stub goes. The gitleaks stub answers `version`. The Go version cases (`:1752`, `:1793-1804`) are removed or rewritten, with the reason in the commit.
- **`.github/renovate.json:356-365`:** the gitleaks manager's patterns match `GITLEAKS_VERSION=` in the install script and the Tools row, instead of `gitleaks/v8@v…`. The gate's-tools group description drops its Go-module note (`:188`). The checksum needs no Renovate handling (choice 7).
- **Comments that name setup-go or Go:** `ci-rehearsal.sh:29` and `:62`, and `provision.sh:21-22`.
- **The gate host:** it needs the release binary in place of the `go install` build, which prints no version. Q7: the release binary through `scripts/install-gitleaks.sh`, in the directory of today's `command -v gitleaks`; Go stays on the host.

**Acceptance:**
- [ ] `install-gitleaks.test.sh` passes, and refuses each broken case with its message.
- [ ] ci.yml's and release.yml's jobs have no Go. zizmor and check-jsonschema pass.
- [ ] The rehearsal installs gitleaks with the script on 22.04, 24.04 and 26.04 amd64, and `check.sh task`'s secrets step runs it.
- [ ] `renovate-config-validator --strict` passes. A local dry run (`--platform=local`) proposes gitleaks' next release for both patterns and nothing for Go.
  - Both use the Renovate version the hosted app logs, which the maintainer gives, or the newest stable if none is given (stated as such).
  - They run in a throwaway container, because the host stays offline.
- [ ] `check.sh` refuses a gitleaks of another version, and reports "gitleaks not installed" without one (`checkRun` cases).

**Test-first:** the install test's wrong-sum case fails before the check exists. The `checkRun` version case fails while check.sh still reads `go version -m`.

**Verify:**
- `scripts/install-gitleaks.test.sh`;
- `node --test scripts/floor-guard.test.mjs`;
- the real install into a scratch `bin/`, then `gitleaks version`;
- the Renovate checks;
- `check.sh task <base>`; the batch's `full` includes the rehearsal.

**Reviews:**
- code-reviewer;
- security-auditor: a binary downloaded into the gate and CI, verified against its release's checksums, and the Renovate path.

**Floor guard:**
- **Blocking, `rule-removed`:** the Go row of the Tools table in CONSTRAINTS.md.
- **Blocking, possible:** `threshold-removed` or `threshold-changed` on the gitleaks Tools row, whose Install column loses `v8` and `@v8.30.1`.
- **Reported:** the removed and changed check.sh lines (`tool_version`'s `go` case, `tools=`).

**Files:** S and M in sum, so split it:
- 2.9a: the install script, its test, check.sh and the floor-guard stubs;
- 2.9b: the workflows, Renovate, CONSTRAINTS.md and the comments.

**Scope:** L, split as above.

**Dependencies:** 1.8 (ci.yml), Q7.

### Checkpoint: batch 2

- [ ] The amd64 rehearsal passes. It installs gitleaks from the release on all three releases, and runs no setup-go.
- [ ] `check.sh full <batch start>`'s log ends `check full: all passed`.
- [ ] The colour, `SHELL`, `HOME`, git-config and `GIT_DIR` reproductions all pass.

---

## Batch 3: oracles that can't fail, and the closed-reader redesign

### Task 3.1: the specs say the stream ends when the reader goes, with no new line needed

**Findings:** MD2.

**What:**
- **`docs/specs/tui.md`, the inline fallback (`:303-306`):** the stream ends when the reader goes away, at once, even with no new line. sb watches stdout for POLLERR/POLLHUP alongside the follow, when stdout is a pipe, FIFO or socket, as `journalctl -f` (v249 and later) and GNU `tail -f` (8.28 and later) do. Before, it ended only at the next line written. Attributed "(the maintainer, 2026-11-26)".
- **The testing list (`:636-638`):** "a closed reader ending it with 141, before the stream and after the tail with no new line"; and "an appended entry after the follow started, and one appended between the page and the follow".
- **`docs/specs/cli-core.md`:**
  - the exit codes (`:379-381`) and "Every ending" (`:444`): the quiet 141 also comes from a watched stdout whose reader went away, not only from a failed write;
  - the watch's contract: what it watches, when, and how it stops.
- **CHANGELOG `[Unreleased]`:** "`sb logs | head` ends when head does".

**Acceptance:**
- [ ] The spec's text matches MD2. No other spec text still says "at the next line written".

**Verify:** `check.sh fast` (docs only).

**Reviews:** none for the commit (docs). The maintainer reads it at approval.

**Floor guard:** none.

**Files:**
- `docs/specs/tui.md`
- `docs/specs/cli-core.md`
- `CHANGELOG.md`

**Scope:** S.

**Dependencies:** none.

### Task 3.2: cli-core's reader-gone watch

**Findings:** MD2 (the closed-reader research; choice 6).

**What:**
- An API in sb-cli-core, designed with `agent-skills:api-and-interface-design`. Its shape: `ReaderGone::stdout()`, an async `wait()` that resolves when fd 1's reader is gone, and a drop that stops it.
- When stdout is a pipe, FIFO or socket (`fstat`), it polls fd 1 with no requested events, so only POLLERR, POLLHUP and POLLNVAL return. It polls together with a stop `eventfd`, on `spawn::blocking`, as `FollowWait` does.
- When stdout is anything else, it never resolves and starts no thread.
- It resolves to cli-core's `output_error(io::ErrorKind::BrokenPipe.into())`, so the existing `quiet_exit` maps it to 141.
- Its doc comments cite rustix's `poll` and `eventfd`, pipe(7) and `fs/pipe.c`'s POLLERR on the write end. They also cite the "why not `AsyncFd`" from `crates/host/src/journal.rs`.

**Acceptance (unit tests, on `std::io::pipe()` and a socketpair):**
- [ ] The watch resolves within a bounded wait after the reader end is dropped, with no write.
- [ ] It stays pending while a reader is open: checked after the writer wrote, and after a bounded wait.
- [ ] On a regular file, it neither resolves nor holds a thread.
- [ ] Dropping it frees its blocking thread at once.
- [ ] `quiet_exit` gives `Some(141)` for its error.

**Test-first:** the first test is written against the API's signature and fails before the poll exists.

**Verify:** `cargo test -p sb-cli-core`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (I/O, event loop).

**Floor guard:**
- **Not needed:** an `#[expect]` (no unsafe; `poll` takes a `BorrowedFd`).
- **Needed only if:** a `disallowed_methods` exception is needed. None should be, since `spawn::blocking` is the allowed path.

**Files:**
- `crates/cli-core/src/exit.rs`, or a new `crates/cli-core/src/output.rs`
- `crates/cli-core/src/lib.rs`
- `crates/cli-core/Cargo.toml` (rustix `event` feature)

**Scope:** M.

**Dependencies:** 3.1.

### Task 3.3: piped `sb logs` ends on the watch; the in-process tests; the follow's first-batch case

**Findings:** MD2 and MD3:
- the in-process `streamed` test with a BrokenPipe writer (research 6.2);
- "a case that appends before the follow starts" (choice 4).

**What:**
- **`stream_log` (`crates/tui/src/lib.rs:277-326`):**
  - it takes a reader-gone future and selects on it beside the token and the events;
  - when it fires, it returns cli-core's output error, so the run ends with the quiet 141;
  - the run log records `stream ended` with the ending.
- **`streamed` tests (`lib.rs:367-400`):**
  - (a) a writer that accepts the tail, then returns `BrokenPipe` on the next write or flush, after an `Event::Followed`: the ending's chain holds the output error, and `quiet_exit` is 141;
  - (b) a reader-gone future fired with no further event: the same ending.
- **sb-tui's journal source tests:** `Request::Tail`, the page, an append, then `Request::Follow(true)`. The appended entry arrives in the follow's first batch, under both rule sets: 24.04's and 26.04's, through the injected `Host`.

**Acceptance:**
- [ ] (a), (b) and the source test pass. Each fails against a local revert of its fix: (b) hangs within its own `tokio::time::timeout` bound and fails with a message.
- [ ] The source test runs once per rule set, named by the release.

**Test-first:** (b) fails before the select arm exists.

**Verify:** `cargo test -p sb-tui --lib` and `-p sb-tui source::journal`, under `cargo test` and nextest (its `journal-follow` group); `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (event loop, I/O).

**Floor guard:** none expected.

**Files:**
- `crates/tui/src/lib.rs`
- `crates/tui/src/source/journal.rs` (tests)

**Scope:** M.

**Dependencies:** 3.2.

### Task 3.4: the end-to-end closed-reader test owns its pipe

**Findings:** MD3 (research 6.1, 6.3 and 6.4). It replaces `crates/sb/tests/logs.rs` `a_closed_reader_ends_the_stream_with_141`.

**What:**
- **A harness in `logs_pty`:** `Pty::spawn_with_stdout_pipe`.
  - It keeps the portable-pty pair, the reader thread and vt100.
  - It opens the slave by its `tty_name()` with `O_RDWR | O_NOCTTY` (rustix).
  - It makes `(r, w) = std::io::pipe()`.
  - It runs `setsid --ctty <sb> logs`, with stdin and stderr on the slave, stdout `w`, and `world.envs()`.
  - It returns the child, so `wait()` gives sb's own code: `setsid` execs in place, since a `Command` child isn't a group leader (`setsid.c:98`).
  - A comment explains why not portable-pty's spawn (it closes fds above 2, then dups the slave onto 0, 1 and 2) and why not `CommandExt::setsid` (unstable) or `pre_exec` (unsafe). It cites the URLs.
- **Case "gone before the stream":**
  1. wait for `Services (3)`;
  2. `drop(r)`;
  3. send `jj\r`;
  4. sb's code is 141;
  5. the terminal is restored, and the screen has no `✗`.
- **Case "gone after the tail, nothing new" (the new behaviour):**
  1. read the three tail lines from `r`, with a deadline;
  2. wait for `journal follow started` in the run log;
  3. `drop(r)`;
  4. with no append, sb's code is 141 within `WAIT`;
  5. the terminal is restored, there is no `✗`, and the run log has the ending.
  - On failure, it prints the run log, so "no follow line" can be told from "no exit".
- **`piped_stdout_picks_on_the_terminal_and_streams_the_log`:** waits for `journal follow started` before `world.append()` (research 6.3), which pins the inotify path.
- **No `| head` and no shell** in any of them. The old test and its marker files go. The tests are root-gated (1.4).

**Acceptance:**
- [ ] Both new cases and the streaming test pass 20 times in a row on the gate host, and 5 times in the rehearsal's 26.04 image.
  - If the streaming test misses the append on 26.04 there, a debugging task opens (`agent-skills:debugging-and-error-recovery`). It is not retried.
- [ ] "Gone after the tail" hangs and fails at `WAIT` against a local revert of 3.3. That is the "can fail" proof.

**Test-first:** as in the last acceptance item. Record the result.

**Verify:** `cargo test -p sb --test logs` as root, and `cargo nextest run -p sb --test logs`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (PTY, process, I/O).

**Floor guard:**
- **Reported:** `assertion-removed` (the old test's assertions); the reason goes in the commit.
- **Not expected:** `test-removed`, since the attribute count grows.
- **Not expected:** `output-discarded`; stdout is the pipe, read.

**Files:**
- `crates/sb/tests/logs.rs`
- `crates/sb/tests/logs_pty/mod.rs`

**Scope:** M.

**Dependencies:** 1.4 and 3.3.

### Task 3.5: the systemd parallelism test sees half-serial calls

**Findings:** B3: `crates/host/src/systemd.rs:1048-1080` (3.2-5.8 s passes either half-serial run; `tokio::join!` at `:299` and `:330`).

**What:** a handshake in the stand-in:
- `list-units` creates `a` and waits (bounded, under the 2 s call timeout) for `b`; `list-unit-files` creates `b` and waits for `a`;
- the same for `show` and `list-timers`;
- only a loose upper bound on time remains.

**Acceptance:**
- [ ] Removing either `join!` (a local plant) fails the test outright.

**Test-first:** the plant passes today; after the change it fails.

**Verify:** `cargo test -p sb-host systemd`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (host, process).

**Floor guard:** none.

**Files:** `crates/host/src/systemd.rs`

**Scope:** S.

**Dependencies:** none.

### Task 3.6: "no poll after the interrupt" can fail

**Findings:** B4:
- `crates/docker/src/control/commands.rs:666-686`;
- `crates/docker/src/control/client.rs:871-891`;
- the cause: `with_wait`'s first argument is also the poll interval (`control/mod.rs:365-371`).

**What:** a 20 ms interval. After the interrupt, `advance` the paused clock several intervals where the test is on a paused clock; otherwise wait several intervals. Then assert the count.

**Acceptance:**
- [ ] A plant that keeps polling after Ctrl-C fails both tests.

**Test-first:** the plant passes today.

**Verify:** `cargo test -p sb-docker control`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (docker).

**Floor guard:** none.

**Files:**
- `crates/docker/src/control/commands.rs`
- `crates/docker/src/control/client.rs`

**Scope:** S.

**Dependencies:** none.

### Task 3.7: bench's cancelled gather and the hung mounts, on a channel

**Findings:**
- D7: `crates/ops/src/bench/system.rs:1584-1600` (`elapsed < 30s` against a 300 ms hang);
- D6, moved here: `:1567-1580` (a 300 ms hang against a 20 ms limit loses to starvation).

**What:** the hung listing blocks on a channel the test drops after its assertion, so the runtime can still shut down. Then:
- D6 asserts `facts.disk` is `None` whatever the scheduler does;
- D7 asserts `elapsed` far under the hang: under 5 s against a hang that ends only on the drop.

**Acceptance:**
- [ ] Swapping `run_until_cancelled` for a plain `timeout` (a plant) fails D7's test.
- [ ] Under a 4% CPU quota, D6's test passes 10 of 10 runs.

**Test-first:** the plant passes today.

**Verify:** `cargo test -p sb-ops bench::system`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none.

**Files:** `crates/ops/src/bench/system.rs`

**Scope:** S.

**Dependencies:** none.

### Task 3.8: errors are asserted by kind: journal corruption and cli-core's log refusals

**Findings:**
- B8: `crates/journal/src/file.rs:3050-3051`, `:3223`, `:3484-3485`, `:3628`, `:4831` (bare `is_err()`);
- C4, first part: `crates/cli-core/src/log.rs:706`, `:725`, `:739` (three refusal rules, each `is_err()`).

**What:**
- `expect_err` on each, then assert the error's kind or text, as the neighbouring tests do.
- B8's cases without a positive control get one, as `:3484-3486` has.
- C4's planted-link refusal asserts its own error, so it can't be confused with the other two rules.

**Acceptance:**
- [ ] Each assertion names its rule's error.
- [ ] Swapping two of the rules' checks (a plant) fails the tests.

**Test-first:** the plant passes today.

**Verify:**
- `cargo test -p sb-journal file` and `-p sb-cli-core log`;
- the journal matrix and fuzz by hand at the checkpoint, because `crates/journal/` changed;
- `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (cli-core).

**Floor guard:** **reported:** `assertion-removed`, for the bare `assert!(… .is_err())` lines replaced; the reason goes in the commit.

**Files:**
- `crates/journal/src/file.rs`
- `crates/cli-core/src/log.rs`

**Scope:** S.

**Dependencies:** none.

### Task 3.9: the 150 ms drain is checked by the child's own clock

**Findings:** C2: `crates/tui/tests/term.rs:516-534` (`…drains_the_whole_150_ms…` never checks 150 ms).

**What:** read the child's `millis=` from its run log (`term.rs:483-486` writes it), and assert it is at least about 145 ms. Add a shared `run_log_millis()` helper, which Task 4.6 reuses.

**Acceptance:**
- [ ] `DRAIN` cut to 10 ms (a plant) fails the test.

**Test-first:** the plant passes today.

**Verify:** `cargo test -p sb-tui --test term`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (terminal).

**Floor guard:** none.

**Files:** `crates/tui/tests/term.rs`

**Scope:** XS.

**Dependencies:** 1.7 (same file).

### Task 3.10: tests that pass whichever way the race goes decide the race

**Findings:** A6:
- `crates/exec/tests/attached_late_signal.rs:1-38` (it passes about 1 run in 4 without reaching its check);
- `crates/sb/tests/hangup.rs:231` (`for run in 1..=5`);
- `crates/sb/tests/update.rs:543-551` (it accepts either path).

**What:**
- A `testing` seam that delivers the signal at the chosen point, built on `Shutdown`'s `signalled()` and `hold` hooks, feature-gated (M-TEST-UTIL).
- Each test drives its intended path and asserts that path was the one taken: the run log's record, or the seam's own record.
- `hangup.rs`'s loop of 5 and `update.rs`'s either-or go.

**Acceptance:**
- [ ] Each test asserts it reached its case, and fails if the other path ran (a plant that forces it).
- [ ] cargo-mutants on those lines gives a stable verdict: 3 runs of `--in-diff` on a planted change agree.

**Test-first:** the forced-other-path plant passes today.

**Verify:** `cargo test -p sb-exec --test attached_late_signal`, `-p sb --test hangup --test update`, 10 runs each under a 4% quota; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (exec, signals, process).

**Floor guard:**
- **Reported:** `assertion-removed`, where the either-or's alternative assertion goes.
- **Not expected:** an `#[expect]`. If one appears, it needs an Exceptions row, which is a blocking `new-exception`.

**Files:**
- `crates/exec/tests/attached_late_signal.rs`
- the `Shutdown` seam (cli-core or exec, `testing`)
- `crates/sb/tests/hangup.rs`
- `crates/sb/tests/update.rs`

**Scope:** M, or L once the seam is placed; then split exec from sb.

**Dependencies:** none.

### Task 3.11: weak assertions in sb's and completion's tests

**Findings:** A9:
- `blank_line.rs:122` (a tautology);
- `edit.rs:283` (a re-check of an earlier world);
- the absence-only checks `theme_config.rs:31` and `:66`, and `colour.rs:189`;
- `motd.rs:158` (a host-dependent either-or);
- `run_lock.rs:236` and `:424` (the status ignored);
- `fact.rs:959-962` (`code().is_some()`);
- `completion/tests/install.rs` `probes_run_clean` (`!USER=` only).

**What:**
- Assert the version line exactly.
- Check the world that just ran.
- Add the positive half (the expected `✗ …` line).
- motd's either-or: assert a bar for an invented mount, through a mounts seam fed invented entries (M-MOCKABLE-SYSCALLS) if sb-motd has one. Otherwise move the check to an sb-motd unit test with invented entries.
- Assert the exit codes.
- Assert that the probe's environment is exactly the expected set, plus what `sh` and sb-exec add.

**Acceptance:**
- [ ] Each assertion fails against a plant of the regression its finding names. For example, sb printing nothing passes today's `theme_config.rs:31` and fails the new one.

**Test-first:** the plants, recorded.

**Verify:** the named sb and completion test binaries; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (sb, completion's process code).

**Floor guard:**
- **Reported:** `assertion-removed`, where an assertion is rewritten.
- **Reported:** `test-removed`, if motd's either-or test moves into sb-motd (a test leaving its file); the reason goes in the commit.

**Files:** eight test files across `crates/sb/tests/` and `crates/completion/tests/`, possibly `crates/motd/src/`. Split into 3.11a (sb) and 3.11b (completion and motd) if the review gets large.

**Scope:** M.

**Dependencies:** none.

### Task 3.12: tests that restate the code

**Findings:**
- B7:
  - `crates/host/src/lib.rs:290-293` and `:251-260`;
  - `crates/host/src/cores.rs:173-179` (also its `.expect("cores")`);
  - `crates/host/src/journal.rs:478-485`;
  - `crates/host/src/lib.rs:340-348`;
  - `crates/journal/src/testing/follow_slot.rs:136-143`.
- C10: `motd/src/providers/tests.rs:114`, `cli-core/src/cancel.rs:820` and `:871`, `motd/src/docker.rs:899` and `:1204`, `config.rs:1131-1132`, `logins.rs:759`, `packages.rs:626`, `gpu.rs:503-504`, `banner.rs:901-903`.
- D14: `crates/ops/src/bench/system.rs:1604-1618`.

**What (Q6: keep them as stated change detectors):**
- **B7's recomputations become property checks:**
  - `1 <= children_at_once() <= available_parallelism()`, without the `.expect`;
  - on a host whose `/etc/os-release` says a supported Ubuntu, the release is asserted;
  - the coredump uid is checked against the `passwd` entry's form.
- **The sb-go parity pins and the constant pins** (B7's three, C10's, D14) stay. Each gets a comment that says it is a change detector, naming the source line it mirrors (sb-go's, or the spec's) and the mutants it kills. No ADR.
- **C10's duration pins** (`DEFAULT_TIMEOUT`, `REPEAT_WINDOW`) stay too. The behaviour tests use the constants, so the pins catch a silent change to the number itself.

**Acceptance:**
- [ ] B7's six can each fail on a regression (a plant for each), or say in a comment why they exist (coverage of a real syscall line).
- [ ] The cores test passes on a container without sysfs CPU topology.

**Test-first:** the cores test fails today in a container without `/sys/devices/system/cpu/*/topology`.

**Verify:**
- the crates' unit tests;
- one container run for the cores test;
- the journal matrix by hand, if `follow_slot.rs` changed;
- `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (host, motd, cli-core).

**Floor guard:** **reported:** `assertion-removed`, for rewritten assertions.

**Files:** M, in about 10 files of one line each. Split into host and journal (3.12a), then motd, cli-core and ops (3.12b).

**Scope:** M, split as above.

**Dependencies:** Q6.

### Task 3.13: the docker fakes surface their own failures

**Findings:** B9:
- `crates/docker/src/testing/mod.rs:261-263` (`let _ = axum::serve`), `:368-371` (the join discarded in `Drop`), `:38-40` (`unwrap_or_default` on the fixture);
- `crates/docker/src/testing/controller.rs:178-180` and `:223-226`.

**What:**
- In `Drop`: `if let Err(p) = thread.join() { if !std::thread::panicking() { resume_unwind(p) } }`.
- Handler panics are caught with tower-http's `CatchPanicLayer` (cited), and a test asserts none happened.
- `inspects()` uses `expect("containers.json parses")`.

**Acceptance:**
- [ ] A handler that panics (a plant) fails the test with the panic's message, not with a transport error.
- [ ] A broken fixture fails with "containers.json parses".

**Test-first:** both plants pass, or fail blind, today.

**Verify:** `cargo test -p sb-docker` and `-p sb --test docker_engine --test docker_control`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (docker, I/O).

**Floor guard:**
- **Dependencies:** tower-http is already in `Cargo.lock` (checked at 48f367ea), so adding it to sb-docker's dependencies brings in no new crate or licence. A new crate would raise cargo-deny's licence and ban checks: `licence-added`, or `wrappers-widened` if sb-docker's deny entry changed; both are blocking.

**Files:**
- `crates/docker/src/testing/mod.rs`
- `crates/docker/src/testing/controller.rs`
- possibly `crates/docker/Cargo.toml`

**Scope:** S.

**Dependencies:** none.

### Task 3.14: the shipped binary runs the tests that need no seam

**Findings:** A10: `crates/sb/Cargo.toml:46-84` (the dev build with `testing`); `SB_TEST_BINARY` unused (`check.sh:49`); `entropy.rs:32` (it inspects the test binary).

**What:**
- `check.sh full` runs sb's non-seam test binaries once more, with `SB_TEST_BINARY=target/release/sb`, in the sandbox. Those are help, errors, version, colour and docker parse errors; the list sits in check.sh with why.
- `entropy.rs` says what `current_exe()` is, as `key_exchange.rs` does.

**Acceptance:**
- [ ] The step runs at `full` and fails on a failure (a `checkRun` case).
- [ ] The listed tests honour `SB_TEST_BINARY`; a stand-in binary that fails shows it.

**Test-first:** the `checkRun` case fails before the step exists.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:**
- **Reported:** check.sh lines changed.
- **Possible:** `threshold-changed` on the Tests row, if a number in it changes.

**Files:**
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`
- `crates/sb/tests/entropy.rs`
- `CONSTRAINTS.md` (Tests row)

**Scope:** S.

**Dependencies:** none.

### Task 3.15: nextest's LEAK status is shown in the release-build run

**Findings:** D9, first part: `.config/nextest.toml:246-247` (`status-level = "slow"` and `final-status-level = "slow"` hide LEAK).

**What:** `status-level = "leak"` and `final-status-level = "leak"` in `release-tests`, citing nextest's status-level docs.

**Acceptance:**
- [ ] The release-build run prints LEAK for a planted test whose child keeps stdout open.
- [ ] The known leaks (A13's orphans) are listed for Task 4.8.

**Test-first:** the plant shows nothing today.

**Verify:** `cargo nextest run --profile release-tests -p sb-exec`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** **blocking, `tool-config-changed`:** `.config/nextest.toml`. Ask then.

**Files:** `.config/nextest.toml`

**Scope:** XS.

**Dependencies:** none.

### Checkpoint: batch 3

- [ ] The amd64 rehearsal passes. The closed-reader cases and the streaming test pass on all three releases, 26.04 included.
- [ ] `check.sh full <batch start>`'s log ends `check full: all passed`.
- [ ] The journal matrix and fuzz harness pass by hand, for Task 3.8.
- [ ] Piped `sb logs` behaviour, the user-visible change of MD2, is confirmed in the maintainer's own terminal:
  - with an sb built by `cargo build --release -p sb`, run `sb logs | head -n 2` against a quiet unit;
  - it ends with head.

---

## Batch 4: timing and hangs

### Task 4.1: the coverage run ends a hung test, and the hang-shaped tests bound themselves

**Findings:**
- C6: `scripts/check.sh:1207` (`cargo llvm-cov`, no per-test timeout) and `:1209` (doc tests). The hang-shaped tests are `crates/tui/src/term.rs:1803` and the `start_paused` tests at `:1149-1410`.
- D3: the same step, and `crates/ansible/tests/cache.rs:359-366` (`while !pid.exists()` with no deadline).

**What:**
- The coverage run becomes `cargo llvm-cov nextest` with a nextest profile that ends a test after 60 s, as decided. It keeps `--lcov --output-path target/lcov.info`, from cargo-llvm-cov's README "Get coverage of cargo-nextest", at 0.9.1.
- The run where tests share a process: the release-build run, under `cargo test --profile release-tests` inside GNU `timeout` (Q4).
- The doc-test step gets GNU `timeout` with a long limit that names the step when it expires. libtest has no per-test timeout.
- The hang-shaped tests wrap their futures in `tokio::time::timeout(..).await.expect("ended")`.
- `cache.rs`'s `pressed` loop gets a deadline that panics with the missing file's name, as `appears()` (`python-env/tests/staging.rs:435`) does.

**Acceptance:**
- [ ] A planted hang in a unit test ends the coverage step at about 60 s, naming the test (a `checkRun` case with stubs, then one real planted run).
- [ ] `project_coverage`'s figures under nextest are printed, and compared with the batch-3 figures.
  - If they move below the floor's tolerance, that is a measurement change for the maintainer (ADR-025), never a quiet re-baseline.
- [ ] The three hang-shaped tests fail with a message against a planted hang.

**Test-first:** the planted hang holds today's coverage step (record it with a short scope timeout, then stop it by PID).

**Verify:** `node --test scripts/floor-guard.test.mjs`; a real `check.sh task <base>` for the timing; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (tui event loop).

**Floor guard:**
- **Blocking, `tool-config-changed`:** `.config/nextest.toml`, if a coverage profile is added. The `release-tests` profile may be reused instead (to check with cargo-llvm-cov's docs).
- **Possible:**
  - `threshold-changed` on the Tests and Coverage rows of CONSTRAINTS.md, where `cargo llvm-cov` becomes `cargo llvm-cov nextest`;
  - a project-coverage floor change, if Q4 or the figures need one. That would be `threshold-loosened` if a floor is lowered, and needs an ADR as well.
- **Reported:** the changed check.sh lines.

**Files:**
- `scripts/check.sh`
- `.config/nextest.toml`
- `CONSTRAINTS.md`
- `crates/tui/src/term.rs`
- `crates/ansible/tests/cache.rs`
- `scripts/floor-guard.test.mjs`

**Scope:** L, so split it:
- 4.1a: the gate step and CONSTRAINTS.md;
- 4.1b: the three tests' own bounds.

**Dependencies:** Q4; 1.1, because the skip summary must work under the new runner.

### Task 4.2: signal tests end on events and on bounds measured at the moment

**Findings:** A5:
- `crates/sb/tests/docker_control_signals.rs:38` (`ENDS_WITHIN = 1s`) and `:347-349` (300 ms between two SIGINTs, which coalesce);
- `crates/exec/tests/demo_signals.rs:144` (`took < 2s`, against a measured worst of 3 s);
- `crates/sb/tests/edit.rs:1056` (`< 2s`);
- `crates/sb/tests/logs.rs:555` (`< 6s` against a 5 s refresh; `units.json` is rewritten non-atomically);
- `crates/exec/tests/stream.rs:104` (`exit 3 · 0.0s`).

**What:**
- Use `groups.rs`' pattern: a bound that tells "ended on the signal" from "waited the grace", against a baseline measured now; for example, under half of a grace of 8 s or more.
- The second SIGINT waits for an observable effect of the first: the run log's cancellation record, or the row drawn as interrupted.
- `units.json` is written with write-then-rename, as `World::append` does.
- `stream.rs` matches `exit 3 · \d+\.\ds`.

**Acceptance:**
- [ ] Each test passes 10 of 10 runs under a 4% CPU quota.
- [ ] Each still fails against a plant that waits the grace.

**Test-first:**
- The coalescing case: a plant that delays sb's signal thread by 400 ms fails today's test, and passes after.
- The others: the waits-the-grace plant must still fail after the change.

**Verify:** the five binaries under the quota; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (exec, signals, sb PTY).

**Floor guard:** none expected. A widened time bound in a test is not a gate threshold.

**Files:**
- `docker_control_signals.rs`
- `demo_signals.rs`
- `edit.rs`
- `logs.rs`
- `logs_pty/mod.rs`
- `stream.rs`

**Scope:** M, with six files changed lightly. Split into 4.2a (sb's tests) and 4.2b (exec's) for review size.

**Dependencies:** 3.4 (logs.rs).

### Task 4.3: the double-click tests stretch the drain instead of retrying

**Findings:** A4: `crates/sb/tests/edit.rs:1497-1535` and `fact.rs:569-620` (stale vt100 state, and a hidden retry loop of 3).

**What:**
- A `testing`-feature seam stretches sb-tui's exit drain (`crates/tui/src/term.rs:96`, 150 ms) to 2 s in these runs, feature-gated (M-TEST-UTIL).
- The in-test retry goes.
- The ordering proof stays in sb-tui's unit drain test.

**Acceptance:**
- [ ] Both tests pass 10 of 10 runs under a 4% quota, with no retry.
- [ ] The seam exists only with `testing` (`check.sh`'s `testing_feature_check` still passes).

**Test-first:** under a planted 150 ms stall of the test thread, today's tests fail; after, they pass.

**Verify:** `cargo test -p sb --test edit --test fact` under the quota; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (terminal, input).

**Floor guard:** **reported:** `assertion-removed`, if the retry's "too slow" branch held an assertion.

**Files:**
- `crates/tui/src/term.rs`
- `crates/sb/tests/edit.rs`
- `crates/sb/tests/fact.rs`

**Scope:** S.

**Dependencies:** 3.9 (term.rs).

### Task 4.4: host's and docker's fixed delays wait for state

**Findings:** B5:
- `crates/host/src/apt.rs:1398-1419` (SIGINT after 300 ms) and `:1423-1452` (300 ms after `count`);
- `crates/host/src/setup.rs:299-313` (a 200 ms timeout);
- `crates/docker/src/control/client.rs:850-866` (real time with 300 ms margins).

**What:**
- Wait for `calls.log` before signalling.
- Wait for the step's retry line on the in-memory terminal, or a seam that marks "in delay".
- Use a 2 s hung-check timeout.
- The give-up test's schedule assertions move to the paused clock. The real-time test keeps "timed out, at least 3 polls".

**Acceptance:**
- [ ] Each test passes with a planted SIGSTOP of 1 s of the stand-in, the stall B5 names.

**Test-first:** the SIGSTOP plant fails today's tests.

**Verify:** `cargo test -p sb-host apt setup` and `-p sb-docker control::client`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (host, docker).

**Floor guard:** none.

**Files:**
- `crates/host/src/apt.rs`
- `crates/host/src/setup.rs`
- `crates/docker/src/control/client.rs`

**Scope:** S.

**Dependencies:** 3.6 (client.rs).

### Task 4.5: ops' Ctrl-C follows the event it is meant to follow; the helper fails out loud

**Findings:**
- D5: `crates/ops/tests/update.rs:2702` (`ctrl_c_delay = 1500ms`) and `crates/ops/src/bench/mod.rs:1271` (`interrupt_after(500ms)`);
- D11, moved here: `crates/ops/tests/update.rs:155-166` (the `ctrl_c` helper presses anyway after 30 s).

**What:**
- `update.rs` waits for the Saltbox step's recorded ownership call, or for its line on the screen.
- bench presses once `slow.seen()` lists `/app/speedtest.tgz`.
- The `ctrl_c` helper panics, naming the files, when its deadline passes.

**Acceptance:**
- [ ] A planted 2 s stall in the ownership step keeps `update.rs`' test green. Today it fails.
- [ ] A planted slow gather (more than 500 ms) makes bench's test fail if Ctrl-C didn't reach the download.
- [ ] A missing marker panics with its name.

**Test-first:** the plants, recorded.

**Verify:** `cargo test -p sb-ops --test update` and `bench`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** none.

**Files:**
- `crates/ops/tests/update.rs`
- `crates/ops/src/bench/mod.rs`

**Scope:** S.

**Dependencies:** none.

### Task 4.6: thin PTY and motd margins

**Findings:** C7:
- `crates/tui/tests/term.rs:499-507` (`left >= 950 ms`, harness-measured);
- `:461-486` (900 ms against a 1 s cap);
- `:795` (median latency under 100 ms);
- `crates/motd/src/providers/tests.rs:209`, `:347`, `:367`, `:379`, `:435`.

**What:**
- The 950 ms and 900 ms checks read the child's `millis=` (Task 3.9's helper).
- The latency median is bounded against the harness's own baseline, measured first, as the file already does for its loop.
- motd's upper bounds take `groups.rs`' load-relative form: a baseline run of the same provider path measured at the same moment, against the timeout under test.
- None failed under C's contention runs. This is hardening.

**Acceptance:**
- [ ] Each passes 10 of 10 under 4 copies on half a core.
- [ ] Each fails against a plant that breaks its bound (a cap of 2 s, a hung provider).

**Test-first:** the plants.

**Verify:** `cargo test -p sb-tui --test term` and `-p sb-motd providers`, under the contention; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (terminal, motd).

**Floor guard:** none.

**Files:**
- `crates/tui/tests/term.rs`
- `crates/motd/src/providers/tests.rs`

**Scope:** S.

**Dependencies:** 3.9.

### Task 4.7: negative waits become recorded order; the redundant sleeps go

**Findings:** A9, the last two groups:
- **Negative waits:** `run_lock.rs:212` and `:353`, `hangup.rs:388`, `demo_attached.rs:158`, `:198` and `:232`, `attached_late_signal.rs:36`.
- **Sleeps:** `self_update.rs:478` (200 ms after `downloaded()`), `logs_containers/mod.rs:121-123` (300 ms, and `assert_eq!(lists(), 2)` with no message).

**What:**
- Where the order can be recorded, the stand-in records it and the sleep goes. Example: fake-playbook notes whether the holder's release file exists when it starts.
- Where it can't, the sleep stays with a comment saying why. That is A9's "acceptable as a known limit", stated per site.
- The `self_update` sleep goes.
- `logs_containers` waits on sb's run-log record, and its assertion gets a message.

**Acceptance:**
- [ ] `run_lock`'s "didn't run yet" becomes an exact check with no sleep.
- [ ] Each remaining negative wait has its reason in place.

**Test-first:** `run_lock`'s exact check fails against a plant that lets the second run start early. Today's sleep-based test passes the plant under load.

**Verify:** the named binaries; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (exec, sb).

**Floor guard:** none.

**Files:**
- `run_lock.rs`
- `hangup.rs`
- `self_update.rs`
- `logs_containers/mod.rs`
- `crates/exec/tests/demo_attached.rs`
- `attached_late_signal.rs`

**Scope:** M. Split sb's tests (4.7a) from exec's (4.7b).

**Dependencies:** 3.10 (`attached_late_signal.rs`) and 4.2.

### Task 4.8: test orphans close their output, and a leak fails the release-build run

**Findings:**
- A13, second bullet: `crates/exec/tests/stream.rs:213-260` (`sleep 10 &`, `sleep 30 &`) and `capture.rs:231`;
- D9, second part: `leak-timeout = { period = "500ms", result = "fail" }`.

**What:**
- Each deliberate orphan is reaped, or killed by its test, before the test ends: by PID, from the shell's `$!`, written to a file.
- Then `release-tests` gets the leak timeout.

**Acceptance:**
- [ ] The release-build run shows no LEAK.
- [ ] A planted leak fails it.
- [ ] No `sleep` orphan of a test survives its test. Checked by PID after a run of the exec binaries.

**Test-first:** the planted leak passes today, even after 3.15 (it is printed, not failed).

**Verify:** `cargo nextest run --profile release-tests -p sb-exec`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (exec, process).

**Floor guard:** **blocking, `tool-config-changed`:** `.config/nextest.toml`.

**Files:**
- `crates/exec/tests/stream.rs`
- `crates/exec/tests/capture.rs`
- `.config/nextest.toml`

**Scope:** S.

**Dependencies:** 3.15 and 4.1 (nextest.toml).

### Task 4.9: the journal matrix scripts wait for state and stop the right process

**Findings:** B10:
- `crates/journal/tests/compare.sh:117-118`, `compat.sh:87-90` and `stress.sh:75-76` (`sleep 2` after starting journald);
- `compare.sh:133-140` (the wait times out silently; `systemd-cat` failures are unchecked);
- `follow.sh:143-144` (`pgrep -n -x`);
- `follow.sh:318-330` (the overflow is never checked).

**What:**
- Share `follow.sh`'s `start_jd`, in a sourced `crates/journal/tests/lib.sh`.
- `compare.sh`'s wait calls `bad` on timeout, and the writers check `systemd-cat`.
- The followers' PIDs come from `$!`. In stamp mode, each pipeline writes its follower's PID to a file.
- The example prints a line on `IN_Q_OVERFLOW`, or counts its rescans, and the overflow scenario checks for it in `fa.err`.

**Acceptance:**
- [ ] Each script passes on 22.04, 24.04 and 26.04 by hand, capped.
- [ ] `compare.sh` fails on a planted short write.
- [ ] The overflow scenario fails when the watch mask is changed (a plant).
- [ ] No `pgrep` is left.

**Test-first:** the planted short write passes today's `compare.sh`.

**Verify:** the matrix and fuzz harness by hand, as CONSTRAINTS.md's Journal matrix row says; `check.sh task <base>`. The test scripts' own floor checks run in fast.

**Reviews:** code-reviewer.

**Floor guard:**
- **Blocking, `check-disabled`:** if an added line matches a disabler (`exit 0`, `set +e`, `|| true` is allowed in test scripts). Avoid these.
- **Blocking, `threshold-changed`:** if a `*_MAX` or `*_MIN` constant in the scripts changes.

**Files:**
- `compare.sh`
- `compat.sh`
- `stress.sh`
- `follow.sh`
- new `lib.sh`
- the journal example's overflow line, in `crates/journal/examples/`

**Scope:** M.

**Dependencies:** none.

### Checkpoint: batch 4

- [ ] The amd64 rehearsal passes. The coverage step shows nextest's per-test end.
- [ ] `check.sh full <batch start>`'s log ends `check full: all passed`.
- [ ] The journal matrix and fuzz pass by hand.
- [ ] No orphaned processes after the runs (checked by PID).

---

## Batch 5: blind failures and style

### Task 5.1: exit-code asserts and `.status()` show the child's output

**Findings:** A7, first and third groups:
- 26 `assert_eq!(….code(), Some(n))` and 11 `assert!(….success())`:
  - `motd.rs`: 79, 92, 134, 174, 225, 262, 327, 623, 645, and the rest of its 14;
  - `colour.rs`: 37, 69, 115;
  - `run_log.rs`: 33, 75, 138;
  - `edit.rs`: 287, 297, 318, 340, 371, 419, 442, 1345;
  - `ansible.rs`: 929, 951, 1558, 1673;
  - `fact.rs`: 356.
- `run_log.rs:33` uses `.status()`.
- `validate_config_signals.rs:117` doesn't read the `script`/`timeout` stderr it pipes.

**What:**
- Each assert gets `"{}", text(&out.stderr)` (or `{out:?}`).
- `.status()` becomes `.output()` or `ChildOutput`.
- The piped stderr is read and shown.

**Acceptance:**
- [ ] A `grep` lists no message-less exit or status assert in `crates/sb/tests/`.
- [ ] A planted wrong exit shows sb's stderr.

**Test-first:** not a behaviour change. The proof is the planted failure's message, before and after.

**Verify:** the sb test binaries; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (process).

**Floor guard:**
- **Reported:** `assertion-removed`, for each rewritten assertion line. The commit body says they gained messages only.

**Files:** M, in six files of mechanical edits. Split it (5.1a motd and colour; 5.1b the rest) if the review asks.

**Scope:** M.

**Dependencies:** none.

### Task 5.2: wait loops show sb's state when they time out

**Findings:** A7, second group:
- `ansible_signals.rs:151-156` (`start_in`);
- `run_lock.rs:182-189` (`wait_for`, with stderr compared exactly at 209, 272, 466 and 494).

**What:** `try_wait` in the loop. On timeout or an early exit, panic with sb's output, as `update.rs:312-318` does.

**Acceptance:**
- [ ] A planted early exit of sb fails at once with its stderr, not after 10 s.

**Test-first:** the plant waits 10 s and fails blind today.

**Verify:** `cargo test -p sb --test ansible_signals --test run_lock`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (process).

**Floor guard:** none.

**Files:**
- `crates/sb/tests/ansible_signals.rs`
- `crates/sb/tests/run_lock.rs`

**Scope:** S.

**Dependencies:** 4.7 (`run_lock.rs`).

### Task 5.3: the tui PTY asserts say why

**Findings:** C3: `crates/tui/tests/term.rs:328`, `:331`, `:379`, `:416`, `:446`, `:471`, `:498`, `:500`, `:522`, `:524`, `:545`, `:556`, `:643`, `:670`, `:672`, `:684`, `:782`, `:785`, `:791`.

**What:** a `pty.assert_success()` helper that prints the status, `screen.contents()`, the raw bytes and `run_log()`. The `showing` and `wrote` asserts get the same message.

**Acceptance:**
- [ ] No message-less assert is left in `term.rs`.
- [ ] A planted child panic shows its message in the failure.

**Test-first:** the plant's failure today says only "assertion failed".

**Verify:** `cargo test -p sb-tui --test term`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (PTY).

**Floor guard:** **reported:** `assertion-removed`, for each rewritten assert.

**Files:** `crates/tui/tests/term.rs`

**Scope:** S.

**Dependencies:** 4.6 (same file).

### Task 5.4: bare `is_ok`, `is_err`, `is_some` and `is_none` checks keep their value

**Findings:** C4, the rest: about 90 across review C, for example:
- `crates/tui/src/lib.rs:486`;
- `crates/term-ui/src/block.rs:695` and `:720`;
- `crates/motd/src/docker.rs:922-923`;
- `crates/motd/src/gpu.rs:658`.

**What:** `.expect(..)` or `.expect_err(..)`, or `assert!(matches!(..), "{x:?}")`, crate by crate.

**Acceptance:**
- [ ] A `grep` lists no message-less `assert!(….is_ok|is_err|is_some|is_none())` in tui, term-ui, motd and cli-core.

**Test-first:** not a behaviour change.

**Verify:** the four crates' tests; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (all four are terminal or I/O crates).

**Floor guard:** **reported:** `assertion-removed`, for each rewritten line.

**Files:** one commit per crate (5.4a to 5.4d), about 90 lines in all.

**Scope:** M each.

**Dependencies:** 3.8 (cli-core's log.rs).

### Task 5.5: lost failure reasons in cli-core, term-ui and ops

**Findings:**
- C12, first and second bullets: `crates/cli-core/tests/relaunch.rs:49` (the record is read after the code assert); `crates/term-ui/tests/signals.rs:23-37` (`on_a_terminal` drops the `script`/`timeout` status and stderr);
- D16: `crates/ops/tests/gha.rs:125`, `134`, `143`, `156`, `172`, `187`, and `crates/ops/tests/setup.rs:563-638` (`assert!(!ok)`).

**What:**
- Read the record first and put it in the message.
- Return the status and stderr in `on_a_terminal`'s text.
- Use `assert!(!ok, "{screen}")`.

**Acceptance:**
- [ ] Each planted failure (an exec that fails, a missing `script`, a wrong `ok`) shows its reason.

**Test-first:** not a behaviour change. The proof is the plants' messages.

**Verify:** `cargo test -p sb-cli-core --test relaunch`, `-p sb-term-ui --test signals` and `-p sb-ops --test gha --test setup`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (cli-core, term-ui).

**Floor guard:** **reported:** `assertion-removed`.

**Files:**
- `relaunch.rs`
- `signals.rs`
- `gha.rs`
- `setup.rs`

**Scope:** S.

**Dependencies:** none.

### Task 5.6: the motd matrix fails with its cause

**Findings:** C11: `crates/motd/tests/matrix.sh` (the `ssh -tt` lines send everything to `/dev/null` with no status check, about `:119-124`; the boot wait carries on after 300 s, about `:240`).

**What:**
- Each ssh's stderr goes to a file under `/var/tmp` inside the container, which is the container's own path and not the host's. Its status is checked, and the file is printed when a later check fails.
- After the wait loop, the script fails with "systemd did not boot".

**Acceptance:**
- [ ] A planted sshd that doesn't start fails with sshd's message.
- [ ] A planted non-booting image fails with "systemd did not boot".
- [ ] The fixture allow-list check still passes.

**Test-first:** both plants give unrelated FAILs today.

**Verify:** a by-hand `matrix.sh` run, capped; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** **blocking, `check-disabled`:** if an added line matches a disabler. Avoid these.

**Files:** `crates/motd/tests/matrix.sh`

**Scope:** S.

**Dependencies:** none.

### Task 5.7: style in sb's tests

**Findings:**
- **A12.** `docker_control.rs:362` waits through the real 5 s poll three times. `help.rs`' `no_short_help_wraps_at_120_columns` spawns sb twice for every command.
- **A13, first bullet.** `assert_cmd::cargo::cargo_bin("sb")` at `announcements.rs:112`, `restore.rs:99/275/346` and `self_update.rs:64` becomes `env!("CARGO_BIN_EXE_sb")`.
- **A13, third bullet.** `isolation.rs`' file-level scan.

**What:**
- **Fixed:** the `env!` change.
- **Fixed:** `docker_control.rs`.
  - Its poll-limit checks move to sb-docker's unit tests, on `with_wait` (`control/mod.rs:365`), where its own comment says they belong.
  - The end-to-end test keeps one poll. That saves about 10 s a run.
- **Declined:** `help.rs`, with the reason in the commit. It checks the shipped binary's own rendering, end to end. Rendering clap's `Command` in-process would no longer test what users see, and the test stays far under its 60 s limit.
- **Declined:** the isolation scan, with the reason in the commit. It is documented as a guard, not proof, and 1.4 adds the root-gate check beside it. Parsing Rust to tighten it would cost more than the cases it would catch.

**Acceptance:**
- [ ] No `cargo_bin(` is left.
- [ ] `docker_control`'s binary runs at least 8 s faster, with the poll-limit cases covered in sb-docker. Coverage and mutation of those lines hold, per `check.sh`.

**Test-first:** the moved poll-limit unit tests fail against a plant of a wrong limit.

**Verify:** `cargo test -p sb --test docker_control --test announcements --test restore --test self_update`, and `-p sb-docker control`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (docker, sb).

**Floor guard:**
- **Reported:** `test-removed` and `assertion-removed`, if end-to-end poll cases leave `docker_control.rs`. The reason goes in the commit: they moved to sb-docker.

**Files:**
- `announcements.rs`
- `restore.rs`
- `self_update.rs`
- `docker_control.rs`
- `crates/docker/src/control/`

**Scope:** M.

**Dependencies:** 2.2 (the harnesses in the same files).

### Task 5.8: style in the crates

**Findings:**
- **C8.** `crates/tui/tests/term.rs:85-88`: the no-op `pty_child` `#[test]`.
- **C9.** `crates/term-ui/src/lib.rs:95-125`: console's colour switches are left changed.
- **B12.** `crates/host/src/testing.rs:7-10` and `crates/repo/tests/update.rs:218-221`: the `SPAWNS` comment.

**What:**
- **C8.** The child becomes a `testing`-feature binary of sb-tui, run as `CARGO_BIN_EXE_<name>`, as term-ui's `demo` is (`crates/term-ui/tests/runner.rs:9-11`). Its `#[test]` goes.
- **C9.** The test puts console's switches back at the end, through a drop guard so a panic also restores them.
- **B12, confirm first.** Plant, locally, a writer that holds the program open in the test process. With `SPAWNS` and without, check whether ETXTBSY appears.
  - If it doesn't, the lock's comment says what it still serialises.
  - If the lock protects nothing, it goes from both files.

**Acceptance:**
- [ ] term.rs has no test that returns at once.
- [ ] console's switches equal their prior values after the test: asserted in a sibling test binary that reads them.
- [ ] The `SPAWNS` comment states what the plant showed.

**Test-first:** C9's restore check fails today.

**Verify:** `cargo test -p sb-tui --test term`, `-p sb-term-ui --lib` and `-p sb-host -p sb-repo`; `check.sh task <base>`.

**Reviews:** code-reviewer and library-reviewer (tui, term-ui, host).

**Floor guard:**
- **Reported:** `test-removed` (C8's no-op test); the reason goes in the commit.
- **Not expected:** a new `#[ignore]`; the binary route avoids it.

**Files:**
- `crates/tui/tests/term.rs`
- `crates/tui/Cargo.toml`
- a new `crates/tui/src/bin/` or `examples/` child
- `crates/term-ui/src/lib.rs`
- `crates/host/src/testing.rs`
- `crates/repo/tests/update.rs`

**Scope:** M. Split C8 (5.8a) from C9 and B12 (5.8b).

**Dependencies:** 5.3 (term.rs).

### Task 5.9: the gate takes one view of what changed

**Findings:** D15: `check.sh` uses `git diff "$base"` for the mutation diff and `changed_files`, while the floor guard (`floor-guard.mjs:46`) and diff-cover use the merge base.

**What:** check.sh refuses, with exit 2, a base that isn't an ancestor of HEAD (`git merge-base --is-ancestor`), naming it. Its documented use is "the commit the work started from".

**Acceptance:**
- [ ] `checkRun` cases: a side-branch base gives exit 2 and the message; an ancestor runs.

**Test-first:** the side-branch case runs today.

**Verify:** `node --test scripts/floor-guard.test.mjs`; `check.sh task <base>`.

**Reviews:** code-reviewer.

**Floor guard:** **reported:** the changed check.sh lines.

**Files:**
- `scripts/check.sh`
- `scripts/floor-guard.test.mjs`

**Scope:** XS.

**Dependencies:** 1.8 (ci.yml's base is always an ancestor after it).

### Checkpoint: complete

- [ ] The amd64 rehearsal passes.
- [ ] `check.sh full <batch start>`'s log ends `check full: all passed`.
- [ ] Every finding is fixed, or declined with its reason in a commit (table below).
- [ ] The standing agreements are walked: worktrees and images cleaned, and the fixtures, snapshots and data read against the Public repository rules.
- [ ] `scripts/check-commit-messages.sh 48f367ea HEAD` passes.
- [ ] The plan is archived under `tasks/test-hardening/`.

---

## Every finding, and its task

| Finding | Task | | Finding | Task |
|---|---|---|---|---|
| A1 | 1.4 | | C1 | 1.7 |
| A2 | 2.1 | | C2 | 3.9 |
| A3 | 1.1, 1.3, 1.5 | | C3 | 5.3 |
| A4 | 4.3 | | C4 | 3.8 (log refusals), 5.4 (the rest) |
| A5 | 4.2 | | C5 | 1.1, 1.3 (root), 2.6 (pci.ids) |
| A6 | 3.10 | | C6 | 4.1 |
| A7 | 5.1, 5.2 | | C7 | 4.6 |
| A8 | 2.2 (`SHELL`, cwd; one harness declined), 2.3 (git) | | C8 | 5.8 |
| A9 | 3.11, 4.7 | | C9 | 5.8 |
| A10 | 3.14 | | C10 | 3.12 |
| A11 | 2.7 | | C11 | 5.6 |
| A12 | 5.7 (docker_control fixed; help.rs declined) | | C12 | 2.1 (`CLICOLOR`), 5.5 |
| A13 | 5.7 (`env!`; isolation scan declined), 4.8 (orphans) | | D1 | 1.8 |
| B1 | 1.6 | | D2 | 1.9 |
| B2 | 1.6 | | D3 | 4.1 |
| B3 | 3.5 | | D4 | 2.4 |
| B4 | 3.6 | | D5 | 4.5 |
| B5 | 4.4 | | D6 | 3.7 |
| B6 | 2.5 | | D7 | 3.7 |
| B7 | 3.12 | | D8 | 1.1, 1.2, 1.3 |
| B8 | 3.8 | | D9 | 3.15, 4.8 |
| B9 | 3.13 | | D10 | 1.10 |
| B10 | 4.9 | | D11 | 4.5 |
| B11 | 1.3 | | D12 | 2.3 |
| B12 | 5.8 | | D13 | 2.8 |
| MD2/MD3 | 3.1-3.4 | | D14 | 3.12 |
| MD4 | 2.9 | | D15 | 5.9 |
| | | | D16 | 5.5 |

Declined, with the reason in the commit:
- A8's single PTY harness;
- A12's `help.rs` change;
- A13's isolation scan.

C10 and D14 are kept as change detectors, per Q6.

## Floor-guard approvals that may be asked for

None is written ahead. Each is asked for when the guard shows it.

| Task | Rule | Path | When |
|---|---|---|---|
| 1.2, 1.5, 3.14, 4.1 | `threshold-changed` (possible) | CONSTRAINTS.md, the Tests and Coverage rows | If a reworded row changes a number in it |
| 1.3 | `rule-changed` | CONSTRAINTS.md, the Floor bullet "No test made easier" | The bullet gains runtime skips; a tightening, but every word change is flagged |
| 2.9 | `rule-removed` | CONSTRAINTS.md, the Tools table's Go row | Go leaves the gate (MD4) |
| 2.9 | `threshold-removed` / `threshold-changed` (possible) | CONSTRAINTS.md, the Tools table's gitleaks row | The Install column loses `v8` and `@v8.30.1` |
| 3.13 | `licence-added` / `wrappers-widened` (not expected) | deny.toml, Cargo.lock | Only if a crate not already in the lock is added (tower-http already is) |
| 3.15, 4.8, 4.1 | `tool-config-changed` | `.config/nextest.toml` | The status levels, the leak timeout, and a coverage profile if one is added |
| 4.1 | `threshold-loosened` (only if) | check.sh `PROJECT_*_MIN` | Only if nextest's coverage reads below the floor and the maintainer chooses to reset it (that needs an ADR too) |
| 4.9, 5.6 | `check-disabled` / `threshold-changed` (avoidable) | the journal and motd test scripts | Only if an added line matches a disabler, or a `*_MIN`/`*_MAX` moves |

Reported, not blocking:
- `assertion-removed`: 2.4, 3.4, 3.8, 3.10-3.12, 4.3, 5.1, 5.3-5.5, 5.7;
- `test-removed`: 3.11, 5.7, 5.8;
- `gate-line-removed`: every check.sh change.

Each one's reason goes in its commit.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| The 26.04 follow miss is a real bug, not the old test's race | High | The streaming test (3.4) and the first-batch source test (3.3) run per release, and the rehearsal's 26.04 image runs them. A miss opens a debugging task; it is never retried away |
| Mutation on CI pushes (1.8) exceeds the 120-minute job limit on 4-vCPU runners | Med | Measure it in the batch-1 rehearsal; Q3 keeps mutation local, documented, if it doesn't fit |
| Coverage under nextest (4.1) reads different figures from the libtest run, which ADR-025's floors were measured with | Med | Print and compare at 4.1. A move below tolerance goes to the maintainer as a measurement change, never a quiet reset |
| Moving the coverage run to nextest drops the only run where tests share a process | Med | Q4: the release-build run stays under `cargo test` |
| Instrumented debug tests take more than 60 s under nextest | Med | Measure at 4.1. A coverage profile with a longer end is the fallback (a `tool-config-changed` approval) |
| The gitleaks download in CI depends on GitHub release availability | Low | `curl --retry 3`. The same holds for every other download in ci.yml |
| The host's go-built gitleaks fails 2.9's version read | Med | Q7: the release binary is installed before 2.9 lands, or the gate exits 2 on the version |
| Batches touch the same files (`term.rs`, `logs.rs`, `check.sh`, `floor-guard.test.mjs`) | Low | The dependencies above serialise them. Work in main's tree, one task at a time. No parallel worktrees unless the maintainer asks |

## The plan's questions, decided

The maintainer, 2026-11-26. Each was one decision; the answer and the
alternative weighed are kept.

- **Q1. How the gate reports skips:** the runtime helper (1.1) plus
  check.sh's per-run skip summary.
  - *Not chosen:* `#[ignore = "needs root"]` with `--run-ignored all` and `--include-ignored`. That is the Rust Book's convention, and nextest and libtest then list skips themselves. But it needs a `test-made-easier` approval for each of about 330 tests, and changes every test command in check.sh.
- **Q2. Where the systemd hangup test runs** (it needs systemd, which the
  sandbox hides): in `check.sh full`, when `crates/` or `scripts/` changed
  (root and Docker are there already).
  - *Not chosen:* a GitHub job on push (the runners have systemd; not rehearsable); or it stays by hand.
- **Q3. Whether CI runs the mutation passes on a push**, over
  `github.event.before..HEAD`, on every runner: yes, if batch 1's
  measurement (Task 1.8's rehearsal) shows it fits the 120-minute job
  limit. Otherwise mutation stays local: CI runs coverage on a push,
  mutation is judged by the local gate only, and ci.yml and CONSTRAINTS.md
  say so.
- **Q4. The run where tests share a process.** Moving coverage to
  `cargo llvm-cov nextest` leaves no run with libtest's thread per test,
  which catches shared-process races (signal listeners, tracing interest,
  `OnceLock` state). One is kept: the release-build run under
  `cargo test --profile release-tests`, inside GNU `timeout` naming the
  step. nextest's per-test end then holds for the coverage run alone.
  - *Not chosen:* accept the loss.
- **Q5. The rehearsal's triggers (D13):** the list in Task 2.8:
  `Cargo.lock`, the manifests, `build.rs`, and `src/` test and testing
  files.
  - *Not chosen:* any change outside `docs/` and `tasks/`. That is simpler and complete, but rehearses nearly every `full`.
- **Q6. Tests that restate constants (B7, C10, D14):** kept as stated
  change detectors, with a comment naming the source they mirror, and no
  ADR.
  - *Not chosen:* an ADR that allows `mutants::skip` with an Exceptions row for constant definitions, and drop the pins.
- **Q7. gitleaks on the gate host:** the gate host gets gitleaks' release
  binary through the new `scripts/install-gitleaks.sh` (Task 2.9), in the
  directory of today's `command -v gitleaks`, in place of the
  `go install` build. Go stays on the host.
- **Q8. Commit cadence:** `check.sh task` before each commit, and one
  `check.sh full` plus the amd64 rehearsal at each batch's end. At `full`
  the gate rehearses whenever `crates/*/tests/` or `scripts/` changed,
  which is nearly every task here; this rehearses once per batch.
  - *Not chosen:* `full` before each commit. Each commit's own diff is then gated at `full`, at a rehearsal per task.
- **Q9. The base of the batch-end `full`:** the batch's start commit, so
  the batch's whole diff is judged once. `full HEAD` on a clean tree would
  check no changed lines: no diff-cover, no mutation and no commit
  messages, which is D1's own finding.
