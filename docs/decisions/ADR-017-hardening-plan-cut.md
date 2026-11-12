# ADR-017: The hardening plan is cut to what affects sb, and its id check retired

## Status

Accepted (maintainer, 2026-11-12)

## Date

2026-11-12

## Context

The re-review of every crate at 96e940a gave 174 finding ids. The
hardening plan (approved 2026-11-11) turned them into 52 tasks: every
High and Medium fixed, every Low given a place, before the TUI log
viewer spec. Tasks 1 to 6, on the gates and records, were done first; one
of them added `scripts/check-plan-ids.mjs`, which fails unless every id
is in the plan's tables and in a task, and `scripts/check.sh fast` ran
its tests.

The review of the process (ADR-016) found the plan to be the largest
single piece of work that changes nothing sb does. A triage of the 152
open ids against how sb runs on a Saltbox server found 29 that can
happen in real use (about 21 distinct defects), 21 that matter only to
a feature not yet built, and 102 that need crafted files, can't be
reached on sb's runtime, belong to a dependency, match sb-go on
purpose, or are about tests, documents and the gate.

## Decision

- **The plan keeps the 29 ids** and the 22 tasks that hold them. The 21
  deferred ids come back with the feature each belongs to (the log
  viewer spec, release preparation). The 102 dropped ids are closed,
  config-M1 among them (confirmed on its own: it needs a file the
  server's owner would have to write). `tasks/triage.md` records each
  id's verdict and reason.
- **This replaces two of the plan's decisions:** "fix all High and
  Medium before the viewer spec" (one High, a test script that can pass
  vacuously, and 14 Mediums are dropped; 5 Mediums are deferred) and
  "Lows are not ignored".
- **`scripts/check-plan-ids.mjs` and its tests are removed**, with their
  step in `check.sh` and their entries in the floor guard's lists of
  watched files. A plan of 22 tasks is checked by reading it.
- **Findings from a later review are triaged the same way first**
  (CLAUDE.md, "Fresh-context review, by tier").

## Why this is a loosening, and its limits

A check that ran is gone, and defects that reviewers rated High and
Medium stay unfixed.

- The dropped High is a defect of a test script, and its Mediums need
  input no server produces or code no command reaches; each reason is
  in `tasks/triage.md`.
- A deferred id is not closed. The spec of the feature that reaches its
  code takes it up.
- Nothing in `CONSTRAINTS.md` changes: the thresholds, the floor guard
  and the journal rows stand.

## Alternatives considered

### Keep the plan and run it unattended
- Pros: every finding gets an answer.
- Cons: 46 more tasks, most on code no command reaches, each with its
  checks and review, before any feature.
- Rejected.

### Keep only what sb's commands reach today
- Pros: about 13 tasks.
- Cons: the journal's page and follow defects would be built on by the
  viewer, the next piece of work.
- Rejected.

### Keep the id check and teach it the dropped and deferred ids
- Pros: the plan and the findings can't drift.
- Cons: more work on the gate, for a plan a person can read.
- Rejected.

## Consequences

- The viewer spec starts after 22 tasks.
- The floor guard reports each removed line; they are acknowledged in
  `scripts/floor-acknowledged.txt` (ADR-015).
