# Constraints

sb-rust's quality bar. Read it before writing code. Never weaken it to make a
change pass: loosening a rule needs an ADR, tightening needs none
(CLAUDE.md). The scripts mirror this file; if they drift, this file wins.

Last reviewed: 2026-10-30 by saltydk (maintainer interview: all four
dimensions, failing checks block, stricter numbers, a few minutes per task).

## Scope

Production code: the `crates/` workspace members and the repository's own
tooling.

## Floor (always enforced)

- Formatting is rustfmt's; clippy and rustc warnings are errors.
- `unsafe` is forbidden (`unsafe_code = "forbid"`).
- No `#[allow]` (`clippy::allow_attributes`). A justified exception uses
  `#[expect(lint, reason = "...")]` and a row in the Exceptions table.
- Every child process starts through sb-exec's `Cmd`: `clippy.toml`
  disallows `std::process::Command::new`, `tokio::process::Command::new`
  and portable-pty's `CommandBuilder` constructors, and `check.sh` denies
  that for library and binary code (maintainer, 2026-11-13). Tests,
  examples and `testing`-feature support code may start processes
  directly.
- Every thread and blocking-pool task starts through sb-cli-core's
  `spawn` (`thread`, `scoped_thread`, `blocking`), which carries the
  caller's tracing span, so every run-log record has the run id:
  `clippy.toml` disallows `std::thread::spawn`, `Builder::spawn`,
  `Builder::spawn_scoped`, `Scope::spawn` and tokio's `spawn_blocking`
  (the free function, `Handle`'s, `Runtime`'s and `JoinSet`'s, `_on`
  included), with the same exemptions and the same check (maintainer,
  2026-11-16). Threads a dependency starts itself (hyper-util's
  resolver) are outside it: their records keep no run id, and the run
  log keeps dependencies at WARN only.
- No unfinished work: `todo!`, `unimplemented!` and `dbg!` are denied by
  clippy; `TODO`/`FIXME`/`XXX` comments and empty stubs don't land.
- No test made easier: no new `#[ignore]`, no deleted test, no assertion
  removed from code that stays, without a reason in the commit message.
