#!/usr/bin/env bash
# scripts/install-gitleaks.sh against an invented release served from a
# file:// URL (GITLEAKS_BASE_URL): the right SHA-256 installs its
# program; a wrong one, a missing line, a line twice, a missing checksums
# file, an archive without gitleaks and one whose gitleaks is a link each
# refuse, saying why, and leave BIN_DIR without gitleaks. A link already
# at BIN_DIR/gitleaks is replaced, its target kept. No network. check.sh
# runs it in its fast stage, in the gitleaks installer's tests step.
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

# release NAME [MEMBER]: an invented release in $fixture/NAME, its archive
# holding MEMBER (by default a stand-in gitleaks that prints the version)
# and a README, and its checksums file listing it and another archive.
release() {
  local dir=$fixture/$1 content=$fixture/$1.content
  mkdir -p "$dir" "$content"
  case ${2:-program} in
  program) printf '%s\n' '#!/bin/sh' "[ \"\$1\" = version ] && echo $version" >"$content/gitleaks" ;;
  link) ln -s /bin/true "$content/gitleaks" ;;
  none) ;;
  esac
  [[ ! -e $content/gitleaks || -L $content/gitleaks ]] || chmod 0755 "$content/gitleaks"
  echo "an invented release" >"$content/README.md"
  # Each member by its name, with no ./ prefix, as the release's archives
  # hold them: the installer asks for the member `gitleaks`.
  local members=()
  mapfile -t members < <(cd "$content" && ls -A)
  tar -czf "$dir/$archive" -C "$content" "${members[@]}"
  (cd "$dir" && sha256sum "$archive" >"$sums")
  echo "0000000000000000000000000000000000000000000000000000000000000000  gitleaks_${version}_darwin_x64.tar.gz" >>"$dir/$sums"
}

# run_installer NAME BIN: the installer against release NAME into BIN;
# its status, output in $out.
run_installer() {
  local status=0
  out=$(GITLEAKS_BASE_URL="file://$fixture/$1" "$script" "$2" 2>&1) || status=$?
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

# The right SHA-256: installed, 0755, and its version printed.
release good
run_installer good "$fixture/bin" || fail "good: refused: $out"
[[ $out == "$version" ]] || fail "good: printed '$out'"
[[ -f $fixture/bin/gitleaks && ! -L $fixture/bin/gitleaks ]] || fail "good: no program in BIN_DIR"
[[ $(stat -c %a "$fixture/bin/gitleaks") == 755 ]] || fail "good: mode $(stat -c %a "$fixture/bin/gitleaks")"
[[ $("$fixture/bin/gitleaks" version) == "$version" ]] || fail "good: the installed program"

# The same under mawk, Ubuntu 22.04's default awk, when the host has it:
# the installer's awk is the first on PATH.
if command -v mawk >/dev/null; then
  mkdir -p "$fixture/mawk"
  ln -s "$(command -v mawk)" "$fixture/mawk/awk"
  status=0
  out=$(PATH="$fixture/mawk:$PATH" GITLEAKS_BASE_URL="file://$fixture/good" "$script" "$fixture/bin-mawk" 2>&1) || status=$?
  if ((status != 0)) || [[ $out != "$version" ]]; then fail "mawk: $out"; fi
fi

# A link at BIN_DIR/gitleaks (a `go install` build's, linked onto PATH):
# replaced by the program, its target untouched.
mkdir -p "$fixture/linked" "$fixture/target"
echo "the old build" >"$fixture/target/gitleaks"
ln -s "$fixture/target/gitleaks" "$fixture/linked/gitleaks"
run_installer good "$fixture/linked" || fail "linked: refused: $out"
[[ ! -L $fixture/linked/gitleaks && -f $fixture/linked/gitleaks ]] || fail "linked: the link is still there"
[[ $(cat "$fixture/target/gitleaks") == "the old build" ]] || fail "linked: the link's target changed"

# A wrong SHA-256.
release wrong
sed -i "s/^[0-9a-f]\{64\}  $archive\$/$(printf '%064d' 1)  $archive/" "$fixture/wrong/$sums"
refused wrong "doesn't match its SHA-256 in $sums"

# A malformed line for the archive (upper-case hex): no line for it.
release upper
sed -i "s/^\([0-9a-f]\{64\}\)  $archive\$/\U\1\E  $archive/" "$fixture/upper/$sums"
grep -q "^[0-9A-F]\{64\}  $archive\$" "$fixture/upper/$sums" || fail "upper: the line wasn't made upper-case"
refused upper "$sums lists no $archive"

# No line for the archive.
release missing
sed -i "/  $archive\$/d" "$fixture/missing/$sums"
refused missing "$sums lists no $archive"

# The archive's line twice.
release twice
line=$(sed -n "/  $archive\$/p" "$fixture/twice/$sums")
echo "$line" >>"$fixture/twice/$sums"
refused twice "$sums lists $archive more than once"

# No checksums file at all.
release unlisted
rm "$fixture/unlisted/$sums"
refused unlisted "could not download file://$fixture/unlisted/$sums"

# A release URL that isn't file://: refused before any download.
release https
bin=$fixture/bin-https
status=0
out=$(GITLEAKS_BASE_URL="https://127.0.0.1:9/none" "$script" "$bin" 2>&1) || status=$?
((status != 0)) || fail "https override: installed: $out"
grep -qF "GITLEAKS_BASE_URL must be a file:// URL" <<<"$out" || fail "https override: $out"
[[ ! -e $bin/gitleaks ]] || fail "https override: left $bin/gitleaks"

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
members=()
mapfile -t members < <(cd "$fixture/broken.content" && ls -A)
tar -czf "$fixture/broken/$archive" -C "$fixture/broken.content" "${members[@]}"
(cd "$fixture/broken" && sha256sum "$archive" >"$sums")
refused broken "$archive's gitleaks doesn't run"
[[ -z $(ls -A "$fixture/bin-broken") ]] || fail "broken: left $(ls -A "$fixture/bin-broken")"

# An archive without gitleaks, and one whose gitleaks is a link.
release empty none
refused empty "$archive holds no gitleaks"
release linkonly link
refused linkonly "$archive's gitleaks isn't a regular file"

echo "PASS: install-gitleaks.sh installs a listed archive's gitleaks and refuses each broken release"
