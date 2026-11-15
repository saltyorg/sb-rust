# ADR-024: Binary size with tui-markdown and syntect

## Status

Accepted (the maintainer, 2026-11-15: keep syntect's
highlighting despite the size). Proposed with module build M74,
2026-11-15.

## Date

2026-11-15

## Context

M74 brings tui-markdown 0.3.12 into sb's binary for the announcements'
inline form (tui spec, Scope; ops spec, "Shown"). Its default
`highlight-code` feature pulls in syntect 5.3.0 with its built-in syntax
and theme dumps and the Oniguruma regex engine; the maintainer kept that
feature for YAML highlighting in announcements (2026-10-30, CONSTRAINTS.md
exception E1). The capability map's stack table lists tui-markdown at
about 0.2 MB, measured in proto-markdown without separating syntect's
share. CONSTRAINTS.md's "Production binary size" row allows growth of at
most 5% per change; beyond that a change needs an ADR (the row itself
waits for the complete binary, ADR-023).

Measured as `scripts/check.sh full` builds (cross, the production
profile, the x86_64 glibc target, ADR-020), on the M74 tree and on its
base:

| Build | Bytes | Change |
|---|---|---|
| sb at M74's base (6f087e8) | 16811072 | — |
| sb with M74, tui-markdown without `highlight-code` | 17487312 | +676240 (+4.0%) |
| sb with M74 as committed (`highlight-code` on) | 18543872 | +1732800 (+10.3%) |

So highlighting costs about 1.06 MB (6.3% of the base) on its own: the
syntax set and themes compiled into the binary, the regex engine and
their parsers. The rest (pulldown-cmark, tui-markdown, sb-ops and the
command) is within the 5% tolerance.

What highlighting gives: fenced code with a language syntect knows is
coloured (in announcements, YAML snippets), in the colour output only;
piped and plain output never shows it. Saltbox's and Sandbox's current
announcement files have no fenced code (public GitHub, 2026-11-15).

`sb --help` and `sb version` don't load the syntax set (it is loaded on
first use, for a code block with a language), so startup doesn't change
(3.6 and 4.0 ms medians in the same gate run).

## Decision

Keep tui-markdown's default features, highlighting included, as
exception E1 decided, and accept the +1.73 MB (+10.3%) growth for M74.
The production binary stays far below CONSTRAINTS.md's ceiling of
45711520 bytes; the ratchet row isn't moved now (ADR-023: it is
re-measured when the binary is complete).

## Alternatives considered

- **`default-features = false` on tui-markdown:** +4.0% instead of
  +10.3%, no syntect, bincode or yaml-rust in sb's graph, so exception E1
  and its two advisory ignores could go. Fenced code would show in the
  sheet's `code` style (one colour) instead of highlighted. This is the
  alternative for the maintainer to choose instead; it is a one-line
  change to the workspace manifest plus E1's removal.
- **Another renderer (termimad):** rejected in proto-markdown (a second
  rendering stack with its own width code).

## Consequences

- If accepted: E1 stays until its expiry (2027-01-28), and the size
  growth stays in the binary.
- If the alternative is chosen: a follow-up commit turns the feature off,
  removes E1's row and its deny.toml ignores, and amends the capability
  map's Markdown row and this ADR's status.
