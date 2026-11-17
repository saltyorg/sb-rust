# docker start, stop and restart: tasks

Plan: [tasks/docker-control/plan.md](plan.md). Spec: [docs/specs/docker.md](../../docs/specs/docker.md),
"Start, stop and restart". Every task follows CLAUDE.md: the skills, TDD
(each case failing first), `scripts/check.sh fast` after edits, `full`
before committing, a code-reviewer (and library-reviewer for HTTP,
process and terminal code) on each commit, `Library-check:` and
`Review:` lines. sb runs as root only with `SB_LOG_DIR` in `target/` or
a temporary directory. No test talks to a real controller.

## Task 1: `control.rs`'s client and the job request, on a fake controller

**Description:** `Action`, `Controller` (fields and `saltbox()`),
`ControlError`, the HTTP client and `request(action, ignore)`, with the
fake controller in sb-docker's `testing` feature.

**Acceptance criteria:**
- [x] `POST <base>/start` and `/stop`, `ignore` repeated and encoded
      (sb-go's space and ampersand cases), `Content-Type:
      application/json`, no body; the job ID from a 200 answer; a bare
      base, a base with a trailing slash and one with a path each give
      the right path (no `//start`).
- [x] Each request failure is its `ControlError` variant with sb-go's
      text: transport (and the request timeout), non-200 (a 307 too, its
      target never asked), a body that doesn't decode (one cut at the
      body limit included), a missing or blank job ID; trailing data
      after the JSON ignored.
