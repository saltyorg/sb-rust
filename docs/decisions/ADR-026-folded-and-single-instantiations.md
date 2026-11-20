# ADR-026: Identical code folding and one copy of shared generic code

## Status

Accepted: the maintainer approved the size item and its routes on
2026-11-18, under one rule: no size change may cost any performance.
Each route below was kept only because its measured instruction counts
didn't rise.

## Date

2026-11-18

## Context

The production binary carried second and third copies of the same
generic code. Without the unstable `-Zshare-generics`, each crate that
uses a generic function or awaits an `async fn` compiles its own copy,
and fat LTO doesn't merge the copies. Grouping the unstripped production
binary's symbols by demangled name, on main at f726634, the copies came
to 1,896,444 bytes:

| Family | Duplicate bytes | Compiled in |
|---|---:|---|
| bollard models and serde_json | 717,544 | sb, sb-tui, sb-motd (`connect::list`) |
| hyper, hyper-util and futures plumbing | 360,862 | sb, sb-tui, sb-motd, sb-docker |
| psl's lookup | 159,320 | sb-config, sb-motd |
| tui-markdown and pulldown-cmark | 115,979 | sb-tui, sb-ops |
| saphyr-parser's scanner | 91,718 | sb-config, sb-cli-core |
| everything else | 451,021 | many crates |

The capability map's size notes keep the build profile as it is
(2026-11-17: opt-level 3, panic unwinding, syntax highlighting). Both
routes here leave the profile's settings alone.

## Decision

1. **lld's identical code folding** (`-Wl,--icf=all`) for the sb binary,
   in optimised builds for `x86_64-unknown-linux-gnu`, through
   `crates/sb/build.rs` (`cargo::rustc-link-arg-bin`). rustc links that
   target with LLD by default since 1.90. The aarch64 build links with
   GNU ld, which has no `--icf`, so it is left out. The build script's
   comment says why it isn't `.cargo/config.toml`: cargo would pass those
   flags to every dev and test build on the host, and a profile's own
   `rustflags` is unstable.
2. **One crate compiles each shared entry point:**
   - motd's registrable domains go through sb-config's
     `cloudflare::registrable_domain`, sb's only call into psl.
   - sb-tui's Markdown rendering behind `write_inline` (which sb-ops
     calls) goes through two non-generic functions, one per style sheet.
   - `sb_docker::connect::list` returns a boxed future.

   Each wrapper is `#[inline(never)]`, so LTO can't copy it back into a
   caller.

## Measurements

