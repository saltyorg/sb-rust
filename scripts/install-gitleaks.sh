#!/usr/bin/env bash
# Installs gitleaks (CONSTRAINTS.md's Tools table), the secrets step's
# scanner, from its GitHub release's prebuilt binary, as the CI
# rehearsal's provisioning installs Node and its root filesystems
# (scripts/ci-rehearsal/provision.sh, rootfs.sh):
# 1. downloads the release's linux archive for this machine (x64 or
#    arm64, from `uname -m`) and its checksums file,
#    gitleaks_<version>_checksums.txt, from the same release;
# 2. checks the archive with `sha256sum --check` against its one line in
#    that file, refusing a missing line, more than one, or a mismatch;
# 3. takes only the `gitleaks` program from the archive, a regular file,
#    and runs its `gitleaks version` beside BIN_DIR's;
# 4. renames it into BIN_DIR, replacing what is there in one step (a
#    link too, its target left as it is), and prints its version.
# Nothing reaches BIN_DIR unless every check passed. The checksums file
# proves the archive is the one its release lists, not who published it
# (the test-hardening plan's choice 7). Renovate bumps GITLEAKS_VERSION
# (.github/renovate.json); the checksums come with each release.
# https://github.com/gitleaks/gitleaks/releases
#
# GITLEAKS_BASE_URL, a file:// URL, replaces the release's download URL
# (its tests serve an invented release from one); without it, only https
# is fetched. Redirects are followed to https only.
#
# Usage: scripts/install-gitleaks.sh BIN_DIR
set -euo pipefail

GITLEAKS_VERSION=8.30.1

bin=$(realpath --canonicalize-missing -- "${1:?usage: install-gitleaks.sh BIN_DIR}")
base=${GITLEAKS_BASE_URL:-https://github.com/gitleaks/gitleaks/releases/download/v$GITLEAKS_VERSION}
protocols='=https'
if [[ -n ${GITLEAKS_BASE_URL:-} ]]; then
  [[ $GITLEAKS_BASE_URL == file://* ]] || {
    echo "install-gitleaks: GITLEAKS_BASE_URL must be a file:// URL" >&2
    exit 1
  }
  protocols='=file'
fi

case $(uname -m) in
x86_64) arch=x64 ;;
aarch64 | arm64) arch=arm64 ;;
*)
  echo "install-gitleaks: no gitleaks release archive for $(uname -m)" >&2
  exit 1
  ;;
esac
archive=gitleaks_${GITLEAKS_VERSION}_linux_$arch.tar.gz
sums=gitleaks_${GITLEAKS_VERSION}_checksums.txt

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
fetch() {
  curl -fsSL --retry 3 --connect-timeout 30 --max-time 300 --proto "$protocols" --proto-redir '=https' \
    -o "$work/$1" "$base/$1" || {
    echo "install-gitleaks: could not download $base/$1" >&2
    exit 1
  }
}
fetch "$sums"
# The archive's line: its SHA-256 (64 lower-case hex digits) and its
# name, the line's only two fields, exactly once.
# No interval expression (`{64}`): Ubuntu 22.04's mawk 1.3.4 20200120,
# its default awk, reads one as literal text.
lines=$(awk -v f="$archive" 'NF == 2 && $2 == f && length($1) == 64 && $1 !~ /[^0-9a-f]/' "$work/$sums")
count=$(printf '%s' "$lines" | awk 'END { print NR }')
case $count in
1) ;;
0)
  echo "install-gitleaks: $sums lists no $archive" >&2
  exit 1
  ;;
*)
  echo "install-gitleaks: $sums lists $archive more than once" >&2
  exit 1
  ;;
esac
fetch "$archive"
(cd "$work" && sha256sum --check --strict --quiet <<<"$lines") || {
  echo "install-gitleaks: $archive doesn't match its SHA-256 in $sums" >&2
  exit 1
}
mkdir "$work/x"
tar -xzf "$work/$archive" -C "$work/x" --no-same-owner --no-same-permissions gitleaks || {
  echo "install-gitleaks: $archive holds no gitleaks" >&2
  exit 1
}
if [[ -L $work/x/gitleaks || ! -f $work/x/gitleaks ]]; then
  echo "install-gitleaks: $archive's gitleaks isn't a regular file" >&2
  exit 1
fi
mkdir -p -- "$bin"
# Beside the old one, as a file of its own (-T: never into a directory a
# link there names), run once, then renamed over it: rename(2) replaces a
# link, not its target, and an interrupted install leaves the old one.
install -T -m 0755 -- "$work/x/gitleaks" "$bin/.gitleaks.new"
"$bin/.gitleaks.new" version >/dev/null || {
  rm -f -- "$bin/.gitleaks.new"
  echo "install-gitleaks: $archive's gitleaks doesn't run" >&2
  exit 1
}
mv -fT -- "$bin/.gitleaks.new" "$bin/gitleaks"
"$bin/gitleaks" version
