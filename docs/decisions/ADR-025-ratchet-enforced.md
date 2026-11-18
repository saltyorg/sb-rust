# ADR-025: The ratchet enforced on the finished tree

## Status

Accepted (maintainer, 2026-11-18, at the module build's Checkpoint Z).
Supersedes [ADR-014](ADR-014-journal-ratchet-update.md)'s values and
its table's "not yet enforced" status.

## Date

2026-11-18

## Context

CONSTRAINTS.md's "Measured, not yet enforced" table held three rows,
with ADR-014's values since 2026-11-11: line coverage at least 98.58%
and function coverage at least 98.11% over `crates/` (tolerance 0.5
points), the production binary at most 11932640 bytes (growth at most 5%
per change), and the `sb --help` median at most 1.18 ms (growth at most
1 ms per change). The floor guard read them, but no gate step measured
them. They waited for every command to be in place (maintainer,
2026-11-13; capability map, "To discuss with the maintainer once every
module is done").

The finished tree was measured on main at c5b79ca (2026-11-18, every M
task merged) as ADR-014 measures:
- **Coverage:** `cargo llvm-cov --workspace --summary-only` as root
  (`SB_TEST_ROOT=1`), totalled over the files under `crates/`.
  Lines: 98.40% (78292 of 79565). Functions: 97.66% (9580 of 9810).
- **Size:** the gate's production glibc build through cross
  (`--profile production --target x86_64-unknown-linux-gnu`, ADR-020),
  21693856 bytes.
- **Startup:** hyperfine, 50 runs, release build: `sb --help` median
  3.90 ms, `sb version` 4.19 ms.

Measured beside the release build of 90e9d4f (main on 2026-11-17) on
the same host and load, the
finished tree's binary takes 3.93 ms and 4.17 ms, against 90e9d4f's
3.70 ms and 4.04 ms.

All three rows read past their values. The coverage drop is within its
tolerance for lines (0.18 points) and functions (0.45 points). The
binary is about 1.8 times its value: the tui, the editors, Markdown with
syntect and the ops commands came in since ADR-014, with their own size
ADRs (ADR-023, ADR-024). The `--help` median is about 3.3 times its
value. The rows' millisecond figures came from older hardware, and on
this host the same binary's median moves by tenths of a millisecond with
the host's load.

## Decision

The maintainer's choices (2026-11-18), from these figures:
- **Line coverage over `crates/`:** an enforced floor of 98.40%, failing
  only more than 0.5 points below it.
- **Function coverage over `crates/`:** an enforced floor of 97.66%, with
  the same tolerance.
- **Production binary:** an enforced ceiling of 21693856 bytes, failing
  only more than 5% above it. The gate's hard ceiling (sb-go's size,
  45711520 bytes) stays.
- **`sb --help` median:** the per-change row is dropped. The 10 ms
  budget in "Enforced with numbers" stays, for `sb --help` and
  `sb version`.

`scripts/check.sh full` enforces the three and prints their figures on
every run:
- **Coverage** is taken from the coverage run the gate already makes
  (`cargo llvm-cov --lcov` over every crate, as root, in the test
  sandbox). Each file's lcov counts under `crates/` are totalled: LF and
  LH for lines, FNF and FNH for functions. Over `crates/` these match
  the JSON summary ADR-014 reads, file for file, on c5b79ca's coverage
  data: 78292 of 79565 lines and 9580 of 9810 functions. Read from
  lcov, the figures need no second report run.
- **Size** is checked in the production build's step, against the
  ratchet ceiling plus 5% (22778548 bytes) and the hard ceiling.
- **The comparison** is in whole hundredths of a percent, so a figure
  exactly at a floor less its tolerance passes.

## Why

- **Floors at today's figures:** the finished tree meets them to the
  hundredth (functions measure 97.6555%). Later changes are judged
  against the tree as built, not against values it had already passed.
  A tolerance is a headroom shared by the changes that follow, not an
  allowance per change: the gate compares each run with the fixed
  floor or ceiling, not with the base commit's figure. When a change
  needs more, the value is reset through an ADR.
- **Enforced in `full`, not `task`:** the figures are the project's,
  not the change's. `task` already holds a change to 90% of its changed
  lines and 80% of its mutants. `full` runs before every commit.
- **No startup row:** a millisecond figure moves with the host's load.
  A per-change limit of 1 ms on a reading that varies by tenths of a
  millisecond is noise, or a standing finding. A real regression, one
  of whole milliseconds, still fails the 10 ms budget.
- **The hard size ceiling stays:** it is sb-go's size, a promise to
  users that holds whatever the ratchet says. The ratchet catches
  growth long before it.

## Alternatives considered

- **Keep the rows unenforced and re-measure at each checkpoint:** the
  tables drifted past their values without any gate failing, because
  the floor guard reads only changes to the numbers, not the measured
  figures. Rejected: the module build is complete, and the ratchet's
  purpose is to catch drift.
- **Enforce at ADR-014's values:** every run of the gate would fail on
  size and startup, and coverage would sit at its tolerance's edge.
  Rejected by the maintainer.
- **Take coverage from `cargo llvm-cov report --json --summary-only`
  after the coverage run:** ADR-014's exact output. It would need a
  second run of the reporter, and JSON is not readable in the gate's
  shell without node, which the gate's stubbed self-tests replace.
  Rejected: the lcov counts give the same totals over `crates/`.
- **A tighter startup row on this host:** rejected by the maintainer.
  Millisecond readings follow the host's load (see Why).

## Consequences

- CONSTRAINTS.md's "Measured, not yet enforced" table goes. Its coverage
  and size rows move to "Enforced with numbers", each with its "Checked
  by" column, and the `sb --help` row is removed. The floor guard's
  findings on those rows (the two coverage floors lowered, the size
  value raised, the startup row and the section removed) are expected
  in this change and acknowledged with this ADR's id.
- The thresholds are in `scripts/check.sh`, named with MIN or MAX, so the
  floor guard reports a later loosening there too.
- A change that drops line coverage below 97.90%, function coverage
  below 97.16%, or grows the binary past 22778548 bytes fails
  `check.sh full`. Raising a floor or lowering the ceiling, as a change
  makes room, is a tightening and needs no ADR. Lowering a floor,
  raising the ceiling, or widening a tolerance needs one.
