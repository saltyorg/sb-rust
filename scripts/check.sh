#!/usr/bin/env bash
# Runs the CONSTRAINTS.md checks. CONSTRAINTS.md is the source of truth; this
# script mirrors its "Checked by" column and must change with it.
#
#   scripts/check.sh fast [base]   after edits: each crate's deny.toml entry
#                                  and lints, fmt, clippy, floor guard (and
#                                  the gate scripts' own tests when scripts/
#                                  changed), secrets
#   scripts/check.sh task [base]   before handing work back: fast + tests,
#                                  changed-line coverage, mutation score,
#                                  startup budget
#   scripts/check.sh full [base]   before committing (and CI): commit
#                                  messages after base, then task +
#                                  dependencies, architecture, binary size
#
# base: the commit the work started from (default HEAD = uncommitted work).
# A change that touches only docs/ and tasks/ ends task and full after the
# fast checks (and full's commit messages): no build can read those files
# (ADR-016). With nothing changed since the base, every step runs.
# Every tool a stage runs is checked against the CONSTRAINTS.md Tools table
# before the stage starts.
# Exit: 0 all passed, 1 a check failed, 2 a check could not run (a tool
# missing, or not the version the Tools table asks for).
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
# Every stage builds in and reads from target/ (the cross build in its own
# directory under it, set below): an inherited target directory, or a
# configuration file's build.target-dir, would send a stage's build
# elsewhere. CARGO_TARGET_DIR is read before build.target-dir:
# https://doc.rust-lang.org/cargo/reference/config.html#buildtarget-dir
# cargo-mutants is run without it (mutation_pass).
unset CARGO_BUILD_TARGET_DIR CARGO_LLVM_COV_TARGET_DIR
export CARGO_TARGET_DIR="$root/target"
# Every diff is git's plain text, whatever the user's git configuration
# or an attributes file says: no external diff, no textconv filter (which
# could hide a value from gitleaks), no colour, and the a/ and b/ prefixes.
diff_flags=(--no-ext-diff --no-textconv --no-color --src-prefix=a/ --dst-prefix=b/)
arch=$(uname -m)

stage="${1:-}"
base="${2:-HEAD}"
case "$stage" in fast | task | full) ;; *)
  echo "usage: scripts/check.sh fast|task|full [base]" >&2
  exit 2
  ;;
esac
if ! git rev-parse --verify --quiet "$base^{commit}" >/dev/null; then
  echo "check: base $base is not a commit" >&2
  exit 2
fi

# Thresholds: keep in step with CONSTRAINTS.md.
COVERAGE_MIN=90       # % of changed lines covered
MUTATION_MIN=80       # % of viable mutants in changed code caught
STARTUP_MAX_MS=10     # median ms for `sb --help` and `sb version`
SIZE_MAX_BYTES=45711520 # production musl binary, at most sb-go's size

