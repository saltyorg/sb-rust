# ADR-027: aws-lc built without CPU-jitter entropy

## Status

Accepted (maintainer, 2026-11-20: option A of the TLS cost comparison).
Amended 2026-11-20: post-quantum key exchange offered first (below).

## Date

2026-11-20

## Context

sb's TLS is rustls on aws-lc-rs (capability map, decision 6 and the
stack table). Since AWS-LC 1.60.0 (aws-lc-rs 1.14.1), aws-lc seeds its
random generator in every process from a CPU-jitter collector, as a
second entropy source beside the kernel's. In sb that costs about
450 M instructions before the first TLS handshake.

Measured on the gate's x86_64 production build (cross, ADR-020),
against local fakes:

| Command | sb-go | main a9035ef | A: no jitter | B: ring |
|---|---:|---:|---:|---:|
| `sb motd`, an https service: instructions | 268 M | 536 M | 85 M | 86 M |
| the same: wall time | 50 ms | 64 ms | 25 ms | 25 ms |
| `sb validate-config`, credential checks: instructions | 260 M | 493 M | 43 M | 44 M |
| the same: wall time | 49 ms | 56 ms | 17.5 ms | 17.5 ms |
| self-update's release check: instructions | n/a | 476 M | 24 M | 25 M |

So two TLS commands needed more instructions than sb-go. Commands
without TLS (`--help`, `list`, motd over http) are within 1% in all
three builds.

Two fixes were built and measured:
- **A:** aws-lc built without the jitter source, with aws-lc-sys's
  documented `AWS_LC_SYS_NO_JITTER_ENTROPY=1`.
- **B:** rustls and reqwest on ring instead of aws-lc-rs.

## Decision

Option A. `.cargo/config.toml` at the workspace root sets
`AWS_LC_SYS_NO_JITTER_ENTROPY = "1"` under `[env]`. Every cargo run
started inside the repository sees it: local builds, every gate stage
and the cross builds of both targets. `crates/sb/tests/entropy.rs`
asserts that `aws_lc_rs::try_fips_cpu_jitter_entropy()` errs and that
no jitterentropy symbol is linked into the test binary, so a renamed or
ignored variable fails the gate, not just the instruction counts.

aws-lc then seeds from `getrandom(2)`, plus RDRAND or RNDR where the CPU
has a usable one: the trust base of ring and of Go's `crypto/rand`
(sb-go). It still blocks until the kernel's pool is ready, and still
aborts if the OS source fails. The per-thread DRBG, its reseeding and
its fork detection don't change. The sources, what is lost and why that
is acceptable for a short-lived TLS client are in
[docs/platform-notes/aws-lc-entropy.md](../platform-notes/aws-lc-entropy.md).

## Alternatives Considered

### B: rustls on ring (measured, declined)

- **Pros:**
  - the same speed as A, within 1 M instructions and 0.2 ms on every
    figure;
  - 2,088,416 bytes (9.9%) off the x86_64 production binary, 7% off
    arm64; aws-lc, cmake and four more build dependencies leave the
    build;
  - one round trip everywhere, because it offers no post-quantum group
    a server could ask for.
- **Cons:**
  - **no post-quantum key exchange:** ring has no ML-KEM, so sb would
    lose X25519MLKEM768, which sb-go has today and Cloudflare, Docker
    Hub, the SVM proxy and Traefik 3.7 accept;
  - **about 30 hand-written lines** across 14 files (+28 −194 with
    Cargo.lock);
  - **a provider-install rule:** reqwest's `rustls-no-provider` panics
    at `Client::build()` unless a process-default provider was
    installed first, so every future HTTP client would have to come
    after `install_provider()`, guarded by a test or a lint;
  - **recorded decisions reversed:** capability map decision 6, its
    stack table and its rejected line on ring, the release spec's
    decision 6, the config and python-env specs' HTTP lines, and the
    mentions in ADR-008 and ADR-020.
- **Rejected:** the size gain is real and costs nothing at run time, but
  it doesn't outweigh losing post-quantum key exchange, a runtime panic
  path and the reversed decisions, when A fixes the same cost with one
  configuration file.

### Warming the generator early on another thread

- **Rejected:** it hides wall time only where there is other work to
  overlap, still spends the ~450 M instructions, and adds code of sb's
  own. aws-lc has no run-time switch for its entropy source.

### Keeping the default

