#!/usr/bin/env bash
# Runs the CONSTRAINTS.md checks. CONSTRAINTS.md is the source of truth; this
# script mirrors its "Checked by" column and must change with it.
#
#   scripts/check.sh fast [base]   after edits: each crate's deny.toml entry
#                                  and lints, fmt, clippy, floor guard (and
#                                  the guard's own tests when scripts/ changed),
#                                  secrets
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
# every stage checks (docker plan Task 9), and python-env's live opt-in
# would start containers from every test run (module build M21).
unset SB_TEST_BINARY SB_LIVE_DOCKER SB_LIVE_PYTHON
# The mutation passes' nextest runs read their settings from
# .config/nextest.toml's `mutants` profile alone: an inherited NEXTEST_*
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
# The releases the production binary must run on: the host spec's runtime
# list, sb-host's SUPPORTED_RUNTIME_RELEASES. The oldest is the image's
# (build/Dockerfile, ADR-020).
RUNTIME_RELEASES=(22.04 24.04 26.04)

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
  cargo-llvm-cov | cargo-mutants | cargo-nextest | cargo-deny | hyperfine | cross)
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
    node --test --test-reporter=dot scripts/floor-guard.test.mjs
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
trap mutation_cleanup EXIT

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
  TMPDIR=$mutation_tmp PATH="$root/target/no-sudo:$PATH" "${mutants[@]}" -o "target/mutants-run-$n" \
    --timeout 300 || rc=$?
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
  step "sb's normal build has no testing feature"
  testing_feature_check
  step "binary size <= ${SIZE_MAX_BYTES} bytes (production glibc, built on ubuntu:${RUNTIME_RELEASES[0]} through cross)"
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
    echo "sb: ${size} bytes"
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
  fi
}

# The tools each stage runs: fast's, then task's, then full's (cross and
# Docker only where the production build runs).
tools=(node go gitleaks)
if [[ $stage != fast ]]; then tools+=(script cargo-llvm-cov diff-cover cargo-mutants cargo-nextest hyperfine); fi
if [[ $stage == full ]]; then tools+=(cargo-deny); fi
if [[ $stage == full && $arch == x86_64 ]]; then tools+=(cross docker); fi
check_tools "${tools[@]}"

# What a killed run's mutation pass left, at the start of every stage.
mutation_sweep_stale
"$stage"
printf '\n\033[1mcheck %s: all passed\033[0m\n' "$stage"
