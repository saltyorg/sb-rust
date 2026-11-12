# ADR-019: The matrix's pread bounds follow the page's second header read

## Status

Accepted (agent, 2026-11-12, while the maintainer is away; reversible,
named in Checkpoint T's message and kept or overruled at Checkpoint C)

## Date

2026-11-12

## Context

The journal spec's success criterion 5 asks that a matrix fixture's
tail, page back and forward page take at most a stated number of
preads. `crates/journal/tests/compare.sh` states them: the default
variant's `-u demo.service` pages of 500 entries, bounded at 26.04's
figures, the highest of the three releases, plus 1% (the sb-journal
plan, "The pread bound").

Hardening Task 33 fixed the page-to-follow handover with the design the
maintainer chose (the plan's decision 23): for each unsealed file, a
filtered page reads the header a second time after its value lookups
and examines the entries counted in between. The decision listed the
cost: the page's read path reads more. At Checkpoint T the matrix
measured it: 34 more preads on each of the three pages, on all three
releases, about 2%, so over the bounds' 1% margin. On 26.04 the tail
went from 1711 to 1745 preads (bound 1728), the page back from 2393 to
2427 (bound 2417), the forward page from 1747 to 1781 (bound 1765).
22.04 and 24.04 measured 1728, 2395 and 1760, within the old bounds
(the tail exactly on it).

## Decision

The three bounds are re-based by the method that set them: 26.04's
figures plus 1%, now rounded up (the old bounds were rounded to the
nearest).

| Bound | Was | Now |
|---|---|---|
| `PREADS_TAIL_MAX` | 1728 | 1763 |
| `PREADS_BACK_MAX` | 2417 | 2452 |
| `PREADS_FORWARD_MAX` | 1765 | 1799 |

## Consequences

- The bound still catches a regression of more than 1% over the
  measured cost; it now measures the reader as it is after Task 33.
- Each raised line is acknowledged in `scripts/floor-acknowledged.txt`,
  citing this ADR.
- If the maintainer overrules decision 23's cost, the re-read and these
  bounds go back together.
