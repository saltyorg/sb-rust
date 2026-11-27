#!/usr/bin/env bash
# shells.sh's glibc check: a binary that needs a newer glibc than a
# release's own is refused before any release's suite runs, exit 2,
# naming the release and both versions; a file that isn't an ELF binary
# is refused too. The binary is a C program built here that needs an
# invented symbol at version GLIBC_2.36, one past 22.04's 2.35, from a
# stub library built with it (a linker version script); it is never run.
# The release workflow runs this before shells.sh.
#
#   [SBC_PREFIX=prefix] [SBC_DOCKER_OPTS=options] \
#     crates/completion/tests/shells.test.sh
#
# Needs gcc, objdump and Docker (shells.sh asks each release's container
# for its glibc version, pulling ubuntu:22.04 and ubuntu:24.04 when
# missing). Its last line is `PASS: ...`.
set -euo pipefail
shopt -s inherit_errexit

shells="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/shells.sh"
fixture=$(mktemp -d)
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The stub library defines the symbol at GLIBC_2.36; the program linked
# against it needs that version, as objdump -T shows it.
printf 'void sbc_needs_new_glibc(void) {}\n' >"$fixture/stub.c"
printf 'GLIBC_2.36 { global: sbc_needs_new_glibc; local: *; };\n' >"$fixture/stub.map"
printf 'void sbc_needs_new_glibc(void);\nint main(void) { sbc_needs_new_glibc(); return 0; }\n' >"$fixture/main.c"
gcc -shared -fPIC -Wl,--version-script="$fixture/stub.map" -Wl,-soname,libsbcstub.so \
  -o "$fixture/libsbcstub.so" "$fixture/stub.c"
gcc -o "$fixture/needs-2.36" "$fixture/main.c" -L"$fixture" -lsbcstub
objdump -T "$fixture/needs-2.36" | grep -q '(GLIBC_2\.36) sbc_needs_new_glibc' ||
  fail "the built program doesn't need GLIBC_2.36"

# $1 the binary, the rest shells.sh's releases: shells.sh's stdout,
# stderr and exit status in out, err and status.
run() {
  local bin=$1
  shift
  status=0
  SB_BIN=$bin "$shells" "$@" >"$fixture/out" 2>"$fixture/err" || status=$?
  out=$(<"$fixture/out")
  err=$(<"$fixture/err")
}

# Refused at 22.04, 24.04's 2.39 being new enough: listed second, 22.04
# is still refused before 24.04's suite (which prints its result line)
# runs.
for releases in "22.04" "24.04 22.04"; do
  # shellcheck disable=SC2086 # word splitting gives the releases
  run "$fixture/needs-2.36" $releases
  [[ $status == 2 ]] || fail "shells.sh $releases: exit $status, not 2; stderr: $err"
  [[ $err == *"needs glibc 2.36, newer than ubuntu:22.04's 2.35"* ]] ||
    fail "shells.sh $releases: stderr doesn't name 22.04 and both versions: $err"
  [[ -z $out ]] || fail "shells.sh $releases: a suite ran before the refusal: $out"
done

# Not an ELF binary: objdump can't read it, so nothing is known about it.
printf '#!/bin/sh\nexit 0\n' >"$fixture/script"
chmod +x "$fixture/script"
run "$fixture/script" 22.04
[[ $status == 2 ]] || fail "a script: exit $status, not 2; stderr: $err"
[[ $err == *"can't read $(realpath "$fixture/script")'s dynamic symbols"* ]] ||
  fail "a script: stderr doesn't say its symbols can't be read: $err"
[[ -z $out ]] || fail "a script: a suite ran: $out"

echo "PASS: shells.sh refuses a binary that needs a newer glibc than a release's, before any suite, and a non-ELF file"