# What changed since the base, tracked and untracked (ADR-016).
# - documents_only: something changed, and all of it is under docs/ or
#   tasks/. Nothing a build, a test or a measurement reads is there.
# - scripts_changed: a file under scripts/ changed, or nothing changed at
#   all (a run over a clean tree checks everything).
mapfile -d '' -t changed_files < <(
  # --no-renames: a file moved into docs/ is listed under its old path
  # too, so a moved test fixture or script isn't taken for a document.
  git diff "${diff_flags[@]}" --no-renames --name-only -z "$base" --
  git ls-files -z --others --exclude-standard
)
documents_only=0
scripts_changed=0
if ((${#changed_files[@]} == 0)); then
  scripts_changed=1
else
  documents_only=1
  for f in "${changed_files[@]}"; do
    case $f in
    docs/* | tasks/*) ;;
    *) documents_only=0 ;;
    esac
    case $f in scripts/*) scripts_changed=1 ;; esac
  done
fi

step() { printf '\n\033[1m== %s\033[0m\n' "$1"; }
skip() { printf 'skipped: %s\n' "$1"; }
need() { command -v "$1" >/dev/null || { echo "check: $1 not installed (see CONSTRAINTS.md)" >&2; exit 2; }; }

# tool_version NAME: the installed version of a Tools table tool, read
# without side effects.
# - The tools cargo installed, from `cargo install --list`, which prints
#   `NAME vX.Y.Z:` for a crates.io release and `NAME vX.Y.Z (source):` for
#   a git or path build. Only a release counts, so a build from elsewhere
#   fails the exact rule. They aren't run: running one could act
#   (`cross --version` syncs a toolchain). The limit: cargo's record is
#   trusted, so another binary of the same name earlier on PATH isn't
#   seen.
# - gitleaks, from Go's build information: a `go install` build prints no
#   version of its own ("version is set by build process"), and
#   `go version -m` prints its module line `mod <path> vX.Y.Z <sum>`:
#   https://pkg.go.dev/cmd/go#hdr-Print_Go_version
# - go, from `go version` ("go version go1.27.3 linux/amd64"), whole: a
#   pre-release (go1.27rc1) is no version number, and fails every rule.
# - The rest, from the first version number `--version` prints on stdout
#   (a warning on stderr can hold a number too):
#   "diff-cover 9.7.2", "v24.20.0" (node), "script from util-linux 2.39.3",
#   "Docker version 29.7.2, build ..." (the client's; no daemon is asked).
tool_version() {
  local out=''
  case $1 in
  cargo-llvm-cov | cargo-mutants | cargo-deny | hyperfine | cross)
    out=$(cargo install --list | sed -n "s/^$1 v\([^ :]*\):\$/\1/p") || out=''
    ;;
  gitleaks)
    out=$(go version -m "$(command -v gitleaks)" |
      awk '$1 == "mod" && $2 == "github.com/zricethezav/gitleaks/v8" { sub(/^v/, "", $3); print $3 }') || out=''
    ;;
  go) out=$(go version | sed -nE '1s/^go version go([^ ]+) .*/\1/p') || out='' ;;
  *) out=$("$1" --version 2>/dev/null | sed -nE '1s/^[^0-9]*([0-9]+(\.[0-9]+)*).*/\1/p') || out='' ;;
  esac
  printf '%s\n' "$out"
}

# check_tools NAME...: each tool is installed, with the version its row in
# the CONSTRAINTS.md Tools table asks for. A row's tool is the first word
# of its first cell, in lower case ("script (util-linux)" is script); its
# rule is exact (the same version), major (the same major version) or
# minimum (that version or a later one).
check_tools() {
  local t row want rule got
  for t in "$@"; do
    need "$t"
    row=$(awk -F'|' -v t="$t" '
      /^## / { on = ($0 ~ /^## Tools[[:space:]]*$/) }
      on && !found && /^\|/ {
        split($2, w, " ")
        if (tolower(w[1]) == t) { gsub(/[[:space:]`]/, "", $3); gsub(/[[:space:]`]/, "", $4); print $3, $4; found = 1 }
      }' CONSTRAINTS.md)
    if [[ -z $row ]]; then
      echo "check: CONSTRAINTS.md's Tools table has no row for $t" >&2
      exit 2
    fi
    read -r want rule <<<"$row"
    got=$(tool_version "$t")
    if [[ -z $got ]]; then
      echo "check: could not read the version of $t (see CONSTRAINTS.md)" >&2
      exit 2
    fi
    # major and minimum compare version numbers: a pre-release fails both.
    if [[ $rule == major || $rule == minimum ]] && ! [[ $got =~ ^[0-9]+(\.[0-9]+)*$ ]]; then
      echo "check: $t is $got, not a release; CONSTRAINTS.md's Tools table asks for $want ($rule)" >&2
      exit 2
    fi
    case $rule in
    exact) [[ $got == "$want" ]] ;;
    major) [[ ${got%%.*} == "${want%%.*}" ]] ;;
    minimum) printf '%s\n' "$want" "$got" | sort -V -C ;;
    *)
      echo "check: CONSTRAINTS.md gives $t the rule '$rule', not exact, major or minimum" >&2
      exit 2
      ;;
    esac || {
      echo "check: $t is $got; CONSTRAINTS.md's Tools table asks for $want ($rule)" >&2
      exit 2
    }
  done
}

# Production crates; prototypes are evidence only.
mapfile -t crates < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml -printf '%h\n' 2>/dev/null |
  sed 's|^crates/|sb-|; s|^sb-sb$|sb|' | sort)
