# Constraints

sb-rust's quality bar. Read it before writing code. Never weaken it to make a
change pass: loosening a rule needs an ADR, tightening needs none
(CLAUDE.md). The scripts mirror this file; if they drift, this file wins.

Last reviewed: 2026-10-30 by saltydk (maintainer interview: all four
dimensions, failing checks block, stricter numbers, a few minutes per task).

## Scope

Production code: the `crates/` workspace members and the repository's own
tooling. `prototypes/` are throwaway evidence and only need to build and
pass clippy and their tests.

## Floor (always enforced)

- Formatting is rustfmt's; clippy and rustc warnings are errors.
- `unsafe` is forbidden (`unsafe_code = "forbid"`).
- No `#[allow]` (`clippy::allow_attributes`). A justified exception uses
  `#[expect(lint, reason = "...")]` and a row in the Exceptions table.
- No unfinished work: `todo!`, `unimplemented!` and `dbg!` are denied by
  clippy; `TODO`/`FIXME`/`XXX` comments and empty stubs don't land.
- No test made easier: no new `#[ignore]`, no deleted test, no assertion
  removed from code that stays, without a reason in the commit message.
- No silenced checker: no `#[coverage(off)]`, `#[mutants::skip]`,
  `gitleaks:allow` or cargo-deny `ignore` without an Exceptions row.
- No secrets in source, tests or logs.
- Library first (CLAUDE.md): every commit changing Rust carries a
  `Library-check:` line.
- Public repository rules (CLAUDE.md): commit subjects follow
  Conventional Commits.
- This file is not weakened to make a change pass.

## Enforced with numbers

| Dimension | Rule | Checked by | Runs at | Why this number |
|---|---|---|---|---|
| Format | No diff | `cargo fmt --check -p <crates>` | fast | rustfmt is the style |
| Lint | Zero clippy warnings | `cargo clippy --workspace --all-targets -- -D warnings` | fast | Warnings left in place get ignored |
| Floor guard | Zero floor violations in the diff | `node scripts/floor-guard.mjs --base <base>` | fast | Catches the cheap roads to green |
| Secrets | Zero findings in the diff | `gitleaks stdin --redact` on `git diff <base>` plus untracked files | fast | Redacted: the value never reaches a log |
| Tests | All pass | `cargo test --workspace` | task | — |
| Coverage | Changed lines ≥ 90% covered | `cargo llvm-cov --lcov` + `diff-cover --fail-under=90` | task | Maintainer chose stricter than the 80% default |
| Mutation | ≥ 80% of viable mutants in changed code caught | `cargo mutants --in-diff` + score from `outcomes.json` | task | Maintainer chose stricter than the 60% default; mutation checks were already part of the work |
| Startup | `sb --help` and `sb version` median ≤ 10 ms | `hyperfine --warmup 5 --runs 50`, release build | task | sb-go takes about 18 ms; the prototypes about 2 ms |
| Dependencies | No RustSec vulnerability, no unmaintained or yanked crate, only GPL-3.0-compatible licences, crates.io only | `cargo deny --manifest-path crates/sb/Cargo.toml check` (`deny.toml`) | full | External opinion: the RustSec database |
| Architecture | Crates depend only along the capability map's "Depends on" column | cargo-deny `bans.deny` with `wrappers`, one entry per crate (`deny.toml`) | full | The compiler can't see the map; cargo-deny can |
| Binary size | Production musl binary ≤ 45,711,520 bytes | `stat` after `cargo build --profile production --target x86_64-unknown-linux-musl` | full | Never larger than sb-go's binary (2026-10-30) |
| Logging | Every root run writes its run record and outcome; logs never contain secret values | Tests in `sb-cli-core` (run log) and in each module that handles secrets (facts, accounts, credentials): the test feeds a known secret and asserts it isn't in the captured log | task | Decision 11; the run log is kept on the host |

All of the above run through `scripts/check.sh fast|task|full [base]`.
`fast` runs after edits, `task` before handing work back (a few minutes),
and `full` before committing and in CI once CI exists. Every failing check
blocks.

## Measured, not yet enforced

Recorded from the first `crates/` build; each must not get worse.

| Metric | Today | Direction |
|---|---|---|
| Project coverage (`crates/`) | not measured yet | must not fall (0.5% tolerance) |
| Production binary size | not measured yet | must not grow more than 5% in one change without a reason in the commit |
| `sb --help` median | not measured yet | must not grow more than 1 ms in one change |

## Tools

Installed locally; `scripts/check.sh` exits 2 naming any tool that's missing.

| Tool | Version | Install |
|---|---|---|
| cargo-llvm-cov | 0.9.1 | `cargo install --locked cargo-llvm-cov` (needs the `llvm-tools` component, pinned in `rust-toolchain.toml`) |
| diff-cover | 9.7.2 | `uv tool install diff-cover==9.7.2` |
| cargo-mutants | 27.1.0 | `cargo install --locked cargo-mutants` |
| cargo-deny | 0.20.2 | `cargo install --locked cargo-deny` |
| hyperfine | 2.0.0 | `cargo install --locked hyperfine` |
| gitleaks | 8.30.1 | `go install github.com/zricethezav/gitleaks/v8@v8.30.1` |
| node | 24 | system package (runs the floor guard) |

## Exceptions

Every `#[expect]`, `#[coverage(off)]`, `#[mutants::skip]`, cargo-deny
`ignore` and other suppression in production code has a row here, is
called out in review, and expires.

| ID | Rule | Path | Reason | Owner | Expires |
|---|---|---|---|---|---|
