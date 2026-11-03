# docker tasks

Plan: [tasks/plan.md](plan.md). Spec: [docs/specs/docker.md](../docs/specs/docker.md).
Every task: TDD where it changes behaviour, a code review (and the
library-reviewer for I/O and terminal code) before committing,
`scripts/check.sh full` before committing, Library-check and Review lines
in each commit, and the binary size measured: a change past the 5%
ratchet tolerance carries its ADR in the same commit (CONSTRAINTS.md).
Never run sb as root without `SB_LOG_DIR` in `target/` or a temporary directory.
Root-gated tests (`crates/sb/tests/root_gate`) run only as root with
`SB_TEST_ROOT=1`, as `scripts/check.sh` sets it; a plain `cargo test`
reports them "not run" and passes, so verification uses check.sh or sets
it explicitly.

Carried forward (open items owned by later work):
- Taken by this plan:
  - term-ui decision 9: sb's crates log at DEBUG and dependencies at WARN
    by default once bollard arrives (Task 2).
  - term-ui: the first command needing a runtime builds it after parsing
    and keeps the startup budget (Task 6).
  - cli-core Task 5: the root relaunch path runs end to end with `sb
    docker ps`; its two equivalent mutants become catchable (Task 6b:
    done, both caught).
  - term-ui criterion 9: the size of term-ui's tables comes with their
    first user (Task 6's ADR).
- Moved by docker decision 1 to `sb validate-config` (the first runner
  command): term-ui criteria 4 and 6 with sb's own commands (the failure
  block, Ctrl-C endings and restore through the runner).
- From cli-core:
  - Task 3: the rexpect PTY smoke test on the built binary moves to the
    first long-running command.
  - Criterion 1: tab completion comes with the completion module.
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
    command whose long text needs it (cli-core Task 11); Task 6 checks
    whether `docker ps`'s long text needs it (about 76 columns per line:
    likely not, and it stays carried).
- From term-ui's spec: prompts (criterion 10) are built with their first
  user, self-update or git's confirm (decision 1).
- From docker Task 4's review: table cells pass control characters
  through (term-ui's `Cell` doesn't sanitise), so a container label with
  an escape code would reach the terminal even with colour off; root
  controls the labels, so the risk is low. Strip or escape control
  characters in term-ui's `Cell` when a command shows less trusted text.
- From the docker spec, for later docker plans: start/stop/restart's
  `--verbose/-v` against term-ui decision 9; their unit-file check,
  timeouts and response cap; the picker's container list with the event
  stream, extending `ps`'s status rule.
- From the prototypes: the install script (tasks/prototypes/todo.md,
  "Later") depends on cli-core and the runner.

## Task 1: Engine API version and dependency spike

