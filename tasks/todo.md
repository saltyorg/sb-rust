# Tasks: `cli-core`

Plan: [tasks/plan.md](plan.md). Spec: [docs/specs/cli-core.md](../docs/specs/cli-core.md).
Prototype tasks and their open items: [tasks/prototypes/todo.md](prototypes/todo.md).

Every task follows CONSTRAINTS.md: test first, `scripts/check.sh fast`
after edits, `task` before handing back, `full` before committing, a
`Library-check:` line in each commit changing Rust, and the library-reviewer
agent before committing terminal, process or I/O code.

## Task 1: Workspace, `sb` binary and `sb version`

**Description:** Add `crates/*` to the workspace with `sb-cli-core`
(library) and `sb` (binary). `sb version` prints
`Saltbox CLI version: <v> (commit: <c>)`; `--json` prints
`{"version","git_commit","uv_version"}`, as sb-go. Version from the package,
commit and uv version set at build time (approach confirmed at source).
Add the cargo-deny `wrappers` entry for `sb-cli-core` and record the first
measurements in CONSTRAINTS.md's "Measured" table.

**Acceptance criteria:**
- [x] `sb version` and `sb version --json` match sb-go's output shape.
- [x] `scripts/check.sh full` runs every step (none skipped) and passes.
- [x] Startup, binary size and project coverage are recorded in CONSTRAINTS.md.

**Verification:**
- [x] `cargo test -p sb -p sb-cli-core`; CLI tests with `assert_cmd`.
- [x] `scripts/check.sh full`.

**Dependencies:** None
**Files:** `Cargo.toml`, `crates/cli-core/{Cargo.toml,src/lib.rs,src/build_info.rs}`, `crates/sb/{Cargo.toml,src/main.rs,tests/version.rs}`, `deny.toml`, `CONSTRAINTS.md`
**Scope:** M

## Task 2: Exit codes and the error block

**Description:** One exit-code policy (0, 1, a requested code, a passed-
through child code; 130/143 come in Task 3) and one error block on stderr:
`✗` and the message, each cause on its own line, then the log pointer when
there is one. Errors already shown exit quietly.

**Acceptance criteria:**
- [x] An error prints one block with its causes and exits 1.
- [x] A command can request its own exit code; a quiet error prints nothing more.
- [x] A parse error keeps clap's message and exit code.

**Verification:**
- [x] Unit tests for formatting and the code mapping; CLI test for a parse error.
- [x] `scripts/check.sh task`.

**Dependencies:** Task 1
**Files:** `crates/cli-core/src/{exit.rs,error.rs,lib.rs}`, `crates/sb/src/main.rs`, `crates/sb/tests/errors.rs`
**Scope:** S

## Task 3: Cancellation on SIGINT and SIGTERM

**Description:** A `CancellationToken` reaches every command. The first
SIGINT or SIGTERM cancels it; a second exits at once with the same code
(130 or 143). Cancellation shows "interrupted", not an error chain. A
command can request shutdown with its own code.

**Acceptance criteria:**
- [x] SIGINT exits 130 and SIGTERM 143; a second signal exits immediately.
- [x] An interrupted run shows "interrupted". Terminal restore moves to the
      first command that changes the terminal (term-ui), where it can be
      tested.
- [x] Commands receive the shutdown handle (and its token).

**Verification:**
- [x] Signal tests against a real process (the test binary re-runs itself
      as the child). The rexpect PTY smoke test on the built binary moves to
      the first long-running command: `sb version` ends before a signal can
      reach it.
- [x] `scripts/check.sh task`; library-reviewer on the diff.

**Dependencies:** Task 2
**Files:** `crates/cli-core/src/{cancel.rs,lib.rs}`, `crates/cli-core/tests/{sigint.rs,sigterm.rs}`, `crates/sb/tests/interrupt_pty.rs`
**Scope:** M

## Checkpoint A: Foundation
- [x] `scripts/check.sh full` passes; `sb version` works on this host.
- [x] Review with the maintainer before Phase 2. (2026-10-30)