**Builds:** sb's production profile for glibc, built in an ubuntu:22.04
container (the release image's base, ADR-020) with the pinned toolchain
(rustc 1.100.0, LLD 23.1.5), one build per variant.

**Counts:** user-space instructions and cycles and the task clock
(`perf stat -e instructions:u,cycles:u,task-clock`) for nine commands:
- `sb --help`, `sb version`, `sb list` and `sb docker ps`;
- `sb motd --all`, and `sb motd --all --share` with a known host whose
  name has a dot, so its registrable domain is taken (the psl route);
- `sb validate-config`, whose Cloudflare check takes the zone's
  registrable domain (psl);
- `sb announcements`, piped (tui-markdown's own sheet) and with colour
  forced (the palette's): the Markdown route.

The commands ran in one capped container (4 pinned CPUs), against the
repository's test fixtures (the YAML corpus's Saltbox announcements
among them) and a fake Docker daemon and services. Each count is the
median of the rounds' means of 20 runs, with the binaries' order rotated
each round. The variants' output was the same as the base's, except for
motd's memory line, which reads the host's live figures.

| Variant | Bytes | Change | Instructions |
|---|---:|---:|---|
| main (f726634) | 21,692,928 | — | — |
| `--icf=all` | 21,436,640 | −256,288 (−1.2%) | −0.04% to +0.01% |
| `--icf=safe` | 21,692,928 | 0 | the same binary |
| psl and Markdown routed | 21,348,544 | −344,384 (−1.6%) | −0.04% to −0.01% |
| psl and Markdown routed, `--icf=all` | 21,104,224 | −588,704 (−2.7%) | −0.06% to −0.00% |
| all three routed | 20,847,424 | −845,504 (−3.9%) | −0.06% to −0.00% |
| all three routed, `--icf=all` | 20,621,536 | −1,071,392 (−4.9%) | −0.06% to −0.01% |
| as committed (`crates/sb/build.rs`) | 20,621,536 | −1,071,392 (−4.9%) | −0.05% to +0.02% |

The first runs, three rounds of the first six commands, measured the
first seven variants. Five rounds of all nine commands then measured
main, `--icf=all`, all three routed, and the build as committed (a
build of the same size as "all three routed, `--icf=all`", the same
link flag given by the build script instead of `RUSTFLAGS`).

How to read the figures:
- **Instructions:** each count's spread over its runs was at most
  ±0.06%, and every variant's figure is within 0.06% of main's. That
  includes the routed paths: both forms of `sb announcements`, `--share`,
  and `sb docker ps` and `sb motd`, whose listing is the boxed one.
- **Cycles and the task clock:** these move with the host's load,
  between −3.8% and +6.9% against main among the variants of the
  five-round run, in no direction a variant keeps across the commands.
  The build as committed read fewer cycles than main on all nine
  commands (−0.3% to −3.3%), and a task clock between −2.2% and +0.3%.
  One three-round run had `sb docker ps`'s task clock 8% to 13% above
  main for every variant, `--icf=all` alone included. The five-round run
  didn't repeat it: there the build as committed read −1.3%.
- **Why `--icf=safe` folds nothing:** rustc emits no address-significance
  table, so LLD treats every function as address-taken.

**Tests:** the whole workspace suite (`cargo nextest run --workspace`)
on the production profile as committed, with `-Wl,--icf=all` given to
every test binary too, as root in an ubuntu:22.04 container: 3553 of
3554 passed. The one that failed, the viewer's
`the_save_notice_wraps_in_the_footer`, depends on the temporary
directory's length: its notice wraps at the path, and the container's
`TMPDIR` was shorter than the gate's. With a `TMPDIR` of the gate's
length it passed on the same build.

**What `--icf=all` does and doesn't fold:**
- It folds functions whose code and references are byte for byte the same.
  On their own that is the 256 KB in the table.
- Most cross-crate copies don't qualify, although their instructions
  match: each copy addresses jump tables of its own, so the copies
  differ in their references.
- That is why routing, not folding, takes the larger share.

## Alternatives considered

- **Boxing every bollard call in sb-docker:** after the three routes,
  hyper's client still has four copies, one in each crate that calls
  bollard (sb-docker, sb, sb-tui, sb-motd), at about 0.5 MB. That is
  more than the table's 361 KB at f726634: sb-docker now compiles its
  own copy, under the boxed listing. Taking them
  out means moving every bollard call behind sb-docker. That is a larger
  change than this item, left for later.
- **One owner for saphyr-parser's scanner (92 KB):** sb-config and
  sb-cli-core drive the parser with their own event receivers. Sharing
  the scanner needs a receiver behind a trait object, or one call per
  parse event across the crates. Both are a cost on every event of
  `validate-config`'s parse, so the rule leaves it out.
- **Shared generics:** stable rustc shares generic instantiations across
  crates only at opt-level 0, 1, `s` and `z`, which the capability map's
  opt-level 3 rules out; `-Zshare-generics` at opt-level 3 is a nightly
  flag, and the toolchain is pinned to a stable release
  (`rust-toolchain.toml`). Neither shares `#[inline]` items such as
  psl's lookup.

## Consequences

- **Size:** the production binary is about 1.07 MB smaller. Resetting
  CONSTRAINTS.md's enforced ceiling (21,693,856 bytes, ADR-025) to the
  new size is the maintainer's call, in an ADR of its own.
- **Tests:** release, production and release-tests builds of the sb binary
  are folded, so the gate's release-build tests run the folded binary.
  Dev and coverage builds aren't folded.
- **The linker:** a build that links `x86_64-unknown-linux-gnu` with a
  linker without `--icf` (GNU ld) fails at the link. LLD, gold and mold
  all accept it.

## Amendments

- **2026-11-20, hyper's client compiled once** (the maintainer's
  decision of 2026-11-20: cut size at no runtime cost, then lower the
  ratchet). The alternative above, every bollard call behind sb-docker,
  is taken:
  - **What changed:** `connect::connect` returns a boxed future, as
    `connect::list` does. The log reads and the container events poll a
    boxed bollard stream that sb-docker makes, in the non-inlined
    `logs::outputs` and `events::messages`. `Daemon::docker` is
    crate-private, and deny.toml lets only sb-docker depend on bollard.
  - **Why it works:** a non-generic function's returned future or stream
    is compiled in the crate that polls it. bollard's `logs` and
    `events` return `impl Stream`, so sb-tui compiled hyper's client for
    the log viewer, and sb and sb-motd compiled it for `connect`'s
    version calls. Coercing to `dyn Future` or `dyn Stream` in sb-docker
    compiles it there, once.
  - **Size:** the production binary through the gate's cross build went
    from 21,712,352 to 21,031,072 bytes (−681,280, −3.1%). The copies of
    bollard's, hyper's and futures-util's code beyond the first fell
    from 567,274 to 23,343 bytes.
  - **Instructions:** the commands' user-space instruction counts didn't
    rise. On builds with aws-lc's jitter entropy source off, a
    measurement aid only that makes the TLS commands' counts repeat
    to within ±0.01%, they moved by −0.11% (`sb version`) to +0.00%
    (motd over https). Over five rounds of 20 runs, every figure's range
    for the cut sat at or below main's. That held for `--help`,
    `version`, `list` and `docker ps`, for validate-config and motd,
    each over http and https, for `sb logs` opened on a journal unit and on a
    container (a fake Engine API serving a 20,000-line log), and for a
    warm `sb update`'s own process.
  - **The shipped build:** the gate's cross builds of main and of the
    change were measured the same way. Instructions moved by −0.13% to
    +0.04%; the +0.04% is validate-config over https, inside the ±0.15%
    that aws-lc's jitter entropy gives each round. Wall time (hyperfine,
    60 runs) moved by −3.45% to +0.65%. Peak RSS was the same or lower
    on every command (512 kB lower on `docker ps`, `list` and
    `--help`). The warm update's own process took 500.3 M instructions
    against main's 501.0 M.
  - **Cycles:** validate-config's cycles read 2.0% to 2.8% higher on the
    change's shipped build, with its instructions the same and its wall
    time within 0.2%. The extra cycles sit in rustls-pki-types' base64
    decoder of the root certificates, a loop the change doesn't touch,
    whose instruction count is the same: its placement moved. The same
    code placed elsewhere reads differently. A build with this change
    and the two routes below read 3.5% above main stripped, as shipped,
    and 0.8% above main unstripped. Those two routes on their own,
    which leave this path alone, moved it by −1.2% to +1.6%. Placement
    moves with any change to the binary.
  - **Measured and left out:** two more routes cut size, but each raised
    an instruction count on the jitter-off builds, so the rule leaves
    them out. The crates named are those that compiled a copy, through
    sb-host's and the other crates' own `async fn`s:

    | Route | Bytes | Instructions |
    |---|---:|---|
    | `Systemd::services` boxed (sb, sb-motd and sb-tui had copies) | −78,432 | motd +0.01%, above main's range |
    | `Cmd::capture` boxed (sb, sb-motd and sb-tui had copies) | −16,352 | motd +0.04%, the warm update's process +0.2% |

  - **Checked, nothing to take:**
    - The duplicate versions in sb's tree (`cargo tree -d`): base64,
      itertools, nom and signal-hook have two versions each, at a few
      kilobytes each, and the choice is upstream's. darling and syn are
      build-time only.
    - Features: syntect's YAML and plist loaders, its HTML output, and
      encoding_rs leave no code in the binary, because LTO drops what
      nothing calls.
    - csv and csv-core, the new dependencies since ee870b1, take 3.4 kB.
  - **The ratchet:** the ceiling the Consequences above leave to the
    maintainer was lowered in ADR-025's amendment, as a tightening.
  - **Method:** builds were made in ubuntu:22.04 containers with the
    pinned toolchain, one variant each, attributed with cargo-bloat and
    by the instantiating crate in each symbol's v0 mangling. Each route
    was measured on its own against main. The measurements ran in one
    capped container (4 pinned CPUs), with the binaries' order rotated
    each round:
    - `perf stat`'s instructions, cycles and task clock;
    - hyperfine's wall time;
    - GNU time's peak RSS, and for the TUI and update runs `/proc`'s
      VmHWM of sb's own process.
