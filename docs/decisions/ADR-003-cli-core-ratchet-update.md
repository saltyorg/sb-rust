# ADR-003: Ratchet values after cli-core Phases 1 and 2

## Status

Accepted (maintainer, 2026-10-30, cli-core Checkpoint B); values superseded
by [ADR-004](ADR-004-yaml-stack-binary-size.md)

## Date

2026-10-30

## Context

CONSTRAINTS.md records measured values that must not get worse ("Measured,
not yet enforced"). A value that gets worse beyond its tolerance is a
loosening and needs an ADR. Three values (two rows) went beyond their
tolerance during cli-core Tasks 3 to 6:

- **Production binary size:** 742072 bytes after Task 1, 824096 after
  Task 3, 1107040 after Task 6 (tolerance: 5% per change). The growth comes
  from dependencies the spec chose: signal-hook, tokio-util and rustix
  (cancellation, Task 3), sysinfo (host check, Task 4) and tracing,
  tracing-subscriber and tracing-appender (run log, Task 6). The Task 3
  step was recorded with a reason in its commit but without an ADR; this
  ADR covers it too.
- **Project line coverage:** 99.02% after Task 1, 98.8% after Task 3,
  98.06% after Task 6 (tolerance 0.5%, in percentage points). The uncovered lines are:
  - the sudo exec in `root::relaunch_as_root`: the tests run it, but in a
    child process that exec replaces before it writes coverage data;
  - the `?` branch of a signal registration in `cancel.rs`, which doesn't
    fail in practice;
  - one test assertion that must never run (`panic!` in a closure proving
    exec isn't called).
- **Project function coverage:** 100% to 97.12%: the exec function and
  two test closures that must never be called (one shares a line with code
  that runs, so it costs no line coverage). The row had recorded 100%.

The signal thread's loop was uncovered for a similar reason (its test
child ended through `_exit` without writing coverage) and was won back
with an in-process test.

## Decision

Record the current values as the new limits: binary at most 1107040 bytes,
line coverage at least 98.06%, function coverage at least 97.12%,
`sb --help` median at most 0.59 ms (an improvement).

## Alternatives considered

- **Keep the old coverage floor and cover the remaining lines:** the exec
  can't be measured in-process (forbidden unsafe code would be needed to
  flush coverage before exec), and the other two lines are a defensive
  branch and a deliberate never-run assertion. Rejected by the maintainer.
- **Shed dependencies to hold the size:** each one replaces code the
  library rule says not to write; sb is still 2.4% of sb-go's 45.7 MB.

## Consequences

- The floor guard reports this ADR's CONSTRAINTS.md commit whenever it is
  in a check's range. Checks of later work use a base at or after that
  commit (`scripts/check.sh full <that commit>`).
- From here the ratchet holds at the new values.
