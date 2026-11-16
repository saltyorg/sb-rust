#!/usr/bin/env bash
# install.sh's policy, in process (ops spec, Testing Strategy, "install
# script"; sb-go's scripts/test-install-os-policy.sh rebuilt): the script
# is sourced, which runs nothing (its own `return` test), and every step that
# changes the host is replaced by one that records its call, so each case
# shows what ran before a refusal and that nothing changed. Cases: the
# release lists (fresh and --repair), an existing Saltbox root, the
# options (-b as one argument to `sb setup`, -v not passed, a missing -b
# value, an unknown option, -h), `sb setup`'s stdin and exit code, the
# tag from the proxy or GitHub (an invalid one refused), the pinned
# download URL, curl's https-only flags, and the downloaded file's checks
# (size, ELF, the version line on stdout matching the tag, stderr aside).
#
#   install/tests/policy.sh
#
# Run only in a throwaway container (install/tests/containers.sh does):
# the cases stub every change, but the script is a root installer and is
# never run on a host. Its last line is `PASS: ...`.
set -euo pipefail
shopt -s inherit_errexit

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
script="$here/../install.sh"

# The production functions; sourced, the script runs nothing.
# shellcheck source=install/install.sh
source "$script"
trap - EXIT

root=$(mktemp -d)
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup_test() { rm -rf "$root"; }
trap cleanup_test EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# `$1` is a line of the file `$2`.
has_line() { grep -Fxq -- "$1" "$2"; }

# A case's world under $root/$1: an os-release naming VERSION_ID $2, an
# empty trace, a sentinel standing for the installed binary, no Saltbox
# root. Prints the case directory.
world() {
  local dir="$root/$1"
  mkdir -p "$dir"
  printf 'ID=ubuntu\nVERSION_ID="%s"\n' "$2" >"$dir/os-release"
  : >"$dir/trace"
  printf 'original-binary\n' >"$dir/installed-binary"
  printf '%s\n' "$dir"
}

# main in a subshell on the world `$1`, every changing step stubbed; the
# options are the rest. Its status, stdout and stderr go to the world.
run_main() {
  local dir="$1"
  shift
  local status=0
  (
    OS_RELEASE_FILE="$dir/os-release"
    SALTBOX_ROOT="$dir/saltbox"
    TEMP_DIR=""
    trace() { echo "$1" >>"$dir/trace"; }
    check_privileges() { trace check_privileges; }
    detect_platform() { trace detect_platform; }
    check_dependencies() { trace check_dependencies; }
    initialize_temp_dir() {
      trace initialize_temp_dir
      TEMP_DIR="$dir/installer-temp"
    }
    get_latest_version() {
      trace get_latest_version
      echo "v9.9.9"
    }
    download_binary() {
      trace download_binary
      echo "$dir/fake-download"
    }
    verify_binary() { trace verify_binary; }
    install_binary() {
      trace install_binary
      printf 'installed-binary\n' >"$dir/installed-binary"
    }
    run_setup() { trace run_setup; }
    parse_args "$@"
    main
  ) >"$dir/stdout" 2>"$dir/stderr" || status=$?
  echo "$status" >"$dir/status"
}

# The world `$1` ended with status `$2`.
status_is() {
  local got
  got=$(cat "$1/status")
  [[ $got == "$2" ]] || fail "$1: status $got, want $2; stderr: $(tr '\n' ' ' <"$1/stderr")"
}

# Nothing past the checks ran in the world `$1`, and the binary is the
# original; its stderr holds `$2`.
refused_before_changes() {
  local entry
  for entry in check_dependencies initialize_temp_dir get_latest_version download_binary \
    verify_binary install_binary run_setup; do
    if has_line "$entry" "$1/trace"; then
      fail "$1 ran $entry before refusing"
    fi
  done
  has_line original-binary "$1/installed-binary" || fail "$1 changed the binary"
  grep -Fq -- "$2" "$1/stderr" || fail "$1: no '$2' in: $(tr '\n' ' ' <"$1/stderr")"
}

# Every step ran in the world `$1`, setup as `$2` says (yes or no).
ran_through() {
  local entry
  for entry in check_privileges detect_platform check_dependencies initialize_temp_dir \
    get_latest_version download_binary verify_binary install_binary; do
    has_line "$entry" "$1/trace" || fail "$1 didn't run $entry"
  done
  has_line installed-binary "$1/installed-binary" || fail "$1 didn't install"
  if [[ $2 == yes ]]; then
    has_line run_setup "$1/trace" || fail "$1 didn't run setup"
  elif has_line run_setup "$1/trace"; then
    fail "$1 ran setup"
  fi
}