- **Rejected:** sb would stay slower than sb-go on its TLS commands, and
  exposed to the jitter health test's abort on some arm64 boards
  (https://github.com/aws/aws-lc/issues/3604).

## Consequences

- motd over https, validate-config's credential checks, `sb update` and
  self-update lose the jitter cost: 2 to 2.8 times faster than sb-go in
  wall time on the first two. The production binary is 19,648 bytes
  smaller.
- **One entropy source on some CPUs:** on AMD Zen 2 or older and on
  arm64 without FEAT_RNG, the kernel is the only source, as for ring and
  sb-go. Intel, Zen 3 or newer and arm64 with FEAT_RNG keep two.
- **Builds outside the repository** (for example `cargo install --git`,
  or `--manifest-path` from another directory) don't read the file and
  build with jitter. Releases build in the repository. A value already
  in the environment, even an empty one, overrides `[env]`; the gate's
  tests then fail.
- **The production cross builds aren't asserted by the gate.** The tests
  check the native test builds; the production builds are separate
  aws-lc-sys builds. Nothing sets the variable for them, `Cross.toml`
  passes only `SB_*` into cross's container, and their build-script
  output was checked by hand for both targets. A gate step that greps
  it was weighed and declined: no path in the gate or the release
  workflow can drop the setting today.
- **FIPS:** a FIPS build refuses the setting. sb has none; needing one
  would bring the cost back.
- **Post-quantum key exchange stays,** but rustls's default order sends
  no ML-KEM key share, so Cloudflare, Docker Hub, the SVM proxy and
  Traefik ask for a retry first. rustls's `prefer-post-quantum` feature
  is a separate question. *Answered by the amendment below.*
- **Revisit** when sb makes long-lived keys or signatures with aws-lc's
  generator, when upstream makes jitter a fallback only
  (https://github.com/aws/aws-lc/issues/3180), or if sb needs FIPS.
- aws-lc#3818, an abort unrelated to this setting, is listed to watch in
  the capability map.

## Amendments

- **2026-11-20, post-quantum key exchange offered first** (the
  maintainer's decision, from the measurements below).
  - **Why:** reqwest's rustls has its `prefer-post-quantum` feature off
    (rustls turns it on by default; reqwest disables rustls's defaults).
    rustls then lists X25519MLKEM768 last and sends only an X25519 key
    share, so every server that prefers the hybrid answers with a
    HelloRetryRequest: a round trip more per connection.
  - **The change:** each crate that builds a reqwest client (sb-config,
    sb-docker, sb-motd, sb-ops, sb-release) depends on rustls with
    `prefer-post-quantum`, next to its reqwest line
    (https://docs.rs/rustls/0.23.46/rustls/#crate-features: it "merely
    alters the order of `rustls::crypto::aws_lc_rs::DEFAULT_KX_GROUPS`").
    The feature is set in each crate, so each one's own tests run with
    what ships; the workspace entry has none, so sb's test can name
    rustls without turning the feature on. No shared crate fits: the
    ones all five use (sb-cli-core, sb-term-ui) have no HTTP, and aws-lc
    would be compiled into every crate under them. In the `sb` crate
    alone it would ship as well, but the five crates' own test builds
    would then run without it.
  - **What it sends:** X25519MLKEM768 first, with a key share for it and
    one for X25519 (its classical half, at no extra cost), as sb-go
    does. The ClientHello grows from about 260 to about 1,480 bytes.
  - **Measured** on the production build (x86_64), against the TLS fakes
    and the real endpoints:

    | Server | ClientHellos per connection, before → after |
    |---|---|
    | api.cloudflare.com | 2 (retry) → 1 |
    | hub.docker.com (Docker Hub's login) | 2 (retry) → 1 |
    | svm.saltbox.dev (the SVM proxy) | 2 (retry) → 1 |
    | Traefik 3.7.16 | 2 (retry) → 1 |
    | github.com, api.github.com, registry-1.docker.io | 1 → 1 (X25519, from the second share) |

    The retry costs one round trip to the server, so the saving grows
    with the distance to it. Against Traefik on loopback, the time from
    the first ClientHello to the last ServerHello fell from 3.5 to
    1.9 ms (median of 15 connections). Against the local fakes, motd over https, validate-config's
    credential checks and self-update's check moved by under 1% in
    instructions: 0.1 to 0.3 M fewer against a server with ML-KEM (no
    retry), 0.2 to 0.6 M more against one without (an unused ML-KEM key).
    Wall times moved by at most 1 ms. Certificate checks and TLS 1.2
    servers behave as before.
  - **Costs:** the larger ClientHello takes two TCP segments, and
    rustls's manual warns that some middleboxes mishandle large hellos.
    Every endpoint above accepted it, and sb-go already sends a hello of
    about the same size (1,510 to 1,530 bytes) to the same servers.
    Servers without ML-KEM pay for a key that goes unused.
  - **Pinned** twice:
    - `crates/sb/tests/key_exchange.rs`: the provider reqwest builds on
      (`rustls::crypto::aws_lc_rs::default_provider()`, as no process
      default is installed) lists X25519MLKEM768 first and also offers
      X25519. It fails with the feature off.
    - `scripts/check.sh full`: sb's normal dependency graph, as a release
      build resolves it, has rustls's `prefer-post-quantum`. A test build
      includes the dev-dependencies' features, so a dev-dependency
      turning rustls's defaults on would otherwise hide a drop.
