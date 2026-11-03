# Implementation Plan: `docker` — `sb docker ps` (approved 2026-11-03)

Spec: [docs/specs/docker.md](../../docs/specs/docker.md) (approved
2026-11-03, decisions 1-4). Quality bar: [CONSTRAINTS.md](../../CONSTRAINTS.md).
Earlier plans: [tasks/term-ui/](../term-ui/plan.md) and
[tasks/cli-core/](../cli-core/plan.md) (done; their open items are carried in
[tasks/docker/todo.md](todo.md)), [tasks/prototypes/](../prototypes/plan.md).

## Overview

Build `sb-docker` with `ps`, and wire `sb docker` / `sb docker ps
[--json]` into sb: sb-go's columns with the reviewed status rule, sorted
cells and theme colours, through term-ui's `Table`; bollard with sb's own
`DOCKER_HOST` dispatch and an API version that works on older daemons;
the first root-needing command (relaunch end to end), the first async
command (runtime after parsing), the first chatty dependency (the `log`
bridge and default levels), and the size ADR for bollard and the first
use of term-ui's tables. `start`, `stop`, `restart`, the picker's list
and log streams are later plans.

## Architecture decisions

- **Evidence first for the API version (risk first).** The spec's
  floor-then-reconnect design rests on how bollard builds versioned paths
  and on the daemons' minimum API versions (newer Docker releases raised
  theirs), which the review read from source but nobody ran. Task 1
  checks it against a real daemon (read-only) and the Engine API
  documentation, and picks the mechanism (a floor, `/_ping`'s
  `Api-Version` header, or the "too new" answer); it confirms the
  failed-start state from the documentation and moby's source (creating a
  failing container would be a write), runs cargo-deny on bollard's tree,
  and chooses the fake Engine API server crate. Its findings go to the maintainer at
  Checkpoint A, with a spec amendment if the design changes.
- **Pure rules, thin I/O.** The status rule, Traefik rule, bindings,
  display name, rows, table and JSON are pure functions over bollard's
  model types (Tasks 3-4), tested first with synthetic fixtures. The
  connection and requests (Task 5) are a thin layer over bollard, tested
  against the fake Engine API.
- **One crate, used only by sb.** `crates/docker` (`sb-docker`) depends on
  `sb-cli-core` and `sb-term-ui`; deny.toml's wrappers follow.
- **The runtime belongs to sb's `main`.** `main` builds tokio's
  current-thread runtime after parsing, only for commands that need it
  (`version` and help never do), with the I/O and time drivers (term-ui's
  demo enables only time; bollard's sockets need I/O).
- **One fake Engine API, built before the command.** Task 5 builds it
  (behind a `testing` feature of `sb-docker`) and tests the connection on
  it; Task 6a adds the basic end-to-end cases with the command, so each
  commit passes the changed-line coverage and mutation gates; Tasks 7-8
  add the rest. Tests never inherit the host's `DOCKER_*` variables.
- **The size ADR with the growth.** bollard enters sb's binary in Task 6a,
  so that commit carries the ADR (Proposed); Task 9 completes it.
- **Errors through cli-core.** Daemon and inspect failures become
  cli-core errors (error block, exit 1); the quiet exits and signal codes
  are cli-core's.

## Task list

### Phase 1: Evidence and groundwork

- [x] Task 1: Engine API version and dependency spike
- [x] Task 2: Logging: the `log` bridge and default levels

### Checkpoint A: Evidence

- [x] Task 1's findings reviewed by the maintainer; spec amended if the
      version design changes (approved 2026-11-03)

### Phase 2: The command

- [x] Task 3: Crate and the pure rules (status, Traefik, bindings, name)
- [x] Task 4: Rows, table and JSON from bollard's models
- [x] Task 5: Fake Engine API, connection and requests
- [x] Task 6a: `sb docker ps` in sb (command, runtime, contract, basic
      end-to-end, size ADR)
- [x] Task 6b: The relaunch end to end (fake sudo, cli-core's mutants)

### Checkpoint B: The command works

- [x] `scripts/check.sh full`; the maintainer tries a release build on a
      real host (approved 2026-11-03)

### Phase 3: Proof

- [x] Task 7: End-to-end: errors, settings and output modes
- [x] Task 8: End-to-end: signals and the run log
- [x] Task 9: Success criteria and measurements

### Checkpoint C: Complete

- [x] All tasks done; spec success criteria met or carried by name;
      `scripts/check.sh full` (approved 2026-11-03)

Details, acceptance criteria and verification: [tasks/docker/todo.md](todo.md).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| No single floor API version works across supported daemons (newer releases raised their minimum) | High | Task 1 tests the alternatives (`/_ping` header, the "too new" answer) before code; Checkpoint A decides |
| bollard's models fail to parse an older daemon's JSON | Med | Task 1 and the old-daemon fixture in that version's shape (Task 7) |
| Binary size grows a lot (hyper, tokio's net stack) | Med | Measured each task; Task 6a's ADR, completed in Task 9; still far below sb-go's |
| A real sudo run would log to the host's `/var/log/sb` | Med | The relaunch is tested with cli-core's fake sudo; the real run is a manual maintainer check with an explicit log location |
| Signal tests flaky against a 9 ms command | Low | The fake server holds an inspect open and the test signals once it sees the request |
| Tests reaching the host's daemon (inherited `DOCKER_*`, or a mutant) and leaking real container names | Med | `common::sb()` and sb-docker's helpers point `DOCKER_HOST` at a socket that doesn't exist and clear the other variables; synthetic fixtures only |
| A licence or advisory problem in bollard's tree found late | Med | Task 1 runs cargo-deny on it in the spike |

## Open questions

None beyond Task 1's findings, which go to Checkpoint A.
