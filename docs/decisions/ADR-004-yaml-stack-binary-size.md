# ADR-004: Ratchet values after cli-core Phase 3

## Status

Accepted (maintainer, 2026-10-30, cli-core Checkpoint C); values superseded
by [ADR-005](ADR-005-term-ui-ratchet-update.md)

## Date

2026-10-30

## Context

CONSTRAINTS.md records measured values that must not get worse beyond a
tolerance without an ADR ([ADR-003](ADR-003-cli-core-ratchet-update.md)
set the current ones). cli-core Tasks 7 to 10 moved them.

**Production binary size** (recorded limit 1107040 bytes, tolerance 5%
per change; production musl build, as `scripts/check.sh` measures it):

| Step | Bytes | Change |
|---|---|---|
| ADR-003 (Task 6) | 1107040 | — |
| Tasks 7 and 8 (`--color`, colour profiles), measured on Task 8's tree | 1185120 | +7.1% |
| Task 9 (theme config), measured on its commit's tree | 2278816 | +92.3% |
| Task 10 (inventory read with saphyr) | 1537408 | −32.5% |

Neither the Task 8 nor the Task 9 commit recorded its step; this ADR
covers both. Task 7 and 8 brought colorchoice-clap and anstyle-lossy.
Task 9 brought toml (about 88 KiB of code with toml_parser) and
serde-saphyr to read one inventory key; `cargo bloat --crates` put
serde-saphyr, granit-parser and annotate-snippets (which serde-saphyr's
`deserialize` feature requires) at about 740 KiB of code.

The capability map and proto-yaml's findings put Saltbox YAML files on
saphyr (`MarkedYaml`) and only schemas and sb-owned typed YAML on
serde-saphyr, and the inventory is a Saltbox file. Measured alone on the
same one-key read (production musl build of a scratch binary): saphyr
adds 180224 bytes to a 389816-byte baseline, serde-saphyr 942208. Task 10
switched the inventory read to saphyr; sb is 741408 bytes smaller than
with serde-saphyr. It also follows sb-go's yaml.v3 more closely: only the
first document is parsed, and a custom-tagged value is read as its
string. One difference stays, recorded in the code: saphyr doesn't follow
YAML merge keys (`<<`), which Saltbox's host_vars don't use.

**`sb --help` median:** at most 0.59 ms recorded; now 0.76 ms (`sb
version`: 0.90 ms), within the 1 ms tolerance and far below the 10 ms
budget.

**Project coverage** improved: 99.01% of lines (1801 of 1819) and 98.50%
of functions (263 of 267), against 98.06% and 97.12% (rounded down).

## Decision

Record the current values as the new limits: binary at most 1537408
bytes, `sb --help` median at most 0.76 ms, line coverage at least 99.01%,
function coverage at least 98.50%.

## Alternatives considered

- **Keep serde-saphyr for the inventory:** 741408 bytes more for one key,
  and against the map's split. Rejected.
- **A line scan for the key:** hand-written YAML parsing, which the
  library rule forbids.
- **Read the inventory only for commands that need the theme:** every
  command shows messages, help included, so every run needs it.
- **Keep the old size limit and shed dependencies elsewhere:** the
  remaining growth is the theme file (toml) and `--color` and profiles,
  all chosen in the spec; sb is 3.4% of sb-go's 45.7 MB.

## Consequences

- When this ADR is accepted, the CONSTRAINTS.md "Measured, not yet
  enforced" rows and their pointer move from ADR-003 to this ADR's values.
- The floor guard reports that CONSTRAINTS.md commit whenever it is in a
  check's range; later checks use a base at or after it.
- serde-saphyr arrives again with the config-validation module (schemas);
  that module's ADR or commit records its size then.
