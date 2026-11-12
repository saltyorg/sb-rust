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
| Lint | Zero clippy warnings, with the whole `clippy::pedantic` group and rustc's `missing_debug_implementations` and `trivial_numeric_casts` on for `crates/` (workspace `[lints]`; a prototype keeps the lints it was written under); opting one lint out workspace-wide needs an ADR | `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo clippy --workspace --lib --bins -- -D warnings` for the build without dev-dependency features; `check.sh`'s crate check, that each crate has `[lints] workspace = true` | fast | Warnings left in place get ignored; pedantic catches missing `# Errors` docs, `#[must_use]` and lossy casts (maintainer, 2026-11-05) |
| Floor guard | Zero unacknowledged floor violations in the diff | `node scripts/floor-guard.mjs --base <base>` | fast | Catches the cheap roads to green |
| Secrets | Zero findings in the diff | `gitleaks stdin --redact` on `git diff <base>` plus untracked files | fast | Redacted: the value never reaches a log |
| Tests | All pass | `cargo llvm-cov` for the crates' unit and integration tests (the coverage run), then `cargo test --doc` for the crates and `cargo test` for the prototypes | task | Each test runs once (ADR-016) |
| Coverage | Changed lines ≥ 90% covered | `cargo llvm-cov --lcov` + `diff-cover --fail-under=90` | task | Maintainer chose stricter than the 80% default |
| Mutation | ≥ 80% of viable mutants in changed code caught, totalled over the passes; a crate in check.sh's `MUTATION_TESTERS` is judged by its own and its listed crates' tests, every other crate by every crate's tests | `cargo mutants --in-diff` per pass + score from each `outcomes.json` | task | Maintainer chose stricter than the 60% default; mutation checks were already part of the work; the per-crate scope is ADR-012 |
| Startup | `sb --help` and `sb version` median ≤ 10 ms | `hyperfine --warmup 5 --runs 50`, release build | task | sb-go takes about 18 ms; the prototypes about 2 ms |
| Dependencies | No RustSec vulnerability, no unmaintained or yanked crate, only GPL-3.0-compatible licences, crates.io only | `cargo deny --manifest-path crates/sb/Cargo.toml check` (`deny.toml`) | full | External opinion: the RustSec database |
| Architecture | Crates depend only along the capability map's "Depends on" column | cargo-deny `bans.deny` with `wrappers`, one entry per crate (`deny.toml`); `check.sh`'s crate check, that each crate but `sb` has its entry (at fast) | full | The compiler can't see the map; cargo-deny can |
| Binary size | Production musl binary ≤ 45,711,520 bytes | `stat` after `cross build --profile production --target x86_64-unknown-linux-musl`, in its own target directory (see the cross ADR); a host of another architecture skips it with a printed line | full | Never larger than sb-go's binary (2026-10-30) |
| Commit messages | Each commit after the base: a Conventional Commits subject whose description's first letter is lower case, a `Library-check:` line when it changes Rust, a `Review:` line when it changes a file outside `docs/` and `tasks/`; a true merge is skipped | `scripts/check-commit-messages.sh <base> HEAD` | full, when the base isn't HEAD | Public repository rules and Library first (CLAUDE.md) |
| Review line | The body of every commit that changes a file outside `docs/` and `tasks/` says which reviewers ran, on what diff, and what was fixed or declined | `scripts/check-commit-messages.sh` (`Review:`) | full, when the base isn't HEAD | Fresh-context review, by tier (CLAUDE.md, ADR-016) |
| Logging | Every root run writes its run record and outcome; logs never contain secret values | Tests in `sb-cli-core` (run log) and in each module that handles secrets (facts, accounts, credentials): the test feeds a known secret and asserts it isn't in the captured log | task | Decision 11; the run log is kept on the host |
| Journal matrix | compare.sh, compat.sh, follow.sh and stress.sh pass on each supported release, with only the journal spec's listed Differences | `crates/journal/tests/` (the journal spec, [docs/specs/journal.md](docs/specs/journal.md), "Commands") | by hand (below) | journalctl of each release is the reference the reader must match (journal spec, assumption 4 and success criterion 3) |
| Journal fuzz | The fuzz harness passes, its bounds enforced by itself | the `fuzz` example over seeds from `crates/journal/tests/seeds.sh` (journal spec, "Commands") | by hand (below) | The reader's bounds must hold on damaged and mutated files (journal spec, success criterion 4) |

All of the above but the two journal rows run through
`scripts/check.sh fast|task|full [base]`.
`fast` runs after edits, `task` before handing work back (a few minutes),
and `full` before committing and in CI once CI exists. Every failing check
blocks.

