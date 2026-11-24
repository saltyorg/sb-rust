# ADR-026: Renovate, with exact pins never automerged

## Status

Accepted (maintainer, 2026-11-18); amended 2026-11-24 (see Amendments).
The config is written locally for the future `saltyorg/sb-rust`
repository; nothing is pushed and no repository is created.

## Date

2026-11-18

## Context

sb-rust's dependencies move through Cargo, but several pinned values
live outside it:
- uv's version (`crates/cli-core/src/build_info.rs`, `UV_VERSION`);
- the Rust toolchain (`rust-toolchain.toml`);
- the release build image (`build/Dockerfile`, ADR-020);
- Speedtest's archives, as a version with a size and SHA-256 per
  architecture (`crates/ops/src/bench/speedtest.rs`);
- the gate's tools (CONSTRAINTS.md's Tools table);
- the GitHub Actions of `.github/workflows/`.

Four crates are pinned exactly, because a patch release can change an
unstable feature or an API sb relies on: clap_complete, and ratatui with
ratatui-core (ADR-001), and bollard (docker spec, "API version").
sb-go's `.github/renovate.json` is the style and preset base.

## Decision

`.github/renovate.json`:

1. **Ordinary crates:**
   - Patch updates are grouped and automerged once CI passes on their
     PR (see Precondition).
   - Minor updates are grouped and major updates open one PR each (a
     monorepo's packages together, `config:recommended`), both for
     review.
   - Everything runs weekly (`schedule:weekly`). Vulnerability fixes run
     at any time, from GitHub's alerts and from OSV, which carries
     RustSec.
2. **Always a PR, never automerged:**
   - **clap_complete and bollard:** one PR each, with ADR-001's
     procedure, the `=` kept.
   - **ratatui and ratatui-core:** each ratatui release requires its own
     ratatui-core, so they move together in one PR. ratatui-widgets, a
     transitive dependency, moves with Cargo.lock's weekly maintenance
     PR, which is never automerged either, so its version is read
     against ADR-001.
   - **The toolchain:** through Renovate's own `rust-toolchain` manager.
     CONSTRAINTS.md moves it only with an ADR.
   - **The build image:** Renovate pins its digest and then follows it.
     The tag never moves, as ADR-020 fixes the oldest supported release.
   - **Every regex-managed value:** uv, Speedtest and the gate's tools.
3. **Speedtest:**
   - Renovate follows the version in Ookla's download URLs, read from
     Ookla's CLI page with a custom datasource. It can't recompute the
     archives' sizes and digests. That would need a download-and-hash
     `postUpgradeTasks` step: the hosted app allows only an undocumented
     approved set, and this step isn't assumed to be in it.
   - So its PR waits for approval on the dependency dashboard, and the
     size and digest are updated by hand in the same PR.
   - The bench unit test pins each archive's URL as a literal, with its
     size and digest, so a version-only PR fails CI and can't merge half
     done.
4. **The Tools table:** its exact pins are tracked from their
   registries, each with its install line where that repeats the
   version:
   - cargo-llvm-cov, cargo-mutants, cargo-nextest, cargo-deny,
     hyperfine and cross from crates.io;
   - diff-cover from PyPI;
   - gitleaks from GitHub releases. A gitleaks major release would also
     need its Go module path (`/v8`) changed by hand.

   Go (`minimum`) and node (`major`) are system packages with a range,
   not a pin, so they aren't tracked. cross builds in `build/Dockerfile`'s
   image (ADR-020).
5. **GitHub Actions** are grouped as sb-go's: minor and patch together,
   majors apart.
6. **Automerge is Renovate's own** (`platformAutomerge: false`). Renovate
   merges only after checking the branch's statuses itself, and rebases
   a branch that is behind before merging (`rebaseWhen`'s default with
   automerge), so a group isn't merged on a result from an old base.

## Precondition

Renovate automerges once every status it sees is green, and the config
names no check. Already today, journal-matrix.yml runs on PRs that
change Cargo.lock, so a patch PR could merge on that workflow alone.

So, before Renovate is turned on for the repository:
- a CI workflow must run `scripts/check.sh task` on PRs;
- branch protection must require that workflow's status, so GitHub
  refuses Renovate's merge without it.

Both are the maintainer's to do when the repository is created.

## Alternatives considered

- **Automerge minor updates too:** rejected. A crate's minor release
  can change behaviour that sb's tests don't cover, so each is read.
- **Let the exact pins follow patch automerge:** rejected (ADR-001).
  Their patch releases are the risk the pins exist for.
- **Dependabot:** rejected. sb-go uses Renovate, and Dependabot has no
  regex managers for the values outside Cargo.

## Amendments

- 2026-11-24 (maintainer, before the first push): third-party GitHub
  Actions are pinned to full commit hashes with their version in a
  comment, `uses: owner/repo@<40-hex> # vX.Y.Z`, and
  `helpers:pinGitHubActionDigests` keeps each digest and comment in
  step; saltyorg's own actions stay on their published version tags
  (`pinDigests: false`). The gate's zizmor (CONSTRAINTS.md, Workflows)
  requires the hashes. Item 4 gains actionlint (GitHub releases) and
  zizmor (PyPI), each tracked in CONSTRAINTS.md and in the workflows'
  install lines.
