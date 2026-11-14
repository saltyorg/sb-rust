#!/usr/bin/env bash
# The ansible spec's container check, by hand (Testing strategy, "By hand,
# capped, in a container"; module build M35): sb against a real Ansible.
# A throwaway ubuntu:24.04 installs ansible-core from pip, then
# fixtures/container/inside.sh runs sb in it against stand-in Saltbox and
# Sandbox checkouts (fixtures/container/): every `-e` case of the spec
# reaches the play whole, Ctrl-C on a terminal stops the play and sb
# passes Ansible's 99 through, and the real `--list-tags` output is
# recorded.
#
#   [SB_BIN=binary] [ANSIBLE_CORE=version] [SBA_PREFIX=prefix] \
#     [SBA_DOCKER_OPTS=options] crates/ansible/tests/container.sh
#
# SB_BIN: the sb to check (target/debug/sb by default), a build whose
# glibc is no newer than 24.04's (2.39). ANSIBLE_CORE: the pip version
# (2.21.3 by default). The container, $SBA_PREFIX-ansible-check-<pid>
# (sba-... by default), is removed on exit, and touches no other
# container or image. Docker caps it at 2 CPUs and 4 GiB, as the journal
# scripts do; a systemd scope around this script doesn't reach it, so
# pin its cores and weight with SBA_DOCKER_OPTS (`--cpuset-cpus`,
# `--cpu-shares`). It needs the network for apt and pip, and nothing else
# outside it. SBA_DOCKER_OPTS adds `docker run` options: a dry run of the
# script itself offline mounts a stand-in at
# /usr/local/bin/ansible-playbook and adds `--network none`.
#
# Results are in target/ansible-container/. After a run with a real
# ansible-core where every check passed, the listing is copied to
# fixtures/list-tags-real.txt.
set -euo pipefail
cd "$(dirname "$0")/../../.."
bin=${SB_BIN:-target/debug/sb}
if [ ! -x "$bin" ]; then
  echo "container.sh: no sb at $bin: build it (cargo build -p sb) or set SB_BIN" >&2
  exit 2
fi
bin=$(realpath "$bin")
name=${SBA_PREFIX:-sba}-ansible-check-$$
fixtures=$PWD/crates/ansible/tests/fixtures
out=$PWD/target/ansible-container
rm -rf "$out"
mkdir -p "$out"
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# shellcheck disable=SC2086 # word splitting gives SBA_DOCKER_OPTS's options
docker run --rm --init --name "$name" --memory 4g --cpus 2 \
  -e ANSIBLE_CORE="${ANSIBLE_CORE:-2.21.3}" \
  -v "$bin:/usr/local/bin/sb:ro" \
  -v "$fixtures/container:/check:ro" \
  -v "$PWD/crates/sb/tests/fixtures/detect-virt/none:/detect-virt:ro" \
  -v "$out:/out" \
  ${SBA_DOCKER_OPTS:-} \
  ubuntu:24.04 bash /check/inside.sh
echo "container.sh: every check passed; versions:"
cat "$out/versions.txt"
# A stand-in's listing (a dry run) never replaces the fixture.
if grep -q '^ansible-playbook \[core 2\.' "$out/versions.txt"; then
  cp "$out/list-tags.txt" "$fixtures/list-tags-real.txt"
  echo "container.sh: the listing is in $fixtures/list-tags-real.txt"
fi