# sb-go's release cases: setup's list for a fresh install, the runtime
# list for --repair, both refused before anything changes.
d=$(world ubuntu-22-fresh 22.04)
run_main "$d"
status_is "$d" 1
refused_before_changes "$d" "Unsupported Ubuntu version for fresh installation: 22.04"
d=$(world ubuntu-22-repair 22.04)
run_main "$d" --repair
status_is "$d" 0
ran_through "$d" no
d=$(world ubuntu-20-fresh 20.04)
run_main "$d"
status_is "$d" 1
refused_before_changes "$d" "Unsupported Ubuntu version for fresh installation: 20.04"
d=$(world ubuntu-20-repair 20.04)
run_main "$d" --repair
status_is "$d" 1
refused_before_changes "$d" "Unsupported Ubuntu version for repair: 20.04"
for rel in 24.04 26.04; do
  d=$(world "ubuntu-$rel-fresh" "$rel")
  run_main "$d"
  status_is "$d" 0
  ran_through "$d" yes
done
d=$(world debian 12)
sed -i 's/^ID=ubuntu/ID=debian/' "$d/os-release"
run_main "$d"
status_is "$d" 1
refused_before_changes "$d" "Unsupported Linux distribution: debian"

# An existing Saltbox root (a directory, or a dangling symlink) is
# refused, except with --repair, which only restores sb.
d=$(world existing 24.04)
mkdir "$d/saltbox"
run_main "$d"
status_is "$d" 1
refused_before_changes "$d" "already exists"
d=$(world existing-link 24.04)
ln -s "$d/nowhere" "$d/saltbox"
run_main "$d"
status_is "$d" 1
refused_before_changes "$d" "already exists"
d=$(world existing-repair 24.04)
mkdir "$d/saltbox"
run_main "$d" --repair
status_is "$d" 0
ran_through "$d" no

# Options: a missing -b value and an unknown option fail before anything
# runs; -h prints the usage and exits 0.
for args in "-b" "-b -v" "-b --repair"; do
  d=$(world "no-branch${args// /}" 24.04)
  # shellcheck disable=SC2086 # the case's options, split
  run_main "$d" $args
  status_is "$d" 1
  grep -Fq -- "-b needs a branch name" "$d/stderr" || fail "no-branch $args: $(cat "$d/stderr")"
  [[ ! -s $d/trace ]] || fail "no-branch $args ran: $(tr '\n' ' ' <"$d/trace")"
done
d=$(world unknown 24.04)
run_main "$d" --bogus
status_is "$d" 1
grep -Fq -- "Unknown option: --bogus" "$d/stderr" || fail "unknown: $(cat "$d/stderr")"
[[ ! -s $d/trace ]] || fail "unknown ran: $(tr '\n' ' ' <"$d/trace")"
d=$(world help 24.04)
run_main "$d" -h
status_is "$d" 0
grep -Fq -- "-b <branch>" "$d/stdout" || fail "help: $(cat "$d/stdout")"
[[ ! -s $d/trace ]] || fail "help ran: $(tr '\n' ' ' <"$d/trace")"

# run_setup: `sb setup -b <branch>`, the branch one argument, -v not
# passed, stdin /dev/null (never the piped script), stdout and stderr
# the script's own (a terminal when the case runs under one), and sb's
# exit code passed on.
stub_sb() {
  local dir="$1" code="$2"
  cat >"$dir/sb" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >'$dir/argv'
readlink /proc/self/fd/0 >'$dir/stdin'
terminal=no
if [ -t 1 ] && [ -t 2 ]; then terminal=yes; fi
echo "\$terminal" >'$dir/terminal'
exit $code
EOF
  chmod +x "$dir/sb"
}
d=$(world setup-args 24.04)
stub_sb "$d" 0
status=0
(
  parse_args -v -b "my branch"
  INSTALL_PATH="$d/sb"
  run_setup
) >"$d/stdout" 2>"$d/stderr" || status=$?
[[ $status == 0 ]] || fail "setup-args: status $status; $(cat "$d/stderr")"
[[ $(cat "$d/argv") == $'setup\n-b\nmy branch' ]] || fail "setup-args: argv $(cat "$d/argv")"
[[ $(cat "$d/stdin") == /dev/null ]] || fail "setup-args: stdin $(cat "$d/stdin")"
if [[ ${SB_INSTALL_TEST_TERMINAL:-} == yes ]]; then
  # Under script(1) this test's stdout and stderr are a terminal: run
  # without redirecting them, sb setup gets them as they are.
  d=$(world setup-terminal 24.04)
  stub_sb "$d" 0
  (
    parse_args
    INSTALL_PATH="$d/sb"
    run_setup
  )
  [[ $(cat "$d/terminal") == yes ]] || fail "setup-terminal: sb setup wasn't on the terminal"
