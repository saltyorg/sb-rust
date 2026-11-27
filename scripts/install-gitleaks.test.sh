#!/usr/bin/env bash
# scripts/install-gitleaks.sh against an invented release served from a
# file:// URL (GITLEAKS_BASE_URL), each run by a copy of the installer
# whose pin for this machine's archive names the invented archive's
# SHA-256 (the installer has no way to change its pins from outside): the
# pinned archive installs its program; an archive that doesn't match its
# pin (also when the release's own checksums file lists it), a missing or
# malformed pin, a pin of another release, a missing archive, an archive
# without gitleaks and one whose gitleaks is a link each refuse, saying
# why, and leave BIN_DIR without gitleaks. A link already at
# BIN_DIR/gitleaks is replaced, its target kept. The installer's own pins
# are each a tag and a SHA-256, at its version. No network. check.sh runs
# it in its fast stage, in the gitleaks installer's tests step.
#
#   scripts/install-gitleaks.test.sh
#
# Its last line is `PASS: ...`.
set -euo pipefail
shopt -s inherit_errexit

script="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/install-gitleaks.sh"
version=$(sed -n 's/^GITLEAKS_VERSION=//p' "$script")
fixture=$(mktemp -d)
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

case $(uname -m) in
x86_64) arch=x64 ;;
aarch64 | arm64) arch=arm64 ;;
*) fail "no release archive for $(uname -m)" ;;
esac
archive=gitleaks_${version}_linux_$arch.tar.gz
sums=gitleaks_${version}_checksums.txt
pin_var=GITLEAKS_PIN_LINUX_${arch^^}

# The installer's own pins: one line each for the two archives it
# installs, its release's tag and a SHA-256 (64 lower-case hex digits).
for a in X64 ARM64; do
  [[ $(grep -c "^GITLEAKS_PIN_LINUX_$a=" "$script") == 1 ]] || fail "pins: not one GITLEAKS_PIN_LINUX_$a"
  grep -qE "^GITLEAKS_PIN_LINUX_$a='v$version [0-9a-f]{64}'\$" "$script" ||
    fail "pins: GITLEAKS_PIN_LINUX_$a isn't v$version and a SHA-256"
done

# pack NAME: NAME's archive from $fixture/NAME.content, each member by
# its name, with no ./ prefix, as the release's archives hold them (the
# installer asks for the member `gitleaks`), and the release's checksums
# file listing it and another archive, as the real release's does.
pack() {
  local dir=$fixture/$1 members=()
  mapfile -t members < <(cd "$fixture/$1.content" && ls -A)
  tar -czf "$dir/$archive" -C "$fixture/$1.content" "${members[@]}"
  (cd "$dir" && sha256sum "$archive" >"$sums")
  echo "0000000000000000000000000000000000000000000000000000000000000000  gitleaks_${version}_darwin_x64.tar.gz" >>"$dir/$sums"
}

# release NAME [MEMBER]: an invented release in $fixture/NAME, its archive
# holding MEMBER (by default a stand-in gitleaks that prints the version)
# and a README.
release() {
  local content=$fixture/$1.content
  mkdir -p "$fixture/$1" "$content"
  case ${2:-program} in
  program) printf '%s\n' '#!/bin/sh' "[ \"\$1\" = version ] && echo $version" >"$content/gitleaks" ;;
  link) ln -s /bin/true "$content/gitleaks" ;;
  none) ;;
  esac
  [[ ! -e $content/gitleaks || -L $content/gitleaks ]] || chmod 0755 "$content/gitleaks"
  echo "an invented release" >"$content/README.md"
  pack "$1"
}

# pinned NAME [PIN]: $fixture/NAME.sh, the installer with this machine's
# pin set to PIN, by default the tag and release NAME's archive's
# SHA-256, as the release's own pin would be.
pinned() {
  local pin=${2-v$version $(sha256sum <"$fixture/$1/$archive" | cut -d' ' -f1)}
  sed "s/^$pin_var=.*/$pin_var='$pin'/" "$script" >"$fixture/$1.sh"
  grep -qxF "$pin_var='$pin'" "$fixture/$1.sh" || fail "$1: no $pin_var to set"
  chmod 0755 "$fixture/$1.sh"
}

# run_installer NAME BIN [INSTALLER]: INSTALLER (by default $fixture/NAME.sh)
# against release NAME into BIN; its status, output in $out.
run_installer() {
  local status=0
  out=$(GITLEAKS_BASE_URL="file://$fixture/$1" "${3:-$fixture/$1.sh}" "$2" 2>&1) || status=$?
  return "$status"
}

# refused NAME MESSAGE: the installer refuses release NAME, saying
# MESSAGE, and BIN_DIR has no gitleaks.
refused() {
  local bin=$fixture/bin-$1
  if run_installer "$1" "$bin"; then fail "$1: installed: $out"; fi
  grep -qF -- "$2" <<<"$out" || fail "$1: no '$2' in: $out"
  [[ ! -e $bin/gitleaks && ! -L $bin/gitleaks ]] || fail "$1: left $bin/gitleaks"
}

# The pinned archive: installed, 0755, and its version printed.
release good
pinned good
run_installer good "$fixture/bin" || fail "good: refused: $out"
[[ $out == "$version" ]] || fail "good: printed '$out'"
[[ -f $fixture/bin/gitleaks && ! -L $fixture/bin/gitleaks ]] || fail "good: no program in BIN_DIR"
[[ $(stat -c %a "$fixture/bin/gitleaks") == 755 ]] || fail "good: mode $(stat -c %a "$fixture/bin/gitleaks")"
[[ $("$fixture/bin/gitleaks" version) == "$version" ]] || fail "good: the installed program"

