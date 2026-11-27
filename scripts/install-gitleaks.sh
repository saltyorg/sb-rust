#!/usr/bin/env bash
# Installs gitleaks (CONSTRAINTS.md's Tools table), the secrets step's
# scanner, from its GitHub release's prebuilt binary, as the CI
# rehearsal's provisioning installs Node and its root filesystems
# (scripts/ci-rehearsal/provision.sh, rootfs.sh):
# 1. takes this machine's pin (x64 or arm64, from `uname -m`) below: the
#    release's tag and its archive's SHA-256, refusing a pin of another
#    version or one without a SHA-256 (64 lower-case hex digits);
# 2. downloads the release's linux archive for this machine and checks it
#    with `sha256sum --check` against that pin, refusing a mismatch;
# 3. takes only the `gitleaks` program from the archive, a regular file,
#    and runs its `gitleaks version` beside BIN_DIR's;
# 4. renames it into BIN_DIR, replacing what is there in one step (a
#    link too, its target left as it is), and prints its version.
# Nothing reaches BIN_DIR unless every check passed.
#
# The pins, not the release's checksums file: gitleaks' releases are
# mutable and carry no attestations, so whoever can change a release's
# archive can change its checksums file too. A pinned SHA-256 holds the
# archive to the one reviewed when its pin was committed (the maintainer,
# 2026-11-27). The checksums file isn't read: once the archive matches
# its pin, the file proves nothing more, and a changed one would only
# refuse an archive that matches.
#
# Renovate updates GITLEAKS_VERSION and both pins in one PR, held for the
# maintainer's approval on the dependency dashboard (.github/renovate.json):
# its github-release-attachments datasource finds each pinned SHA-256 in
# the current release's checksums file and takes the same archive's line
# from the new release's
# (https://github.com/renovatebot/renovate/blob/45.1.0/lib/modules/datasource/github-release-attachments/index.ts).
# Each pin carries the release's tag because that lookup reads the
# release by its tag. Before merging that PR, compare each new pin with
# its asset's `digest` in GitHub's release API
# (`gh api repos/gitleaks/gitleaks/releases/tags/<tag>`), and refuse an
# asset updated after the release was published; a PR that moves a pin's
# tag but not its SHA-256 found no pinned archive in the current release,
# and isn't merged.
# https://github.com/gitleaks/gitleaks/releases
#
# GITLEAKS_BASE_URL, a file:// URL, replaces the release's download URL
# (its tests serve an invented release from one, with copies of this
# script whose pins name it); without it, only https is fetched.
# Redirects are followed to https only.
#
# Usage: scripts/install-gitleaks.sh BIN_DIR
set -euo pipefail

GITLEAKS_VERSION=8.30.1
# '<tag> <SHA-256>' of gitleaks_<version>_linux_<arch>.tar.gz.
GITLEAKS_PIN_LINUX_X64='v8.30.1 551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb'
GITLEAKS_PIN_LINUX_ARM64='v8.30.1 e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080'

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
x86_64) arch=x64 pin_var=GITLEAKS_PIN_LINUX_X64 pin=${GITLEAKS_PIN_LINUX_X64:-} ;;
aarch64 | arm64) arch=arm64 pin_var=GITLEAKS_PIN_LINUX_ARM64 pin=${GITLEAKS_PIN_LINUX_ARM64:-} ;;
*)
  echo "install-gitleaks: no gitleaks release archive for $(uname -m)" >&2
  exit 1
  ;;
esac
archive=gitleaks_${GITLEAKS_VERSION}_linux_$arch.tar.gz
read -r pin_tag sha256 pin_rest <<<"$pin"
if [[ ! $sha256 =~ ^[0-9a-f]{64}$ || -n $pin_rest ]]; then
  echo "install-gitleaks: no pinned SHA-256 for $archive in $pin_var" >&2
  exit 1
fi
if [[ $pin_tag != "v$GITLEAKS_VERSION" ]]; then
  echo "install-gitleaks: $pin_var pins $pin_tag, not v$GITLEAKS_VERSION" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
curl -fsSL --retry 3 --connect-timeout 30 --max-time 300 --proto "$protocols" --proto-redir '=https' \
  -o "$work/$archive" "$base/$archive" || {
  echo "install-gitleaks: could not download $base/$archive" >&2
  exit 1
}
(cd "$work" && sha256sum --check --strict --quiet <<<"$sha256  $archive") || {
  echo "install-gitleaks: $archive doesn't match its pinned SHA-256 ($pin_var)" >&2
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