What a run covers follows the files that differ from the base
([ADR-016](docs/decisions/ADR-016-lighter-process.md)). A change under
`docs/` and `tasks/` only ends `task` and `full` after the fast checks
(and `full`'s commit messages). The gate scripts' own tests run in `fast`
when a file under `scripts/` changed. With nothing changed since the
base, every step runs.

The journal matrix and the fuzz harness run by hand, capped, at each
checkpoint and after any task that touches journal reading: a change
under `crates/journal/`, or a version change of one of sb-journal's
dependencies. They run so until CI is authorised: the matrix workflow
(`.github/workflows/journal-matrix.yml`) is local and unpushed, and
pushing and running it, and afl.rs fuzzing after it, wait for the
maintainer's go-ahead (journal spec, decisions 3 and 7). A failing run
blocks as a failing check does.

A floor finding that is expected and accepted (a guard line changed on
purpose, an assertion moved to another test) is acknowledged in
`scripts/floor-acknowledged.txt`, in the commit that causes it: one line
for each finding, with the rule, the file, the flagged line and a reason.
The guard prints each acknowledged finding with its reason, and fails on
any other finding and on an entry that matches none. An entry from an
earlier commit acknowledges nothing, and entries are never edited or
removed ([ADR-015](docs/decisions/ADR-015-acknowledged-floor-findings.md)).

## Measured, not yet enforced

Recorded from the first `crates/` build and updated at each checkpoint,
measured the way `scripts/check.sh` runs (as root, so the root tests run;
without them coverage reads lower).
The "Today" values are written as limits, so the floor guard reports any
value that gets worse. Within its tolerance, the reason goes in the
commit; beyond it, the change needs an ADR (docs/decisions/), as any
loosening does. Current values: [ADR-014](docs/decisions/ADR-014-journal-ratchet-update.md). Keep digits out of the rows except the values themselves:
the guard reads every number as a threshold.

| Metric | Today | Direction |
|---|---|---|
| Project coverage (`crates/`) | at least 98.58% of lines, at least 98.11% of functions (see the ratchet ADR) | must not fall; tolerance at most 0.5% |
| Production binary size | at most 11932640 bytes (see the ratchet ADR) | must not grow; growth at most 5% per change |
| `sb --help` median | at most 1.18 ms | must not grow; growth at most 1 ms per change |

## Tools

Installed locally; `scripts/check.sh` exits 2 naming any tool that's missing
or not at the version its row asks for, before a stage runs. Rule: exact is
the same version; major, the same major version; minimum, that version or a
later one. A tool cargo installed is read from `cargo install --list`,
gitleaks from `go version -m`, the others from their `--version`. The
system packages have a major or minimum rule, so a host package update
doesn't fail every run.

| Tool | Version | Rule | Install |
|---|---|---|---|
| cargo-llvm-cov | 0.9.1 | exact | `cargo install --locked cargo-llvm-cov` (needs the `llvm-tools` component, pinned in `rust-toolchain.toml`) |
| diff-cover | 9.7.2 | exact | `uv tool install diff-cover==9.7.2` |
| cargo-mutants | 27.1.0 | exact | `cargo install --locked cargo-mutants` |
| cargo-deny | 0.20.2 | exact | `cargo install --locked cargo-deny` |
| hyperfine | 2.0.0 | exact | `cargo install --locked hyperfine` |
| gitleaks | 8.30.1 | exact | `go install github.com/zricethezav/gitleaks/v8@v8.30.1` |
| Go | 1.27 | minimum | system package (installs gitleaks and reports its version) |
| node | 24 | major | system package (runs the floor guard) |
| script (util-linux) | 2.39 | minimum | system package (gives the colour tests a terminal) |
| cross | 0.2.5 | exact | `cargo install --locked cross --version 0.2.5` (the production musl build, ADR-008) |
| Docker | 29 | minimum | system package (runs cross's build images; its first run pulls them) |

## Exceptions

Every `#[expect]`, `#[coverage(off)]`, `#[mutants::skip]`, cargo-deny
`ignore` and other suppression in production code has a row here, is
called out in review, and expires.

| ID | Rule | Path | Reason | Owner | Expires |
|---|---|---|---|---|---|
| E1 | cargo-deny `unmaintained`: RUSTSEC-2025-0141 (bincode 1), RUSTSEC-2024-0320 (yaml-rust) | `deny.toml` `[advisories] ignore`; reached through `tui-markdown`'s default `highlight-code` feature (syntect 5.3.0) | YAML highlighting in announcements, chosen by the maintainer (2026-10-30). The crates are unmaintained, not vulnerable, and only read syntax data built into the binary. No other ratatui Markdown renderer has comparable adoption, and syntect's latest release still needs both. On expiry: check for a syntect or tui-markdown release without them, else decide again | saltydk | 2027-01-28 |
| E2 | clippy `cast_precision_loss` | `crates/config/src/scalar.rs`, the `!!float` of an int | yaml.v3 decodes an int tagged `!!float` as Go's `float64(int64)`, rounded to the nearest `f64` beyond 2^53; sb must give the same value, and std has no conversion from `i64` to `f64` without `as`. On expiry: check for a std rounding conversion from `i64` to `f64` that isn't `as`, else renew | saltydk | 2027-05-06 |