## Task 4: Host support check hook (`sb-host`)

**Description:** `cli-core` runs a startup hook before anything else.
Start `crates/host` with only the OS release check: supported Ubuntu
releases as sb-go's `layout` lists them, `/etc/os-release` parsed with a
library if one fits (confirmed at source), sb-go's messages. Every command
is blocked on an unsupported host, `--help` and `version` included.

**Acceptance criteria:**
- [x] On a supported host every command runs; otherwise each exits 1 with sb-go's message.
- [x] The decision is tested with injected values (sysinfo reads the real
      `/etc/os-release` at a fixed path); cargo-deny `wrappers` allow only
      `sb` to use `sb-host` for now. The cli-core startup function that
      takes the check as a hook lands with the wiring in Task 10.

**Verification:**
- [x] Unit tests for supported, unsupported, not Ubuntu and missing version.
- [x] `scripts/check.sh full`.

**Dependencies:** Task 2
**Files:** `crates/host/{Cargo.toml,src/lib.rs}`, `crates/cli-core/src/startup.rs`, `crates/sb/src/main.rs`, `deny.toml`
**Scope:** M

## Task 5: Root relaunch for commands that need it

**Description:** Each command declares `needs_root` in an exhaustive match
(no default). When needed and not root, exec
`sudo --preserve-env=VISUAL,EDITOR,SB_RELAUNCHED <exe> <args>`; otherwise
run as the user. sudo's own prompt handles passwords; a relaunched run that
still isn't root stops instead of looping.

**Acceptance criteria:**
- [x] `version`, `--help` and completion never relaunch; a root-needing command does, with the same arguments.
- [x] Exec replaces the process: the exit code and signals belong to the real run.
- [x] A relaunched run that still isn't root stops instead of looping
      (found when a mutant looped through the real sudo; see the commit).

**Verification:**
- [x] Unit tests for the decision and `ensure_root` (injected exec); the
      real exec tested with a fake `sudo` on `PATH` (records arguments and
      the marker, exits 7); `version` and `--help` run as `nobody` with a
      fake sudo that would show as exit 42.
- [x] `scripts/check.sh task`; library-reviewer on the diff.
- Open until the first root-needing command (`sb docker ps`): two
  mutants are equivalent today (`needs_root` always false, effective UID
  always 0) and the relaunch path in `main` runs end to end only then.

**Dependencies:** Task 4
**Files:** `crates/cli-core/src/{root.rs,command.rs}`, `crates/cli-core/tests/relaunch.rs`
**Scope:** S

## Task 6: Run log on the host

**Description:** Root runs log to `/var/log/sb` (directory injectable for
tests), rotated daily by tracing-appender (`max_log_files`; since
2026-10-30 one `sb.log` rotated by Saltbox's logrotate), with a run
span carrying the run id, the command's name (never its arguments), each
event plain text, and the outcome at the end. Unprivileged runs don't log. A log that can't be
opened is a warning. The error block's pointer names the log and run id.

**Acceptance criteria:**
- [x] A root run writes its start, events and outcome; failures point at the log.
- [x] Argument values are never logged (they can carry secrets): the
      record holds the command's name; an end-to-end test checks an
      argument never reaches the log (CONSTRAINTS.md, Logging).
- [x] Unwritable log directory: a warning and the run continues.

**Verification:**
- [x] Tests against temporary directories (root tests gated on
      `SB_TEST_ROOT=1`).
- [ ] The real `/var/log/sb`: moved to Task 10's manual verification, run
      only with the maintainer's go-ahead (an automatic test would create
      it on this host at every check run).
- [x] `scripts/check.sh task`; library-reviewer on the diff.

**Dependencies:** Task 5
**Files:** `crates/cli-core/src/{log.rs,error.rs}`, `crates/cli-core/tests/run_log.rs`
**Scope:** M

Carried forward from the cli-core code review (2026-10-30):
- The exec module removes `SB_RELAUNCHED` from child processes
  (`env_remove`), or an unprivileged sb started by one would refuse to
  relaunch (root.rs module docs).