- No silenced checker: no `#[coverage(off)]`, `#[mutants::skip]`,
  `gitleaks:allow` or cargo-deny `ignore` without an Exceptions row. A
  workflow linter's suppression (an entry in `.github/actionlint.yaml` or
  `.github/zizmor.yml`, a `# zizmor: ignore[...]` comment) has its reason
  in place, next to it, instead of a row (the maintainer, 2026-11-24).
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
| Lint | Zero clippy warnings, with the whole `clippy::pedantic` group and rustc's `missing_debug_implementations` and `trivial_numeric_casts` on for `crates/` (workspace `[lints]`); opting one lint out workspace-wide needs an ADR | `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo clippy --workspace --lib --bins -- -D warnings` for the build without dev-dependency features (the first leaves out `disallowed_methods`, which binds library and binary code only, and the second denies it); `check.sh`'s crate check, that each crate has `[lints] workspace = true` | fast | Warnings left in place get ignored; pedantic catches missing `# Errors` docs, `#[must_use]` and lossy casts (maintainer, 2026-11-05) |
| Floor guard | Zero blocking findings in the diff without the maintainer's approval: the gate's configuration, its thresholds and the suppressions (below); removed assertions, removed or deleted tests and changed gate-script lines are printed for the code reviewer and don't block | `node scripts/floor-guard.mjs --base <base>` | fast | Catches the cheap roads to green where a silent change does the most harm; the rest is review's ([ADR-029](docs/decisions/ADR-029-floor-guard-narrowed.md), a loosening by the maintainer's decision, 2026-11-24) |
| Secrets | Zero findings in the diff | `gitleaks stdin --redact` on `git diff <base>` plus untracked files | fast | Redacted: the value never reaches a log |
| Workflows | Zero findings in `.github/`: actionlint's on the workflows, shellcheck's on their `run:` scripts included, and zizmor's at its auditor persona on the workflows and the Saltbox updater's action; a third-party action is pinned to a full commit hash with its version in a comment, saltyorg's own to a published version tag (`.github/zizmor.yml`); a suppression is a `# zizmor: ignore[<audit>]` comment on the finding, a `# shellcheck disable=<code>` directive in a workflow's `run:` script, or an entry in `.github/actionlint.yaml` for what the pinned actionlint predates, each with its reason beside it | `actionlint` (`.github/actionlint.yaml`), then `zizmor --offline --persona=auditor --no-progress .github/` (`.github/zizmor.yml`); CI's `zizmor-online` job runs zizmor online too, at the same persona, with the job's read-only token, for the audits that ask GitHub (each hash pin's tag comment, impostor commits) | fast when `.github/` changed; full always | Workflows run only on GitHub, so the first push is their first run; zizmor's `unpinned-uses` asks for every action pinned to a hash; the auditor persona runs every audit, so a finding the default persona leaves out still fails (maintainer, 2026-11-24) |
| Tests | All pass | `check.sh`'s check that no insta pending snapshot (`.pending-snap`, `.snap.new`) is under `crates/`, then `cargo llvm-cov` for the crates' unit and integration tests (the coverage run), then `cargo test --doc` for the crates, then `cargo nextest run` (the `release-tests` cargo and nextest profiles: the release profile's code, every failure reported) for the crates' tests in a release build; each of these runs, the startup timing, the floor guard's own tests and the release scripts' tests (`scripts/release.test.sh` and the Saltbox updater's in `.github/actions/update-saltbox-python/`, at fast when `scripts/` or `.github/` changed), as root in the mutation passes' sandbox (ADR-021: every mount read-only but the run's own `TMPDIR`, `target/` and the tests' log directory, the namespace's own `/proc`, `/dev/pts`, a private `/tmp` and a private `/dev/shm`; `/proc/sys`, `/sys` and `/dev` read-only; `/run` empty; its own loopback and PID namespace); hangup's session-scope test, which needs systemd, runs by hand in `crates/sb/tests/hangup-systemd.sh` | task | The crates' unit and integration tests run once in each build (ADR-016): a failure that showed only in a release build passed the debug run (maintainer, 2026-11-16); every test run in the sandbox, after a completion test wrote the host's /etc/fish (maintainer, 2026-11-17) |
| Coverage | Changed lines ≥ 90% covered | `cargo llvm-cov --lcov` + `diff-cover --fail-under=90` | task | Maintainer chose stricter than the 80% default |
| Project coverage (`crates/`) | at least 98.40% of lines, at least 97.66% of functions, totalled over the files under `crates/`; tolerance at most 0.5 points below either, a headroom shared by the changes until a floor is reset; measured as root, as the gate runs (without the root tests coverage reads lower) | `check.sh`'s `project_coverage`: the coverage run's lcov report (`target/lcov.info`), each file's line counts (`LF`, `LH`) and function counts (`FNF`, `FNH`) under `crates/` added up, compared in whole hundredths of a percent; the figures printed every run | full | The finished tree's figures, as ADR-025 measured them; enforced once every command was in place (maintainer, 2026-11-18, [ADR-025](docs/decisions/ADR-025-ratchet-enforced.md)) |
| Mutation | ≥ 80% of viable mutants in changed code caught, totalled over the passes; a crate in check.sh's `MUTATION_TESTERS` is judged by its own and its listed crates' tests, every other crate by every crate's tests | `cargo mutants --in-diff --test-tool=nextest` per pass, each test ended after 60 s and counted as failed (`.config/nextest.toml`'s `mutants` profile), each mutant after 300 s and counted as not caught, each pass as root in private mount and network namespaces (`unshare`, `mount`, `ip`: system packages): every mount read-only but the pass's directory (its `TMPDIR`), its output and the log directory, the namespace's own `/proc`, `/dev/pts`, a private `/tmp` and a private `/dev/shm`, `/run` empty, its own loopback (the host's PID namespace: ADR-021's limit); score from each `outcomes.json` | task | Maintainer chose stricter than the 60% default; mutation checks were already part of the work; the per-crate scope is ADR-012; the per-test timeout and the sandbox are ADR-021 |
| Startup | `sb --help` and `sb version` median ≤ 10 ms | `hyperfine --warmup 5 --runs 50`, release build | task | sb-go takes about 18 ms; the prototypes about 2 ms; no per-change row: a millisecond median moves with the host's load, and the budget catches a real regression (maintainer, 2026-11-18, [ADR-025](docs/decisions/ADR-025-ratchet-enforced.md)) |
| Dependencies | No RustSec vulnerability, no unmaintained or yanked crate, only GPL-3.0-compatible licences, crates.io only | `cargo deny --manifest-path crates/sb/Cargo.toml check` (`deny.toml`) | full | External opinion: the RustSec database |
| Architecture | Crates depend only along the capability map's "Depends on" column | cargo-deny `bans.deny` with `wrappers`, one entry per crate (`deny.toml`); `check.sh`'s crate check, that each crate but `sb` has its entry (at fast) | full | The compiler can't see the map; cargo-deny can |
| Binary size | Production glibc binary ≤ 45,711,520 bytes, and `sb version` runs on each release of the host spec's runtime list | `stat` after `cross build --profile production --target x86_64-unknown-linux-gnu` in `build/Dockerfile`'s image of the oldest supported release (`Cross.toml`), in its own target directory (ADR-020); then `docker run` of the binary in an `ubuntu:<release>` container of each release; then the aarch64 production build in the same image, and its `sb version` under QEMU user-mode on the oldest release's arm64 glibc (`build/qemu.Dockerfile`, no binfmt handler); a host of another architecture skips them all with a printed line | full | Never larger than sb-go's binary (2026-10-30); a binary one supported release can't load never ships (ADR-020); arm64 ships too, and its build breaking shows before CI's arm runners (maintainer, 2026-11-18) |
| Production binary size | at most 21031072 bytes; tolerance at most 5% above it, a headroom shared by the changes until the value is reset | `stat` of the same production glibc build, against that size plus the tolerance; the figures printed every run | full | The finished tree's size; Binary size's ceiling above still holds (maintainer, 2026-11-18, [ADR-025](docs/decisions/ADR-025-ratchet-enforced.md); lowered to the tree's size after the size cut of 2026-11-20) |
| Commit messages | Each commit after the base: a Conventional Commits subject whose description's first letter is lower case, a `Library-check:` line when it changes Rust, a `Review:` line when it changes a file outside `docs/` and `tasks/`, each `Floor-approved:` line in its form; a true merge is skipped | `scripts/check-commit-messages.sh <base> HEAD` | full, when the base isn't HEAD | Public repository rules and Library first (CLAUDE.md) |
| Review line | The body of every commit that changes a file outside `docs/` and `tasks/` says which reviewers ran, on what diff, and what was fixed or declined | `scripts/check-commit-messages.sh` (`Review:`) | full, when the base isn't HEAD | Fresh-context review, by tier (CLAUDE.md, ADR-016) |
| Logging | Every root run writes its run record and outcome; logs never contain secret values | Tests in `sb-cli-core` (run log) and in each module that handles secrets (facts, accounts, credentials): the test feeds a known secret and asserts it isn't in the captured log | task | Decision 11; the run log is kept on the host |
| Journal matrix | compare.sh, compat.sh, follow.sh and stress.sh pass on each supported release, with only the journal spec's listed Differences | `crates/journal/tests/` (the journal spec, [docs/specs/journal.md](docs/specs/journal.md), "Commands") | by hand (below) | journalctl of each release is the reference the reader must match (journal spec, assumption 4 and success criterion 3) |
| Journal fuzz | The fuzz harness passes, its bounds enforced by itself | the `fuzz` example over seeds from `crates/journal/tests/seeds.sh` (journal spec, "Commands") | by hand (below) | The reader's bounds must hold on damaged and mutated files (journal spec, success criterion 4) |

All of the above but the two journal rows run through
`scripts/check.sh fast|task|full [base]`.
`fast` runs after edits, `task` before handing work back (a few minutes),
and `full` before committing. In CI, ci.yml runs `task` and release.yml
runs `full` on each release tag. Every failing check blocks.

What a run covers follows the files that differ from the base
([ADR-016](docs/decisions/ADR-016-lighter-process.md)). A change under
`docs/` and `tasks/` only ends `task` and `full` after the fast checks
(and `full`'s commit messages). The gate scripts' own tests run in `fast`
when a file under `scripts/` changed, and the workflows' linters when a
file under `.github/` changed (at `full`, always). With nothing changed
since the base, every step runs.

The journal matrix and the fuzz harness run by hand, capped, at each
checkpoint and after any task that touches journal reading: a change
under `crates/journal/`, or a version change of one of sb-journal's
dependencies. On GitHub the matrix also runs from
`.github/workflows/journal-matrix.yml`: daily, by hand, and for a pull
request or a push to main that changes the files it lists. afl.rs
fuzzing waits for the maintainer's go-ahead (journal spec, decision 7).
A failing run blocks as a failing check does.

A blocking floor finding (a threshold, a lint level, deny.toml, the
toolchain pin, a tool configuration, a gate step switched off, a new
`#[ignore]`, `#[expect]`, `coverage(off)` or `mutants::skip`, an edit to
actionlint's or zizmor's settings, a new `# zizmor: ignore[...]`
comment under `.github/` or a new `# shellcheck disable=` directive in a
workflow there, a CONSTRAINTS.md rule or number) passes only with the maintainer's approval
([ADR-029](docs/decisions/ADR-029-floor-guard-narrowed.md)). An agent asks
the maintainer first and never writes an approval on its own. Approved,
the commit that causes the finding carries
`Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)` in
its body, and until that commit exists the gate is given the same line in
the `FLOOR_APPROVED` environment variable (one line each). The guard
prints each approved finding with its line, and
`scripts/check-commit-messages.sh` checks the line's form. A removed
assertion, a removed or deleted test and a removed or changed gate-script
line are printed for the code reviewer, who judges each against the
commit message; they don't fail the run.

## Tools

Installed locally; `scripts/check.sh` exits 2 naming any tool that's missing
or not at the version its row asks for, before a stage runs. Rule: exact is
the same version; major, the same major version; minimum, that version or a
later one. A cargo tool is read from `cargo install --list` when cargo
installed it, else from its own `--version` (CI installs release binaries
cargo has no record of), cross only from cargo's record (`cross --version`
syncs a toolchain); gitleaks from `go version -m`; the others from their
`--version`. The
system packages have a major or minimum rule, so a host package update
doesn't fail every run.

| Tool | Version | Rule | Install |
|---|---|---|---|
| cargo-llvm-cov | 0.9.1 | exact | `cargo install --locked cargo-llvm-cov` (needs the `llvm-tools` component, pinned in `rust-toolchain.toml`) |
| diff-cover | 9.7.2 | exact | `scripts/install-diff-cover.sh <venv-dir> <bin-dir>`, which installs `diff-cover==9.7.2` with uv into a virtual environment of its own, every package at `scripts/diff-cover/requirements.txt`'s version and hashes (`--require-hashes`, wheels only); the lock is `requirements.in` compiled with the command in its header |
| cargo-mutants | 27.1.0 | exact | `cargo install --locked cargo-mutants` |
| cargo-nextest | 0.9.151 | exact | `cargo install --locked cargo-nextest` (runs the mutation passes' tests, ADR-021, and the release-build tests) |
| cargo-deny | 0.20.2 | exact | `cargo install --locked cargo-deny` |
| hyperfine | 2.0.0 | exact | `cargo install --locked hyperfine` |
| gitleaks | 8.30.1 | exact | `go install github.com/zricethezav/gitleaks/v8@v8.30.1` |
| actionlint | 1.7.12 | exact | `go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12` |
| zizmor | 1.32.0 | exact | `uv tool install zizmor==1.32.0` |
| Go | 1.27 | minimum | system package (installs gitleaks and actionlint, and reports gitleaks' version) |
| node | 24 | major | system package (runs the floor guard) |
| jq | 1.6 | minimum | system package (the release scripts and their tests; checked when their step runs) |
| python3 | 3.10 | minimum | system package (the Saltbox updater's isolation tests; checked when their step runs) |
| script (util-linux) | 2.37 | minimum | system package (gives the colour tests a terminal; they use only `-e`, `-q` and `-c`, which Ubuntu 22.04's 2.37.2 has) |
| shellcheck | 0.8 | minimum | system package (actionlint runs it on each workflow `run:` script; checked when the workflows step runs) |
| cross | 0.2.5 | exact | `cargo install --locked cross --version 0.2.5` (the production glibc build, in the image `Cross.toml` names, ADR-020) |
| Docker | 29 | minimum | system package (builds and runs cross's image from `build/Dockerfile`, and the `ubuntu:<release>` containers the binary runs in; the first run pulls them) |

## Images the gate keeps

`check.sh full` leaves one Docker image of its own on the host, as a
cache:

- `sb-rust-gate/qemu-check:<hash>`, the first 12 hex digits of
  `build/qemu.Dockerfile`'s SHA-256, built only when missing, so a change
  to the file builds a new one. It runs the aarch64 production binary
  under QEMU user-mode (Binary size row). Remove it, and older tags left
  by earlier versions of the file, with
  `docker images sb-rust-gate/qemu-check --format '{{.Repository}}:{{.Tag}}' | xargs -r docker rmi`.

cross builds and keeps its own images from `build/Dockerfile`
(`Cross.toml`), and the release runs pull `ubuntu:<release>`.

## Exceptions

Every `#[expect]`, `#[coverage(off)]`, `#[mutants::skip]`, cargo-deny
`ignore` and other suppression in production code has a row here, is
called out in review, and expires. The workflow linters' suppressions
are the exception: an entry in `.github/actionlint.yaml` or
`.github/zizmor.yml`, a `# zizmor: ignore[...]` comment and a
workflow's `# shellcheck disable=` directive document their reason in
place, next to the suppression, and have no row; the floor
guard blocks a change to any of them without the maintainer's approval
(the maintainer, 2026-11-24).

| ID | Rule | Path | Reason | Owner | Expires |
|---|---|---|---|---|---|
| E1 | cargo-deny `unmaintained`: RUSTSEC-2025-0141 (bincode 1), RUSTSEC-2024-0320 (yaml-rust) | `deny.toml` `[advisories] ignore`; reached through `tui-markdown`'s default `highlight-code` feature (syntect 5.3.0) | YAML highlighting in announcements, chosen by the maintainer (2026-10-30). The crates are unmaintained, not vulnerable, and only read syntax data built into the binary. No other ratatui Markdown renderer has comparable adoption, and syntect's latest release still needs both. On expiry: check for a syntect or tui-markdown release without them, else decide again | saltydk | 2027-01-28 |
| E2 | clippy `cast_precision_loss` | `crates/config/src/scalar.rs`, the `!!float` of an int | yaml.v3 decodes an int tagged `!!float` as Go's `float64(int64)`, rounded to the nearest `f64` beyond 2^53; sb must give the same value, and std has no conversion from `i64` to `f64` without `as`. On expiry: check for a std rounding conversion from `i64` to `f64` that isn't `as`, else renew | saltydk | 2027-05-06 |
| E3 | clippy `disallowed_methods` (`std::process::Command::new`) | `crates/cli-core/src/root.rs`, `relaunch_as_root` | The sudo relaunch replaces sb's process (`CommandExt::exec`) before the command runs, and cli-core can't use sb-exec, which depends on it. On expiry: check that it is still the only process cli-core starts, else move it | saltydk | 2027-11-14 |
| E4 | clippy `disallowed_methods` (`tokio::process::Command::new`) | `crates/exec/src/run.rs`, `Cmd::command` | sb-exec is where every child process starts; the rule exists to send every other crate here. Its PTY half (`portable_pty::CommandBuilder::new` in `Cmd::pty_child`) went with the unused PTY path, 2026-11-18 (exec decision 15). On expiry: check that this is sb-exec's only spawn site in library code, else renew | saltydk | 2027-11-14 |
| E5 | clippy `disallowed_methods` (`std::process::Command::new`) | `crates/release/build.rs`, the stand-in sb's rustc | The build script runs rustc to compile the stand-in sb that self-update's tests serve (release spec, "Testing strategy"): at build time only, only with the `testing` feature, never inside sb. A build script can't use sb-exec, and without the feature (every release and production build) it starts nothing. On expiry: check that the stand-in is still needed and still built only under `testing` (or that cargo has a stable way to build a test binary other packages' tests can run, artifact dependencies), else renew | saltydk | 2027-11-14 |
| E6 | clippy `disallowed_methods` (`std::thread::Builder::spawn`, `Builder::spawn_scoped`, `tokio::task::spawn_blocking`) | `crates/cli-core/src/spawn.rs`, `thread`, `scoped_thread` and `blocking` | The rule sends every thread and blocking-pool task here, where the caller's span is carried in (`Span::current()` and `in_scope`); these three are where the spawns themselves happen. On expiry: check that they are still the only spawn sites in library and binary code, else renew | saltydk | 2027-11-16 |
