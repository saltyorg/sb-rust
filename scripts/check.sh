#!/usr/bin/env bash
# Runs the CONSTRAINTS.md checks. CONSTRAINTS.md is the source of truth; this
# script mirrors its "Checked by" column and must change with it.
#
#   scripts/check.sh fast [base]   after edits: fmt, clippy, floor guard, secrets
#   scripts/check.sh task [base]   before handing work back: fast + tests,
#                                  changed-line coverage, mutation score,
#                                  startup budget
#   scripts/check.sh full [base]   before committing (and CI): task +
#                                  dependencies, architecture, binary size
#
# base: the commit the work started from (default HEAD = uncommitted work).
# Exit: 0 all passed, 1 a check failed, 2 a check could not run.
set -euo pipefail
shopt -s inherit_errexit

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

stage="${1:-}"
base="${2:-HEAD}"
case "$stage" in fast | task | full) ;; *)
  echo "usage: scripts/check.sh fast|task|full [base]" >&2
  exit 2
  ;;
esac

# Thresholds: keep in step with CONSTRAINTS.md.
COVERAGE_MIN=90       # % of changed lines covered
MUTATION_MIN=80       # % of viable mutants in changed code caught
STARTUP_MAX_MS=10     # median ms for `sb --help` and `sb version`
SIZE_MAX_BYTES=45711520 # production musl binary, at most sb-go's size

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
skip() { printf 'skipped: %s\n' "$1"; }
need() { command -v "$1" >/dev/null || { echo "check: $1 not installed (see CONSTRAINTS.md)" >&2; exit 2; }; }

# Production crates (none until cli-core lands); prototypes are evidence only.
mapfile -t crates < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml -printf '%h\n' 2>/dev/null |
  sed 's|^crates/|sb-|; s|^sb-sb$|sb|' | sort)
pkg_args=()
test_pkg_args=()
for c in "${crates[@]}"; do
  pkg_args+=(-p "$c")
  test_pkg_args+=(--test-package "$c")
done

fast() {
  step "format"
  if ((${#crates[@]})); then cargo fmt --check "${pkg_args[@]}"; else skip "no crates/ yet"; fi
  step "clippy (warnings are errors)"
  cargo clippy --workspace --all-targets --quiet -- -D warnings
  step "floor guard"
  node scripts/floor-guard.mjs --base "$base"
  step "secrets (gitleaks, redacted)"
  need gitleaks
  { git diff "$base" --; git ls-files -z --others --exclude-standard | xargs -0 -r cat; } |
    gitleaks stdin --redact --no-banner --log-level error
}

task() {
  fast
  step "tests"
  cargo test --workspace --quiet
  if ((${#crates[@]} == 0)); then
    step "coverage, mutation, startup"
    skip "no crates/ yet"
    return
  fi
  step "changed-line coverage >= ${COVERAGE_MIN}%"
  need cargo-llvm-cov
  need diff-cover
  cargo llvm-cov --quiet "${pkg_args[@]}" --lcov --output-path target/lcov.info
  # New files are untracked until committed; without --include-untracked
  # they would not count as changed lines.
  diff-cover target/lcov.info --compare-branch="$base" --include-untracked \
    --fail-under="$COVERAGE_MIN"
  step "mutation score on changed code >= ${MUTATION_MIN}%"
  need cargo-mutants
  # Tracked changes plus untracked crate files (git diff alone misses new
  # files; `--no-index` exits 1 when the files differ, which they do).
  {
    git diff "$base" -- crates
    git ls-files -z --others --exclude-standard -- crates | while IFS= read -r -d '' f; do
      git diff --no-index -- /dev/null "$f" || true
    done
  } >target/changes.diff
  if [[ ! -s target/changes.diff ]]; then
    skip "no changed crate code"
  else
    # Every crate's tests judge every mutant: end-to-end tests in `sb`
    # cover code in the library crates (cargo-mutants otherwise runs only
    # the mutated package's tests).
    cargo mutants --in-diff target/changes.diff -o target --timeout 300 \
      "${pkg_args[@]}" "${test_pkg_args[@]}" || true
    node -e '
      const d = require("./target/mutants.out/outcomes.json");
      const tested = d.caught + d.missed + d.timeout;
      const score = tested ? (100 * d.caught) / tested : 100;
      console.log("caught " + d.caught + " of " + tested + " viable mutants: " + score.toFixed(1) + "%");
      process.exit(score >= Number(process.argv[1]) ? 0 : 1);
    ' "$MUTATION_MIN"
  fi
  step "startup: sb --help and sb version, median <= ${STARTUP_MAX_MS} ms"
  need hyperfine
  cargo build --quiet --release -p sb
  hyperfine --warmup 5 --runs 50 --export-json target/startup.json \
    'target/release/sb --help' 'target/release/sb version' >/dev/null
  node -e '
    const r = require("./target/startup.json").results;
    let ok = true;
    for (const x of r) {
      // hyperfine 2.0 keeps wall-clock statistics under summary.
      const ms = x.summary?.time_wall_clock?.median * 1000;
      if (!Number.isFinite(ms)) {
        console.error(x.command + ": no wall-clock median in the hyperfine output");
        process.exit(2);
      }
      console.log(x.command + ": median " + ms.toFixed(2) + " ms");
      if (ms > Number(process.argv[1])) ok = false;
    }
    process.exit(ok ? 0 : 1);
  ' "$STARTUP_MAX_MS"
}

full() {
  task
  if [[ ! -f crates/sb/Cargo.toml ]]; then
    step "dependencies, architecture, binary size"
    skip "no crates/sb yet"
    return
  fi
  step "dependencies and architecture (cargo-deny, shipped graph)"
  need cargo-deny
  cargo deny --manifest-path crates/sb/Cargo.toml check
  step "binary size <= ${SIZE_MAX_BYTES} bytes (production musl)"
  cargo build --quiet -p sb --profile production --target x86_64-unknown-linux-musl
  size=$(stat -c %s target/x86_64-unknown-linux-musl/production/sb)
  echo "sb: ${size} bytes"
  ((size <= SIZE_MAX_BYTES))
}

"$stage"
printf '\n\033[1mcheck %s: all passed\033[0m\n' "$stage"