fi
d=$(world setup-default 24.04)
stub_sb "$d" 0
(
  parse_args
  INSTALL_PATH="$d/sb"
  run_setup
) >"$d/stdout" 2>"$d/stderr"
[[ $(cat "$d/argv") == $'setup\n-b\nmaster' ]] || fail "setup-default: argv $(cat "$d/argv")"
d=$(world setup-fails 24.04)
stub_sb "$d" 3
status=0
(
  parse_args
  INSTALL_PATH="$d/sb"
  run_setup
) >"$d/stdout" 2>"$d/stderr" || status=$?
[[ $status == 3 ]] || fail "setup-fails: status $status, want sb's 3"
grep -Fq "Setup command failed" "$d/stderr" || fail "setup-fails: $(cat "$d/stderr")"

# The tag: the proxy's JSON, else GitHub's (spaced) JSON; a tag that
# isn't a version is refused. Stand-in curl and wget on PATH record
# their arguments and print what the case gives them.
stub_fetch() {
  local dir="$1" proxy="$2" github="$3" name
  mkdir -p "$dir/bin"
  for name in curl wget; do
    cat >"$dir/bin/$name" <<EOF
#!/usr/bin/env bash
printf '%s\n' "$name \$*" >>'$dir/fetches'
case "\$*" in
  *svm.saltbox.dev*) printf '%s' '$proxy' ;;
  *api.github.com*) printf '%s' '$github' ;;
  *) exit 22 ;;
esac
EOF
    chmod +x "$dir/bin/$name"
  done
}
for tool in curl wget; do
  d=$(world "tag-proxy-$tool" 24.04)
  stub_fetch "$d" '{"tag_name":"v1.2.3","name":"x"}' ''
  got=$(PATH="$d/bin:$PATH" DOWNLOAD_TOOL=$tool get_latest_version 2>"$d/stderr")
  [[ $got == v1.2.3 ]] || fail "tag-proxy-$tool: got '$got'"
  grep -q -- "https://svm.saltbox.dev/version?url=https://api.github.com/repos/saltyorg/sb-go/releases/latest" \
    "$d/fetches" || fail "tag-proxy-$tool: $(cat "$d/fetches")"
  d=$(world "tag-github-$tool" 24.04)
  stub_fetch "$d" '' '{ "tag_name": "v1.2.4", "name": "x" }'
  got=$(PATH="$d/bin:$PATH" DOWNLOAD_TOOL=$tool get_latest_version 2>"$d/stderr")
  [[ $got == v1.2.4 ]] || fail "tag-github-$tool: got '$got'"
  grep -Fq "SVM proxy unavailable" "$d/stderr" || fail "tag-github-$tool: $(cat "$d/stderr")"
  d=$(world "tag-invalid-$tool" 24.04)
  # shellcheck disable=SC2016 # a literal $(id): a tag that must never run
  stub_fetch "$d" '{"tag_name":"v1.2.3/../x"}' '{ "tag_name": "$(id)" }'
  status=0
  (PATH="$d/bin:$PATH" DOWNLOAD_TOOL=$tool get_latest_version) >"$d/stdout" 2>"$d/stderr" || status=$?
  [[ $status == 1 ]] || fail "tag-invalid-$tool: status $status, stdout $(cat "$d/stdout")"
  grep -Fq "Failed to fetch latest release version" "$d/stderr" || fail "tag-invalid-$tool: $(cat "$d/stderr")"
  # curl's https-only flags, on every fetch (wget's --https-only is
  # there too, though it covers the given URL only).
  if [[ $tool == curl ]]; then
    ! grep -v -- "--proto =https" "$d/fetches" | grep -q . || fail "tag-invalid-curl: $(cat "$d/fetches")"
  else
    ! grep -v -- "--https-only" "$d/fetches" | grep -q . || fail "tag-invalid-wget: $(cat "$d/fetches")"
  fi
done

# The download: GitHub's releases path for the tag and the asset only,
# https only, into the temporary directory; a failing tool is retried,
# then refused.
for tool in curl wget; do
  d=$(world "download-$tool" 24.04)
  mkdir -p "$d/bin" "$d/tmp"
  cat >"$d/bin/$tool" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >>'$d/fetches'