**Description:** Evidence for the spec's open mechanics, no production
code, in a scratch project in a temporary directory (removed after,
build output included). Against a real daemon, read-only (version, ping,
list, inspect; `SB_LOG_DIR` in a temporary directory; resources capped), and the
Engine API documentation and moby's source: which paths bollard 0.21.1
actually sends (versioned or not, `/_ping` included); what a daemon
answers for a version above its maximum and below its minimum (status,
body, the `Api-Version` header); the minimum API versions of supported
Docker releases; so which version mechanism works (a floor, `/_ping`'s
header, or the "too new" answer). How a failed start is recorded (state,
exit code, `State.Error`), from the documentation and moby's source only:
creating a failing container would be a write to Docker, which the spec
puts under "ask first". `cargo deny check` on the scratch project with
bollard 0.21.1 against sb's deny.toml (licences, advisories, bans), so a
problem in bollard's tree shows now, not at Task 6. Which maintained
crate serves the fake Engine API on a Unix socket. Findings in
`docs/platform-notes/docker-engine-api.md` (versions and method, no host
details) and a proposed spec amendment if the design changes (also: the
end-to-end tests live in `crates/sb/tests/`, as they drive the built
binary, not `crates/docker/tests/` as the spec's layout says).

**Acceptance criteria:**
- [x] The paths bollard sends and the daemon's answers recorded with
      versions and method; a version mechanism chosen with evidence.
- [x] The failed-start row confirmed or dropped from documented
      behaviour; bollard's tree passes sb's deny.toml (or the problem is
      named); the test server crate chosen (library first).

**Verification:**
- [x] The scratch project removed.
- [x] The maintainer reviews the findings (Checkpoint A).

**Dependencies:** None
**Files:** `docs/platform-notes/docker-engine-api.md`,
`docs/specs/docker.md` (if amended)
**Scope:** M

## Checkpoint A: Evidence
- [x] Task 1's findings reviewed; the spec amended and approved if the
      version design changed (approved 2026-11-03; also: every log line
      names its source).

## Task 2: Logging: the `log` bridge and default levels

**Description:** Forward `log` records into cli-core's run log with
tracing-log; a default filter with sb's own crates at DEBUG and
dependencies at WARN (term-ui decision 9; today the run log's subscriber
is tracing-subscriber's `fmt()` default, INFO and above for everything,
so sb's DEBUG records are dropped). Independent of the docker code, so it comes first and
later tasks can check their records as they write them.

**Acceptance criteria:**
- [x] A dependency's `log` WARN record reaches the run log and its DEBUG
      record doesn't; sb's own DEBUG records do (a separate test binary,
      as the existing log tests, since tracing caches callsite interest).

**Verification:**
- [x] `scripts/check.sh full`.

**Dependencies:** None (may run beside Task 1)
**Files:** `Cargo.toml`, `crates/cli-core/Cargo.toml`,
`crates/cli-core/src/log.rs`, `crates/cli-core/tests/log_levels.rs`
**Scope:** S

## Task 3: Crate and the pure rules

**Description:** `crates/docker` (`sb-docker`) in the workspace and
deny.toml (wrappers: `sb-docker` added to `sb-cli-core`'s and
`sb-term-ui`'s, its own entry allowing only `sb`). Pure functions, tested
first: the status rule over bollard's state and health enums (every row
of the spec's table, as amended at Checkpoint A), the Traefik rule (empty
values dropped, deduplicated, numeric then text order), bindings (format,
padding, `0.0.0.0`, IPv6 brackets, no host port left out, the spec's
order), the display name and short ID (sb-go's `TestShortContainerID`
cases). cargo-deny's gate reads sb's graph, which doesn't include
`sb-docker` until Task 6; bollard's tree was checked in Task 1.

**Acceptance criteria:**
- [x] Every status row, Traefik case and binding case has a test that
      failed against a stub first.

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh full`.

**Dependencies:** Checkpoint A
**Files:** `Cargo.toml`, `deny.toml`, `crates/docker/Cargo.toml`,
`crates/docker/src/lib.rs`, `crates/docker/src/ps.rs`
**Scope:** M

## Task 4: Rows, table and JSON from bollard's models

**Description:** Build rows from bollard's list and inspect model types
(filled from synthetic JSON fixtures), sorted by name; render them with
term-ui's `Table` (headings, right-aligned port columns, maximum column
width 100, roles) and as the spec's JSON (serde_json, compact, key order,
`role`, `null` health, numeric ports, one trailing newline, `[]` when
empty).

**Acceptance criteria:**
- [x] Table snapshot in the reviewed look; fits 120, 70 and 50 columns;
      natural width when no width is given; no colour codes with colour
      off; no containers: headings only (success criterion 3).
- [x] Exact-string JSON tests: key order, `null` health, an unnamed
      container, bindings, `[]`; never colour codes (criterion 3a).

**Verification:**
- [x] `cargo test -p sb-docker`; `scripts/check.sh full`.

**Dependencies:** Task 3
**Files:** `crates/docker/src/ps.rs`, `crates/docker/src/json.rs`,
`crates/docker/src/fixtures/` (one synthetic set, owned by sb-docker and
reused by the fake Engine API and sb's end-to-end tests)
**Scope:** M

## Task 5: Fake Engine API, connection and requests

**Description:** The fake Engine API, once, with the crate chosen in
Task 1: a server on a Unix socket in a temporary directory answering
`/version`, the list and inspect from fixtures, able to refuse paths
above a version, hold an inspect open and record the peak number of open
inspects; exposed behind an `sb-docker` `testing` feature (as term-ui's
in-memory target) so sb's end-to-end tests reuse it. It lives in
`src/`, so the coverage and mutation gates count it: it gets tests of its
own (no exclusion). Then the
connection: `DOCKER_HOST` dispatch (`unix://`, `tcp://`, `http://`;
unset or empty: the default socket; `https://`, `ssh://`,
`DOCKER_TLS_VERIFY`, `DOCKER_CERT_PATH`: an error naming the setting),
bollard pinned to `=0.21.1`; one `version()` call after connecting,
logged (Checkpoint A: bollard 0.21.1 sends unversioned paths;
`negotiate_version()` and `DOCKER_API_VERSION` are not used), bollard's
120 s timeout kept, the list (`all=true`)
and inspect by ID at most 16 at a time (`futures-util`
`buffer_unordered`), a 404 on inspect skipped (DEBUG), other failures
collected with the short ID, cancellation through cli-core's token.
sb-docker's test helpers never inherit `DOCKER_*` from the environment.
From Task 4's review: the fixtures (`src/fixtures/containers.json`) and
the step from an inspect to a list summary move into the `testing`
module, shared by the fake and the unit tests (today's `fixture_rows`
lives in a test module); the unnamed fixture means the fake's list
leaves `Names` out; the rows are sorted where they are collected (after
`buffer_unordered`, whose order isn't fixed), not by a step the caller
must remember.

**Acceptance criteria:**
- [x] The `DOCKER_HOST` decision (scheme, empty, TLS settings) is a pure
      function with a test per case, failing first.
- [x] Against the fake: rows, a vanished container, a failing inspect, an
      unreachable socket, an old daemon (unversioned paths at its own
      version, versioned ones above it refused), a newer daemon's shape,
      a daemon refusing unversioned paths (error), an unknown state word
      in the list (list failure) and in one inspect (inspect failure);
      the recorded paths
      asserted unversioned, the failure message pointing at the spec's
      API version bullet; the 120 s timeout set; axum's tree passes
      cargo-deny (`cargo deny --manifest-path crates/docker/Cargo.toml
      --features testing check`; check.sh's gate covers it from Task 6a,
      once sb dev-depends on the feature); with more
      than 16 inspects held open, the recorded peak is above 1 and at
      most 16. The fake's own behaviours have tests.

**Verification:**
- [x] `cargo test -p sb-docker`; library-reviewer on the diff;
      `scripts/check.sh full`.

**Dependencies:** Task 4
**Files:** `crates/docker/Cargo.toml`, `crates/docker/src/connect.rs`,
`crates/docker/src/ps.rs`, `crates/docker/src/testing.rs`
**Scope:** M

## Task 6a: `sb docker ps` in sb

**Description:** The `docker` command group (sb-go's short and long
texts; alone: help on stdout, exit 0) and `ps [--json]`; `needs_root`
true; the contract test's `EXTENSIONS` gains `sb docker ps --json`; sb's
current-thread runtime built after parsing, only for `docker ps`, with
the I/O and time drivers (bollard's sockets need I/O; term-ui's demo
enables time only); errors to cli-core's block (partial inspect failures
after the table or JSON, exit 1; nothing on stdout when there is no
table). `common::sb()` sets `DOCKER_HOST` to a socket that doesn't exist
and removes `DOCKER_API_VERSION`, `DOCKER_TLS_VERIFY` and
`DOCKER_CERT_PATH`, so no test reaches the host's daemon. The basic
end-to-end cases on the fake Engine API: rows, `--json`, an unreachable
daemon, a partial failure. This is where bollard, hyper and tokio's net
stack first enter sb's binary, so this commit carries the size ADR
(Proposed), and cargo-deny now checks bollard's tree.

**Acceptance criteria:**
- [x] `sb docker` alone: help on stdout, exit 0; `sb docker --color=never`
      and `sb docker nope`: exit 2; long texts snapshot (`insta` added to
      sb's dev-dependencies); the contract test passes with the new
      extension.
- [x] The basic end-to-end cases pass (root-gated).
- [x] `sb --help` within its ratchet and `sb version` within 10 ms; the
      size ADR written; `cargo deny check` passes.

**Verification:**
- [x] `scripts/check.sh full` (root tests included).

From Task 5: `connect` and `list` are plain futures with nothing
spawned, so cancelling is dropping them (`token.run_until_cancelled`, as
term-ui's demo does); the runtime needs the time driver too (bollard
wraps each request in `tokio::time::timeout`); the fake's socket sits in a
root-only temporary directory, so a `nobody` run can't reach it (6b only
checks the sudo exec).

**Dependencies:** Tasks 2, 5
**Files:** `crates/sb/Cargo.toml`, `crates/sb/src/main.rs`,
`crates/sb/src/contract.rs`, `crates/sb/tests/common/mod.rs`,
`crates/sb/tests/docker.rs`, `docs/decisions/` (the size ADR)
**Scope:** M

## Task 6b: The relaunch end to end

**Description:** `unprivileged.rs`'s `sb_as_nobody` moved into its own
`tests/nobody` module (so binaries that don't switch users don't carry it),
with the same `DOCKER_*` isolation as `common::sb()` (it builds its own
`setpriv … env` command today and would inherit them), and used with a
recording fake sudo (as cli-core's tests/relaunch.rs): `sb docker ps` as
`nobody` execs sudo with cli-core's arguments and marker. Then a one-off
`cargo mutants` run on `crates/cli-core/src/root.rs` and
`Command::needs_root` with the whole workspace's tests, set up as check.sh's
mutation step: `SB_TEST_ROOT=1`, the refusing sudo first on `PATH`
(`target/no-sudo`; a relaunch mutant run as root with passwordless sudo once
looped into thousands of processes), `SB_LOG_DIR` in `target/`, a timeout.

**Acceptance criteria:**
- [x] The fake-sudo test (root-gated) asserts cli-core's arguments and
      marker.
- [x] The one-off run catches the two mutants cli-core Task 5 left
      equivalent; the result is recorded in the commit body (2026-11-03:
      4 of 4 caught: needs_root true/false, effective_uid 0/1).

**Verification:**
- [x] `scripts/check.sh full`; the one-off mutation run as described.

**Dependencies:** Task 6a
**Files:** `crates/sb/tests/nobody/mod.rs`, `crates/sb/tests/unprivileged.rs`,
`crates/sb/tests/docker.rs`
**Scope:** S

## Checkpoint B: The command works
- [x] `scripts/check.sh full` passes.
- [x] The maintainer runs a release build of `sb docker ps` and `--json`
      on a real host (standalone binary, `SB_LOG_DIR` set): looks right
      (2026-11-03); ADR-007 accepted.

## Task 7: End-to-end: errors, settings and output modes

**Description:** On the fake Engine API through the built binary:
a vanished container; an old daemon refusing every path above its
version, its responses in that version's shape; TLS settings and an
`https://` host (errors, no request sent); an empty `DOCKER_HOST`; no
containers; piped (plain, natural width), `NO_COLOR`, `--color=never`,
`--color=always` with `--json` (no codes); `--json` for each case.

**Acceptance criteria:**
- [x] Every case above is a test; success criteria 3, 3a, 4 and 8 are
      covered through the binary. No host data in fixtures.
      (An empty `DOCKER_HOST` means the host's default socket, so it stays
      with sb-docker's `address` unit tests rather than a run. The old
      daemon's answers keep today's shape: no field sb reads postdates API
      1.41, and the newer-shape case is sb-docker's test.)

**Verification:**
- [x] `scripts/check.sh full` (or `SB_TEST_ROOT=1 cargo test -p sb --test
      docker_engine` as root).

**Dependencies:** Checkpoint B
**Files:** `crates/sb/tests/docker_engine.rs`,
`crates/docker/src/fixtures/` (new cases in the shared set)
**Scope:** M

## Task 8: End-to-end: signals and the run log

**Description:** An inspect held open while the test sends SIGINT, then
SIGTERM: 130/143, quiet, no table and nothing on stdout with `--json`,
terminal restored, on a PTY (as term-ui's signal tests); and a run's log
records (daemon address, API version, counts, each failure, the time
taken).

**Acceptance criteria:**
- [x] Success criteria 6 and 7 covered through the binary.
      (Piped: `ps` changes no terminal state, so there is nothing to
      restore and no PTY is needed; criterion 6 amended to say so. sb must
      end within 5 s of the signal, which a removed cancellation fails.)

**Verification:**
- [x] `scripts/check.sh full`.

**Dependencies:** Task 7
**Files:** `crates/sb/tests/docker_signals.rs`,
`crates/sb/tests/docker_engine.rs`
**Scope:** S

## Task 9: Success criteria and measurements

**Description:** Walk the spec's success criteria with evidence. Measure
the production musl binary and startup and complete the size ADR's
step-by-step record for the maintainer. Run the musl build against the
fake Engine API (an override pointing the end-to-end helper at another
binary, or a documented command). Add the opt-in live test
(`SB_LIVE_DOCKER=1`, read-only, capped). The maintainer: the real-sudo
relaunch with an explicit log location, a run against a real daemon, and
the one-off comparison with sb-go on the fake Engine API (differences
must be the spec's list). `sb docker ps`'s median beside sb-go's, for
information.

**Acceptance criteria:**
- [ ] Each success criterion has a test or a recorded manual check.
- [ ] Size and startup recorded; the size ADR complete for acceptance.

**Verification:**
- [ ] `scripts/check.sh full`; the maintainer's manual checks.

**Dependencies:** Task 8
**Files:** `tasks/todo.md`, `docs/decisions/`, `crates/docker/tests/live.rs`
**Scope:** S

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes.
- [ ] Spec success criteria met or carried by name; the maintainer has
      tried `sb docker ps` and `--json`.