- sb-go also exports COLORTERM for child processes (truecolor, 256color,
  unset for ansi and ascii); sb-rust doesn't yet. Decide when the exec
  module starts children (ansible).
- Check sudoers(5): a policy without `setenv` may refuse `--preserve-env`
  names outside env_keep (VISUAL and EDITOR carry the same risk); record
  the answer in docs/platform-notes/.
- The first command that takes argument values (for example
  `install -e`) adds an end-to-end test that a real secret value never
  reaches the run log; today's test uses a flag.

## Checkpoint B: Startup
- [ ] `scripts/check.sh full` passes.
- [ ] Review with the maintainer before Phase 3.

## Task 7: `--color` on every output path, including help

**Description:** `colorchoice-clap` flag; its choice becomes anstream's
global one before the real parse, through a clap pre-pass, so clap's help
follows it. console and crossterm get `always`/`never` when term-ui brings
them in.
`auto` honours NO_COLOR and terminal detection.

**Acceptance criteria:**
- [x] `--color=never` gives no escape codes anywhere, help included; `--color=always` colours piped output.
- [x] `NO_COLOR` turns colour off unless `--color=always`.

**Verification:**
- [x] CLI tests piped and in a PTY.
- [x] `scripts/check.sh task`.

**Dependencies:** Task 2
**Files:** `crates/cli-core/src/{colour.rs,lib.rs}`, `crates/sb/tests/colour.rs`
**Scope:** S

## Task 8: Colour profiles (ADR-002)

**Description:** Move the palette into `sb-cli-core::theme` as role names.
`SB_COLOR_PROFILE` (`truecolor`, `ansi256`, `ansi`, `ascii`; unknown warns
and means truecolor) resolves every role once with anstyle-lossy into an
`anstyle::Color`, installed as the run's theme. sb's error block is its
first user, through anstream. The ratatui, console and tabled adapters
come with term-ui and tui, which bring those libraries.

**Acceptance criteria:**
- [x] Each profile's output uses only its kind of colour code (guard tests per profile).
- [x] Nothing outside the resolver converts colours.

**Verification:**
- [x] Profile guard tests; `scripts/check.sh task`; library-reviewer on the diff.

**Dependencies:** Task 7
**Files:** `crates/cli-core/src/{theme.rs,exit.rs,root.rs}`, `crates/sb/src/main.rs`, `crates/sb/tests/colour.rs`
**Scope:** M

## Task 9: Theme config

**Description:** Read `<server_appdata_path>/sb/config.toml`
(`server_appdata_path` from the Saltbox inventory, `/opt` by default): a
named built-in theme (`default`) plus per-role overrides, and an optional
`color_profile` (the environment variable wins). Missing means built-in;
invalid warns (and logs) and falls back.

**Acceptance criteria:**
- [x] A theme file changes the resolved palette; no file means the built-in theme.
- [x] An invalid file or unreadable inventory warns and falls back; a moved `server_appdata_path` is followed.

**Verification:**
- [x] Tests with temporary inventory and config files.
- [x] `scripts/check.sh task`.

**Dependencies:** Task 8
**Files:** `crates/cli-core/src/{config.rs,theme.rs}`, `crates/sb/src/main.rs`, `crates/sb/tests/theme_config.rs`
**Scope:** M

## Task 10: Startup wiring, contract test and success criteria

**Description:** `sb` runs the steps in the spec's order. (Planned as one
call into `cli-core`; kept in `sb`'s `main` instead: a generic startup
would need a trait over the clap type with sb as its only user, and the
order is held by the end-to-end tests. Each step is a cli-core function.)
A contract test compares sb-rust's commands and flags with
sb-go's (every sb-rust name exists in sb-go). Walk the spec's success
criteria.

**Acceptance criteria:**
- [x] Spec success criteria 2, 5, 6 (with `SB_TEST_ROOT=1`), 7, 8 and 9 pass; 3 is covered by Task 5's test until a root command exists.
- [ ] Criterion 1: help and version as a normal user pass; tab completion
  comes with the completion module.
