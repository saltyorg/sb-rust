# ADR-007: Binary size with `sb docker ps`

## Status

Accepted (maintainer, 2026-11-03, docker Checkpoint B); binary size value
superseded by [ADR-009](ADR-009-validate-config-binary-size.md)

## Date

2026-11-03

## Context

CONSTRAINTS.md records the production musl binary at most 1652096 bytes
([ADR-005](ADR-005-term-ui-ratchet-update.md)), growing at most 5% per
change. term-ui's runner and tables added nothing while no command used
them; ADR-005 said their size would come with `sb docker ps`. docker
Task 2 added the `log` bridge (1661376 bytes, +0.56%, within the
tolerance). Task 6a links `sb-docker` into sb, and with it bollard and
the first table sb draws:

| Step | Bytes | Change |
|---|---|---|
| ADR-005 | 1652096 | — |
| docker Task 2 (the `log` bridge) | 1661376 | +0.56% |
| docker Task 6a (`sb docker ps`) | 3595968 | +116.4% |

The file grew by 1934592 bytes (about 1889 KiB). `cargo bloat --crates`
measures code only; its largest new contributors, together about 780
KiB, are the HTTP stack bollard talks to the daemon through (hyper,
hyper-util, tokio's I/O driver, url, futures-util: about 340 KiB), JSON
for bollard's models (serde_json, about 105 KiB, and the serde code for
its many model types; serde itself was already in for the theme file),
the table (tabled and papergrid, term-ui's `Table`: about 175 KiB),
bollard and its models (about 85 KiB) and sb-docker itself (about 75
KiB). The rest is smaller crates' code, generic code from std and serde
instantiated for bollard's many model types, read-only data (strings,
tables) and unwinding tables, which grow with the code. The capability
map always expected this: bollard is its Docker client, and every later
Docker command (start/stop/restart need an HTTP client too, the log
viewer streams through bollard) reuses the same stack.

`sb --help` and `sb version` don't build the runtime, so their startup is
unchanged (the full check's medians: 0.7 to 0.8 ms and 0.9 ms against the
10 ms budget). sb is now 7.9% of sb-go's 45711520-byte binary, below
CONSTRAINTS.md's ceiling of never being larger than sb-go's.

ADR-005 expected `sb docker ps` to bring the runner's size too; docker
decision 1 (no runner for `ps`) moved that to `sb validate-config`, the
first command that will run steps. Only the tables' share is here.

## Decision

Record 3595968 bytes as the production binary's limit.

## Alternatives considered

- **Run the `docker` binary and parse its output:** the spec forbids it
  for `ps` (no docker CLI dependency, and its output isn't a stable
  interface).
- **A hand-written HTTP/1 client over the Unix socket:** the library-first
  rule; hyper is what bollard, and later the controller client, use.
- **Trim bollard's features:** its defaults are only `http` and `pipe`;
  without `http` it would refuse `tcp://` hosts sb-go follows, and the
  savings are small next to hyper itself.
- **Draw the table without tabled:** the reviewed look (rounded borders,
  wrapping, per-line alignment) is tabled's; hand-drawing it is the
  library rule again.

## Consequences

- When this ADR is accepted, CONSTRAINTS.md's "Production binary size"
  row and its pointer move to this ADR's value, in their own commit.
- The next Docker commands add little: the HTTP and JSON stack is shared.