# The release's archive replaced after its pin was taken, and its
# checksums file with it: the archive matches the release's own list,
# not the pin, and is refused.
release swapped
pinned swapped
printf '%s\n' '#!/bin/sh' "[ \"\$1\" = version ] && echo $version" 'echo another build' >"$fixture/swapped.content/gitleaks"
pack swapped
(cd "$fixture/swapped" && sha256sum --check --strict --quiet --ignore-missing "$sums") || fail "swapped: the checksums file doesn't list the new archive"
refused swapped "$archive doesn't match its pinned SHA-256"

# A pin that doesn't match the archive.
release wrong
pinned wrong "v$version $(printf '%064d' 1)"
refused wrong "$archive doesn't match its pinned SHA-256"

# The archive's own SHA-256 in upper case: no well-formed pin.
release upper
pinned upper "v$version $(sha256sum <"$fixture/upper/$archive" | cut -d' ' -f1 | tr a-f A-F)"
refused upper "no pinned SHA-256 for $archive"

# No SHA-256 in the pin, a field after it, an empty pin, and no pin.
release nohash
pinned nohash "v$version"
refused nohash "no pinned SHA-256 for $archive"
release trailing
pinned trailing "v$version $(sha256sum <"$fixture/trailing/$archive" | cut -d' ' -f1) x"
refused trailing "no pinned SHA-256 for $archive"
release empty
pinned empty ""
refused empty "no pinned SHA-256 for $archive"
release nopin
pinned nopin
sed -i "/^$pin_var=/d" "$fixture/nopin.sh"
refused nopin "no pinned SHA-256 for $archive"

# The pin of another release (a version bumped without its pins).
release stale
pinned stale "v0.0.1 $(sha256sum <"$fixture/stale/$archive" | cut -d' ' -f1)"
refused stale "$pin_var pins v0.0.1, not v$version"

# No archive in the release.
release noarchive
pinned noarchive
rm "$fixture/noarchive/$archive"
refused noarchive "could not download file://$fixture/noarchive/$archive"

# A release URL that isn't file://: refused before any download.
bin=$fixture/bin-https
status=0
out=$(GITLEAKS_BASE_URL="https://127.0.0.1:9/none" "$fixture/good.sh" "$bin" 2>&1) || status=$?
((status != 0)) || fail "https override: installed: $out"
grep -qF "GITLEAKS_BASE_URL must be a file:// URL" <<<"$out" || fail "https override: $out"
[[ ! -e $bin/gitleaks ]] || fail "https override: left $bin/gitleaks"

# A link at BIN_DIR/gitleaks (a `go install` build's, linked onto PATH):
# replaced by the program, its target untouched.
mkdir -p "$fixture/linked" "$fixture/target"
echo "the old build" >"$fixture/target/gitleaks"
ln -s "$fixture/target/gitleaks" "$fixture/linked/gitleaks"
run_installer good "$fixture/linked" || fail "linked: refused: $out"
[[ ! -L $fixture/linked/gitleaks && -f $fixture/linked/gitleaks ]] || fail "linked: the link is still there"
[[ $(cat "$fixture/target/gitleaks") == "the old build" ]] || fail "linked: the link's target changed"

# A link to a directory at BIN_DIR/gitleaks: replaced by the program,
# nothing written into the directory it names.
mkdir -p "$fixture/dirlink" "$fixture/elsewhere"
ln -s "$fixture/elsewhere" "$fixture/dirlink/gitleaks"
run_installer good "$fixture/dirlink" || fail "dirlink: refused: $out"
[[ ! -L $fixture/dirlink/gitleaks && -f $fixture/dirlink/gitleaks ]] || fail "dirlink: the link is still there"
[[ -z $(ls -A "$fixture/elsewhere") ]] || fail "dirlink: wrote into the link's directory"

# A refused release leaves a link already there, and its target, as they
# were, and nothing beside them.
mkdir -p "$fixture/kept"
ln -s "$fixture/target/gitleaks" "$fixture/kept/gitleaks"
if run_installer wrong "$fixture/kept"; then fail "kept: installed: $out"; fi
[[ $(readlink "$fixture/kept/gitleaks") == "$fixture/target/gitleaks" ]] || fail "kept: the link changed"
[[ $(cat "$fixture/target/gitleaks") == "the old build" ]] || fail "kept: the link's target changed"
[[ $(ls -A "$fixture/kept") == gitleaks ]] || fail "kept: left $(ls -A "$fixture/kept")"

# A program that doesn't run: refused, and BIN_DIR without it.
release broken
printf '#!/bin/sh\nexit 1\n' >"$fixture/broken.content/gitleaks"
pack broken
pinned broken
refused broken "$archive's gitleaks doesn't run"
[[ -z $(ls -A "$fixture/bin-broken") ]] || fail "broken: left $(ls -A "$fixture/bin-broken")"

# An archive without gitleaks, and one whose gitleaks is a link.
release nomember none
pinned nomember
refused nomember "$archive holds no gitleaks"
release linkonly link
pinned linkonly
refused linkonly "$archive's gitleaks isn't a regular file"

# The installer itself, with its own pins, refuses the invented archive.
release real
if run_installer real "$fixture/bin-real" "$script"; then fail "real: installed: $out"; fi
grep -qF "$archive doesn't match its pinned SHA-256" <<<"$out" || fail "real: $out"

echo "PASS: install-gitleaks.sh installs the archive its pin names and refuses each broken release"