exit 22
EOF
  chmod +x "$d/bin/$tool"
  status=0
  (
    PATH="$d/bin:$PATH"
    DOWNLOAD_TOOL=$tool
    TEMP_DIR="$d/tmp"
    RETRY_DELAY=0
    download_binary v1.2.3
  ) >"$d/stdout" 2>"$d/stderr" || status=$?
  [[ $status == 1 ]] || fail "download-$tool: status $status"
  [[ $(wc -l <"$d/fetches") == 3 ]] || fail "download-$tool: tries $(cat "$d/fetches")"
  grep -Fq -- "https://github.com/saltyorg/sb-go/releases/download/v1.2.3/sb_linux_amd64" "$d/fetches" \
    || fail "download-$tool: $(cat "$d/fetches")"
  grep -Fq -- "$d/tmp/sb" "$d/fetches" || fail "download-$tool: not into the temp dir"
  grep -Fq "Failed to download binary after 3 attempts" "$d/stderr" || fail "download-$tool: $(cat "$d/stderr")"
done

# The downloaded file: too small, not an ELF executable, or failing to
# run is refused; nothing is installed.
d=$(world verify 24.04)
TEMP_DIR="$d"
printf 'tiny' >"$d/small"
status=0
(verify_binary "$d/small" v1.2.3) >/dev/null 2>"$d/stderr" || status=$?
if [[ $status != 1 ]] || ! grep -Fq "is too small" "$d/stderr"; then
  fail "verify small: $(cat "$d/stderr")"
fi
head -c 2000000 /dev/zero >"$d/zeros"
status=0
(verify_binary "$d/zeros" v1.2.3) >/dev/null 2>"$d/stderr" || status=$?
if [[ $status != 1 ]] || ! grep -Fq "not a valid Linux executable" "$d/stderr"; then
  fail "verify zeros: $(cat "$d/stderr")"
fi
# An ELF executable that isn't sb: bash, run as `bash version`, fails.
cp "$(command -v bash)" "$d/bash"
status=0
(verify_binary "$d/bash" v1.2.3) >/dev/null 2>"$d/stderr" || status=$?
[[ $status == 1 ]] || fail "verify bash: status $status; $(cat "$d/stderr")"
grep -Fq "Binary failed to execute" "$d/stderr" || fail "verify bash: $(cat "$d/stderr")"

# The version line: the same copy of bash run as `bash version` in a
# directory holding a `version` script, which bash runs, so the ELF says
# what each case needs. A warning on stderr (sb's about a bad theme
# config) doesn't spoil the line; another version or another line is
# refused.
version_case() {
  local name="$1" stdout="$2" tag="$3"
  local dir="$root/version-$name"
  mkdir -p "$dir"
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\necho "warning: theme config is broken" >&2\n' "$stdout" \
    >"$dir/version"
  status=0
  (cd "$dir" && TEMP_DIR="$dir" verify_binary "$d/bash" "$tag") >"$dir/out" 2>"$dir/err" || status=$?
  echo "$status"
}
[[ $(version_case good "Saltbox CLI version: 1.2.3 (commit: abc1234)" v1.2.3) == 0 ]] \
  || fail "verify good: $(cat "$root/version-good/err")"
[[ $(version_case bare-tag "Saltbox CLI version: 1.2.3 (commit: abc1234)" 1.2.3) == 0 ]] \
  || fail "verify bare tag: $(cat "$root/version-bare-tag/err")"
[[ $(version_case mismatch "Saltbox CLI version: 1.2.2 (commit: abc1234)" v1.2.3) == 1 ]] \
  || fail "verify mismatch passed"
grep -Fq "Version mismatch!" "$root/version-mismatch/err" || fail "verify mismatch: $(cat "$root/version-mismatch/err")"
[[ $(version_case format "sb 1.2.3" v1.2.3) == 1 ]] || fail "verify format passed"
grep -Fq "Version output format is incorrect!" "$root/version-format/err" \
  || fail "verify format: $(cat "$root/version-format/err")"
grep -Fq "warning: theme config is broken" "$root/version-format/err" \
  || fail "verify format: stderr not shown"
[[ $(version_case no-commit "Saltbox CLI version: 1.2.3 (commit: )" v1.2.3) == 1 ]] \
  || fail "verify a raw build passed"

# A download cut short under `curl ... | bash`: the brace group around the
# script makes every cut a syntax error, never a run that exits 0 with
# nothing done (so `&& cd /srv/git/saltbox` can't follow it). Any cut
# after the opening brace on line 2 (one inside the shebang line leaves
# bash a comment).
size=$(stat -c%s "$script")
for cut in 23 200 2000 $((size / 2)) $((size - 3)); do
  status=0
  head -c "$cut" "$script" | bash >/dev/null 2>&1 || status=$?
  [[ $status != 0 ]] || fail "a script cut at $cut of $size bytes exited 0"
done

echo "PASS: install.sh's release lists, root guard, options, setup call, tag, download and checks"
