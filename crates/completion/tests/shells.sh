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
# 22.04's (2.35). Each container, $SBC_PREFIX-<release>-<pid> (sbc-... by
# default), is removed on exit and touches no other container or image.
# Docker owns its cgroup, so a systemd scope around this script doesn't
# cap it: it gets `--cpus 2 --memory 2g` with no swap and a pids limit,
# and SBC_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus`
# to pin its cores. It needs the network for apt only; the host's trees
# are never mounted but for sb itself, read-only. Results go to
# target/completion-shells/<release>/; the last line per release is its
# result.
set -euo pipefail
cd "$(dirname "$0")/../../.."
bin=${SB_BIN:-target/debug/sb}
if [ ! -x "$bin" ]; then
  echo "shells.sh: no sb at $bin: build it (cargo build -p sb) or set SB_BIN" >&2
  exit 2
fi
bin=$(realpath "$bin")
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "shells.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
here=$PWD/crates/completion/tests
container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
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
    -v "$out:/out" \
    ${SBC_DOCKER_OPTS:-} \
    "ubuntu:$rel" bash /check/inside.sh "$rel" >"$out/run.log" 2>&1 || status=1
  container=""
  grep -E '^(PASS|FAIL|NOTE):' "$out/run.log" || true
  tail -n 1 "$out/run.log"
done
exit "$status"
