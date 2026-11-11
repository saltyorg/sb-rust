# ADR-014: Ratchet values after sb-journal

## Status

Accepted (maintainer, 2026-11-11, sb-journal Checkpoint C)

## Date

2026-11-11

## Context

CONSTRAINTS.md's "Measured, not yet enforced" table records values that
must not get worse beyond a tolerance. Its current values are
[ADR-011](ADR-011-exec-ratchet-update.md)'s: line coverage at least
98.34%, function coverage at least 97.87%, the production binary at most
11645408 bytes, the `sb --help` median at most 1.09 ms. Tightening a
value needs no ADR; loosening one does, whether or not the change is
within its tolerance (CONSTRAINTS.md). The sb-journal plan (Tasks 0 to
11: the `sb-journal` crate, sb-host's wrapper, term-ui's shared
sanitizer and the security audit's fixes) measured the ratchet at
Checkpoints A and B and leaves the update to this ADR.

Measured on the plan's final tree (Task 12, after the prototype's
removal), as root, the way `scripts/check.sh` runs:

**Project coverage** (`SB_TEST_ROOT=1 SB_LOG_DIR=<target>/test-logs
cargo llvm-cov --workspace --summary-only`, totalled over the `crates/`
rows only; no prototype's tests reach `crates/`): 98.59% of lines (29247
of 29666, 98.588%) and 98.12% of functions (3549 of 3617, 98.119%),
against the recorded 98.34% and 97.87%. Both rose: the project grew
from 14843 lines at ADR-011 to 29666, most of it sb-journal, ported with its tests and held
to the changed-line bar (90%) and the mutation bar (80%) slice by slice.
Checkpoint A measured 98.47% and 98.02%, Checkpoint B 98.58% and 98.10%.
What stays uncovered is of the kinds ADR-005 and ADR-011 named: error
mappings on failure paths no test reaches, process-ending paths, and
test helpers' panic messages.

**Production binary size** (`cross build --profile production --target
x86_64-unknown-linux-musl -p sb`, in its own target directory, as
`check.sh full` builds it): 11932640 bytes, against the recorded
11645408: 287232 bytes (2.47%) more, within the 5% tolerance. No `sb`
command uses sb-journal yet, so the reader adds nothing (the linker
drops it; plan, "Architecture decisions"). The growth came from two
changes:
- b7ebf17 (sb docker start, stop and restart, the previous plan): exec's
  first user in the binary. ADR-011 expected it ("docker start, stop and
  restart ... will measure its own size against this value"), and its
  breakdown, by `cargo bloat`, is recorded in 50ede52. It was already
  present at Checkpoint A (11924448 bytes).
- 8192 bytes (0.07%) since Checkpoint A, likely from term-ui's
  `DisplayText` in the runner (580a124: anstyle-parse in place of
  `console::strip_ansi_codes`); not measured commit by commit. Tasks 10
  and 11 left the size unchanged.

**Startup medians** (`hyperfine --warmup 5 --runs 50`, release build):
`sb --help` 1.18 ms and `sb version` 1.36 ms, against the recorded
1.09 ms for `sb --help`: 0.09 ms more, within the 1 ms tolerance, and far
below the 10 ms budget. This is run-to-run noise, not new startup work:
none of the plan's changes runs before `--help` is printed (sb-journal
and sb-host's wrapper have no caller in `sb`, and the sanitizer runs only
on a child's output). Over the plan's checks the `sb --help` median read
1.13 to 1.24 ms (Checkpoint A 1.17, Checkpoint B 1.18, Task 10 1.13,
Task 11 1.24), and ADR-011 itself saw 1.06 to 1.21 ms.

## Decision

Record the measured values as the new limits:
- **coverage, a tightening:** line coverage at least 98.58%, function
  coverage at least 98.11% (the measured values rounded down to two
  places, so the measurement meets them);
- **binary size, a loosening:** at most 11932640 bytes;
- **`sb --help` median, a loosening:** at most 1.18 ms.

This ADR is what allows the two loosenings: both figures are within
their tolerances, but CONSTRAINTS.md asks for an ADR for any loosening,
so the floor guard's `threshold-loosened` findings on those two rows are
expected in the commit that applies it.

## Alternatives considered

- **Keep the old size limit:** the next change would be measured against
  a value the tree no longer meets, so the 5% tolerance would already be
  half spent on growth recorded and explained in 50ede52. Rejected:
  recording the real size keeps the ratchet tight for the first consumer
  of sb-journal (the tui picker and viewer), which brings the reader's
  expected 240 KiB into the binary.
- **Keep the old `sb --help` limit:** within tolerance either way, but a
  limit below every measurement this plan made turns noise into a
  standing finding. Rejected, as ADR-011 did for the same reason.
- **Record the lowest startup reading (1.13 ms):** tighter, but it came
  from a lighter-loaded run; the final tree's measurement is the
  comparable one.
- **Add an `sb version` row:** its median (1.36 ms) is recorded here, but
  the table tracks `sb --help` alone, and the 10 ms budget in "Enforced
  with numbers" covers both commands. Adding a row would be a tightening
  for the maintainer to choose; it needs no ADR.

## Consequences

- CONSTRAINTS.md's table and its "Current values" pointer move to this
  ADR with it (sb-journal plan Task 12, as the maintainer asked). On
  acceptance, ADR-011's status names this ADR as superseding its values;
  if the maintainer rejects it, the rows and the pointer go back to
  ADR-011's values.
- The next change that lowers line coverage by more than 0.5 points from
  98.58%, or function coverage by more than 0.5 points from 98.11%,
  needs an ADR.
- The next change that grows the binary more than 5% from 11932640
  bytes, or the `sb --help` median more than 1 ms from 1.18 ms, needs an
  ADR; the tui viewer, sb-journal's first consumer, will measure its
  size against this value.
