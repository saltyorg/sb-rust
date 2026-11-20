# aws-lc's entropy sources, and sb's build without CPU jitter

Verified 2026-11-20, for [ADR-027](../decisions/ADR-027-aws-lc-without-cpu-jitter.md).

**Software:** aws-lc-rs 1.18.2, aws-lc-sys 0.46.0 (AWS-LC 5.11.0,
jitterentropy-library 3.6.3), rustls 0.23.46; Linux 5.15, 6.8 and 7.0
(Ubuntu 22.04, 24.04 and 26.04's kernels).

**Method:**
- Read aws-lc-sys's build script (`builder/main.rs`, `cc_builder.rs`,
  `cmake_builder.rs`) and the AWS-LC sources it vendors
  (`crypto/fipsmodule/rand/`, `crypto/rand_extra/urandom.c`,
  `crypto/fipsmodule/self_check/fips.c`), at the versions above; the
  kernel's `drivers/char/random.c` at v5.15, v5.18, v6.8 and v7.0; the
  upstream issues and documentation linked below.
- Measured the gate's x86_64 production build (cross, `build/Dockerfile`)
  with and without the setting: `perf stat` instruction counts and
  hyperfine wall times, in containers without a network, against local
  fakes of the services sb talks to.
- Built aarch64 the same way, and ran it under QEMU user-mode.
- Both tests in `crates/sb/tests/entropy.rs` fail with the setting
  overridden to `0` and pass with it.

## The setting

