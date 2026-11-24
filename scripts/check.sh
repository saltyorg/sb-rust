#!/usr/bin/env bash
# Runs the CONSTRAINTS.md checks. CONSTRAINTS.md is the source of truth; this
# script mirrors its "Checked by" column and must change with it.
#
#   scripts/check.sh fast [base]   after edits: each crate's deny.toml entry
#                                  and lints, fmt, clippy, floor guard (and
#                                  the guard's own tests when scripts/ changed,
#                                  the release scripts' when scripts/ or
#                                  .github/ did), the workflows' linters
#                                  (when .github/ changed), secrets
#   scripts/check.sh task [base]   before handing work back: fast + tests
#                                  (under coverage, then in a release
#                                  build), changed-line coverage, mutation
#                                  score, startup budget
#   scripts/check.sh full [base]   before committing (and CI): commit
#                                  messages after base, then task +
#                                  project coverage floors, the workflows'
#                                  linters (always), dependencies,
#                                  architecture, binary size, the
#                                  aarch64 build under QEMU
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
# every stage checks (docker plan Task 9), and python-env's live opt-in
# would start containers from every test run (module build M21). A
# leftover SB_TEST_NO_SYSTEMD would skip a test outside the sandbox, the
# only place that sets it (sandbox_argv).
unset SB_TEST_BINARY SB_LIVE_DOCKER SB_LIVE_PYTHON SB_TEST_NO_SYSTEMD
# nextest's runs (the mutation passes' and the release-build tests) read
# their settings from .config/nextest.toml's profiles alone: an inherited NEXTEST_*
# variable (a profile, retries, a test-thread count, another config file)
# would override it (https://nexte.st/docs/configuration/env-vars/). So
# would an inherited CARGO_MUTANTS_* one (cargo-mutants 27.1.0 reads its
# job count and its minimum test timeout from them): more mutants at once
# slow every test towards the per-test timeout (ADR-021).
while IFS= read -r v; do unset -v "$v"; done < <(compgen -e NEXTEST_; compgen -e CARGO_MUTANTS_)
# Every stage builds in and reads from target/ (the production build in its
# own directory under it, set below): an inherited target directory, or a
# configuration file's build.target-dir, would send a stage's build
# elsewhere. CARGO_TARGET_DIR is read before build.target-dir:
# https://doc.rust-lang.org/cargo/reference/config.html#buildtarget-dir
# cargo-mutants is run without it (mutation_pass).
unset CARGO_BUILD_TARGET_DIR CARGO_LLVM_COV_TARGET_DIR
# How long an orphan of a mutation pass must have run before the pass's
# sweeper kills it (see mutation_sweeper).
SB_MUTATION_ORPHAN_AGE=${SB_MUTATION_ORPHAN_AGE:-120}
# No leading zero: bash would read the number as octal.
if [[ ! $SB_MUTATION_ORPHAN_AGE =~ ^(0|[1-9][0-9]*)$ ]]; then
  echo "check: SB_MUTATION_ORPHAN_AGE must be a whole number of seconds" >&2
  exit 2
fi
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
SIZE_MAX_BYTES=45711520 # production glibc binary, at most sb-go's size
# The ratchet (ADR-025), at full: the finished tree's figures, each failing
# only past its tolerance.
PROJECT_LINES_MIN=98.40     # % of the lines under crates/ covered
PROJECT_FUNCTIONS_MIN=97.66 # % of the functions under crates/ covered
PROJECT_COVERAGE_TOLERANCE_MAX=0.5 # percentage points below a coverage floor
SIZE_RATCHET_MAX_BYTES=21031072 # production glibc binary
SIZE_TOLERANCE_MAX_PCT=5    # % above the size ratchet
# The releases the production binary must run on: the host spec's runtime
# list, sb-host's SUPPORTED_RUNTIME_RELEASES. The oldest is the image's
# (build/Dockerfile, ADR-020).
RUNTIME_RELEASES=(22.04 24.04 26.04)