pkg_args=()
test_pkg_args=()
for c in "${crates[@]}"; do
  pkg_args+=(-p "$c")
  test_pkg_args+=(--test-package "$c")
done

# The prototypes: their tests run on their own, outside coverage. sed -s
# reads each manifest on its own, so one file's [package] range can't run
# into the next.
proto_args=()
while IFS= read -r name; do proto_args+=(-p "$name"); done < <(
  find prototypes -mindepth 2 -maxdepth 2 -name Cargo.toml -print0 2>/dev/null | sort -z |
    xargs -0 -r sed -s -n '/^\[package\]/,/^\[/s/^name *= *"\(.*\)"/\1/p'
)

# Each crate under crates/ inherits the workspace lints (`[lints]` with
# `workspace = true`; CONSTRAINTS.md, Lint) and has its architecture entry
# in deny.toml's bans.deny, `{ crate = "<name>", wrappers = [...] }` on
# one line (CONSTRAINTS.md, Architecture). sb has no entry: it is the root
# of the graph cargo-deny checks, and an entry for it would ban it.
crate_check() {
  local c manifest bad=0
  for c in "${crates[@]}"; do
    manifest="crates/${c#sb-}/Cargo.toml"
    if ! awk '/^[[:space:]]*\[/ { t = $0 }
      t ~ /^[[:space:]]*\[lints\][[:space:]]*(#.*)?$/ && /^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true[[:space:]]*(#.*)?$/ { ok = 1 }
      END { exit !ok }' "$manifest"; then
      echo "check: $manifest doesn't inherit the workspace lints ([lints] workspace = true)" >&2
      bad=1
    fi
    if [[ $c != sb ]] && ! grep -qE "^[[:space:]]*\{[[:space:]]*crate[[:space:]]*=[[:space:]]*\"$c\"[[:space:]]*,[[:space:]]*wrappers[[:space:]]*=" deny.toml; then
      echo "check: deny.toml has no bans.deny entry for $c (crate = \"$c\", wrappers = [...])" >&2
      bad=1
    fi
  done
  ((bad == 0))
}

fast() {
  step "crates: workspace lints and a deny.toml entry each"
  crate_check
  step "format"
  cargo fmt --check "${pkg_args[@]}"
  step "clippy (warnings are errors)"
  cargo clippy --workspace --all-targets --locked --quiet -- -D warnings
  # The plain build too: building any test target turns dev-dependency
  # features on (a `testing` self dev-dependency), so the step above never
  # lints a crate without them. This one builds no dev-dependencies, and a
  # dead-code expectation that is no longer needed fails it
  # (`unfulfilled_lint_expectations`; sb-journal plan, "Code with no user
  # yet").
  step "clippy, library and binary targets only (warnings are errors)"
  cargo clippy --workspace --lib --bins --locked --quiet -- -D warnings
  need node
  step "floor guard's own tests, and the plan id check's"
  if ((scripts_changed)); then
    # The dot reporter: one character a test, then each failure in full (the
    # failure report is as Node 24 prints it; the page describes the dots
    # only): https://nodejs.org/docs/latest-v24.x/api/test.html#test-reporters
    node --test --test-reporter=dot scripts/floor-guard.test.mjs
    node --test --test-reporter=dot scripts/check-plan-ids.test.mjs
  else
    skip "nothing under scripts/ changed since $base"
  fi
  step "floor guard"
  node scripts/floor-guard.mjs --base "$base"
  step "secrets (gitleaks, redacted)"
  need gitleaks
  { git diff "${diff_flags[@]}" "$base" --; git ls-files -z --others --exclude-standard | xargs -0 -r cat; } |
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
# sb-journal: judged by its own tests and sb-host's, the one crate that
# wraps it (sb-journal plan, Task 10).
MUTATION_TESTERS[sb-journal]="sb-host"

# mutation_pass N ARGS...: one cargo-mutants pass over the changed code with
# the given package arguments; appends its outcomes file to `outcomes`.
mutation_pass() {
  local n=$1 list rc=0
  shift
  # Without CARGO_TARGET_DIR: cargo-mutants builds each mutant in a copy of
  # the tree, and 27.1.0 leaves the variable as it finds it, so with it set
  # every mutant would build into the repository's target/.
  local mutants=(env -u CARGO_TARGET_DIR cargo mutants --in-diff target/changes.diff --cargo-arg=--locked "$@")
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
    git diff "${diff_flags[@]}" "$base" -- crates
    git ls-files -z --others --exclude-standard -- crates | while IFS= read -r -d '' f; do
      git diff "${diff_flags[@]}" --no-index -- /dev/null "$f" || true
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
  if ((documents_only)); then
    step "tests, coverage, mutation and startup"
    skip "only docs/ and tasks/ changed since $base"
    return
  fi
  # The crates' unit and integration tests run once, under coverage: a
  # failing test fails cargo-llvm-cov as it fails cargo test
  # (https://github.com/taiki-e/cargo-llvm-cov#usage: "cargo llvm-cov" runs
  # `cargo test`). It runs no doc tests on a stable toolchain, and no
  # prototype, so those two run after it (ADR-016).
  step "tests (crates), with changed-line coverage >= ${COVERAGE_MIN}%"
  need script
  need cargo-llvm-cov
  need diff-cover
  cargo llvm-cov --locked --quiet "${pkg_args[@]}" --lcov --output-path target/lcov.info
  step "tests (doc tests, prototypes)"
  cargo test --locked --quiet --doc "${pkg_args[@]}"
  if ((${#proto_args[@]})); then cargo test --locked --quiet "${proto_args[@]}"; fi
  step "changed-line coverage >= ${COVERAGE_MIN}% (diff-cover)"
  # New files are untracked until committed; without --include-untracked
  # they would not count as changed lines.
  diff-cover target/lcov.info --compare-branch="$base" --include-untracked \
    --fail-under="$COVERAGE_MIN"
  mutation_check
  step "startup: sb --help and sb version, median <= ${STARTUP_MAX_MS} ms"
  need hyperfine
  cargo build --locked --quiet --release -p sb
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
  # First: it takes seconds, so a bad subject fails before any build.
  step "commit messages after $base (scripts/check-commit-messages.sh)"
  if [[ $(git rev-parse "$base^{commit}") == "$(git rev-parse HEAD)" ]]; then
    skip "the base $base is HEAD: no commits to check"
  else
    scripts/check-commit-messages.sh "$base" HEAD
  fi
  task
  if ((documents_only)); then
    step "dependencies, architecture and binary size"
    skip "only docs/ and tasks/ changed since $base"
    return
  fi
  step "dependencies and architecture (cargo-deny, shipped graph)"
  need cargo-deny
  cargo deny --locked --manifest-path crates/sb/Cargo.toml check
  step "binary size <= ${SIZE_MAX_BYTES} bytes (production musl, through cross)"
  if [[ $arch != x86_64 ]]; then
    # cross's images, and so the production build, are x86_64 (ADR-008).
    skip "the production musl build and its size limit: cross builds it on x86_64 only, and this host is $arch"
  else
    # aws-lc-sys compiles C for the target, so the build runs in cross's
    # image, with a target directory of its own: build scripts compiled in
    # the image can't run on the host or in another image (ADR-008).
    need cross
    need docker
    local cross_target="$root/target/cross-musl"
    CARGO_TARGET_DIR="$cross_target" cross build --locked --quiet -p sb --profile production \
      --target x86_64-unknown-linux-musl
    size=$(stat -c %s "$cross_target/x86_64-unknown-linux-musl/production/sb")
    echo "sb: ${size} bytes"
    ((size <= SIZE_MAX_BYTES))
  fi
}

# The tools each stage runs: fast's, then task's, then full's (cross and
# Docker only where the production build runs).
tools=(node go gitleaks)
if [[ $stage != fast ]]; then tools+=(script cargo-llvm-cov diff-cover cargo-mutants hyperfine); fi
if [[ $stage == full ]]; then tools+=(cargo-deny); fi
if [[ $stage == full && $arch == x86_64 ]]; then tools+=(cross docker); fi
check_tools "${tools[@]}"

"$stage"
printf '\n\033[1mcheck %s: all passed\033[0m\n' "$stage"
