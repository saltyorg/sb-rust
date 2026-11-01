# Implementation Plan: `term-ui` (approved 2026-11-01)

Spec: [docs/specs/term-ui.md](../docs/specs/term-ui.md) (approved
2026-10-30, decisions 1-6; Checkpoint A 2026-11-01, decision 7).
Quality bar: [CONSTRAINTS.md](../CONSTRAINTS.md).
Earlier plans: [tasks/cli-core/](cli-core/plan.md) (done; its open items
are carried in [tasks/todo.md](todo.md)), [tasks/prototypes/](prototypes/plan.md).

## Overview

Build `sb-term-ui`, the inline output layer the first slice draws through:
the colour hook-up and theme adapters for console and tabled, the task
runner (proto-runner's reviewed tree, with labels, notices, nesting,
concurrent steps, plain mode, the failure block, run-log records), and
tables (proto-inline's `docker ps` look). Prompts are specified but built
with their first user (decision 1). cli-core gains the second-signal
restore hooks (decision 5).

## Architecture decisions

- **Spike, then API, then code (decisions 2-4).** Nesting and concurrent
  steps need more live bars than proto-runner's fixed pair, and its
  no-blink fix relied on a constant bar count. Task 1 measures a fixed pool
  sized for the deepest nesting and widest concurrency, with decision 3's
  newest-line-live. Task 4 designs the runner API from those findings. The
  maintainer reviews both, the look of notices and the concurrency
  semantics at Checkpoint A, before any runner code.
- **Async steps on the command's runtime (decision 4).** Steps are `async`
  closures given cli-core's cancellation token. sb builds its tokio runtime
  after argument parsing (help and version never pay for it): tokio's
  current-thread runtime, and a tokio task drives the frames (amended
  decision 4; approved at Checkpoint A).
- **Rendering only.** term-ui starts no processes; output lines are handed
  in (exec, later, will feed child output).
- **Errors through cli-core.** A failed step returns a quiet `Exit(1)`
  after the runner has shown it; an interruption is marked in the tree and
  cli-core's block shows "interrupted" once.
- **A demo instead of a command.** Until `docker` and `config` exist, the
  runner and tables are shown through `cargo run -p sb-term-ui --bin
  demo` (a bin of term-ui, never part of sb's build; the coverage run
  builds bins for integration tests, not examples). It ends through the
  same cli-core functions sb's `main` uses, so PTY tests on it cover the
  "shown once" criteria; it logs only under a temporary `SB_LOG_DIR`.
- **Binary size follows use.** Link-time optimisation leaves out what sb
  doesn't call, so term-ui's real cost appears with its first command
  (`docker ps`). Size is measured in every task; an ADR comes in the commit
  that crosses the tolerance.
- **Dependencies, confirmed at source in their task and cited:** indicatif
  0.18.6, console 0.16, termtree, tabled 0.22, rustix `termios` (all in the
  spec's table), plus two the spec's table doesn't list, which need the
  maintainer's approval ("ask first"), given with the plan: **tokio**
  (decision 4's async steps; the workspace has only tokio-util) and
  **insta** (snapshot tests, named in the spec's testing strategy and the
  capability map).
  Approved at Checkpoint A (2026-11-01): anyhow and tracing in term-ui,
  and indicatif's `in_memory` feature for tests only.

## Task list

### Phase 1: Foundation
- [x] Task 1: No-blink spike for nested and concurrent steps
- [x] Task 2: `sb-term-ui` crate, colour hook-up and theme adapters
- [x] Task 3: cli-core runs restore hooks before the second-signal exit
- [x] Task 4: Runner API design

### Checkpoint A: Foundation
- [x] Spike, notices' look, concurrency semantics and runner API reviewed
      by the maintainer

### Phase 2: Runner
- [x] Task 5: Sequential runner: tree, labels, async steps, plain mode
- [x] Task 6: Output lines, live tail, notices and run-log records
- [x] Task 7: Failure block
- [ ] Task 8: Interruption and terminal restore
- [ ] Task 9: Nested and concurrent steps

### Checkpoint B: Runner
- [ ] The maintainer has tried the runner demo

### Phase 3: Tables and close
- [ ] Task 10: Tables in the reviewed `docker ps` look
- [ ] Task 11: Success criteria and measurements
- [ ] Task 12: 16-colour OK green (maintainer decision)

### Checkpoint C: Complete
- [ ] All tasks done; spec success criteria met or carried by name;
      `scripts/check.sh full`

Details, acceptance criteria and verification: [tasks/todo.md](todo.md).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A fixed bar pool can't hide nesting and concurrency without blinking | High | Task 1 measures it first; fallbacks in its findings (cap the concurrency shown, or accept erases only when the tree's shape changes) for the maintainer to choose |
| indicatif's ticker and tokio interact badly | Med | Task 1 runs steps on tokio with indicatif's steady tick and notes the behaviour |
| Terminal restore races on double Ctrl-C | Med | Task 3's hooks on cli-core's signals thread, with a deadline; PTY tests in Task 8 |
| Concurrency semantics (a failing sibling, Ctrl-C with two running) left implicit | Med | Task 1 lists them; settled at Checkpoint A; tested in Task 9 |
| Secrets in step output reach the run log | High | Task 6's sensitive marking and the CONSTRAINTS logging test |
| Binary growth hidden until the first command | Low | Measured each task; ADR with the first runner command (Task 11 records it) |

## Open questions

None. tokio and insta were approved with the plan (maintainer,
2026-11-01).
