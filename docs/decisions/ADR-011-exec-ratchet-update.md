# ADR-011: Ratchet values after exec

## Status

Proposed (exec Checkpoint C, 2026-11-06)

## Date

2026-11-06

## Context

CONSTRAINTS.md records measured values that must not get worse beyond a
tolerance; [ADR-005](ADR-005-term-ui-ratchet-update.md) set the coverage
values and [ADR-009](ADR-009-validate-config-binary-size.md) the binary
size and `sb --help` median. The exec module (Tasks 1 to 9: capture,
stream, pty and attached children, cli-core's hold on signals, the
review's fixes) moved them. Checkpoint B carried the project coverage
measurement here.

**Project coverage** (`crates/`, as root with the root tests on, as
`scripts/check.sh` runs its tests: `cargo llvm-cov --workspace
--summary-only`, totalled over the `crates/` rows; no prototype's tests
reach `crates/`): 98.34% of lines (14598 of 14843) and 97.87% of
functions (1887 of 1928), against the recorded 98.32% and 98.20%. Lines
are 0.02 points over; functions 0.33 points under, within the 0.5-point
tolerance. The project grew from 5431 lines at ADR-005 to
14843. What stays uncovered is of the kinds ADR-005 named, plus exec's:

- error mappings on failure paths a test doesn't reach: a `pidfd_open`
  that fails, portable-pty giving no tty name, pid or exit status, a
  client that can't be built (and a malformed `CLOUDFLARE_BASE_URL`,
  which a test could set but none does);
- process-ending paths: cli-core's forced exit (`run_then_end_by`) and
  term-ui's restore for it, and `relaunch_as_root`;
- sb's production URLs for the credential checks, which every test
  replaces with a fake service;
- the PTY leader's `terminate`, a no-op written out because a PTY child
  always has its session's group (`supervise` calls `terminate` only
  without a group);
- closures in tests that by design never run (backoffs a test checks
  are never asked; the panic messages of test helpers);
- paths that run only in a child whose death by a signal ends it before
  its coverage profile is written (exec-demo's attached child).

**Production binary size**: 11645408 bytes, against the recorded
11641312 (+4096 bytes, +0.04%, within the 5% tolerance): exec isn't in
sb's binary yet (its first user, docker start, stop and restart, is the
next plan). The growth is term-ui's Task 5 change (835ca3f: a failed
child's ending and `runner_live`); cli-core's hold (8e54fa8) added 4096
bytes more, which Task 9's config change (6ee7947) took back off.

**`sb --help` median**: 1.09 ms in the last full checks (1.06 to 1.21 ms
over this module's checks, the higher figures on a loaded machine),
against the recorded 1.07 ms: within the 1 ms tolerance, far below the
10 ms budget.

## Decision

Record the current values as the new limits: line coverage at least
98.34%, function coverage at least 97.87%, binary at most 11645408
bytes, `sb --help` median at most 1.09 ms.

## Alternatives considered

- **Keep 98.20% for functions and cover what's missing:** the remaining
  functions are failure paths and the PTY no-op above; covering them
  needs seams added only for a number (rejected in ADR-005 for the same
  reason) or `#[coverage(off)]`, which is unstable and needs an
  Exceptions row per use.
- **Keep the old size and median limits:** both are within tolerance;
  recording the measured values keeps the ratchet tight for the next
  plan, which adds exec to the binary.

## Consequences

- On acceptance, CONSTRAINTS.md's rows and its "Current values" pointer
  move to this ADR, in a commit of their own (as for ADR-005 and
  ADR-009).
- The next change that lowers line coverage by more than 0.5 points
  from 98.34%, or the `sb --help` median by more than 1 ms from 1.09 ms,
  needs an ADR too.
- The next change that lowers function coverage by more than 0.5 points
  from 97.87%, or grows the binary more than 5% from 11645408 bytes,
  needs an ADR; docker start, stop and restart (exec's first user in
  the binary) will measure its own size against this value.
