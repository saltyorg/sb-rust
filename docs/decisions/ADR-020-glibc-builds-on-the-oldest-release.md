# ADR-020: Release builds link glibc, built on the oldest supported release

## Status

Accepted (maintainer, 2026-11-13). Supersedes
[ADR-008](ADR-008-musl-builds-through-cross.md) and the musl half of
the capability map's decision 6. The build landed with hardening Task
53 (the gate and the release build); the journal matrix follows with
Task 54.

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
   GCC and its arm64 glibc (the same 2.35). Task 53 built it and ran it
   (below, "Measured").
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

## Route (Task 53): cross 0.2.5 with the repository's Dockerfile

cross runs this build, so it stays, and the gate keeps no `docker run`
of its own for the build.

- **The evidence, from cross 0.2.5's own docs and source.**
  `docs/cross_toml.md` documents `target.TARGET.dockerfile` (`file`,
  `context`, `build-args`) and `image`
  (https://github.com/cross-rs/cross/blob/v0.2.5/docs/cross_toml.md#targettargetdockerfile).
  `src/docker/custom.rs` builds the named Dockerfile with `docker
  build` and tags it `cross-custom-<workspace>:<target>-<hash>`; the base
  image is the Dockerfile's own `FROM` (the `CROSS_BASE_IMAGE` build
  argument it passes is only used by a Dockerfile that asks for it).
  `src/lib.rs` runs any `--target` in the container, the host's own
  triple included ("It's totally fine to call cross with
  `--target=$host_triple`, for example to test custom docker images"),
  and `src/docker/local.rs` mounts the host's toolchain at `/rust`, so
  the image needs only the C toolchain. `pre-build` wasn't needed: the
  Dockerfile installs the compilers.
- **The files.** `Cross.toml` names `build/Dockerfile`, with `build/` as
  the build context (the repository's `target/` is never sent to the
  daemon), for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu`. The Dockerfile is `FROM ubuntu:22.04` and
  installs `gcc`, `libc6-dev`, `gcc-aarch64-linux-gnu` and
  `libc6-dev-arm64-cross`, and sets cargo's arm64 linker and the cc
  crate's arm64 compiler and archiver.
- **The gate.** `check.sh full` runs `cross build --profile production
  --target x86_64-unknown-linux-gnu` with `target/cross-gnu` as its own
  target directory, gates the size, then runs `sb version` in an
  `ubuntu:<release>` container (`--rm`, `--network none`) for 22.04,
  24.04 and 26.04; a release that can't run it fails the gate.
- **What cross passes in.** cross gives the container every host
  `CARGO_*` variable but a few (`src/docker/shared.rs`), and these win
  over the Dockerfile's `ENV`: a `CARGO_PROFILE_*` or a linker setting
  in the builder's environment changes the binary. check.sh sets the
  target directory and nothing else; a builder keeps such settings out
  of the release build.
- **Why not a `docker run` of our own.** It would re-do what cross
  already does (the toolchain mount, the user, the cargo cache, the
  target directory) in shell, against "Library first".

## Measured (Task 53, 2026-11-13)

On an x86_64 Ubuntu 22.04 build host with Docker 29, the Dockerfile
above, rustc 1.99.1:

- **x86_64:** the production binary is 11,843,920 bytes (the musl
  binary of the same tree: 11,961,376; 117,456 bytes smaller). Its
  highest symbol version is `GLIBC_2.34` (`objdump -T`), within 22.04's
  2.35. It needs libc, libm, libgcc_s and the loader (`readelf -d`).
  `sb version` runs in `ubuntu:22.04`, `ubuntu:24.04` and
  `ubuntu:26.04` containers. The build ran in the image: cross built
  and used `cross-custom-<workspace>:x86_64-unknown-linux-gnu-<hash>`,
  the build log shows the workspace at `/project`, and the arm64 build
  below can't have come from the host, which has no aarch64 compiler.
- **Startup** (`hyperfine -N --warmup 10 --runs 200`, medians, beside
  the musl binary): `sb --help` 2.11 ms (musl 2.16 ms), `sb version`
  2.43 ms (musl 2.58 ms); peak memory 5.5 MiB (musl 3.0 MiB). Inside
  the 10 ms budget; the startup the ratchet measures is the release
  build, which was already glibc.
- **arm64:** `cross build --profile production --target
  aarch64-unknown-linux-gnu` in the same Dockerfile builds, aws-lc-sys
  included, with Ubuntu's `aarch64-linux-gnu-gcc` (GCC 11). The binary
  is 9,974,576 bytes, needs `GLIBC_2.34` at most, and `sb version`
  prints its version under Ubuntu 22.04's qemu-user 6.2
  (`qemu-aarch64 -L /usr/aarch64-linux-gnu`, in a container of the
  image). qemu-user 6.2 answered the theme config read's `openat2`
  with ENOSYS ("Function not implemented"), so sb fell back to the
  built-in theme there, as it does for any unreadable config; that is
  the emulator, not the build. An arm64
  host hasn't run it yet.

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
  the C compiler is old (cross 0.2.5's gnu images are `FROM ubuntu:16.04`;
  ADR-008 noted GCC 9.2.0 in its musl images)
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
  ratchet. The ratchet rows are re-measured when the binary is complete
  (the capability map, "When the binary is complete").
- **musl is revisited when the CLI is feature complete** (the
  maintainer, 2026-11-13). With every command in place, glibc and musl
  builds of the same tree are measured side by side on the same host:
  startup, peak memory, and the commands' own run times. The maintainer
  then decides whether the difference is a problem. The figures so far
  disagree on startup: the libsystemd probe measured glibc 0.1 to 0.6 ms
  slower, while Task 53's builds measured it slightly faster (`--help`
  2.11 against 2.16 ms). Both agree glibc uses more peak memory (5.5
  against 3.0 MiB). A move back to musl would supersede this ADR.
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

## Confirmed (maintainer, 2026-11-17)

glibc with its own malloc stays, ahead of Checkpoint Z: the
side-by-side measurement that Consequences calls for ("musl is revisited
when the CLI is feature complete"), taken on 90e9d4f with the
allocators added, found nothing that favours a change.

**Method.** sb's `production` profile for x86_64, built six ways from
the same tree with rustc 1.100.0 in one ubuntu:22.04 container: glibc
(as above) and static musl (`musl-gcc` for the C dependencies), each
with its libc's malloc, mimalloc 0.1.52 and tikv-jemallocator 0.6.1
(jemalloc 5.3.0) through `#[global_allocator]`, in a scratch tree only.
All six were run in one ubuntu:22.04 container with fixed CPU and
memory limits: startup with `hyperfine -N` (three rounds of 200 runs,
the variants' order rotated), four read-only commands against the
crates' own test fixtures and local fakes (three rounds of 40 runs),
and peak resident memory with GNU time (20 runs). Every variant ran
every command to exit 0 first.

**Figures** (medians):

| | glibc | glibc + mimalloc | glibc + jemalloc | musl | musl + mimalloc | musl + jemalloc |
|---|---:|---:|---:|---:|---:|---:|
| Size (bytes) | 20,303,872 | 20,631,096 | 20,944,792 | 20,448,240 | 20,773,680 | 21,091,920 |
| `sb --help` (ms) | 2.78 | 4.35 | 3.12 | 3.97 | 3.41 | 2.54 |
| `sb version` (ms) | 3.06 | 5.11 | 3.48 | 4.20 | 4.02 | 2.79 |
| `sb list` (ms) | 6.77 | 8.58 | 7.37 | 8.49 | 7.58 | 6.54 |
| `sb motd --all` (ms) | 29.77 | 32.05 | 31.93 | 42.23 | 30.48 | 31.77 |
| `sb docker ps` (ms) | 5.78 | 7.24 | 6.30 | 7.71 | 7.01 | 5.58 |
| `sb validate-config` (ms) | 15.01 | 16.08 | 15.40 | 23.43 | 15.30 | 14.86 |
| Peak, `--help` (MiB) | 7.0 | 15.2 | 8.5 | 5.0 | 13.1 | 6.0 |
| Peak, `motd --all` (MiB) | 13.5 | 23.2 | 15.0 | 11.5 | 23.2 | 13.5 |

**Why glibc with its own malloc.** It is the smallest binary and ties
for fastest on every figure with musl and jemalloc (within about 0.25 ms
on startup and 2 ms on `motd`, which overlap between rounds); its only
cost is 1 to 2 MiB more peak memory than the musl builds. Plain musl's
malloc is what makes musl slow: about 1.2 ms more startup, 8 ms more on
`validate-config` and 12 ms more on `motd`, all gone with jemalloc.
mimalloc's defaults reserve a 1 GiB arena and commit it eagerly, which
cost 6 to 10 MiB more peak memory, and it was never the fastest.
jemalloc on glibc adds 641 KB and is slightly slower to start. Every
variant is inside the 10 ms startup budget and the size limit. A
static binary, if ever wanted, would be musl with jemalloc, not plain
musl.
