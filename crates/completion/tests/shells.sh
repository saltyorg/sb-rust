#!/usr/bin/env bash
# The completion spec's container check, by hand (Testing strategy, "Real
# shells, in containers"; module build M57): sb's completion in real bash
# (with bash-completion), zsh and fish from apt, in a throwaway
# ubuntu:<release> container per release, one at a time. Each shell runs
# as a user's login shell in tmux with real Tab presses
# (shells-inside.sh says what is checked and how).
#
#   [SB_BIN=binary] [SBC_PREFIX=prefix] [SBC_DOCKER_OPTS=options] \
#     crates/completion/tests/shells.sh [RELEASE]...
#
# Default releases: 22.04 24.04 26.04. SB_BIN: the sb to check
# (target/debug/sb by default), a build whose glibc is no newer than
# 22.04's (2.35), such as the production build through cross in the
# oldest release's image (ADR-020; Cross.toml), in
# target/cross-gnu/x86_64-unknown-linux-gnu/production/sb.
#
# Before any release's suite, the newest glibc version SB_BIN needs (its
# dynamic symbols' `GLIBC_` versions, objdump -T) is checked against each
# release's own glibc (`getconf GNU_LIBC_VERSION` in its container). A
# binary built on a newer release is refused (exit 2), naming the release
# and both versions; otherwise it would fail in that release's container
# only, after its apt run, as "GLIBC_2.39 not found". A file objdump
# can't read dynamic symbols from is refused too. shells.test.sh tests
# both refusals.
#
# Each container, $SBC_PREFIX-<release>-<pid> (sbc-... by default), is
# removed on exit and touches no other container or image.
# Docker owns its cgroup, so a systemd scope around this script doesn't
# cap it: it gets `--cpus 2 --memory 2g` with no swap and a pids limit,
# and SBC_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus`
# to pin its cores. It needs the network for apt only; the host's trees
# are never mounted but for sb itself, read-only. Results go to
# target/completion-shells/<release>/, handed back to the caller by the
# container's root (a caller that isn't root, such as a CI runner, can
# then remove them, as the next run does); the last line per release is
# its result.
set -euo pipefail
cd "$(dirname "$0")/../../.."
bin=${SB_BIN:-target/debug/sb}
if [ ! -x "$bin" ]; then
  echo "shells.sh: no sb at $bin: build it (cargo build -p sb, on a host no newer than 22.04)" \
    "or set SB_BIN, such as to the production build (see this script's header)" >&2
  exit 2
fi
bin=$(realpath "$bin")
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "shells.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
here=$PWD/crates/completion/tests
owner=$(id -u):$(id -g)
container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# Paths under DIR not handed back to its caller: not its own, or a
# directory it can't write to, and find's errors on any it can't read.
# A root caller can remove anything, so only a caller that isn't root
# finds one.
not_removable() { # dir
  find "$1" \( ! -user "$(id -u)" -o -type d ! -writable \) -print 2>&1 || true
}
# The newest glibc version the binary's dynamic symbols need; a file
# objdump can't read them from is refused.
if ! symbols=$(objdump -T "$bin" 2>&1); then
  echo "shells.sh: can't read $bin's dynamic symbols (objdump -T): $symbols" >&2
  exit 2
fi
need=$(grep -o 'GLIBC_[0-9][0-9.]*' <<<"$symbols" | sed 's/^GLIBC_//' | sort -uV | tail -n 1) || true
for rel in "${releases[@]}"; do
  [ -n "$need" ] || break
  container=${SBC_PREFIX:-sbc}-$rel-$$
  # shellcheck disable=SC2086 # word splitting gives SBC_DOCKER_OPTS's options
  if ! have=$(docker run --rm --name "$container" --network none --cpus 2 --memory 2g \
    --memory-swap 2g --pids-limit 512 ${SBC_DOCKER_OPTS:-} \
    "ubuntu:$rel" getconf GNU_LIBC_VERSION); then
    echo "shells.sh: can't read ubuntu:$rel's glibc version" >&2
    exit 2
  fi
  container=""
  have=${have#glibc }
  if [ "$(printf '%s\n' "$need" "$have" | sort -V | tail -n 1)" != "$have" ]; then
    echo "shells.sh: $bin needs glibc $need, newer than ubuntu:$rel's $have, so it can't run there:" \
      "build it in the oldest release's image, as sb ships (ADR-020):" \
      "CARGO_TARGET_DIR=\$PWD/target/cross-gnu cross build --locked -p sb --profile production --target x86_64-unknown-linux-gnu," \
      "then SB_BIN=target/cross-gnu/x86_64-unknown-linux-gnu/production/sb" >&2
    exit 2
  fi
done
status=0
for rel in "${releases[@]}"; do
  out=$PWD/target/completion-shells/$rel
  rm -rf "$out"
  mkdir -p "$out"
  container=${SBC_PREFIX:-sbc}-$rel-$$
  # shellcheck disable=SC2086 # word splitting gives SBC_DOCKER_OPTS's options
  docker run --rm --init --name "$container" --cpus 2 --memory 2g --memory-swap 2g \
    --pids-limit 512 \
    -v "$bin:/usr/local/bin/sb:ro" \
    -v "$here/shells-inside.sh:/check/inside.sh:ro" \
    -v "$here/fixtures/sb-go:/check/sb-go:ro" \
    -v "$out:/out" \
    ${SBC_DOCKER_OPTS:-} \
    "ubuntu:$rel" bash -c 'bash /check/inside.sh "$1"; s=$?; chown -R "$2" /out || s=1; exit "$s"' \
    _ "$rel" "$owner" >"$out/run.log" 2>&1 || status=1
  container=""
  left=$(not_removable "$out")
  [ -z "$left" ] || { printf 'shells.sh: %s: not removable by %s:\n%s\n' "$rel" "$owner" "$left"; status=1; }
  grep -E '^(PASS|FAIL|NOTE):' "$out/run.log" || true
  tail -n 1 "$out/run.log"
done
exit "$status"
