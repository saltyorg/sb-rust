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

**Description:** Each command declares `needs_root` (default yes). When
needed and not root, exec `sudo --preserve-env=VISUAL,EDITOR <exe> <args>`;
otherwise run as the user. sudo's own prompt handles passwords.

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
tests), rotated daily by tracing-appender (`max_log_files`), with a run
span carrying the run id and arguments, each event plain text, and the
outcome at the end. Unprivileged runs don't log. A log that can't be
opened is a warning. The error block's pointer names the log and run id.

**Acceptance criteria:**
- [ ] A root run writes its start, events and outcome; failures point at the log.
- [ ] A test feeds a known secret through the logging path and asserts it never appears (CONSTRAINTS.md, Logging).
- [ ] Unwritable log directory: a warning and the run continues.

**Verification:**
- [ ] Tests against a temporary directory; `SB_TEST_ROOT=1` test for the real path.
- [ ] `scripts/check.sh task`; library-reviewer on the diff.

**Dependencies:** Task 5
**Files:** `crates/cli-core/src/{log.rs,error.rs}`, `crates/cli-core/tests/run_log.rs`
**Scope:** M

## Checkpoint B: Startup
- [ ] `scripts/check.sh full` passes.
- [ ] Review with the maintainer before Phase 3.

## Task 7: `--color` on every output path, including help

**Description:** `colorchoice-clap` flag; `always`/`never` passed on to
anstream, console and crossterm; clap's help follows it (`Command::color`).
`auto` honours NO_COLOR and terminal detection.

**Acceptance criteria:**
- [ ] `--color=never` gives no escape codes anywhere, help included; `--color=always` colours piped output.
- [ ] `NO_COLOR` turns colour off unless `--color=always`.

**Verification:**
- [ ] CLI tests piped and in a PTY.
- [ ] `scripts/check.sh task`.

**Dependencies:** Task 2
**Files:** `crates/cli-core/src/{colour.rs,lib.rs}`, `crates/sb/tests/colour.rs`
**Scope:** S

## Task 8: Colour profiles (ADR-002)

**Description:** Move the palette into `sb-cli-core::theme` as role names.
`SB_COLOR_PROFILE` (`truecolor`, `ansi256`, `ansi`, `ascii`; unknown warns
and means truecolor) resolves every role once with anstyle-lossy into an
`anstyle::Color`, with adapters for ratatui (ratatui-core `anstyle`),
console, tabled and anstream.

**Acceptance criteria:**
- [ ] Each profile's output uses only its kind of colour code (guard tests per profile).
- [ ] Nothing outside the resolver converts colours.

**Verification:**
- [ ] Profile guard tests; `scripts/check.sh task`; library-reviewer on the diff.

**Dependencies:** Task 7
**Files:** `crates/cli-core/src/theme/{mod.rs,palette.rs,profile.rs}`, `crates/cli-core/Cargo.toml`
**Scope:** M

## Task 9: Theme config

**Description:** Read `<server_appdata_path>/sb/config.toml`
(`server_appdata_path` from the Saltbox inventory, `/opt` by default): a
named built-in theme (`default`) plus per-role overrides, and an optional
`color_profile` (the environment variable wins). Missing means built-in;
invalid warns (and logs) and falls back.

**Acceptance criteria:**
- [ ] A theme file changes the resolved palette; no file means the built-in theme.
- [ ] An invalid file or unreadable inventory warns and falls back; a moved `server_appdata_path` is followed.

**Verification:**
- [ ] Tests with temporary inventory and config files.
- [ ] `scripts/check.sh task`.

**Dependencies:** Task 8
**Files:** `crates/cli-core/src/{config.rs,theme/mod.rs}`, `crates/cli-core/tests/theme_config.rs`
**Scope:** M

## Task 10: Startup wiring, contract test and success criteria

**Description:** `sb` runs the steps in the spec's order with one call into
`cli-core`. A contract test compares sb-rust's commands and flags with
sb-go's (every sb-rust name exists in sb-go). Walk the spec's success
criteria.

**Acceptance criteria:**
- [ ] Spec success criteria 1, 2, 4, 5, 6 (with `SB_TEST_ROOT=1`), 7, 8 and 9 pass; 3 is covered by Task 5's test until a root command exists.
- [ ] Startup budget holds: `sb --help` and `sb version` median ≤ 10 ms.

**Verification:**
- [ ] `scripts/check.sh full`.
- [ ] Manual: `sb --help`, `sb version`, Ctrl-C and `--color` on this host, shown to the maintainer.

**Dependencies:** Tasks 3, 6, 9
**Files:** `crates/sb/src/main.rs`, `crates/cli-core/src/startup.rs`, `crates/sb/tests/contract.rs`
**Scope:** M

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes.
- [ ] Spec success criteria met; the maintainer reviews `sb --help`, `sb version` and Ctrl-C.