`AWS_LC_SYS_NO_JITTER_ENTROPY=1`, in `[env]` of the workspace's
`.cargo/config.toml`. The aws-lc-rs user guide documents it
(https://aws.github.io/aws-lc-rs/resources.html#entropy-configuration):
"When set to `1`, disables the CPU jitter entropy source in the build.
… a one-time-per-process latency cost, typically around 50ms … This
option is only available for `aws-lc-sys`." Neither crate has a Cargo
feature for it.

How aws-lc-sys 0.46.0 reads it:
- `builder/main.rs` looks up `AWS_LC_SYS_NO_JITTER_ENTROPY_<target>`
  (the target in lower case, `-` as `_`), then
  `AWS_LC_SYS_NO_JITTER_ENTROPY`. Each gets `cargo:rerun-if-env-changed`,
  so a change rebuilds aws-lc-sys, and a value found is printed as a
  build warning: `Environment Variable found
  'AWS_LC_SYS_NO_JITTER_ENTROPY': '1'` in the build script's output.
- A value starting with a digit other than `0`, or `on`, `y…` or `t…`,
  counts as true; `0`, `off`, `n…` and `f…` as false. Anything else,
  an empty value included, counts as unset, and the build script's own
  detection decides (jitter on, on Linux).
- The cc builder (the default) then defines `DISABLE_CPU_JITTER_ENTROPY`
  for every file and leaves out the six jitterentropy-library sources,
  which are otherwise compiled on their own at `-O0`.
- AWS-LC's own CMake option text: "With this configuration, randomness
  generation might not use two independent entropy sources." A FIPS
  build refuses it at configure time. sb has no FIPS build.

Cargo applies `[env]` to build scripts and reads `.cargo/config.toml`
from the working directory upward, not from the manifest's
(https://doc.rust-lang.org/cargo/reference/config.html#env): a build
started outside the repository with `--manifest-path`, or a
`cargo install --git`, doesn't see it. cross runs cargo inside its
container in the mounted workspace, so the file reaches the cross
builds too: their aws-lc-sys build-script output carries the warning
above for both targets. Cross.toml passes only the `SB_*` variables
into that container, so a host value can't reach it.

What wins over `[env]`: a value already in the environment, even an
empty one (which then counts as unset), and the per-target form
`AWS_LC_SYS_NO_JITTER_ENTROPY_<target>`, even over `force = true`. The
test's failing case was made that way.

## Where randomness comes from

Common to both builds (`crypto/fipsmodule/rand/rand.c`):
- Each thread draws from its own AES-256 CTR-DRBG, reseeded after 4096
  calls.
- A forked child reseeds before its first output: one page is marked
  `MADV_WIPEONFORK`, and the child sees it zeroed.
- The OS source is the `getrandom(2)` system call (`/dev/urandom` only
  where the call is missing). If the kernel's pool isn't initialised,
  aws-lc prints a line and blocks until it is. A failed read aborts.
- VM-snapshot detection exists only through `/dev/sysgenid`, an Amazon
  device that mainline Linux doesn't have. Without it, neither build
  detects a snapshot.

| Input | Default build (jitter) | With the setting |
|---|---|---|
| Seed, 48 bytes | a per-process DRBG tree seeded by jitterentropy | `getrandom` |
| Personalisation at seed, additional input at reseed | `getrandom` | RDRAND (x86_64) or RNDR (arm64) where usable, else `getrandom` again |
| Additional input on every call | RDRAND or RNDR where usable | none |

The setting's source is the one upstream already selects at run time on
hosts with `/dev/sysgenid` (`entropy/entropy_sources.c`,
`opt_out_cpu_jitter_entropy_source_methods`).

"Where usable" is narrower than "where the CPU has it":
- AWS-LC masks RDRAND on AMD family 0x17 (Zen, Zen+, Zen 2) and older
  (`cpucap/cpu_intel.c`). Intel and Zen 3 or newer keep it.
- RNDR needs FEAT_RNG (Armv8.5). Cortex-A72, Cortex-A76 and Neoverse N1
  don't have it.

So with the setting:
- **Intel, AMD Zen 3 or newer, arm64 with FEAT_RNG:** two sources, the
  kernel and the CPU's generator. They aren't fully independent: the
  kernel mixes the same CPU generator into its own output. In a VM,
  RDRAND is there only if the hypervisor exposes it.
- **AMD Zen 2 or older, arm64 without FEAT_RNG:** the kernel only. That
  is what ring (`getrandom` per call) and Go's `crypto/rand` (sb-go) use
  on every CPU. Most arm64 hosts today are in this row: Neoverse N1
  servers (Ampere Altra, AWS Graviton2) and Raspberry Pi 4 and 5
  (Cortex-A72, Cortex-A76).

## What is lost, and why that is acceptable for sb

What the setting gives up: a source that doesn't depend on the kernel,
and the CPU generator's input on every call.

Why it doesn't matter for sb:
- **What sb uses randomness for:** sb is a short-lived TLS client. Its
  random bytes are the ClientHello random and the ephemeral key-exchange
  secrets. It makes no long-lived keys and no signatures with them.
- **Early boot:** `getrandom` blocks until the kernel's pool is
  initialised, and since Linux 5.4 a waiting caller makes the kernel
  collect timing jitter itself. sb runs from a login or an SSH session,
  after sshd needed the same pool.
- **VM clones:** a cold clone seeds its own kernel at boot. A live
  snapshot resumed twice gets a kernel reseed at once with vmgenid
  (mainline since 5.18). Without it, both copies return the same bytes
  until the next reseed: at most 60 s on 6.8, 7.0 and the stable 5.15.y
  kernels (which carry the 5.17/5.18 rework, as Ubuntu's 5.15 does),
  300 s on mainline 5.15 as tagged. Two sb processes would then send
  the same client random and key share. Each session still gets its own
  keys from the server's share, and no private key leaks.
- **The trust base:** with the setting, sb trusts the kernel generator,
  as sb-go, sshd, Docker and Traefik on the same host already do.

## What it also fixes

- **The cost:** the jitter collector runs once per process, before the
  first handshake. In the gate's x86_64 production build, the setting
  took the user-space instructions from 536 M to 85 M for `sb motd` with
  an https service, 493 M to 43 M for `sb validate-config` with its
  credential checks, and 476 M to 24 M for self-update's release check.
  motd's wall time fell from 64 to 25 ms. Commands without TLS were
  unchanged within 1%, and the binary is 19,648 bytes smaller.
- **An abort on some arm64 boards:** the collector's health test can
  fail on slow-timer cores, and aws-lc then aborts the process
  (https://github.com/aws/aws-lc/issues/3604: about 12% of process starts
  on RK3399's Cortex-A72 cores; open). The fix
  (https://github.com/aws/aws-lc/pull/3631) isn't merged.
- **Fewer CPU-generator reads:** a failed RDRAND or RNDR read aborts too
  (https://github.com/aws/aws-lc/issues/3594, open). The setting reads it
  at seed and reseed only.
- **No `-O0` sub-build:** compiler flags leaking into it have broken
  aws-lc-sys builds before (aws-lc-rs issues 1097, 1154 and 1252).

## How it is checked

`crates/sb/tests/entropy.rs` has two tests, run in every gate run's
test builds (the coverage run and the release-build run). A renamed
variable would build fine and fail both.
- **At run time:** `aws_lc_rs::try_fips_cpu_jitter_entropy()` returns
  `Err`
  (https://docs.rs/aws-lc-rs/1.18.2/aws_lc_rs/fn.try_fips_cpu_jitter_entropy.html:
  "Return an error if the underlying implementation is not using CPU
  jitter entropy"). It calls `FIPS_is_entropy_cpu_jitter()`, which only
  compares the configured source's id and starts no collection. On a
  host with `/dev/sysgenid` a build with jitter selects the opt-out
  source at run time too, so this test alone would pass there; mainline
  kernels and Ubuntu's don't have the device.
- **In the linked code:** the test binary's symbol table (read with the
  `object` crate) has no `jent_entropy_init` symbol. With jitter, the
  entropy-source table pulls jitterentropy's code in
  (`aws_lc_0_46_0_jent_entropy_init_ex` and two helpers, in this
  version); without it, the library isn't compiled. A symbol aws-lc-rs
  always links is asserted first, so a stripped binary can't pass.

What the tests don't cover: the production cross builds, a separate
build of aws-lc-sys for each target. Nothing in the gate, the release
workflow, `build/Dockerfile` or `Cross.toml` sets the variable, and the
cross builds' build-script output was checked by hand for both targets
on 2026-11-20 (the warning above).

## Not changed by it

aws-lc#3818 (https://github.com/aws/aws-lc/issues/3818, opened
2026-11-18 against aws-lc-sys 0.46.0, open): a thread whose first
`RAND_bytes` call comes after process-exit clean-up has begun aborts the
process (it read-locks a lock it already holds). The code is in
`rand.c`, which both builds share. It is listed to watch in the
capability map.

## Revisit when

- sb makes long-lived keys or signatures with aws-lc's generator;
- upstream makes jitter a fallback for CPUs without a hardware
  generator (https://github.com/aws/aws-lc/issues/3180), which would end
  the cost on most CPUs without the setting;
- sb needs a FIPS build, which refuses the setting.

## Links

- aws-lc-rs user guide, "Entropy Configuration":
  https://aws.github.io/aws-lc-rs/resources.html#entropy-configuration
- The regression as rustls found it:
  https://github.com/aws/aws-lc-rs/issues/899
- The rule-of-two design, and the opt-out as a supported option:
  https://github.com/aws/aws-lc/issues/3180,
  https://github.com/aws/aws-lc/pull/2733
- The AWS SDK for Rust's recommendation:
  https://github.com/smithy-lang/smithy-rs/discussions/4541
- AWS-LC's randomness design ("Randomness generation design"):
  https://github.com/aws/aws-lc/blob/v5.11.0/crypto/fipsmodule/FIPS.md
- `getrandom(2)`: https://man7.org/linux/man-pages/man2/getrandom.2.html
