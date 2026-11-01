# term-ui tasks

Plan: [tasks/plan.md](plan.md). Spec: [docs/specs/term-ui.md](../docs/specs/term-ui.md).
Every task: TDD where it changes behaviour, a code review (and the
library-reviewer for terminal code) before committing, `scripts/check.sh
full` before committing, Library-check and Review lines in each commit,
and the binary size measured: a change past the 5% ratchet tolerance
carries its ADR in the same commit (CONSTRAINTS.md).

Carried forward (open items owned by later work):
- From cli-core:
  - Task 5: the root relaunch path in `main` first runs end to end with
    the first root-needing command (`sb docker ps`); two of its mutants
    are equivalent until then.
  - Task 3: the rexpect PTY smoke test on the built binary moves to the
    first long-running command.
  - Criterion 1: tab completion comes with the completion module.
  - Criterion 4: the terminal restore is checked in Task 8 here.
  - The exec module removes `SB_RELAUNCHED` from child processes
    (`env_remove`), or an unprivileged sb started by one would refuse to
    relaunch (root.rs module docs).
  - sb-go exports COLORTERM for child processes (truecolor, 256color,
    unset for ansi and ascii); decide when the exec module starts children.
  - Check sudoers(5) for `--preserve-env` names outside env_keep; record
    the answer in docs/platform-notes/.
  - The first command that takes argument values (`install -e`) adds an
    end-to-end test that a real secret never reaches the run log.
  - Help: fang's 120-column wrap of long abouts comes with the first ported
    command whose long text needs it (cli-core Task 11).
  - Under `SB_COLOR_PROFILE=ansi` the palette's OK green maps to the same
    light grey as muted text (help's flags, H1's fixes); see Task 12.
- From term-ui's spec: prompts (criterion 10) are built with their first
  user, self-update or git's confirm (decision 1).
- From this plan: the first runner command builds sb's tokio runtime after
  argument parsing (help and version never pay for it) and keeps the
  startup budget; the demo builds its own.
- From the prototypes: the install script (tasks/prototypes/todo.md,
  "Later") depends on cli-core and the runner, so this plan unblocks it.

## Task 1: No-blink spike for nested and concurrent steps

**Description:** Extend proto-runner (evidence only) to run async steps on
tokio with nested sub-steps (two levels) and two concurrent siblings,
drawing through one fixed pool of indicatif bars sized for that shape, with
the newest finished line kept live until the next step or the end (decision
3), and info/warning notices under steps. Measure line erases as
proto-runner did, and how indicatif's ticker thread behaves next to tokio.
FINDINGS records the results, what the runner's API must allow, and the
questions for Checkpoint A: what happens to the other siblings when one
fails (sb-go's errgroup cancels them) and on Ctrl-C with two steps
running; whose output tail and which pointer a failure shows.

**Acceptance criteria:**
- [x] Erases measured for sequential, nested and concurrent runs with
      success, failure and Ctrl-C endings, in a PTY; ticker behaviour noted
      (not tested: frames come from a tokio task; raised as a question).
- [x] The tree draws correctly (glyphs, last branch, times column) in all
      three; FINDINGS lists the API needs and the open questions.

**Verification:**
- [x] `scripts/try-prototype.sh runner-spike` shows the new modes; the
      runner lookbook (artifact) replays them with the design options.
- [x] Manual: the maintainer reviewed it through the lookbook (Checkpoint A,
      2026-11-01; spec decision 7).

**Dependencies:** None
**Files:** `prototypes/proto-runner/{Cargo.toml,src/main.rs,FINDINGS.md}`, `scripts/try-prototype.sh`
**Scope:** M

## Task 2: `sb-term-ui` crate, colour hook-up and theme adapters

**Description:** Create `crates/term-ui` (`sb-term-ui`) on `sb-cli-core`.
One function, called from sb's `main` right after the real parse's colour
choice is written (`cli.color.write_global()`), sets console's stdout and
stderr switches from anstream's resolved
choice per stream (both off under `ascii`). Adapters turn a theme role
into styled text (anstyle; console's `Style` writes bright 16-colours as
256-colour codes, spec "Colour on every inline path") and a tabled colour;
nothing under `ascii` or with colour off. deny.toml: the
cli-core entry becomes `wrappers = ["sb", "sb-term-ui"]` and a new
`{ crate = "sb-term-ui", wrappers = ["sb"] }` entry is added.

**Acceptance criteria:**
- [x] console's switches follow the choice per stream (unit tests with an
      injected choice: nothing in sb draws through console yet); help exits
      before the hook-up, and for `version` it only sets the two switches;
      the `sb --help` and `sb version` medians hold.
