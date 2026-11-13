# ADR-020: Release builds link glibc, built on the oldest supported release

## Status

Accepted (maintainer, 2026-11-13). Supersedes
[ADR-008](ADR-008-musl-builds-through-cross.md) and the musl half of
the capability map's decision 6. The build lands with hardening Tasks
53 and 54; until then the gate still builds musl through cross.

## Date

2026-11-13

## Context

Decision 6 of the capability map ships static musl binaries, and
ADR-008 builds them through `cross`, because aws-lc-sys compiles C and a
plain musl build needs a musl C cross-compiler
([musl-c-dependencies.md](../platform-notes/musl-c-dependencies.md)).
musl was chosen so that one binary runs on every supported release.

A glibc binary does the same if it is built against the oldest glibc sb
supports: glibc keeps old symbol versions, so a binary linked on an
older release runs on every newer one. sb runs only on the releases in
the host spec's runtime list (22.04, 24.04, 26.04) and refuses the rest,
so the oldest is 22.04, with glibc 2.35.

The maintainer asked, on 2026-11-13, to drop musl and build on the
oldest supported Ubuntu release. The deep dive into libsystemd
(2026-11-12, [libsystemd-reader.md](../platform-notes/libsystemd-reader.md))
had already measured a glibc build of sb made in an `ubuntu:22.04`
container:

- it needs GLIBC_2.34 at most and links only libc, libm, libgcc_s and
  the loader;
- it runs on 22.04, 24.04 and 26.04;
- it is 118 KB smaller than the musl binary;
- `sb --help` starts 0.1 to 0.6 ms slower, with about 2.4 MB more peak
  memory.

## Decision

1. **Release binaries are dynamically linked against glibc**, for Linux
   amd64 and arm64 (`x86_64-unknown-linux-gnu`,
   `aarch64-unknown-linux-gnu`). No musl target is built or installed.
2. **They are built in an image of the oldest supported release**
   (`ubuntu:22.04` today), with the toolchain `rust-toolchain.toml`
   pins. The image follows the runtime list: when sb drops its oldest
   release, the image moves to the next oldest in the same change. Like
   a toolchain bump, that is a deliberate change.
3. **arm64 is cross-compiled in the same image**, with Ubuntu's aarch64
   GCC and its arm64 glibc (the same 2.35). UNVERIFIED: aws-lc-sys
   hasn't been built this way yet. Task 53 checks it, and a failure
   comes back here.
4. **The gate builds and checks the production binary the same way.**
   `check.sh full` builds the production x86_64 binary in the image,
   gates its size, and runs `sb version` in a container of every
   release on the runtime list, so a binary that one release can't
   load fails the gate.
5. **The journal matrix builds its test binary in the image too**, so
   the binary under test is built the way sb ships, and runs in the
   22.04 container whichever host built it.

The route is chosen by Task 53 under "Library first": `cross` 0.2.5
with a custom image in `Cross.toml`, if it runs this build, else a
`docker run` of the repository's own image with `cross` leaving the
Tools table. Docker stays a Tools row either way.

## Alternatives considered

- **Keep static musl (ADR-008).** One binary that runs anywhere, with
  no loader or libc to match, but it needs a musl C toolchain in every
  build (cross's images today), and musl's allocator and resolver
  differ from what the rest of the host uses. Rejected by the
  maintainer.
- **Build glibc on the developer's or CI's own release.** Simplest, but
  a binary built on 24.04 needs glibc 2.39 and doesn't start on 22.04.
  Rejected: the build host would decide which releases sb runs on.
- **cross's stock gnu images.** They are built on an older Ubuntu than
  any sb supports, so a binary built there would also run on 22.04, but
  the C compiler is old (ADR-008 noted GCC 9.2.0 in the musl images)
  and the build would no longer match a release sb supports. Not
  measured; a custom image does what the maintainer asked.
- **`cargo-zigbuild` with a glibc version suffix** (`--target
  x86_64-unknown-linux-gnu.2.35`). It needs no image, but zig as
  aws-lc-sys's C compiler is still unverified (ADR-008) and would be a
  new tool. Not tried.

## Consequences

- The binary loads glibc at start. That costs the 0.1 to 0.6 ms
  measured, inside the startup budget (CONSTRAINTS.md: 10 ms, and the
  ratchet's 1 ms growth per change). The size shrinks, inside its
  ratchet. Checkpoint C's ratchet ADR re-measures both.
- User lookup (`getpwnam_r`) and name resolution (`getaddrinfo`) go
  through glibc's NSS, as every other program on the host does, instead
  of musl's own `/etc/passwd` and `resolv.conf` readers. On a Saltbox
  host the answers agree. The host spec's user-lookup note changes with
  this.
- Nothing is linked from libsystemd. This doesn't reopen ADR-013: plan
  decision 27's reasons (no reader in the pure-Rust crate, `unsafe` for
  `sd_journal_step_one`, entries lost across a flush, SIGBUS) don't
  depend on musl.
- The changes land with Task 53, in the files the build touches:
  - `rust-toolchain.toml`'s targets;
  - `deny.toml`'s targets;
  - CONSTRAINTS.md's binary-size row and Tools rows;
  - `scripts/check.sh` and the floor guard's tests;
  - the cli-core spec's build command.

  The journal scripts and `.github/workflows/journal-matrix.yml` follow
  in Task 54.
- [proto-musl](../prototypes/proto-musl.md) and
  [musl-c-dependencies.md](../platform-notes/musl-c-dependencies.md)
  stay as records of what was measured. They no longer describe the
  release build.