# What changed since the base, tracked and untracked (ADR-016).
# - documents_only: something changed, and all of it is under docs/ or
#   tasks/. Nothing a build, a test or a measurement reads is there.
# - scripts_changed: a file under scripts/ changed, or nothing changed at
#   all (a run over a clean tree checks everything).
# - release_changed: the same, for a file under scripts/ or .github/ (the
#   release workflow's scripts and the Saltbox updater's action).
# - github_changed: the same, for a file under .github/ (the workflows,
#   their actions and actionlint's settings).
mapfile -d '' -t changed_files < <(
  # --no-renames: a file moved into docs/ is listed under its old path
  # too, so a moved test fixture or script isn't taken for a document.
  git diff "${diff_flags[@]}" --no-renames --name-only -z "$base" --
  git ls-files -z --others --exclude-standard
)
documents_only=0
scripts_changed=0
release_changed=0
github_changed=0
if ((${#changed_files[@]} == 0)); then
  scripts_changed=1
  release_changed=1
  github_changed=1
else
  documents_only=1
  for f in "${changed_files[@]}"; do
    case $f in
    docs/* | tasks/*) ;;
    *) documents_only=0 ;;
    esac
    case $f in scripts/*) scripts_changed=1 ;; esac
    case $f in scripts/* | .github/*) release_changed=1 ;; esac
    case $f in .github/*) github_changed=1 ;; esac
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
# - shellcheck, from the `version: 0.8.0` line of its `--version`, whose
#   first line names the tool only.
# - The rest, from the first version number `--version` prints on stdout
#   (a warning on stderr can hold a number too):
#   "diff-cover 9.7.2", "v24.20.0" (node), "script from util-linux 2.39.3",
#   "v1.7.12" (actionlint), "zizmor 1.32.0",
#   "Docker version 29.7.2, build ..." (the client's; no daemon is asked).
tool_version() {
  local out=''
  case $1 in
  cargo-llvm-cov | cargo-mutants | cargo-nextest | cargo-deny | hyperfine | cross)
    out=$(cargo install --list | sed -n "s/^$1 v\([^ :]*\):\$/\1/p") || out=''
    ;;
  gitleaks)
    out=$(go version -m "$(command -v gitleaks)" |
      awk '$1 == "mod" && $2 == "github.com/zricethezav/gitleaks/v8" { sub(/^v/, "", $3); print $3 }') || out=''
    ;;
  go) out=$(go version | sed -nE '1s/^go version go([^ ]+) .*/\1/p') || out='' ;;
  shellcheck) out=$(shellcheck --version | sed -nE 's/^version: ([0-9]+(\.[0-9]+)*).*/\1/p') || out='' ;;
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

# The crates.
mapfile -t crates < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml -printf '%h\n' 2>/dev/null |
  sed 's|^crates/|sb-|; s|^sb-sb$|sb|' | sort)
pkg_args=()
test_pkg_args=()
for c in "${crates[@]}"; do
  pkg_args+=(-p "$c")
  test_pkg_args+=(--test-package "$c")
done

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

# sb's normal build (no dev-dependencies, as a release or production build
# resolves it) never has a crate's `testing` feature: those let a test
# point sb at a fake, sb-release's at a fake GitHub with its own trust
# root, and a root-run updater must never be redirectable (release spec,
# M26's security review). cargo tree's `-e features` lists each enabled
# feature as `<crate> feature "<name>"`
# (https://doc.rust-lang.org/cargo/commands/cargo-tree.html#tree-options).
# Why not deny.toml's `[bans] features`: cargo-deny's graph includes
# dev-dependencies, where sb's tests turn `testing` on by design.
testing_feature_check() {
  local tree
  tree=$(cargo tree -p sb -e normal,features --locked --quiet --prefix none) || return 1
  if grep -E '^sb-[a-z-]+ feature "testing"' <<<"$tree"; then
    echo "check: sb's normal build enables a testing feature (above)" >&2
    return 1
  fi
}

# sb's normal build turns on rustls's `prefer-post-quantum` (ADR-027's
# amendment): X25519MLKEM768 first, with its key share. sb's
# tests/key_exchange.rs checks the effect, but in a test build, whose
# features include the dev-dependencies': one that turned rustls's
# default features on would keep it green with the shipping crates'
# lines gone. This reads the graph a release build resolves.
post_quantum_feature_check() {
  local tree
  tree=$(cargo tree -p sb -e normal,features --locked --quiet --prefix none) || return 1
  if ! grep -qx 'rustls feature "prefer-post-quantum"' <<<"$tree"; then
    echo "check: sb's normal build lacks rustls's prefer-post-quantum feature" >&2
    return 1
  fi
}

fast() {
  step "crates: workspace lints and a deny.toml entry each"
  crate_check
  step "format"
  cargo fmt --check "${pkg_args[@]}"
  step "clippy (warnings are errors)"
  # Every lint but disallowed_methods (clippy.toml: no child process
  # started outside sb-exec), which binds library and binary code only and
  # is checked by the step after this one: tests start the sb binary and
  # stand-in programs directly, and clippy has no setting to spare tests
  # (https://doc.rust-lang.org/clippy/lint_configuration.html#disallowed-methods).
  cargo clippy --workspace --all-targets --locked --quiet -- -D warnings -A clippy::disallowed_methods
  # The plain build too: building any test target turns dev-dependency
  # features on (a `testing` self dev-dependency), so the step above never
  # lints a crate without them. This one builds no dev-dependencies, and a
  # dead-code expectation that is no longer needed fails it
  # (`unfulfilled_lint_expectations`; sb-journal plan, "Code with no user
  # yet").
  step "clippy, library and binary targets only (warnings are errors)"
  # -D disallowed_methods on its own too: a command-line level comes after
  # the manifests' [lints], so a later `disallowed_methods = "allow"` there
  # can't turn the rule off.
  cargo clippy --workspace --lib --bins --locked --quiet -- -D warnings -D clippy::disallowed_methods
  need node
  step "floor guard's own tests"
  if ((scripts_changed)); then
    # The dot reporter: one character a test, then each failure in full (the
    # failure report is as Node 24 prints it; the page describes the dots
    # only): https://nodejs.org/docs/latest-v24.x/api/test.html#test-reporters
    # In the sandbox too: the tests run check.sh in scratch repositories
    # under their TMPDIR, and its own sandboxes nest inside this one.
    in_sandbox node --test --test-reporter=dot scripts/floor-guard.test.mjs
  else
    skip "nothing under scripts/ changed since $base"
  fi
  # The release workflow's shell logic (release spec, "Release notes and
  # announcement", "Saltbox's toolchain update"): fixture repositories, fake
  # gh and curl, no network. In the sandbox, as the guard's tests run.
  step "release scripts' tests (notes, Discord, the Saltbox gate and updater)"
  if ((release_changed)); then
    # Checked here, not with the stage's tools: only this step runs them.
    check_tools jq python3
    local action=.github/actions/update-saltbox-python
    in_sandbox scripts/release.test.sh
    in_sandbox bash "$action/commit_test.sh"
    in_sandbox bash "$action/download_test.sh"
    in_sandbox bash "$action/publish_test.sh"
    in_sandbox python3 -I "$action/isolation_test.py"
  else
    skip "nothing under scripts/ or .github/ changed since $base"
  fi
  # The workflows and their actions (CONSTRAINTS.md, Workflows), which
  # only GitHub can run: actionlint (its settings in .github/actionlint.yaml),
  # which runs shellcheck on each `run:` script and silently skips that
  # when it is missing, so its version is checked too; then zizmor's
  # offline audits at its default persona. Any finding fails. At fast when
  # .github/ changed, at full always.
  step "workflows (actionlint, zizmor)"
  if ((github_changed)) || [[ $stage == full ]]; then
    # Checked here, not with the stage's tools: only this step runs them.
    check_tools actionlint shellcheck zizmor
    actionlint
    zizmor --offline --no-progress .github/
  else
    skip "nothing under .github/ changed since $base"
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
# wraps it (sb-journal plan, Task 10), and sb's, whose end-to-end tests
# read crafted journals through the log viewer (tui spec, "Gate
# bookkeeping").
MUTATION_TESTERS[sb-journal]="sb-host sb"
# sb-tui: its own tests and sb's end-to-end screens (tui spec).
MUTATION_TESTERS[sb-tui]="sb"

# The temp directory of the mutation pass under way, swept on exit too.
mutation_tmp=

# mutation_pids DIR: the pid of each process whose executable or working
# directory lies under DIR, once each. /proc/PID/exe and cwd name a
# deleted file as "PATH (deleted)"; -lname matches the link's text, so a
# deleted copy's processes match too. DIR itself and "DIR (deleted)" are
# matched for a process started from the pass's directory, which no
# known test does. A zombie has neither link, so it isn't found again.
mutation_pids() {
  local dir=$1
  find /proc -mindepth 2 -maxdepth 2 \( -name exe -o -name cwd \) \( -lname "$dir/*" -o -lname "$dir" -o -lname "$dir (deleted)" \) 2>/dev/null |
    sed 's|^/proc/\([0-9]*\)/[a-z]*$|\1|' | sort -u
}

# mutation_sweep DIR: SIGKILL every process whose executable or working
# directory lies under DIR, the pass's own temp copies, until none is
# left; fails if some still are after about 10 s. (exec's tests leave
# helper shells that run /bin/sh from a directory of the copy: matching
# executables only, hundreds outlived their passes.) A mutant that ignores
# signals outlives its test: nextest ends a timed-out test's process
# group, as cargo-mutants does a timed-out mutant's (cargo-nextest
# 0.9.151's runner/unix.rs, cargo-mutants 27.1.0's process/unix.rs;
# https://nexte.st/docs/features/slow-tests/), a process that left that
# group or was orphaned by the kill runs on: util-linux `script`, which
# the terminal tests run sb under, starts its child in a new session. The
# copies of this pass alone lie under DIR, a directory made for it, so
# another pass or checkout is never touched. Anything else there goes too,
# a shell someone opened in a copy to look at a hung mutant included.
mutation_sweep() {
  local dir=$1 pid tries=0
  local -a pids
  local -A killed=()
  while mapfile -t pids < <(mutation_pids "$dir"); ((${#pids[@]})); do
    if ((tries++ >= 100)); then
      echo "check: processes left under $dir: ${pids[*]}" >&2
      return 1
    fi
    for pid in "${pids[@]}"; do
      # It may have ended since find saw it; the next look settles that.
      if kill -KILL "$pid" 2>/dev/null; then killed[$pid]=1; fi
    done
    sleep 0.1
  done
  if ((${#killed[@]})); then echo "mutants: killed ${#killed[@]} process(es) the pass left running"; fi
}

# The pid of the pass's background sweeper (mutation_sweeper), if one runs.
mutation_sweeper_pid=

# mutation_sweeper DIR: in the background, every 3 s while the pass runs,
# SIGKILL each process whose executable or working directory lies under
# DIR and whose parent is init (ppid 1). The sweep after the pass comes
# too late for a long pass: busy, signal-ignoring mutants of sb piled up
# while it ran (19 at once on 2026-11-13, each over 100% CPU). A process
# orphaned by a timed-out test's kill is adopted by init (proc(5):
# /proc/PID/stat's ppid field;
# https://man7.org/linux/man-pages/man2/prctl.2.html,
# PR_SET_CHILD_SUBREAPER, for the adopting process); a test's live sb
# still has its parent (the test, or `script`), so it is never touched.
# The loop ends by itself once DIR is gone or check.sh is; in the second
# case it then sweeps DIR and removes it (a check.sh killed with SIGKILL
# runs no trap). Only an orphan that has run for at least 120 s is killed
# (SB_MUTATION_ORPHAN_AGE overrides it, for the floor guard's tests):
# some tests orphan sb on purpose (a hangup kills its parent shell, and sb
# must still take its grace and log its ending), and nextest ends any test
# after 60 s (.config/nextest.toml), so a test's own orphan never lives
# that long, where the leaked mutants ran for hours. The age is now minus the process's start time (proc(5):
# /proc/PID/stat field 22, in clock ticks since boot; /proc/uptime, in
# seconds).
mutation_sweeper() {
  local dir=$1 parent=$$ pid stat ppid count fields start now ticks age=$SB_MUTATION_ORPHAN_AGE
  ticks=$(getconf CLK_TCK)
  (
    # The sleep holds none of check.sh's output open: it outlives a
    # stopped sweeper by up to 3 s, and a reader of check.sh's output
    # would wait for it.
    while sleep 3 >/dev/null 2>&1 && [[ -d $dir ]] && kill -0 "$parent" 2>/dev/null; do
      count=0
      while read -r pid; do
        # It may have ended since find saw it.
        { stat=$(<"/proc/$pid/stat"); } 2>/dev/null || continue
        # The fields after the parenthesised command name, from field 3
        # (state): ppid is the 2nd of them, starttime (field 22) the 20th.
        read -r -a fields <<<"${stat##*) }"
        ppid=${fields[1]}
        start=$((fields[19] / ticks))
        read -r now _ </proc/uptime
        now=${now%.*}
        if [[ $ppid == 1 ]] && ((now - start >= age)) && kill -KILL "$pid" 2>/dev/null; then
          count=$((count + 1))
        fi
      done < <(mutation_pids "$dir")
      if ((count)); then echo "mutants: killed $count orphaned process(es) during the pass"; fi
    done
    # check.sh ended without its EXIT trap (SIGKILL): cargo-mutants, adopted
    # by init, goes on through the rest of the pass, and nothing else would
    # sweep what its mutants leave (seen 2026-11-15). So the cleanup on exit
    # is done here: every process of the pass goes, and its copies, without
    # which cargo-mutants' next build and test fail at once. Up to three
    # times: cargo-mutants can start a build in a copy while it goes.
    if [[ -d $dir ]] && ! kill -0 "$parent" 2>/dev/null; then
      for _ in 1 2 3; do mutation_sweep "$dir" && mutation_remove "$dir" && break; done
    fi
  ) &
  mutation_sweeper_pid=$!
}

# Stops the pass's sweeper, if one runs. Its `sleep` ends on its own.
mutation_sweeper_stop() {
  if [[ -n $mutation_sweeper_pid ]]; then
    # Its status (143 from the TERM, or another if its output's reader
    # had gone) decides nothing: the cleanup on exit must go on to the
    # sweep whatever it is.
    if kill "$mutation_sweeper_pid" 2>/dev/null && ! wait "$mutation_sweeper_pid" 2>/dev/null; then :; fi
    mutation_sweeper_pid=
  fi
}

# mutation_remove DIR: removes a pass directory, its check-sh.pid last, so
# that a removal cut short (a SIGKILL, a copy written to while it goes)
# leaves a directory the next run's sweep still finds (mutation_sweep_stale).
mutation_remove() {
  find "$1" -mindepth 1 -maxdepth 1 ! -name check-sh.pid -exec rm -rf {} + && rm -rf "$1"
}

# proc_start PID: PID's start time, in clock ticks since boot (proc(5):
# /proc/PID/stat field 22, the 20th after the parenthesised command name).
# With the pid it names one process: a pid is reused, a start time with it
# is not. Fails once PID has ended.
proc_start() {
  local stat fields
  stat=$(<"/proc/$1/stat") || return 1
  read -r -a fields <<<"${stat##*) }"
  echo "${fields[19]}"
}

# mutation_sweep_stale: sweeps, then removes, each pass directory under the
# temp directory whose check.sh has ended (its pid gone, or now another
# process's). A run killed during a pass with its sweeper runs no EXIT
# trap, and what the pass left runs on, as 185 of exec's helper shells
# from five passes did (2026-11-14). A supervising tool stops a command
# with SIGTERM, then SIGKILL about a second later, to each descendant: the
# sweeper goes with check.sh, and so does the trap's sweep if it takes
# longer; orphans init adopted earlier are no descendants. So the next run
# sweeps them. A directory that names no check.sh (one made before pass
# directories named theirs, or by another kind of run), a symbolic link,
# and another user's directory are left alone; so is one whose check.sh
# still runs.
mutation_sweep_stale() {
  local d pid start
  for d in "${TMPDIR:-/tmp}"/sb-mutants-*/; do
    [[ ! -L ${d%/} && -O $d && -f $d/check-sh.pid ]] || continue
    read -r pid start <"$d/check-sh.pid" || continue
    [[ $pid =~ ^[1-9][0-9]*$ && $start =~ ^[0-9]+$ ]] || continue
    if [[ $(proc_start "$pid" 2>/dev/null) == "$start" ]]; then continue; fi
    # The physical path, as the processes' links name it. Another run's
    # sweep may have removed it since. This is housekeeping: a directory
    # it can't sweep or remove is reported, and the stage goes on.
    d=$(cd "$d" 2>/dev/null && pwd -P) || continue
    if mutation_sweep "$d" && mutation_remove "$d"; then
      echo "mutants: swept $d, left by a run that ended during its pass"
    else
      echo "check: could not sweep or remove $d, left by a run that ended during its pass" >&2
    fi
  done
}

# On exit during a pass: its sweeper stops, its leftovers go, and its
# copies if they could.
mutation_cleanup() {
  mutation_sweeper_stop
  if [[ -n $mutation_tmp ]] && mutation_sweep "$mutation_tmp"; then mutation_remove "$mutation_tmp"; fi
}

# The process group of the test run in_group started, while it runs: its
# leader's pid, which is the group's id.
group_pid=

# in_group CMD...: runs CMD in a process group of its own and waits for
# it, returning its status. A gate stopped by its pid alone (SIGTERM,
# SIGINT, SIGHUP) runs its EXIT trap, but its children aren't signalled:
# a stopped run's coverage pass (cargo-llvm-cov and the cargo test under
# it) ran on, adopted by init (seen twice, 2026-11-17). So:
# - The run starts under util-linux setsid(1), in a new session whose
#   group id is its pid ("setsid runs a program in a new session";
#   started with `&` it leads no group, so it forks nothing, and `-w`
#   keeps its status if it ever did:
#   https://man7.org/linux/man-pages/man1/setsid.1.html), and the trap
#   signals that whole group: kill(2) with a negative pid signals every
#   process in the group
#   (https://man7.org/linux/man-pages/man2/kill.2.html).
# - It is waited for with the `wait` builtin, which a SIGINT ends at once;
#   bash waits out a foreground child first (bash manual, "Signals":
#   https://www.gnu.org/software/bash/manual/html_node/Signals.html).
# - Without job control, "asynchronous commands ignore SIGINT and SIGQUIT"
#   (the same page), and the tests would inherit that: exec's test of a
#   child that SIGINTs itself failed. coreutils env's --default-signal
#   sets both to their defaults for CMD, whatever check.sh was started
#   with (https://www.gnu.org/software/coreutils/manual/html_node/env-invocation.html).
# - Its stdin is /dev/null, as for any `&` command without job control; no
#   test run reads it. Ctrl-C and a hangup reach it through the trap; a
#   terminal's Ctrl-\ and Ctrl-Z reach check.sh alone.
# Not covered: a process that starts a session of its own (script, a
# test's setsid or pty), or that ignores SIGTERM. Each is a test's, which
# ends it.
in_group() {
  setsid -w env --default-signal=INT,QUIT "$@" &
  group_pid=$!
  local status=0
  wait "$group_pid" || status=$?
  group_pid=
  return "$status"
}

# Ends the test run in_group started, if one runs, with its whole group.
group_stop() {
  # A group already gone is nothing to end.
  if [[ -n $group_pid ]] && kill -TERM -- "-$group_pid" 2>/dev/null; then :; fi
}

# On exit: the test run's group, then a mutation pass's leftovers.
on_exit() {
  group_stop
  # Before the pass's cleanup, whose failure would end the trap there.
  if [[ -n $test_tmp ]]; then rm -rf -- "$test_tmp" || true; fi
  mutation_cleanup
}
trap on_exit EXIT

# sandbox_argv DIR... -- CMD...: sets `sandboxed` to the command that runs
# CMD in private mount, network and PID namespaces where every mount is
# read-only but the DIRs, so a test, or a mutant that bends a path or
# drops a safety flag, can't write on the host as root, nor reach its
# daemons or processes. Every test run of the gate goes through it: the
# mutation passes (two mutants wrote files, 2026-11-15; ADR-021) and the
# test steps (a completion test wrote the host's /etc/fish, 2026-11-17;
# the maintainer: every test run in the sandbox).
# - The DIRs are the only writable places of the host, named by the
#   caller: a run's own temp directory (its TMPDIR) and output, never /tmp
#   as a whole, so the sandbox is the same wherever the worktree is.
# - /tmp is the run's own: a tmpfs, 1777 and root's, as the host's, so a
#   test that makes a directory there on purpose (bench's Speedtest step)
#   works and the host's /tmp is never written. What the run needs from
#   the host's /tmp (a DIR, the worktree, a PATH directory, HOME) is bound
#   back at its own path (sandbox_inner).
# - With --own-pids (the test steps), its own processes only: a PID
#   namespace with its own /proc, so no host process can be signalled, nor
#   reached through /proc/<pid>/root; when the namespace's first process
#   ends, every process in it does (pid_namespaces(7)), and unshare's
#   --kill-child ends it when unshare is killed (a stopped gate). The
#   mutation passes keep the host's: their sweeps find and end a pass's
#   leftovers by host pid and start time (mutation_sweep), as their tests
#   check; that a mutant can still signal a host process or reach the
#   host through /proc/<pid>/root is a known limit (ADR-021).
# - The kernel's settings read-only: /proc/sys, /proc/sysrq-trigger and
#   every /sys mount (the cgroup tree too); /dev read-only but /dev/pts
#   (the pty tests' terminals), with a private /dev/shm and /dev/mqueue.
# - /run is an empty read-only tmpfs, so no host daemon's socket (Docker,
#   systemd, D-Bus) is found where it is looked for, and the network
#   namespace has only its own loopback, so neither is a daemon on the
#   host's (the tests' fakes listen on the namespace's). A socket file
#   elsewhere on the host can still be connected to through a read-only
#   mount; no test does. SB_TEST_NO_SYSTEMD=1 tells the one test that needs
#   systemd (sb's hangup test of a session scope's stop) so: it skips here
#   and runs on a systemd of its own in a container,
#   crates/sb/tests/hangup-systemd.sh, by hand (the maintainer,
#   2026-11-17).
# It guards against mistakes, not a hostile test: root keeps
# CAP_SYS_ADMIN in it and could mount again (ADR-021).
# unshare(1), mount(8) and ip(8):
# https://man7.org/linux/man-pages/man1/unshare.1.html,
# https://man7.org/linux/man-pages/man8/mount.8.html,
# https://man7.org/linux/man-pages/man8/ip-link.8.html,
# https://man7.org/linux/man-pages/man7/pid_namespaces.7.html. Root only:
# mount(8) refuses a user namespace's mapped user (util-linux 2.37). Not
# root, CMD runs as it is, with no sandbox, and says so: it then writes
# only what that user may.
# A command array, not a function, so that in_group's setsid can start it.
sandboxed=()
sandbox_argv() {
  local dirs=() pids=() own=0
  if [[ $1 == --own-pids ]]; then
    own=1 pids=(--pid --fork --kill-child --mount-proc)
    shift
  fi
  while [[ $1 != -- ]]; do dirs+=("$1"); shift; done
  shift
  if [[ $(id -u) == 0 ]]; then
    # unshare --fork ignores SIGINT and SIGTERM around its fork, and the
    # program it starts inherits that (util-linux 2.37.2: SigIgn shows
    # SIGTERM ignored there), so a test's SIGTERM trap (a shell can't trap
    # a signal ignored at its start) and the first process's own trap
    # would never run: the three are set to their defaults first
    # (coreutils env's --default-signal).
    sandboxed=(env SB_TEST_NO_SYSTEMD=1 unshare --mount --net "${pids[@]}" --propagation private
      env --default-signal=INT,QUIT,TERM
      bash -c "$(declare -f sandbox_inner)"'; sandbox_inner "$@"'
      sandbox "$own" "${#dirs[@]}" "${dirs[@]}" "$@")
  else
    echo "check: not root: the tests run without the sandbox"
    sandboxed=("$@")
  fi
}

# sandbox_inner OWN N DIR... CMD...: inside the namespaces, a private
# /tmp (a tmpfs, 1777 and root's, as the host's: a test may make a
# directory there, as bench's Speedtest step does on purpose, and the
# host's /tmp is never written), with what the run needs from the host's
# /tmp bound back into it, the N DIRs (physical paths) there among it;
# the other DIRs bound to themselves, so that each keeps its own
# read-write flag; every
# other mount remounted read-only but /proc (the namespace's own),
# /dev/pts, the private /tmp, /dev/shm and /dev/mqueue; /proc/sys and
# /proc/sysrq-trigger read-only; an empty read-only tmpfs on /run; loopback
# up; then CMD: with OWN 1 (its own PID namespace) as the namespace's first
# process's child, else in this shell's place.
# Each step that fails stops the run (status 125), and so does any mount
# left writable, or one that can't be reached and isn't shown to be under
# a later mount: no test runs on a writable host.
sandbox_inner() {
  local own=$1 n=$2 d i j mp fields opts err points=() writable=() ids=()
  shift 2
  local dirs=("${@:1:n}")
  shift "$n"
  fail() { echo "check: the sandbox $*" >&2; exit 125; }
  # A refused DIR: a writable root, or a directory above it, can't be
  # asked for (a log directory that is a link to /, say).
  for d in "${dirs[@]}"; do
    [[ $d == /* && $d != / ]] || fail "refuses the writable directory '$d'"
  done
  # What the run needs from the host's /tmp, to bind back at the same
  # path once the private one covers it: each DIR there, writable; the
  # working directory (a worktree in a scratch directory), each PATH
  # directory and HOME (the gate's own tests' stub tools and home),
  # read-only by the remount below. Resolved now, while the host's /tmp is
  # still there; outer ones first, so that a DIR inside the working
  # directory binds over it; only what the host has, so nothing is made
  # there.
  local need=() back=() p rel
  need+=("${dirs[@]}" "$PWD" "${HOME:-}")
  IFS=: read -r -a fields <<<"${PATH:-}"
  need+=("${fields[@]}")
  # The physical path (cd's own, pwd -P); in byte order a directory sorts
  # before what is under it.
  while IFS= read -r p; do back+=("$p"); done < <(
    for p in "${need[@]}"; do
      [[ -n $p && -d $p ]] || continue
      p=$(cd -- "$p" 2>/dev/null && pwd -P) || continue
      [[ $p == /tmp/* ]] && printf '%s\n' "$p"
    done | LC_ALL=C sort -u
  )
  # The private /tmp is built on /run, over the host's (covered again
  # below), while the host's /tmp is still in place, with what is needed
  # bound in from it, then moved over /tmp in one step: every command
  # until then, and each one after it found through a PATH directory
  # there (the gate's own tests' stub tools), stays reachable, and nothing
  # is made or left in /run. -n: no /run/mount/utab entry. Recursive, so
  # that a mount under a bound-back path is there as on the host (and
  # read-only by the remount below), and mkdir -p only meets directories
  # that exist.
  mount -n -t tmpfs -o mode=1777,nosuid,nodev,size=4g sb-tmp /run || fail "could not make a private /tmp"
  for p in "${back[@]}"; do
    rel=${p#/tmp/}
    mkdir -p "/run/$rel" && mount -n --rbind "$p" "/run/$rel" || fail "could not bind $p back"
  done
  mount -n --move /run /tmp || fail "could not put the private /tmp in place"
  for d in "${dirs[@]}"; do
    [[ $d == /tmp/* ]] && continue
    mount -n --bind "$d" "$d" || fail "could not bind $d"
  done
  # The places a test writes in besides the DIRs: private ones, not the
  # host's.
  mount -n -t tmpfs -o nosuid,nodev,size=256m sb-shm /dev/shm || fail "could not cover /dev/shm"
  if [[ -d /dev/mqueue ]]; then
    mount -n -t tmpfs -o nosuid,nodev,noexec,size=4k sb-mqueue /dev/mqueue || fail "could not cover /dev/mqueue"
  fi
  # Whether a mount point stays as it is: a DIR or under one, the
  # namespace's /proc, a terminal, or a private tmpfs above.
  kept() {
    local d hit=1
    for d in "${dirs[@]}" /dev/pts /dev/shm /dev/mqueue; do
      if [[ $1 == "$d" || $1 == "$d"/* ]]; then hit=0; fi
    done
    if [[ $1 == /proc || $1 == /proc/* ]] && [[ $1 != /proc/sys && $1 != /proc/sys/* ]]; then hit=0; fi
    # The private /tmp itself; what is bound back under it is read-only
    # unless it is a DIR.
    if [[ $1 == /tmp ]]; then hit=0; fi
    # Without a PID namespace of its own, /proc and its mounts are the
    # host's: /proc/sys among them goes read-only below; the others (the
    # binfmt_misc automount) stay.
    ((hit == 0))
  }
  # The mount table's mount points, in order, with their ids. Field 1 of
  # mountinfo is the mount's id, field 5 its mount point, field 6 its
  # options. A backslash starts a three-digit octal escape (space, tab,
  # newline, backslash; proc(5)); printf's %b reads \0NNN, so each gets
  # its 0; -v, not $( ), keeps a trailing newline. The table is read
  # front to back without seeking back (mapfile), not a line at a time:
  # read seeks back to each line's end, and a seek in a table that is
  # changing (the host removing a container's mounts) starts the next
  # line part way through another one. A line that still doesn't read as
  # an id and a path stops the run: a skipped mount below is known by its
  # id.
  table() {
    points=() writable=() ids=()
    local line lines
    mapfile -t lines </proc/self/mountinfo
    for line in "${lines[@]}"; do
      read -r -a fields <<<"$line"
      printf -v mp '%b' "${fields[4]//\\/\\0}"
      [[ ${fields[0]} =~ ^[0-9]+$ && $mp == /* ]] || fail "could not read the mount table: $line"
      ids+=("${fields[0]}")
      points+=("$mp")
      opts=,${fields[5]},
      if [[ $opts == *,ro,* ]]; then writable+=(0); else writable+=(1); fi
    done
  }
  # Whether mount i has left the namespace since the table was read (a
  # fresh read of it, on purpose): the
  # host removed its mount point (as docker does when a container ends),
  # which takes the mount away here too (mount_namespaces(7)), so there is
  # nothing left to make read-only.
  gone() {
    local line lines
    mapfile -t lines </proc/self/mountinfo
    for line in "${lines[@]}"; do
      [[ ${line%% *} == "${ids[$1]}" ]] && return 1
    done
    return 0
  }
  # Whether mount i is under a later one (the same point or one of its
  # directories): hidden, so it can't be reached.
  covered() {
    local i=$1 j
    for ((j = i + 1; j < ${#points[@]}; j++)); do
      if [[ ${points[i]} == "${points[j]}" || ${points[j]} == / || ${points[i]} == "${points[j]}"/* ]]; then return 0; fi
    done
    return 1
  }
  table
  for ((i = 0; i < ${#points[@]}; i++)); do
    mp=${points[i]}
    # Kept, or under a later mount (the host's /proc and its mounts under
    # the namespace's own, an outer sandbox's /run hiding /run/lock).
    kept "$mp" && continue
    covered "$i" && continue
    if [[ ! -e $mp ]]; then
      gone "$i" && continue
      fail "can't reach $mp, which no later mount covers"
    fi
    if ! err=$(mount -n -o remount,bind,ro "$mp" 2>&1); then
      gone "$i" && continue
      fail "could not make $mp read-only: $err"
    fi
  done
  # The kernel's settings through /proc: bound to themselves, read-only.
  for d in /proc/sys /proc/sysrq-trigger; do
    mount -n --bind "$d" "$d" && mount -n -o remount,bind,ro "$d" || fail "could not make $d read-only"
  done
  mount -n -t tmpfs -o ro,nosuid,nodev,noexec,size=4k sb-sandbox /run || fail "could not cover /run"
  ip link set lo up || fail "could not bring up loopback"
  # Every mount that can be reached, read-only but those kept.
  table
  for ((i = 0; i < ${#points[@]}; i++)); do
    mp=${points[i]}
    ((writable[i])) || continue
    covered "$i" && continue
    case $mp in /proc/sys | /proc/sys/* | /proc/sysrq-trigger) fail "left $mp writable" ;; esac
    kept "$mp" || fail "left $mp writable"
  done
  # The working directory still names the mount it was entered through,
  # read-only now: entered again by its path, it is on the new bind mounts
  # when it lies under a DIR.
  cd "$PWD" || fail "could not enter $PWD again"
  ((own)) || exec "$@"
  # This shell is the namespace's first process: it waits for CMD, reaping
  # every orphan the tests leave (they are its children now), passes on a
  # SIGTERM, SIGINT or SIGHUP (the first process gets none it doesn't
  # handle), and ends with CMD's status; then the namespace's processes
  # end with it. CMD starts with SIGINT and SIGQUIT at their defaults, as
  # in_group's: bash ignores both for a command it starts with `&`.
  local child status
  env --default-signal=INT,QUIT "$@" &
  child=$!
  trap 'kill -TERM "$child" 2>/dev/null' TERM
  trap 'kill -INT "$child" 2>/dev/null' INT
  trap 'kill -HUP "$child" 2>/dev/null' HUP
  while true; do
    status=0
    wait "$child" || status=$?
    # A trapped signal ends `wait` early (status > 128) while CMD runs.
    kill -0 "$child" 2>/dev/null || break
  done
  exit "$status"
}

# The test steps' temp directory: each run's own, made on first use under
# TMPDIR (or /tmp), removed on exit (and by a later run's sweep, when this
# one is killed: test_tmp_sweep_stale), the only writable place in the
# sandbox besides target/ and the log directory (test_sandbox).
test_tmp=

# test_tmp_sweep_stale: removes each test steps' temp directory under the
# temp directory whose check.sh has ended (its pid gone, or now another
# process's), as mutation_sweep_stale does pass directories. A run killed
# with SIGKILL runs no EXIT trap; the sandbox's PID namespace ended its
# processes with it, so only files are left. A link, another user's
# directory, and one whose check.sh still runs are left alone.
test_tmp_sweep_stale() {
  local d pid start
  for d in "${TMPDIR:-/tmp}"/sb-check.*/; do
    [[ ! -L ${d%/} && -O $d && -f $d/.sb-check.pid ]] || continue
    read -r pid start <"$d/.sb-check.pid" || continue
    [[ $pid =~ ^[1-9][0-9]*$ && $start =~ ^[0-9]+$ ]] || continue
    if [[ $(proc_start "$pid" 2>/dev/null) == "$start" ]]; then continue; fi
    if rm -rf -- "${d%/}"; then
      echo "check: removed ${d%/}, left by a run that was killed"
    else
      echo "check: could not remove ${d%/}, left by a run that was killed" >&2
    fi
  done
}

# stale_snapshots: fails the gate on any insta pending snapshot under
# crates/ (a `.pending-snap` or `.snap.new`, gitignored), left by an
# earlier run whose snapshot differed. When the assertion passes, insta
# cleans them up: it writes to an existing `.pending-snap` (which fails
# with "Read-only file system" in the sandbox's read-only worktree,
# test_sandbox, and fails the test) and removes the `.snap.new` (which
# fails there too, unreported, and leaves it stale): insta 1.49.0's
# cleanup_passing,
# https://docs.rs/insta/1.49.0/src/insta/runtime.rs.html
# They are the developer's to review or remove, so the gate names them
# and leaves them.
stale_snapshots() {
  local stale
  mapfile -t stale < <(find crates -type f \( -name '*.pending-snap' -o -name '*.snap.new' \) | sort)
  if ((${#stale[@]})); then
    {
      echo "check: insta's pending snapshots, left by an earlier run, are under crates/:"
      printf '  %s\n' "${stale[@]}"
      echo "The tests run with the worktree read-only, where insta can't clean these up: a .pending-snap fails the test that owns it, and a .snap.new stays stale."
      echo "Review them (cargo insta review) or remove them, then run the gate again:"
      echo "  find crates -type f \\( -name '*.pending-snap' -o -name '*.snap.new' \\) -delete"
    } >&2
    exit 1
  fi
  echo "none"
}

# test_sandbox CMD...: sets `sandboxed` to CMD in the sandbox for a test
# step: writable are the run's temp directory (the tests' TMPDIR), the
# worktree's target/ (the builds and coverage data) and the tests' log
# directory, $SB_LOG_DIR (under target/, so the same, unless it is a link
# out of it, as the gate's own tests' scratch repositories make it); the
# rest of the host, the worktree included, is read-only.
test_sandbox() {
  if [[ -z $test_tmp ]]; then
    test_tmp=$(mktemp -d "${TMPDIR:-/tmp}/sb-check.XXXXXX")
    test_tmp=$(cd "$test_tmp" && pwd -P)
    # mktemp makes it 0700; sb's relaunch tests run a stand-in sudo, and
    # sb, as `nobody`, which must be able to enter it, as for the
    # mutation pass's directory.
    chmod 755 "$test_tmp"
    # It names this check.sh, so that a later run can tell a killed run's
    # directory from a live one's (test_tmp_sweep_stale).
    echo "$$ $(proc_start $$)" >"$test_tmp/.sb-check.pid"
  fi
  mkdir -p target "$SB_LOG_DIR"
  sandbox_argv --own-pids "$test_tmp" "$(cd target && pwd -P)" "$(cd "$SB_LOG_DIR" && pwd -P)" -- \
    env TMPDIR="$test_tmp" "$@"
}

# in_sandbox CMD...: CMD in its own process group (in_group), in the test
# steps' sandbox (test_sandbox), returning its status; a sandbox that
# could not be set up (125) is no verdict: exit 2.
in_sandbox() {
  local status=0
  test_sandbox "$@"
  in_group "${sandboxed[@]}" || status=$?
  if ((status == 125)); then
    echo "check: the sandbox could not be set up for: $*" >&2
    exit 2
  fi
  return "$status"
}

# mutation_pass N ARGS...: one cargo-mutants pass over the changed code with
# the given package arguments; appends its outcomes file to `outcomes`.
mutation_pass() {
  local n=$1 list rc=0
  shift
  # Without CARGO_TARGET_DIR: cargo-mutants builds each mutant in a copy of
  # the tree, and 27.1.0 leaves the variable as it finds it, so with it set
  # every mutant would build into the repository's target/.
  # Each mutant's tests run under cargo-nextest with .config/nextest.toml's
  # `mutants` profile, which ends a test that hangs and counts it as
  # failed (ADR-021): https://mutants.rs/nextest.html
  # The profile goes to the test runs only, through --cargo-test-arg
  # (https://mutants.rs/cargo-args.html): --cargo-arg would also reach
  # the build runs and, under --check, `cargo check`, where --profile
  # names a cargo build profile.
  local mutants=(env -u CARGO_TARGET_DIR cargo mutants --in-diff target/changes.diff --cargo-arg=--locked
    --test-tool=nextest --cargo-test-arg=--profile=mutants "$@")
  # A plain assignment, so a failing list stops the check (errexit is off
  # inside an `if` condition, command substitutions included; and `local
  # list=$(...)` would take local's status: declared apart above).
  list=$("${mutants[@]}" --list)
  # Changed tests or comments only: cargo-mutants writes no outcomes.
  [[ -n $list ]] || return 0
  echo "mutants: $*"
  # The pass's copies of the tree go under a directory of its own:
  # cargo-mutants makes them with the tempfile crate, under TMPDIR (27.1.0's
  # copy_tree.rs; https://doc.rust-lang.org/std/env/fn.temp_dir.html).
  # Every process still running from them is killed after the pass, or
  # when check.sh exits during it (mutation_sweep). A new directory under
  # the system's temp directory, never inside a repository: the tests
  # inherit TMPDIR, and sb-repo's expect git to find no repository above
  # their temp directories. The physical path, since a process's
  # executable is named by it.
  mutation_tmp=$(mktemp -d "${TMPDIR:-/tmp}/sb-mutants-$n.XXXXXX")
  mutation_tmp=$(cd "$mutation_tmp" && pwd -P)
  # mktemp makes the directory 0700. sb's relaunch tests run a stand-in
  # sudo, and the copy's sb, as `nobody`, which must be able to enter it:
  # 0755, searchable by everyone as /tmp is, but writable by root only.
  chmod 755 "$mutation_tmp"
  # It names this check.sh, so that a later run can tell the directory of
  # a run killed during its pass from a live one (mutation_sweep_stale).
  echo "$$ $(proc_start $$)" >"$mutation_tmp/check-sh.pid"
  # The whole mutant's 300 s timeout stays as the backstop behind
  # nextest's per-test one.
  mutation_sweeper "$mutation_tmp"
  # In the sandbox: the host read-only but the pass's directory (its
  # TMPDIR), the pass's output and the tests' log directory. Physical
  # paths, as mountinfo names them.
  mkdir -p "target/mutants-run-$n" "$SB_LOG_DIR"
  sandbox_argv "$mutation_tmp" "$(cd "target/mutants-run-$n" && pwd -P)" "$(cd "$SB_LOG_DIR" && pwd -P)" -- \
    "${mutants[@]}" -o "target/mutants-run-$n" --timeout 300
  TMPDIR=$mutation_tmp PATH="$root/target/no-sudo:$PATH" "${sandboxed[@]}" || rc=$?
  mutation_sweeper_stop
  if ! mutation_sweep "$mutation_tmp"; then
    # Kept for a look, and not swept again on exit, nor by a later run:
    # it names no check.sh now.
    rm -f "$mutation_tmp/check-sh.pid"
    mutation_tmp=
    exit 2
  fi
  mutation_remove "$mutation_tmp"
  mutation_tmp=
  # 0 all caught, 2 some missed, 3 some timed out: the score decides. Any
  # other status (4: the unmutated baseline failed, so nothing was tested)
  # means no verdict (https://mutants.rs/exit-codes.html).
  case $rc in
  0 | 2 | 3) ;;
  125)
    echo "check: the mutation sandbox could not be set up for: $*" >&2
    exit 2
    ;;
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
  need cargo-nextest
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
  rm -f target/changes.diff
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
  # Written anew, never through what a test left there (target/ is
  # writable in the sandbox).
  rm -f target/no-sudo/sudo
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
  # Before any test run: a stale pending snapshot fails its tests in the
  # sandbox (stale_snapshots).
  step "no insta pending snapshots under crates/"
  stale_snapshots
  # The crates' unit and integration tests run once, under coverage: a
  # failing test fails cargo-llvm-cov as it fails cargo test
  # (https://github.com/taiki-e/cargo-llvm-cov#usage: "cargo llvm-cov" runs
  # `cargo test`). It runs no doc tests on a stable toolchain, so those run
  # after it (ADR-016).
  step "tests (crates), with changed-line coverage >= ${COVERAGE_MIN}%"
  need script
  need setsid
  need cargo-llvm-cov
  need diff-cover
  # Each test run in a process group of its own, ended with check.sh
  # (in_group).
  # Every test run in the sandbox (sandbox_argv), which hides systemd: the
  # one test that needs it is the container script's.
  if [[ $(id -u) == 0 ]]; then
    echo "in the sandbox; hangup's a_session_scope_stop_takes_the_grace skips (no systemd): run crates/sb/tests/hangup-systemd.sh"
  fi
  in_sandbox cargo llvm-cov --locked --quiet "${pkg_args[@]}" --lcov --output-path target/lcov.info
  step "tests (doc tests)"
  in_sandbox cargo test --locked --quiet --doc "${pkg_args[@]}"
  step "changed-line coverage >= ${COVERAGE_MIN}% (diff-cover)"
  # New files are untracked until committed; without --include-untracked
  # they would not count as changed lines.
  diff-cover target/lcov.info --compare-branch="$base" --include-untracked \
    --fail-under="$COVERAGE_MIN"
  # The crates' tests again, in a release build: a test that passes only
  # at a debug build's speed, or a bug only the optimiser or the missing
  # debug assertions show, fails here. indicatif's rate limit dropped the
  # in-memory screen's last frames in a release build alone, and every
  # debug run passed (8830411; the maintainer, 2026-11-16). The cargo
  # profile is `release-tests` (Cargo.toml: the release profile's code in
  # target/release-tests/, so the test build of sb never sits at
  # target/release/sb), the nextest one `release-tests` too
  # (.config/nextest.toml: a test still running after 60 s fails rather
  # than holds up the gate, though under heavy load on a shared host a
  # slow test could fail there alone; every failure is reported). After
  # the cheap coverage check, so that one fails first. The whole
  # workspace, as measured with the gate's usual scope (8 cores): 40 s with
  # nothing to rebuild, 75 s after a change to a leaf crate, 150 s after
  # one to cli-core; four timing-heavy crates alone (term-ui, tui, exec,
  # cli-core) took 35 to 55 s, a saving of up to about 100 s, with the
  # other crates' release builds left untested.
  step "tests (crates), release build"
  need cargo-nextest
  in_sandbox cargo nextest run --locked --cargo-profile release-tests --profile release-tests "${pkg_args[@]}"
  mutation_check
  step "startup: sb --help and sb version, median <= ${STARTUP_MAX_MS} ms"
  need hyperfine
  cargo build --locked --quiet --release -p sb
  # The timed runs are runs of sb as root, so in the sandbox too; set up
  # once, it adds nothing to a run's time.
  in_sandbox hyperfine --warmup 5 --runs 50 --export-json target/startup.json \
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

# project_coverage: the ratchet's coverage floors (ADR-025), over the files
# under crates/ in the coverage run's lcov report: each file's LF and LH
# (lines found and hit) and FNF and FNH (functions), as cargo-llvm-cov
# writes them (https://github.com/linux-test-project/lcov/blob/master/man/geninfo.1,
# "TRACEFILE FORMAT"). Over crates/ these totals are the ones ADR-014's
# measurement takes from `cargo llvm-cov --json --summary-only`. Compared
# in hundredths of a percent, as integers (the floors have two decimals):
# a figure at a floor less its tolerance passes, one below it fails. A
# file is matched by the root's logical or physical path, whichever the
# report records.
project_coverage() {
  local lcov=target/lcov.info
  [[ -s $lcov ]] || { echo "check: the coverage run left no $lcov" >&2; exit 2; }
  if [[ $(id -u) != 0 ]]; then
    echo "not root: the root tests didn't run, so coverage reads lower than the floors were measured at (ADR-025)"
  fi
  awk -v crates="$root/crates/" -v physical="$(cd crates && pwd -P)/" -v lines="$PROJECT_LINES_MIN" \
    -v functions="$PROJECT_FUNCTIONS_MIN" -v tol="$PROJECT_COVERAGE_TOLERANCE_MAX" '
    /^SF:/ { on = index(substr($0, 4), crates) == 1 || index(substr($0, 4), physical) == 1; next }
    !on { next }
    /^LF:/ { lf += substr($0, 4) }
    /^LH:/ { lh += substr($0, 4) }
    /^FNF:/ { fnf += substr($0, 5) }
    /^FNH:/ { fnh += substr($0, 5) }
    # One figure, printed; true when it is below its floor less the
    # tolerance.
    function judge(name, hit, found, floor,   least, low) {
      least = int((floor - tol) * 100 + 0.5)
      low = hit * 10000 < least * found
      printf "%s: %.3f%% (%d of %d), floor %s%%, failing below %.2f%%%s\n", name, 100 * hit / found, hit, found, floor,
        least / 100, low ? ": below" : ""
      return low
    }
    END {
      if (!lf || !fnf) { print "check: the coverage report has nothing under crates/" > "/dev/stderr"; exit 2 }
      bad = judge("lines", lh, lf, lines)
      bad += judge("functions", fnh, fnf, functions)
      exit bad ? 1 : 0
    }' "$lcov"
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
  step "project coverage (crates/): lines >= ${PROJECT_LINES_MIN}%, functions >= ${PROJECT_FUNCTIONS_MIN}%, less ${PROJECT_COVERAGE_TOLERANCE_MAX} points"
  project_coverage
  step "dependencies and architecture (cargo-deny, shipped graph)"
  need cargo-deny
  cargo deny --locked --manifest-path crates/sb/Cargo.toml check
  step "sb's normal build has no testing feature"
  testing_feature_check
  step "sb's normal build offers post-quantum key exchange first"
  post_quantum_feature_check
  # The ratchet's ceiling: today's size plus its tolerance (ADR-025).
  local size_ratchet=$((SIZE_RATCHET_MAX_BYTES + SIZE_RATCHET_MAX_BYTES * SIZE_TOLERANCE_MAX_PCT / 100))
  step "binary size <= ${size_ratchet} bytes (the ratchet's ${SIZE_RATCHET_MAX_BYTES} plus ${SIZE_TOLERANCE_MAX_PCT}%) and <= ${SIZE_MAX_BYTES} (production glibc, built on ubuntu:${RUNTIME_RELEASES[0]} through cross)"
  if [[ $arch != x86_64 ]]; then
    # The image's C compiler builds for its own architecture, and the
    # production build is x86_64 (ADR-020).
    skip "the production glibc build, its size limit and its runs: the production build is x86_64, made on an x86_64 host only, and this host is $arch"
  else
    # cross builds the image from build/Dockerfile (Cross.toml) and runs
    # the build in it, so the binary needs the oldest release's glibc at
    # most. The target directory is its own: build scripts compiled in the
    # image can't run on the host or in another image (ADR-020).
    # The build runs in cross's container, so a systemd-run scope around
    # check.sh doesn't cap it: CROSS_CONTAINER_OPTS passes the caps to that
    # container's `docker run`, as crates/journal/tests/example.sh does
    # (cross 0.2.5's src/docker/shared.rs and CHANGELOG):
    # https://github.com/cross-rs/cross/blob/v0.2.5/src/docker/shared.rs
    # https://github.com/cross-rs/cross/blob/v0.2.5/CHANGELOG.md
    # https://docs.docker.com/engine/containers/resource_constraints/
    # --cpus can't exceed the host's CPU count: the production build runs
    # on a host of at least 8.
    need cross
    need docker
    local cross_target="$root/target/cross-gnu"
    CARGO_TARGET_DIR="$cross_target" CROSS_CONTAINER_OPTS='--memory 16g --cpus 8' \
      cross build --locked --quiet -p sb --profile production \
        --target x86_64-unknown-linux-gnu
    local sb="$cross_target/x86_64-unknown-linux-gnu/production/sb"
    size=$(stat -c %s "$sb")
    echo "sb: ${size} bytes; at most ${size_ratchet} bytes (the ratchet), ${SIZE_MAX_BYTES} (the ceiling)"
    if ((size > size_ratchet)); then
      echo "check: sb is above the ratchet's ${size_ratchet} bytes (ADR-025)" >&2
      exit 1
    fi
    ((size <= SIZE_MAX_BYTES))
    # A binary one release's glibc or loader can't load fails here. No
    # network: `sb version` needs none. The container is removed on exit;
    # its name says, on a shared Docker host, which run it belongs to.
    # docker's own failures exit 125, a check that could not run (exit 2):
    # https://docs.docker.com/reference/cli/docker/container/run/#exit-status
    step "sb version in a container of each supported release (${RUNTIME_RELEASES[*]})"
    local release out status
    for release in "${RUNTIME_RELEASES[@]}"; do
      status=0
      out=$(docker run --rm --name "sb-check-$$-${release/./}" --network none -e SB_LOG_DIR=/tmp/sb-log \
        -v "$sb:/sb:ro" "ubuntu:$release" /sb version) || status=$?
      if ((status == 125)); then
        echo "check: docker could not run a ubuntu:$release container" >&2
        exit 2
      elif ((status != 0)); then
        echo "check: ubuntu:$release can't run the production binary" >&2
        exit 1
      fi
      echo "ubuntu:$release: $out"
    done
    # arm64 ships too (release spec, "Release channels"): its production
    # build, from the same image, and its `sb version` under QEMU
    # user-mode with 22.04's arm64 glibc (build/qemu.Dockerfile), so a
    # break shows before CI's arm runners. The test suite runs on those
    # runners only: emulated, it would be several times slower within the
    # CPU cap, and its timing tests would turn flaky.
    step "the aarch64 production build, and its sb version under QEMU on 22.04's arm64 glibc (build/qemu.Dockerfile)"
    CARGO_TARGET_DIR="$cross_target" CROSS_CONTAINER_OPTS='--memory 16g --cpus 8' \
      cross build --locked --quiet -p sb --profile production \
        --target aarch64-unknown-linux-gnu
    local arm="$cross_target/aarch64-unknown-linux-gnu/production/sb"
    echo "sb (aarch64): $(stat -c %s "$arm") bytes"
    # The gate's own image, kept as a cache and named by its Dockerfile's
    # hash, so a change builds a new one; built only when missing
    # (CONSTRAINTS.md, "Images the gate keeps").
    local sum qemu_image
    sum=$(sha256sum build/qemu.Dockerfile)
    qemu_image="sb-rust-gate/qemu-check:${sum:0:12}"
    if ! docker image inspect "$qemu_image" >/dev/null 2>&1; then
      status=0
      docker build --quiet --tag "$qemu_image" --file build/qemu.Dockerfile build >/dev/null || status=$?
      if ((status != 0)); then
        echo "check: docker could not build build/qemu.Dockerfile" >&2
        exit 2
      fi
    fi
    status=0
    out=$(docker run --rm --name "sb-check-$$-arm64" --network none -e SB_LOG_DIR=/tmp/sb-log \
      -v "$arm:/sb:ro" "$qemu_image" qemu-aarch64-static -L /usr/aarch64-linux-gnu /sb version) || status=$?
    if ((status == 125)); then
      echo "check: docker could not run the QEMU container" >&2
      exit 2
    elif ((status != 0)) || [[ $out != "Saltbox CLI version: "* ]]; then
      echo "check: the aarch64 production binary doesn't run under QEMU: $out" >&2
      exit 1
    fi
    echo "aarch64 (QEMU): ${out%%$'\n'*}"
  fi
}

# The tools each stage runs: fast's, then task's, then full's (cross and
# Docker only where the production build runs).
tools=(node go gitleaks)
if [[ $stage != fast ]]; then tools+=(script cargo-llvm-cov diff-cover cargo-mutants cargo-nextest hyperfine); fi
if [[ $stage == full ]]; then tools+=(cargo-deny); fi
if [[ $stage == full && $arch == x86_64 ]]; then tools+=(cross docker); fi
check_tools "${tools[@]}"
# The sandbox's tools (util-linux, iproute2), as root (sandbox_argv): every
# test run goes through it, the floor guard's own tests in fast too, and
# each in a process group of its own (in_group: setsid).
need setsid
if [[ $(id -u) == 0 ]]; then
  need unshare
  need mount
  need ip
fi

# What a killed run's mutation pass and test steps left, at the start of
# every stage.
mutation_sweep_stale
test_tmp_sweep_stale
"$stage"
printf '\n\033[1mcheck %s: all passed\033[0m\n' "$stage"