- [x] Per-profile guard tests on the adapters (24-bit, 256, 16, none).
- [x] `cargo deny` passes with the new architecture entries.

**Verification:**
- [x] `cargo test -p sb-term-ui`; `scripts/check.sh full` (size measured).

**Dependencies:** None
**Files:** `crates/term-ui/{Cargo.toml,src/lib.rs,src/style.rs}`, `crates/sb/{Cargo.toml,src/main.rs}`, `deny.toml`, `Cargo.toml`
**Scope:** M

## Task 3: cli-core runs restore hooks before the second-signal exit

**Description:** Decision 5 (cli-core spec amended): the second SIGINT or
SIGTERM is handled on cli-core's signals thread, which runs registered
restore hooks, then exits 130 or 143, instead of exiting in the handler.
Hooks can be removed (a runner that has ended mustn't restore stale
terminal state later), and a panicking hook can't stop the exit; a hook
that blocks is abandoned after a 500 ms deadline and sb exits anyway.
Hooks stay tiny (write a few bytes, one `tcsetattr`).

**Acceptance criteria:**
- [x] A registered hook runs once before the second-signal exit, code
      unchanged; a removed hook doesn't run; a panicking hook doesn't stop
      the exit.
- [x] Without hooks behaviour is as today; the signal tests still pass.

**Verification:**
- [x] cli-core's signal tests plus new ones (re-exec child);
      `scripts/check.sh full`.

**Dependencies:** None
**Files:** `crates/cli-core/src/cancel.rs`, `crates/cli-core/tests/signals.rs`
**Scope:** S

## Task 4: Runner API design

**Description:** Design the `Runner`/`Step` API from Task 1's findings
with `agent-skills:api-and-interface-design`, as the spec asks: async
steps given the token, labels, notices, output lines and sensitive output,
nested and concurrent steps with the semantics settled at Checkpoint A,
the verbose option (sb has no `-v` yet; commands pass it), and runtime
ownership (the command's tokio runtime, built after argument parsing; the
runner uses the current one). Written as rustdoc'd signatures in the
spec's code-style section, marked "proposed" until the maintainer approves
it at Checkpoint A, reviewed before any runner code.

**Acceptance criteria:**
- [x] The API covers every runner behaviour in the spec, including nested
      and concurrent steps, failure and interruption.
- [x] Reviewed (three rounds).
- [x] Approved by the maintainer at Checkpoint A (2026-11-01).

**Verification:**
- [x] Review.

**Dependencies:** Task 1
**Files:** `docs/specs/term-ui.md`
**Scope:** S

## Checkpoint A: Foundation
- [x] Tasks 1-4 committed; `scripts/check.sh full` passes.
- [x] The maintainer has seen the spike and the look of notices and
      settled the concurrency questions (2026-11-01, spec decision 7).
- [x] The maintainer has approved the runner API (Task 4), anstyle styling,
      the new dependencies and the current-thread runtime (2026-11-01).

## Task 5: Sequential runner: tree, labels, async steps, plain mode

**Description:** The runner core on Task 4's API: a root line, async steps
with the token, running/done/failure labels, finished lines printed for
good with the newest kept live (decision 3), times in one column, spinner
and label in the accent colour; plain mode (verbose option, or stderr not a
terminal) prints start and end lines. The last branch's `╰` comes from
termtree's `GlyphPalette` with a custom last item (checked at source,
cited; no ratatui). Snapshots are taken from proto-runner's reviewed
output at a fixed width, so the new code is checked against the reviewed
look. Snapshots draw on indicatif's `InMemoryTerm` (`Target::InMemory`,
behind term-ui's `testing` feature); the no-erase and ending checks run in
a PTY. A `demo` binary shows the runner (a bin, not an example: the
coverage run builds bins for integration tests, not examples); it
logs only under `SB_LOG_DIR` in a temporary directory, never /var/log/sb.