- [x] No proxy with `HTTP_PROXY`, `http_proxy` and `ALL_PROXY` set: a
      unit test that re-runs its own test binary as a child with the
      variables set (`set_var` is unsafe in edition 2024, and unsafe code
      is forbidden; exec's `tests/capture.rs` pattern), with a control
      case: the same client without `no_proxy` does reach a fake proxy.
      `saltbox()`'s values pinned.

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh fast`.
- [x] library-reviewer and code-reviewer on the commit.

**Dependencies:** None
**Files:** `crates/docker/src/control/{mod,client}.rs`,
`crates/docker/src/testing/controller.rs` (testing.rs split into
`testing/`), `crates/docker/src/lib.rs` and `Cargo.toml` (reqwest, with
its `Url` re-export instead of url itself; thiserror; tokio's `time` as
a plain dependency; form_urlencoded for the fake; descriptions no longer
`ps` only)
**Scope:** M

## Task 2: the wait

**Description:** `wait(job)`: the first poll at once, then every
`poll_interval`, at most `max_polls`, each answer judged as the spec says.

**Acceptance criteria:**
- [x] `completed` returns; `pending` and `running` wait; `failed`, an
      unknown status, non-200, a body that doesn't decode, a missing
      status and a transport failure end it at once, each its variant.
- [x] After the last poll, "timeout waiting for job completion": exactly
      `max_polls` asks on the fake, and with a short interval and
      `max_polls = 3` the elapsed time is at least 2 intervals and under
      3 (the first poll at once, no sleep after the last; real time, not
      tokio's paused clock, which runs ahead while a real socket waits).
- [x] The job ID is one escaped path segment (an ID with `/` and `?`; a
      base with a path); a poll's request timeout is its variant.
- [x] Cancelled mid-wait (sb-go's
      `TestWaitForJobCompletionHonorsCancellation`): ends at once, the
      fake asked no further.

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh fast`; library-reviewer
      and code-reviewer.

**Dependencies:** Task 1
**Files:** `crates/docker/src/control/client.rs`,
`crates/docker/src/testing/controller.rs`
**Scope:** S

## Task 3: the service check

**Description:** `check_service`: the unit file's `stat`, then `systemctl
is-active` through exec's `capture` with `check_timeout`, the step's token
cancelling it.

**Acceptance criteria:**
- [x] Ready only for exit 0 and `active` after trimming (trailing
      whitespace too); `inactive`, exit 3, `active` with exit 1, a
      sleeper past the timeout and a missing program are "not running",
      a plain error with exec's outcome logged, not chained (term-ui's
      block shows sb-go's text alone).
- [x] The unit file missing ("does not exist") or behind a regular file
      (ENOTDIR: "error checking service status: stat <path>: <cause>");
      `systemctl` isn't run then.
- [x] Cancelled while `systemctl` runs: the interrupted outcome is
      passed on, not "not running".

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh fast`; library-reviewer
      (process code) and code-reviewer.

**Dependencies:** Task 1 (the error type and `Controller`)
**Files:** `crates/docker/src/control/service.rs`,
`crates/docker/Cargo.toml` (sb-exec), stand-in scripts written by the
tests into their temporary directories
**Scope:** M

## Checkpoint A: the client is done
- [x] Tasks 1-3 done; `scripts/check.sh full` passes.
- [x] Spec success criteria 2, 3 and 5 met by the tests named in them.
- [x] The maintainer reviews the client before the commands are built,
      and confirms the plan's one addition to the spec's shape (the
      `systemctl` path field).

## Task 4: start, stop and restart as term-ui steps

**Description:** `control::start`, `stop(ignore)` and `restart(ignore)`
on a `Runner`: the step tree and labels of the spec's table, decision
5's notice on an interrupted request or wait, the run-log records.

**Acceptance criteria:**
- [x] Each command's steps and labels for success and for a failure at
      each step, the whole tree compared on term-ui's in-memory runner
      (times taken off; insta would need its `filters` feature for that); restart's
      `--ignore` only on its stop, and a failed stop starts nothing (the
      fake asked no `/start`).
- [x] Interrupted during a request and during the wait: the interrupted
      ending with the notice ("starting" for start and restart's start,
      "stopping" for stop and restart's stop); during the service check:
      no notice (no job was asked for).
- [x] The run log records the check's answer, each request and answer,
      each poll's status and the outcome: written by the client (Tasks
      1-3) and checked end to end in Task 5's run-log test, on the built
      sb's real log (tracing caches a callsite's interest process-wide,
      so an in-process subscriber test needs a binary of its own).

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh fast`; library-reviewer
      (term-ui steps) and code-reviewer.

**Dependencies:** Checkpoint A
**Files:** `crates/docker/src/control/commands.rs`,
`crates/docker/Cargo.toml` (term-ui's `testing` feature and indicatif as
dev-dependencies)
**Scope:** M

## Task 5: `sb docker start/stop/restart` in sb, end to end

**Description:** The clap commands with sb-go's help texts and
`--ignore`, the two overrides, the runtime and runner, and end-to-end
tests of the built sb on the fake controller.

**Acceptance criteria:**
- [x] Help texts as the spec's table (snapshot); `--ignore a,b --ignore
      c` sends `a`, `b`, `c`, `a,,b` and `--ignore ""` drop the empty
      names, `" a"` is kept as given (unit tests on the parsed command);
      `-v` is a parse error (exit 2); the contract test passes; root
      needed (the fake-sudo relaunch, which records its arguments and
      never runs sb).
- [x] exec's forced-exit hook registered once with sb's `Shutdown`,
      through sb-docker; both overrides set in `apply_sb_env` (a refused
      URL, a missing unit file), checked end to end (a run with the shared
      environment alone stops at "does not exist"; `tests/isolation.rs`
      checks only that tests call `apply_sb_env`); an
      overridden URL logged ("controller URL overridden"), an empty one
      unset.
- [x] As root with both overrides and a stand-in `systemctl` first on
      `PATH`: start, stop and restart succeed; each failure that answers
      at once shows its block and exits 1 (the timeouts and the poll
      limit are unit-tested only); a `PATH` without `systemctl` is "not
      running"; a malformed `SB_DOCKER_CONTROLLER_URL` is "parse Docker
      controller URL: <cause>" before the runner.
- [x] Piped and on a PTY; the run log records the check's answer, the
      requests and answers, each poll's status and the outcome (Task 4's
      run-log criterion).

**Verification:**
- [x] `cargo test -p sb`; `scripts/check.sh full`; code-reviewer and
      library-reviewer.

**Dependencies:** Task 4
**Files:** `crates/sb/src/main.rs`, `crates/sb/Cargo.toml`,
`crates/sb/tests/docker_control.rs`, `crates/sb/tests/common/mod.rs`,
`crates/sb/tests/snapshots/`; `crates/docker/src/lib.rs` (the hook's
re-export)
**Scope:** M

## Task 6: size, coverage and the spec's measurements

**Description:** Measure the production musl binary (exec and reqwest
linked into sb by Task 5) and project coverage against ADR-011, before
the signal tests (risk first); record them, with an
ADR for the maintainer if either moves past its tolerance.

**Acceptance criteria:**
- [x] Size measured after Task 5, with the share of exec's and reqwest's
      crates (`cargo bloat` on the production build), and the `sb --help`
      and `sb version` medians.
- [x] Project coverage (lines, functions) measured as ADR-011 did.
- [x] Within tolerance: the values and reason in the commit message
      (CONSTRAINTS.md); past it: an ADR proposed, its CONSTRAINTS.md
      change a separate commit after acceptance.

**Verification:**
- [x] `scripts/check.sh full` (eb6a910); no ADR needed (every value
      within tolerance), so code-reviewer reviewed the recording commit
      instead.

**Dependencies:** Task 5
**Files:** `docs/decisions/ADR-012-*.md` (if needed), `CONSTRAINTS.md`
(after acceptance)
**Scope:** S

## Task 7: signals end to end

**Description:** SIGINT and SIGTERM against the built sb at each kind of
wait.

**Acceptance criteria:**
- [x] Mid-wait and during a delayed request answer: the interrupted
      ending with decision 5's notice, death by the signal (`$?` 130 or
      143; exec decision 7) within a second, the terminal restored (on a
      PTY).
- [x] While a stand-in `systemctl` sleeps: the interrupted ending, no
      "not running" block, death by the signal.
- [x] A second signal while a stand-in `systemctl` ignores SIGTERM: exec's
      forced-exit hook SIGKILLs its group, no stand-in left behind.

**Verification:**
- [x] `cargo test -p sb --test docker_control_signals`;
      `scripts/check.sh full`; code-reviewer.

**Dependencies:** Task 6
**Files:** `crates/sb/tests/docker_control_signals.rs`
**Scope:** S

## Checkpoint B: complete
- [x] Tasks 1-7 done; `scripts/check.sh full` passes (b60e76a).
- [x] Spec success criteria 1-9 met with evidence; exec's criterion 9
      (carried here by name from the exec plan's Checkpoint C) met.
- [x] The maintainer runs the commands by hand on a Saltbox host (never
      from a test): stop with `--ignore`, start, restart and a Ctrl-C
      during a wait all behaved as specified (maintainer, 2026-11-06, the
      production musl build of b60e76a).

## Review backlog (taken up when each crate is next touched)

Moved to [tasks/todo.md](../hardening/todo.md) (2026-11-06); kept here as it
stood when the docker start/stop/restart plan closed.

From the whole-codebase review (2026-11-05), moved here from
[tasks/exec/todo.md](../exec/todo.md); Optional findings and nits, by
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
  `ps`'s public API (start/stop/restart's are typed: this plan).
- sb: `warn!("{warning}")` as a field; one runtime builder.
