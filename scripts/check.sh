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
# Root runs of sb log; nothing run from here (tests, mutants, timing) may
# reach the host's /var/log/sb. Tests set their own directories too.
export SB_LOG_DIR="$root/target/test-logs"
# Run the tests that need root (namespaces, switching users) when we are
# root; they never touch the host (cli-core spec, Testing strategy).
if [[ "$(id -u)" == 0 ]]; then
  export SB_TEST_ROOT=1
fi
# Test the build under test, and never the host's Docker daemon: a
# leftover override of the sb binary or the live opt-in would change what
# every stage checks (docker plan Task 9).
unset SB_TEST_BINARY SB_LIVE_DOCKER

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

# Mutation scope (ADR-012). A crate listed here has its mutants judged by
# its own tests plus those of the crates named: the workspace crates whose
# tests reach it. Every other crate's mutants are judged by every crate's
# tests, since the end-to-end tests in `sb` cover the library crates
# (cargo-mutants otherwise runs only the mutated package's tests). A
# narrower scope can't turn a missed mutant into a caught one; it can only
# lower the score, apart from a timeout or a broken build outside its tests
# (ADR-012). Add a crate here when its tests start reaching a listed one.
declare -A MUTATION_TESTERS=()

# mutation_pass N ARGS...: one cargo-mutants pass over the changed code with
# the given package arguments; appends its outcomes file to `outcomes`.
mutation_pass() {
  local n=$1 list rc=0
  shift
  local mutants=(cargo mutants --in-diff target/changes.diff "$@")
  # A plain assignment, so a failing list stops the check (errexit is off
  # inside an `if` condition, command substitutions included; and `local
  # list=$(...)` would take local's status: declared apart above).
  list=$("${mutants[@]}" --list)
  # Changed tests or comments only: cargo-mutants writes no outcomes.
  [[ -n $list ]] || return 0
  echo "mutants: $*"
  PATH="$root/target/no-sudo:$PATH" "${mutants[@]}" -o "target/mutants-run-$n" \
    --timeout 300 || rc=$?
  # 0 all caught, 2 some missed, 3 some timed out: the score decides. Any
  # other status (4: the unmutated baseline failed, so nothing was tested)
  # means no verdict (https://mutants.rs/exit-codes.html).
  case $rc in
  0 | 2 | 3) ;;
  *)
    echo "check: cargo-mutants exited $rc for: $*" >&2
    exit 2
    ;;
  esac
  if [[ ! -f target/mutants-run-$n/mutants.out/outcomes.json ]]; then
    echo "check: cargo-mutants wrote no outcomes" >&2
    exit 2
  fi
  outcomes+=("target/mutants-run-$n/mutants.out/outcomes.json")
}

mutation_check() {
  step "mutation score on changed code >= ${MUTATION_MIN}%"
  need cargo-mutants
  local c t key testers=() outcomes=()
  # A key or tester that names no crate would silently change the scope.
  for key in "${!MUTATION_TESTERS[@]}"; do
    read -ra testers <<<"${MUTATION_TESTERS[$key]}"
    for t in "$key" "${testers[@]}"; do
      if [[ " ${crates[*]} " != *" $t "* ]]; then
        echo "check: MUTATION_TESTERS names $t, which is not a crate" >&2
        exit 2
      fi
    done
  done
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
    return
  fi
  # A mutant can make sb relaunch itself through sudo; as root with
  # passwordless sudo that once looped into thousands of processes. A
  # refusing sudo first on PATH keeps every mutant away from the real one.
  mkdir -p target/no-sudo
  printf '#!/bin/sh\necho "sudo blocked during mutation testing" >&2\nexit 97\n' \
    >target/no-sudo/sudo
  chmod 755 target/no-sudo/sudo
  # Old outcomes go first, so a run that writes none can't pass on them.
  rm -rf target/mutants.out target/mutants-run-*
  local wide=()
  for c in "${crates[@]}"; do
    if [[ -z ${MUTATION_TESTERS[$c]+x} ]]; then wide+=(-p "$c"); fi
  done
  if ((${#wide[@]})); then mutation_pass 0 "${wide[@]}" "${test_pkg_args[@]}"; fi
  local n=0
  for c in "${crates[@]}"; do
    [[ -n ${MUTATION_TESTERS[$c]+x} ]] || continue
    n=$((n + 1))
    local tests=(--test-package "$c")
    read -ra testers <<<"${MUTATION_TESTERS[$c]}"
    for t in "${testers[@]}"; do tests+=(--test-package "$t"); done
    mutation_pass "$n" -p "$c" "${tests[@]}"
  done
  if ((${#outcomes[@]} == 0)); then
    skip "no mutants in the changed code"
    return
  fi
  node -e '
    const [min, ...files] = process.argv.slice(1);
    let caught = 0, missed = 0, timeout = 0;
    for (const f of files) {
      const d = require(require("path").resolve(f));
      if (![d.caught, d.missed, d.timeout].every(Number.isInteger)) {
        console.error("check: " + f + " has no caught, missed and timeout counts");
        process.exit(2);
      }
      caught += d.caught; missed += d.missed; timeout += d.timeout;
    }
    const tested = caught + missed + timeout;
    const score = tested ? (100 * caught) / tested : 100;
    console.log("caught " + caught + " of " + tested + " viable mutants: " + score.toFixed(1) + "%");
    process.exit(score >= Number(min) ? 0 : 1);
  ' "$MUTATION_MIN" "${outcomes[@]}"
}

task() {
  fast
  step "tests"
  need script
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
  mutation_check
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
  step "binary size <= ${SIZE_MAX_BYTES} bytes (production musl, through cross)"
  # aws-lc-sys compiles C for the target, so the build runs in cross's
  # image, with a target directory of its own: build scripts compiled in
  # the image can't run on the host or in another image (ADR-008).
  need cross
  need docker
  local cross_target="$root/target/cross-musl"
  CARGO_TARGET_DIR="$cross_target" cross build --quiet -p sb --profile production \
    --target x86_64-unknown-linux-musl
  size=$(stat -c %s "$cross_target/x86_64-unknown-linux-musl/production/sb")
  echo "sb: ${size} bytes"
  ((size <= SIZE_MAX_BYTES))
}

"$stage"
printf '\n\033[1mcheck %s: all passed\033[0m\n' "$stage"
