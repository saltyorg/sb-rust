# musl builds with C dependencies: aws-lc-sys

Verified 2026-11-03 for the `config` module (config plan, Task 1).
Extends [proto-musl's findings](../../prototypes/proto-musl/FINDINGS.md),
which found the same failure from reqwest's rustls, beside bollard and
portable-pty.

**Software:** Rust 1.99.1 (the repository's pin); reqwest 0.13.5
(`default-features = false`, features `rustls`, `json`), which brings
rustls 0.23.46, aws-lc-rs 1.19.0, aws-lc-sys 0.47.0 and
rustls-platform-verifier 0.7.1; serde-saphyr 2.1.0; saphyr 0.3.3;
saphyr-parser 0.3.3; regex 1.13.1; psl 2.1.246 (sb-config pins 2.1.241, the newest release at least two weeks old when it was added); jiff 0.2.39
(`default-features = false`, features `std`, `tz-system`,
`tzdb-zoneinfo`); tokio 1 (`rt`, `net`, `time`); cross 0.2.5 with its
`ghcr.io/cross-rs/*-unknown-linux-musl:0.2.5` images (GCC 9.2.0, CMake
3.23.1); cargo-bloat 0.12.1; cargo-deny 0.20.2.

**Method:** a scratch binary crate using each of the crates above (a
schema load through serde-saphyr, a `MarkedYaml` load, a saphyr-parser
event walk, a regex match, a `psl::domain` lookup, a jiff zone lookup, a
reqwest GET), with sb's `production` profile (fat LTO, one codegen unit,
stripped). It was built the way `scripts/check.sh` builds sb
(`cargo build --profile production --target x86_64-unknown-linux-musl`),
then with `cross build` for both musl targets. Sizes come from
`cargo bloat --crates` on an unstripped build of the same profile for
`x86_64-unknown-linux-gnu`, because cargo-bloat drives its own build and
the musl one needs a C compiler. The scratch crate was removed afterwards.

## The plain musl build fails

`cargo build --target x86_64-unknown-linux-musl` stops in
`aws-lc-sys`'s build script: `failed to find tool
"x86_64-linux-musl-gcc"`. aws-lc-sys is the only crate in this tree that
compiles C (`cargo tree -i cc`: cc ← cmake ← aws-lc-sys ← aws-lc-rs ←
rustls, rustls-webpki). Everything else is pure Rust. So check.sh's size
step will fail once sb links sb-config (Task 11), on any machine without a
musl C cross-compiler.

## `cross` builds both targets

- `cross build --profile production --target x86_64-unknown-linux-musl`
  succeeds: a static-pie binary, 8,389,760 bytes for the scratch crate.
- `aarch64-unknown-linux-musl` succeeds too (6,681,112 bytes), but only
  in its own target directory. Reusing the x86_64 run's directory failed:
  the build scripts compiled inside one image can't run inside the other
  (`symbol gettid version GLIBC_2.30 not defined`). This matches
  proto-musl's finding: one `CARGO_TARGET_DIR` per cross target, never
  shared with host builds.
- cross needs a container engine (Docker or Podman) and pulls its images
  on first use.

## Routes for check.sh's size step

| Route | What changes | Cost |
|---|---|---|
| A musl C toolchain on the machine | `x86_64-linux-musl-gcc` (for example musl-cross-make, or a distribution's musl-gcc wrapper) added to CONSTRAINTS.md "Tools"; check.sh unchanged | A system install per developer and CI image; aarch64 needs a second toolchain if it's ever checked |
| `cross` in check.sh's size step | The step runs `cross build` with its own target directory; Docker and cross 0.2.5 in "Tools" | Old 0.2.5 images (GCC 9.2); a container engine wherever `full` runs; the first run pulls the image |
| `cargo-zigbuild` | The step runs `cargo zigbuild`; zig and cargo-zigbuild in "Tools" | Not tried here; zig as C compiler for aws-lc-sys is unverified |

Swapping aws-lc-rs for ring doesn't avoid it: ring also compiles C and
assembly for the target (proto-musl).

## Size of each dependency

`cargo bloat --crates`, `production` profile unstripped, `.text` only
(cargo-bloat calls its numbers guesswork; they add up to 5.7 MiB of
`.text` in a 10.8 MiB unstripped file):

| Crate | `.text` | Brought by |
|---|---|---|
| aws_lc_sys | 1.3 MiB | rustls's crypto provider (C) |
| psl | 641 KiB | the public suffix list, compiled in |
| rustls | 383 KiB | reqwest |
| regex_automata, regex_syntax, aho_corasick | 283 + 158 + 136 KiB | regex |
| granit_parser | 221 KiB | serde-saphyr's own YAML parser (not saphyr-parser) |
| serde_saphyr | 200 KiB | schema loading |
| saphyr_parser, saphyr | 150 + 25 KiB | already in sb (cli-core) |
| annotate_snippets | 143 KiB | serde-saphyr's error rendering |
| reqwest, hyper, hyper_util, tokio, http, url, webpki | 91 + 59 + 80 + 57 + 45 + 49 + 63 KiB | reqwest; hyper and tokio are already in sb (sb-docker) |
| jiff | 21 KiB | timezone lookups (no embedded database) |

- **serde-saphyr's features:** with `default-features = false, features =
  ["deserialize"]` (no serializer, no comment support), granit_parser
  drops from 279 to 221 KiB; the rest is unchanged. That is the feature
  set sb-config needs.
- **Two YAML parsers:** serde-saphyr 2.x parses with granit-parser, a
  separate crate from saphyr-parser, which sb already carries for the
  inventory and which sb-config uses for positions. Both end up in the
  binary.
- **What sb gains:** less than the scratch binary, since std, tokio,
  hyper, serde and saphyr are already in sb. Task 11's size ADR measures
  the real growth.

## cargo-deny

The scratch tree against sb's `deny.toml`: `advisories ok, bans ok,
licenses ok, sources ok`. Warnings only:

- duplicates `cfg-if` 0.1.10 / 1.0.5 and `encoding_rs` 0.7.2 / 0.8.44,
  both from serde-saphyr's `encoding_rs_io` 0.1.8 (part of the
  `deserialize` feature);
- unused wrappers and licences, because the scratch crate isn't sb.

When sb-config exists, deny.toml's wrappers gain it for `sb-cli-core` and
`sb-term-ui`, and `sb-config` itself is wrapped by `sb`.