**Acceptance criteria:**
- [x] Rendering matches the reviewed tree snapshots, times aligned; 0 line
      erases while running.
- [x] Plain mode prints start and end lines; no escape codes when not a
      terminal with `auto`; per-profile guard tests on runner output.

**Verification:**
- [x] `cargo test -p sb-term-ui`; `cargo run -p sb-term-ui --bin demo`;
      `scripts/check.sh full` (size measured; the demo's lines and diff-cover
      checked).

**Dependencies:** Checkpoint A
**Files:** `crates/term-ui/{Cargo.toml,src/runner.rs,src/bin/demo.rs,tests/runner.rs}`, `Cargo.toml` (indicatif, console, termtree, tokio minimal features, anyhow, tracing; insta and indicatif's `in_memory` through the `testing` feature, dev only)
**Scope:** M

## Task 6: Output lines, live tail, notices and run-log records

**Description:** Steps stream output lines: a live tail of the newest 8,
capped at half the terminal height, keeping each line's colours; notices
(info, warning) stay under their step. Plain mode prints output lines
(escape codes stripped with colour off) and notices. Every step's start,
outcome, elapsed time, notices and output lines go to the run log in the
run's span; output a step marks sensitive is never logged.

Carried from Task 5's review: notices are not capped in the live area
(running tails are), so many warnings could make it taller than the
terminal; decide and test a cap here. Stream output marks the tree dirty
and redraws on frames rather than per line, if the volume needs it.

**Acceptance criteria:**
- [x] Tail length and cap; notices' look as settled at Checkpoint A; plain
      mode includes output lines and notices.
- [x] Run log holds each step's record; a known secret marked sensitive is
      absent (CONSTRAINTS.md logging rule).

**Verification:**
- [x] `cargo test -p sb-term-ui`; `scripts/check.sh full`.

**Dependencies:** Task 5
**Files:** `crates/term-ui/{src/runner.rs,tests/runner.rs,tests/run_log.rs}`
**Scope:** M

## Task 7: Failure block

**Description:** A failed step shows ✗, its failure label, every line of
its error (indented), its last 10 output lines, then the log pointer when
the run is logged; its own log record carries the error and causes; the
run returns a quiet `Exit(1)` so cli-core doesn't repeat it. The demo ends
through the same cli-core functions sb's `main` uses (`log::outcome`,
`exit::finish_run`, `RunLog::pointer`), so a PTY test on the demo shows
criterion 4 ("shown once, exit 1") end to end.

Carried from Task 6's review: a failed step's own record carries its
error and causes here (Task 6 logs only the outcome, at WARN), through
cli-core's `Sensitive` check; the tree keeps output lines without their
sensitive flag, so the failure block's last lines must not be logged from
the tree (or the tree carries the flag).

**Acceptance criteria:**
- [ ] The failure block as specified, multi-line errors included; no
      pointer when the run isn't logged.
- [ ] Exit 1 with the error shown once (PTY test on the demo).
- [ ] Plain mode ends a failed step with ✗, the error and the pointer.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; `scripts/check.sh full`.

**Dependencies:** Task 6
**Files:** `crates/term-ui/{src/runner.rs,src/bin/demo.rs,tests/failure.rs}`
**Scope:** S

## Task 8: Interruption and terminal restore

