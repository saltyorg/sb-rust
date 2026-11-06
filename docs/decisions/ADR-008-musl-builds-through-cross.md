# ADR-008: Production musl builds through `cross`

## Status

Accepted (maintainer, 2026-11-03, config Checkpoint A)

## Date

2026-11-03

## Context

`scripts/check.sh full` builds sb with `cargo build --profile production
--target x86_64-unknown-linux-musl` and gates its size (CONSTRAINTS.md,
"Binary size"). Until now sb's tree compiled no C, so a plain cargo build
worked anywhere the Rust target was installed.

`sb validate-config` needs reqwest with rustls and the aws-lc-rs provider
(the capability map's stack table). aws-lc-sys compiles C for the target,
so a plain musl build stops with `failed to find tool
"x86_64-linux-musl-gcc"` on any machine without a musl C cross-compiler
([musl-c-dependencies.md](../platform-notes/musl-c-dependencies.md);
[proto-musl](../prototypes/proto-musl.md) found the same).
`cross` 0.2.5 built the planned stack for both
x86_64-unknown-linux-musl and aarch64-unknown-linux-musl.

## Decision

check.sh's production musl build runs through `cross build --profile
production --target x86_64-unknown-linux-musl`, with its own
`CARGO_TARGET_DIR` (never shared with host builds or another cross
target: build scripts compiled in one image can't run in another).
Docker and cross 0.2.5 join CONSTRAINTS.md "Tools". The change lands
with config plan Task 11, the first commit that links aws-lc-sys into
sb.

## Alternatives considered

- **A musl C toolchain on every machine** (`x86_64-linux-musl-gcc`, for
  example from musl-cross-make): check.sh unchanged, but a system install
  on every developer machine and CI image, and a second toolchain for
  aarch64.
- **`cargo-zigbuild`:** not tried; zig as aws-lc-sys's C compiler is
  unverified.
- **ring instead of aws-lc-rs:** ring compiles C and assembly for the
  target too (proto-musl), so it doesn't avoid the compiler.

## Consequences

- `full` needs a container engine and pulls cross's image on its first
  run; check.sh exits 2 naming a missing tool, as for the others.
- cross 0.2.5's images are old (GCC 9.2.0, CMake 3.23.1). They build
  aws-lc-sys today; a newer cross release or `cargo-zigbuild` can replace
  them later through a new ADR.
- Release builds for both architectures can use the same route.
- The size limit itself doesn't change here; Task 11's size ADR records
  the growth.
