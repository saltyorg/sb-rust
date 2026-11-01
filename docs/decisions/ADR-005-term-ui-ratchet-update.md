# ADR-005: Ratchet values after term-ui

## Status

Proposed (term-ui Task 11)

## Date

2026-11-01

## Context

CONSTRAINTS.md records measured values that must not get worse beyond a
tolerance without an ADR ([ADR-004](ADR-004-yaml-stack-binary-size.md)
set the current ones). cli-core Tasks 11 and 12 (help and parse errors)
and term-ui Tasks 1 to 10 moved them.

**Production binary size** (recorded limit 1537408 bytes, tolerance 5%
per change; production musl build, as `scripts/check.sh` measures it):

| Step | Bytes | Change |
|---|---|---|
| ADR-004 | 1537408 | — |
| Help in sb-go's format (78bd03a: the renderer and unicode-width) | 1602944 | +4.26% |
| Parse errors in sb's error style (0b55841; its commit body says +0.5%) | 1615232 | +0.77% |
| Parse errors as the chosen hint layout (ebc3378) | 1619328 | +0.25% |
| term-ui's colour hook-up and theme adapters (2ac0051) | 1627520 | +0.51% |
| Restore hooks before the second-signal exit (d0b641a: the restore thread and its channel) | 1652096 | +1.51% |

Every step stayed within the per-change tolerance; together they are
7.46% over the recorded limit. term-ui's runner and tables add nothing
yet: nothing in sb calls them, so link-time optimisation leaves them out.
Their size comes with the first command that uses them (`sb docker ps`),
which records it then.

**`sb --help` median:** at most 0.76 ms recorded. This period's checks
measured between 0.74 and 1.15 ms (the 1.15 ms on a loaded machine, with
sb's code unchanged from the check before it); Task 11's check: 0.77 ms,
`sb version` 0.92 ms. All within the 1 ms tolerance and far below the
10 ms budget, so the limit stays.

**Project coverage** (measured as `scripts/check.sh` runs it, as root):
98.32% of lines (5340 of 5431) and 98.20% of functions (711 of 724),
against the recorded 99.01% and 98.50%. Functions are 0.30 points under the
recorded value, within the 0.5-point tolerance; lines are 0.69 points
under it, 0.19 beyond the tolerance. The project grew threefold (1819
lines at ADR-004), mostly term-ui's runner, and what stays uncovered is:

- lines inside the runner's own unit tests that a test never reaches by
  design: a step body's `Ok(())` after an awaited group or sleep that the
  test stops, interrupts or fails first;
- timing-dependent runner paths the tests can't force without a seam
  added only for them: a step started after its parent ended, a sibling
  whose slot the dropped group already closed, a task the runtime
  cancelled at shutdown, and the frame task's stop and end returns;
- operating-system paths: `tcgetattr` and `tcsetattr` failing on a
  terminal; and `relaunch_as_root`, which cli-core's relaunch test does
  run (in a child with a stand-in sudo), but whose `exec` replaces that
  child before its coverage profile is written;
- single lines elsewhere that are test code too: the messages of
  assertions, which only run when the test fails, and early returns of
  tests that need conditions the run doesn't meet.

`#[coverage(off)]` is unstable (the toolchain is pinned to stable) and
would need a CONSTRAINTS.md Exceptions row for each use, so test code
counts as it is.
Task 11 added tests for the conversions and accessors that had none
(`Line::from(&String)`, `Cell::from(String)`, `Sensitive::inner`).

## Decision

Record the current values as the new limits: binary at most 1652096
bytes, `sb --help` median at most 0.76 ms (unchanged), line coverage at
least 98.32%, function coverage at least 98.20%.

## Alternatives considered

- **Seams for the timing paths** (hooks to pause a task between its body
  and its ending): production code whose only purpose is a coverage
  number, and each seam is one more thing to get wrong. Rejected.
- **Measure coverage without test code:** cargo-llvm-cov can only drop
  whole files, and the unit tests live beside the code they test;
  `#[coverage(off)]` is unstable and needs an Exceptions row per use.
- **Keep the old size limit and shed dependencies:** the growth is help,
  parse errors and the restore thread, all chosen in the cli-core and
  term-ui specs; sb is 3.6% of sb-go's 45.7 MB.

## Consequences

- When this ADR is accepted, the CONSTRAINTS.md "Measured, not yet
  enforced" rows and their pointer move from ADR-004 to this ADR's values.
- The floor guard reports that CONSTRAINTS.md commit whenever it is in a
  check's range; later checks use a base at or after it.
- `sb docker ps` records the runner's and tables' size when it first
  links them.