**Description:** Ctrl-C or SIGTERM marks the running step interrupted
(`Warn`), keeping its last lines, with no pointer of its own; cli-core's
block then shows "interrupted" and the pointer once. A command's own
`Shutdown::request` is not an interruption. `ECHOCTL` is switched off while
live on the terminal sb draws on (stderr's, or `/dev/tty`), only in the
foreground process group, and restored at every ending; the runner
registers its restore with Task 3's hook and removes it when it ends. This
closes cli-core's carried criterion 4.

**Acceptance criteria:**
- [ ] SIGINT gives 130, SIGTERM 143; "interrupted" shown once; a requested
      shutdown isn't marked interrupted.
- [ ] Cursor and `ECHOCTL` restored after one signal and after two; a
      background run doesn't touch the terminal (PTY tests on the demo).

**Verification:**
- [ ] `cargo test -p sb-term-ui` (PTY tests); `scripts/check.sh full`.

**Dependencies:** Tasks 3, 7
**Files:** `crates/term-ui/{src/runner.rs,src/tty.rs,tests/signals.rs}`, `Cargo.toml` (rustix `termios`)
**Scope:** M

## Task 9: Nested and concurrent steps

**Description:** Sub-steps (a deeper branch) and concurrent siblings in
the shape and with the semantics settled at Checkpoint A, kept in start
order; plain mode prefixes concurrent steps' lines with their label;
concurrent steps' log records carry the run's span (tokio's `Instrument`,
cited).

**Acceptance criteria:**
- [ ] Nested and concurrent runs render as settled, with the erase result
      Task 1 accepted; plain mode stays readable.
- [ ] A sibling failing while another runs, and SIGINT with two steps
      running, behave as settled at Checkpoint A.
- [ ] Concurrent steps' run-log records carry the run span.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; the demo's nested and concurrent modes;
      `scripts/check.sh full`.

**Dependencies:** Task 8
**Files:** `crates/term-ui/{src/runner.rs,tests/runner.rs,src/bin/demo.rs}`
**Scope:** M

## Checkpoint B: Runner
- [ ] Tasks 5-9 committed; `scripts/check.sh full` passes.
- [ ] The maintainer has tried the runner demo in a real terminal.

## Task 10: Tables in the reviewed `docker ps` look

**Description:** A table type over tabled: rounded style, a rule between
rows, bold centred headings, the border in the `Border` role, per-cell
roles and alignment from the caller, an optional maximum column width,
fitted to the terminal (console's size on stdout), natural width when not
a terminal.

**Acceptance criteria:**
- [ ] Matches proto-inline's reviewed table (snapshot from its output) and
      fits 120, 70 and 50 columns.
- [ ] Not a terminal: natural width, no escape codes; `--color=never` on a
      terminal: no SGR codes, bold included; `--color=always` piped: the
      theme's colours; per-profile guard tests.

**Verification:**
- [ ] `cargo test -p sb-term-ui`; the demo's table; `scripts/check.sh full`.

**Dependencies:** Task 5 (the demo and insta)
**Files:** `crates/term-ui/{Cargo.toml,src/table.rs,tests/table.rs,src/bin/demo.rs}`, `Cargo.toml` (tabled)
**Scope:** S

## Task 11: Success criteria and measurements

**Description:** Walk the spec's success criteria 1-9 with evidence and
record startup and binary size. Nothing in sb calls the runner or tables
until `sb docker ps`, so link-time optimisation leaves most of term-ui out
of the binary: unless an earlier task already crossed the tolerance (and
wrote its ADR), the size ADR for term-ui's growth comes with the first
command that uses it, and this task records that.

**Acceptance criteria:**
- [ ] Each success criterion has a test or a recorded manual check, or is
      named as carried (criteria 4 and 6 beyond the demo, and criterion 9's
      size ADR unless an earlier task crossed the tolerance: the first runner
      command).
- [ ] Size and startup recorded; any ADR written where the tolerance was
      crossed.

**Verification:**
- [ ] `scripts/check.sh full`; the maintainer reviews the demo.

**Dependencies:** Tasks 9, 10
**Files:** `tasks/todo.md`, `docs/decisions/` (if an ADR is due)
**Scope:** S

## Task 12: 16-colour OK green (maintainer decision)

**Description:** Carried from cli-core: under `SB_COLOR_PROFILE=ansi` the
OK green maps to the same grey as muted text. Ask the maintainer whether
to give 16-colour mode an explicit colour per role (a change to
cli-core's theme and a reviewed look), now that term-ui adds colour users.

**Acceptance criteria:**
- [ ] The maintainer's decision recorded; if yes, the mapping changed with
      guard tests.

**Verification:**
- [ ] `scripts/check.sh full` if code changed.

**Dependencies:** Task 11
**Files:** `crates/cli-core/src/theme.rs` (if yes)
**Scope:** XS

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes.
- [ ] Spec success criteria met or carried by name; the maintainer has
      reviewed the runner and tables.
