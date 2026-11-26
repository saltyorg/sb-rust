#!/usr/bin/env bash
# sb's hangup test of a session scope's stop (crates/sb/tests/hangup.rs,
# `a_session_scope_stop_takes_the_grace`) on a systemd of its own: the one
# test that needs systemd to start and stop a scope, which the gate's
# sandbox hides (its /run is empty; scripts/check.sh, sandbox_argv), so the
# gate's test runs skip it (SB_TEST_NO_SYSTEMD=1) and it runs here (from
# check.sh full, when a file under crates/ or scripts/ changed, or by
# hand), as the container matrices do.
#
#   [SBH_PREFIX=name] [SBH_IMAGE=image] [SBH_DOCKER_OPTS=options] \
#     crates/sb/tests/hangup-systemd.sh
#
# The test binary and sb are built on the host (a debug build, as the
# gate's, into the repository's target/), then run in a throwaway
# container, $SBH_PREFIX-<pid> (sbhangup-... by default), of SBH_IMAGE
# (ubuntu:24.04 pinned by digest by default), with systemd as its PID 1.
# No added capability and Docker's own AppArmor profile: a cgroup namespace
# of its own, so the cgroup tree it sees is its own subtree, never the
# host's; that tree, which Docker mounts read-only and systemd must write
# (its init.scope), is remounted read-write from the host, into the
# container's mount namespace (nsenter), once apt is done and before
# systemd starts; /run, /run/lock and /tmp are tmpfs. Found by trying each
# grant (2026-11-17, Docker 29.6, cgroup v2; the security audit's way).
# The repository's target/ is mounted read-only at its own path (the test
# finds sb there, assert_cmd's CARGO_BIN_EXE_sb), with a tmpfs over its
# tmp/ (the test's temp directories, CARGO_TARGET_TMPDIR). The tmpfs the
# test writes to allow exec: Docker's default is noexec, and the test's
# stand-in systemctl is a script there, which sb must be able to run.
# Docker owns its cgroup, so a systemd scope around this script doesn't cap
# it: it gets `--cpus 2 --memory 2g` with no swap and a pids limit;
# SBH_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus`.
# Inside, apt fetches systemd and D-Bus. The container is removed on exit;
# the last line is the result. Root on the host (nsenter, docker). Exit: 0
# passed, 1 the test failed, 2 it could not run (a tool missing, Docker
# could not start the container, apt, the remount or systemd failed).
set -euo pipefail
cd "$(dirname "$0")/../../.."
root=$PWD
prefix=${SBH_PREFIX:-sbhangup}
image=${SBH_IMAGE:-ubuntu@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5}
name=$prefix-$$
# shellcheck disable=SC1090 # rustup's environment, where it is.
. ~/.cargo/env 2>/dev/null || true
for tool in docker nsenter node; do
  command -v "$tool" >/dev/null || { echo "hangup-systemd.sh: $tool not installed" >&2; exit 2; }
done
# The test binary is found where cargo builds it: target/ is the only
# directory mounted.
unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR

# The test binary's path from cargo's JSON messages (`executable` of the
# `hangup` test target); sb is built with it.
bin=$(cargo test --locked -p sb --test hangup --no-run --message-format=json 2>/dev/null |
  node -e '
    let out = "";
    for (const line of require("fs").readFileSync(0, "utf8").split("\n")) {
      try { const m = JSON.parse(line); if (m.executable && m.target && m.target.name === "hangup") out = m.executable; } catch {}
    }
    if (!out) { console.error("hangup-systemd.sh: no hangup test binary"); process.exit(2); }
    console.log(out);
  ')

# shellcheck disable=SC2086 # SBH_DOCKER_OPTS is a list of options.
docker run -d --rm --name "$name" --cgroupns private \
  --cpus 2 --memory 2g --memory-swap 2g --pids-limit 1024 ${SBH_DOCKER_OPTS:-} \
  --tmpfs /run --tmpfs /run/lock --tmpfs /tmp:exec \
  -v "$root/target:$root/target:ro" --tmpfs "$root/target/tmp:exec" "$image" \
  bash -c 'export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null && apt-get install -y -qq systemd dbus >/dev/null 2>&1 || exit 1
    : >/run/sb-installed
    while [ ! -e /run/sb-remounted ]; do sleep 0.2; done
    exec /lib/systemd/systemd' >/dev/null || {
  echo "hangup-systemd.sh: docker could not start $name" >&2
  exit 2
}
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# apt done: the cgroup tree read-write, from the host, then systemd.
for _ in $(seq 1 300); do
  docker exec "$name" test -e /run/sb-installed 2>/dev/null && break
  sleep 1
done
docker exec "$name" test -e /run/sb-installed || { echo "hangup-systemd.sh: apt didn't finish in $name" >&2; exit 2; }
pid=$(docker inspect -f '{{.State.Pid}}' "$name")
nsenter -t "$pid" -m mount -o remount,rw /sys/fs/cgroup || {
  echo "hangup-systemd.sh: could not make $name's cgroup tree writable" >&2
  exit 2
}
docker exec "$name" touch /run/sb-remounted

# Until systemd is up (degraded counts: a container lacks some units).
state=
for _ in $(seq 1 120); do
  state=$(docker exec "$name" systemctl is-system-running 2>/dev/null || true)
  case $state in running | degraded) break ;; esac
  sleep 1
done
case $state in
running | degraded) ;;
*)
  echo "hangup-systemd.sh: systemd didn't come up in $name (state: ${state:-none})" >&2
  exit 2
  ;;
esac

status=0
# CARGO_BIN_EXE_sb as cargo test sets it for a run (assert_cmd reads it then).
docker exec -e SB_TEST_ROOT=1 -e SB_LOG_DIR=/tmp/sb-logs -e CARGO_BIN_EXE_sb="$root/target/debug/sb" "$name" \
  "$bin" --exact a_session_scope_stop_takes_the_grace --nocapture --test-threads 1 || status=$?
if ((status == 0)); then
  echo "RESULT: a_session_scope_stop_takes_the_grace passed on systemd $(docker exec "$name" systemctl --version | head -1 | cut -d' ' -f2)"
else
  echo "RESULT: a_session_scope_stop_takes_the_grace failed ($status)"
  exit 1
fi
