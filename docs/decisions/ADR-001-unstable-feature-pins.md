# ADR-001: Exact pins for unstable-feature dependencies

## Status

Accepted (maintainer, 2026-10-30)

## Date

2026-10-30

## Context

The capability map (decision 9) allows a dependency feature its authors
mark unstable, or a build image outside Cargo's semver, only with an exact
pin and an ADR. Unstable features may change or disappear in any release,
including a patch release, so a caret requirement (`"0.30.2"`, which Cargo
reads as `^0.30.2`) can break the build on `cargo update`. The prototypes
used caret requirements; production code in `crates/` must not.

Three are in use, each proved by a prototype:

| Dependency | Unstable part | Used for | Proved in |
|---|---|---|---|
| `ratatui` 0.30.2 | feature `unstable-rendered-line-info` (`Paragraph::line_count`) | Scroll bounds in the log viewer, the announcements viewer and inline Markdown output | proto-logs, proto-markdown, proto-facts |
| `clap_complete` 4.6.11 | feature `unstable-dynamic` (`CompleteEnv`, runtime completers) | Dynamic Ansible tag completion, replacing sb-go's tag_completion | proto-complete |
| `cross` 0.2.5 images | not semver: the image's musl C toolchain | Release builds for x86_64 and aarch64 musl, since every rustls crypto backend compiles C | proto-musl |

## Decision

1. In `crates/`, each of these is pinned exactly:
   `ratatui = { version = "=0.30.2", features = ["unstable-rendered-line-info"] }`
   (and `ratatui-core`, `ratatui-widgets` at the versions that release
   uses), `clap_complete = { version = "=4.6.11", features = ["unstable-dynamic"] }`,
   and `cross` installed as `cross@0.2.5` with its images used as they are.
2. A pin moves only in its own commit. That commit records what changed in
   the unstable API (from the crate's changelog and source), runs
   `scripts/check.sh full`, and updates this ADR's table.
3. Adding a new unstable feature or non-semver build input needs a new ADR
   (or a superseding one), as decision 9 says.
4. Each unstable call site carries a comment naming this ADR, so a failed
   upgrade points here.

## Alternatives considered

### Caret requirements, as in the prototypes

- Pros: picks up fixes automatically.
- Cons: an unstable API can change in a patch release and break the build,
  or change behaviour silently.
- Rejected: decision 9 requires exact pins.

### Avoid the unstable features

- `line_count`: compute wrapped heights ourselves. Rejected: it re-implements
  ratatui's wrapping, which the library-first rule forbids, and would drift
  from what ratatui draws.
- Dynamic completion: generate static completion scripts and shell out for
  tags, as sb-go does. Rejected: proto-complete showed `CompleteEnv` gives
  one completer for bash, zsh and fish, with less code of ours.
- `cross`: install a musl cross toolchain ourselves. Deferred: worth
  evaluating with CI (`cargo-zigbuild`, or cross from git main), recorded
  in the capability map's open items.

## Consequences

- Upgrades of these three are deliberate and reviewed, never a side effect
  of `cargo update`.
- Upstream fixes reach us only when we move a pin, so a security advisory
  on one of them is handled by moving the pin (cargo-deny reports it).
- The cross 0.2.5 workarounds (a target directory per target, linker
  warnings on aarch64) stay until the release tooling is revisited.