- [ ] Criterion 4: SIGINT 130 and SIGTERM 143 pass; nothing draws to the
  terminal yet, so the restore is checked when term-ui lands.
- [x] The `help` subcommand: sb-go removes it; the maintainer chose to keep
  clap's `help` listed (spec updated, 2026-10-30).
- [x] Startup budget holds: `sb --help` and `sb version` median ≤ 10 ms (0.76 and 0.90 ms).

**Verification:**
- [x] `scripts/check.sh full`.
- [ ] Manual: `sb --help`, `sb version`, Ctrl-C and `--color` on this host, shown to the maintainer.
- [x] Manual, with the maintainer's go-ahead: root runs against the real
      `/var/log/sb` (created root-owned 0700, record written; kept). A
      failed root run (`sb version >/dev/full`) printed the pointer with
      the real file and run id, and the log recorded the failure.

**Dependencies:** Tasks 3, 6, 9
**Files:** `crates/sb/src/{main.rs,contract.rs}`, `crates/sb/tests/fixtures/sb-go-command-tree.tsv`, `crates/cli-core/src/config.rs` (inventory read with saphyr)
**Scope:** M

## Task 11: Help in sb-go's format

**Description:** At Checkpoint C the maintainer found clap's default help
far behind sb-go's; proto-help compared the options and the maintainer
chose sb-go's format (look 6). sb draws each command's help from clap's
command data in that format, in the palette, and sets it with clap's
`Command::override_help`, so `-h`, `--help` and `sb help <cmd>` keep
working (spec: Help and parse errors).

**Acceptance criteria:**
- [x] `sb --help`, `sb version --help` and `sb help version` show the
      format: about, headings, shaded usage block (only with colour),
      commands with hints, flags with defaults, one column.
- [x] Colours from the theme; none with `--color=never`, NO_COLOR, piped
      or under `ascii`.
- [x] Startup budget holds (help median ≤ 10 ms; the ratchet value is
      re-measured).

**Verification:**
- [x] Unit tests on the rendering (fixed commands, colour on and off);
      end-to-end help tests; `scripts/check.sh full`.
- [ ] Manual: the maintainer compares `sb --help` with sb-go's.

Deferred: fang wraps the about at min(terminal, 120) columns; today's
abouts are one line, so the wrap comes with the first ported command
whose long text needs it. `sb help help` stays clap's own format.

**Dependencies:** Tasks 7, 8
**Files:** `crates/cli-core/src/help.rs`, `crates/sb/src/main.rs`, `crates/sb/tests/help.rs`
**Scope:** M

## Task 12: Parse errors in sb-go's layout

**Description:** clap's parse errors (unknown command or flag, bad value)
are drawn in sb-go's layout (an ERROR badge, chosen by the maintainer over
a `✗` block; look H1: `hint:` lines, the typed value in the problem
colour, fixes and possible values in green, no help pointer), on stderr,
exit 2 (maintainer: the
usage-error convention, a deliberate difference from sb-go's 1). Not
through a clap `ErrorFormatter`: clap builds its messages in private code,
so sb keeps clap's wording and re-lays out its plain rendering, with the
usage paragraph removed through `Error::remove(ContextKind::Usage)`
(recorded in prototypes/proto-help/FINDINGS.md).

**Acceptance criteria:**
- [x] An unknown command, an unknown flag and a bad value each show one
      ERROR block with clap's message and tip, exit 2.
- [x] `--color` and the profile apply as for help.

**Verification:**
- [x] End-to-end tests; `scripts/check.sh full`.
- [ ] Manual: the maintainer compares with sb-go's error.

**Dependencies:** Task 11
**Files:** `crates/cli-core/src/help.rs` (or `exit.rs`), `crates/sb/src/main.rs`, `crates/sb/tests/errors.rs`
**Scope:** S

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes.
- [ ] Spec success criteria met; the maintainer reviews `sb --help`, `sb version` and Ctrl-C.
