# Prototype: proto-musl

**Question:** Do clap, tokio, reqwest (rustls), bollard and portable-pty build
into one static musl binary for x86_64 and aarch64, at an acceptable size and
build time?

**Crates tried:** clap@4.6.7, tokio@1.54.1, reqwest@0.13.5 (default features:
rustls with the aws-lc-rs provider), bollard@0.21.1, portable-pty@0.9.0,
anyhow@1.0.104. Toolchain 1.99.1, cross@0.2.5.

**What was built:** one binary with three subcommands: an HTTPS GET, a Docker
container listing over the local socket, and a command run under a PTY.

## Result

- **Plain `cargo build --target *-musl` fails.** `aws-lc-sys` (pulled in by
  rustls through reqwest's default TLS) compiles C and needs a musl C
  cross-compiler (`x86_64-linux-musl-gcc`, `aarch64-linux-musl-gcc`). Swapping
  to the `ring` provider doesn't avoid this, since ring also compiles C. Any
  rustls build needs a musl C toolchain.
- **`cross` builds both targets.** Gotchas found:
  - Each target needs its own `CARGO_TARGET_DIR`. The cross 0.2.5 images ship
    different glibc versions, and shared build-script binaries fail with
    `GLIBC_2.28 not found`.
  - Host builds and cross builds must not share a target directory, for the
    same reason.
  - The cross 0.2.5 aarch64 linker emits `linker_messages` warnings. They are
    harmless but noisy, which points at the age of the 0.2.5 images.
- **Output:** `file` reports x86_64 as `static-pie linked` and aarch64 as
  `statically linked`.
- **Size (release, stripped):** x86_64 8.2 MB, aarch64 6.5 MB. 172
  unique crates in the dependency tree.
- **Clean build time (release, cross):** about 30 s per target.
- **Runtime on x86_64 (Ubuntu):**
  - HTTPS GET: 200, so the rustls handshake and root store work.
  - The Docker listing worked over the local socket.
  - The PTY run kept ANSI colours, the child saw a TTY, and a non-zero exit
    was reported.
- **Runtime on aarch64:** ran under qemu via `cross run`. The HTTPS GET
  returned 200 and the PTY run worked. The Docker listing wasn't run, because
  the cross container has no Docker socket.
- **Side finding:** GitHub's API returns 403 without a `User-Agent`. The
  release and self-update clients must set one.

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt with changes.** The stack builds and runs statically on both
architectures. The build pipeline needs a musl C toolchain.

## Map impact

- Stack table: release builds use `cross` (or another musl C toolchain in CI),
  not plain `cargo build`. Keep reqwest's default rustls with aws-lc-rs. ring
  saves nothing, since both need C.
- Follow-up worth evaluating when CI is set up: cross from git main (newer
  images) or `cargo-zigbuild`, to drop the old 0.2.5 images and the
  per-target directory workaround.
- `release` module: the HTTP client sets a `User-Agent`.
