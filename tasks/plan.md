# Implementation Plan: `exec` — child processes (approved 2026-11-04)

Spec: [docs/specs/exec.md](../docs/specs/exec.md) (approved 2026-11-04,
decisions 1-5). Quality bar: [CONSTRAINTS.md](../CONSTRAINTS.md).
Earlier plans: [tasks/config/](config/plan.md), [tasks/docker/](docker/plan.md),
[tasks/term-ui/](term-ui/plan.md) and [tasks/cli-core/](cli-core/plan.md)
(done; their open items are carried in [tasks/todo.md](todo.md)),
[tasks/prototypes/](prototypes/plan.md).

## Overview

Build `sb-exec`: the `Cmd` builder with fail-closed argument logging and
secret masking, the four ways to run (`capture`, `stream`, `pty`,
`attached`), timeouts, and process-group cancellation that also reaches
grandchildren after their parent exits. term-ui learns a failed child's
ending and a "runner live" check; cli-core learns the shell-like signal
handling while an attached child runs (decision 2). No sb command uses
exec in this plan: docker start/stop/restart is next and proves
`capture` through the binary (decision 4). sb's binary still changes
here, in cli-core: the relaunch (Task 1b, decision 6) and the
signal hold (Task 8a), so Task 8a measures size and startup. Until sb
links sb-exec, each task runs cargo-deny on sb-exec's own graph.

## Architecture decisions

- **Evidence first.** Task 1 settles the platform facts the design leans
  on before code: sudoers' `--preserve-env` and `use_pty` with how sudo
  relays signals (a carried item), whether Linux keeps a process-group id
  from reuse while a member lives, portable-pty's and tokio `process`'s
  dependency trees against deny.toml, and signal-hook's `WithOrigin` for
  telling a terminal's SIGINT from a `kill`. (Done: it led to decision 2's
  revision, SIGINT never forwarded, and decision 6, relaunching with
  `sudo -u root` and no marker, built as Task 1b.)
- **Thin slices, each through real children.** The builder and the start
  record first (Task 2), then `capture` with outcomes and the end record
  (Task 3), then the group kill every piped and PTY way shares, with
  exec-demo for real signals (Task 4), term-ui's failed-child ending
  (Task 5, so Checkpoint A sees `Failed`'s shape), then `stream` (Task 6),
  `pty` (Task 7), and last cli-core's signal
  amendment (Task 8a) and `attached` (Task 8b), the riskiest change to
  shared code, behind its own checkpoint.
- **One crate below sb's commands.** `crates/exec` (`sb-exec`) depends on
  `sb-cli-core` and `sb-term-ui`; the capability map's row and deny.toml's
  wrappers follow in Task 2. term-ui owns the failed-child type it draws,
  so term-ui doesn't depend on exec.
- **Tests that can't hurt the harness** (spec, testing strategy): tokens,
  never real signals, in-process; real signals only through
  `crates/exec/src/bin/exec-demo.rs` on a PTY; the group id checked to be
  the child's pid before any group signal; a short grace in tests.

## Task list

### Phase 1: Evidence and the builder
- [x] Task 1: Platform facts and dependency trees (spike)
- [x] Task 1b: cli-core: relaunch with `sudo -u root`, no marker
- [x] Task 1c: cli-core: end an interrupted run by the signal itself
- [x] Task 2: The crate, `Cmd`, masking and the log record

### Phase 2: Capture and cancellation
- [x] Task 3: `capture`
- [x] Task 4: Process groups: cancellation, timeouts, escalation
- [x] Task 5: term-ui: a failed child's ending and the "runner live" check

### Checkpoint A: Capture is done
- [x] `scripts/check.sh full` passes; the maintainer reviews the builder's
      API and `Failed`'s shape, and decides `stdin(bytes)` with `attached`
      (2026-11-04: approved; refused, decision 11).

### Phase 3: Stream and the PTY
- [x] Task 6: `stream`
- [ ] Task 7: `pty`

### Checkpoint B: Before the shared signal change
- [ ] `scripts/check.sh full` passes; coverage, size and startup
      re-measured; the maintainer approves the signal behaviour and
      decides the cases the spec leaves open (signals during an attached
      child's grace, a second SIGTERM) before cli-core changes.

### Phase 4: Attached
- [ ] Task 8a: cli-core's signals while an attached child runs
- [ ] Task 8b: `attached`

### Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes; success criteria
      1-8 met, criterion 9 carried by name to the docker start/stop/restart
      plan (decision 4).

Details, acceptance criteria and verification: [tasks/todo.md](todo.md).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A group signal hits sb's or the test harness's own group | High | Only ids exec created (pid == pgid, asserted in tests); never `getpgid`; attached children are never group-signalled |
| Signal tests interfere with parallel tests | Med | Tokens in-process; real signals only to exec-demo (a child of the test), never to the test process |
| cli-core's signal amendment breaks existing endings | Med | Tasks 8a-8b last, behind cli-core's own tests and sb's signal tests (validate_config_signals.rs, docker_signals.rs) re-run |
| portable-pty's tree fails cargo-deny or grows sb a lot | Med | Task 1 checks the tree; each task runs cargo-deny on sb-exec; sb links exec with the docker plan, which measures the size |
| A group of only zombies looks alive, so escalation waits forever | Med | The escalation is bounded (a last check after SIGKILL, within the grace plus one second); tests reap their own grandchildren with `waitid` on the group |
| Grace waits slow tests and mutation runs | Low | A test-only short grace; 5 s only where a test checks the default |

## Open questions

- The signal cases the spec leaves open while an attached child runs (a
  second SIGTERM, a SIGINT during a forwarded SIGTERM's grace, and
  whether the forced exit kills the child), for Checkpoint B, before
  Task 8a.
