#!/bin/bash
# Captures the systemctl fixtures the service list's tests read (host spec,
# "systemd services"; Testing Strategy: systemd 249, 255 and 26.04's 259).
#
#   crates/host/src/fixtures/systemd/capture.sh 22.04 [24.04 26.04]
#
# Each release boots systemd as PID 1 in a throwaway container (its own
# cgroup namespace, made writable for systemd), installs the units below,
# and writes the four calls' outputs to <release>/. The container is
# removed afterwards. Runs no command on the host's own systemd.
set -euo pipefail
cd "$(dirname "$0")"
prefix=${SBH_PREFIX:-sbh-systemd}

units() {
  cat <<'INNER'
set -eu
d=/etc/systemd/system
u() { printf '%s\n' "$2" > "$d/$1"; }
u saltbox_managed_web.service $'[Unit]\nDescription=web\nWants=saltbox_managed_gone.service\n[Service]\nExecStart=/bin/sleep infinity\n[Install]\nWantedBy=multi-user.target'
u saltbox_managed_crash.service $'[Unit]\nDescription=crash\n[Service]\nExecStart=/bin/false'
u saltbox_managed_restart.service $'[Unit]\nDescription=restart\n[Service]\nExecStart=/bin/false\nRestart=always\nRestartSec=300'
u saltbox_managed_job.service $'[Unit]\nDescription=job\n[Service]\nType=oneshot\nExecStart=/bin/true'
u saltbox_managed_job.timer $'[Unit]\nDescription=job timer\n[Timer]\nOnCalendar=daily\n[Install]\nWantedBy=timers.target'
u saltbox_managed_once.service $'[Unit]\nDescription=once\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true'
u saltbox_managed_left.service $'[Unit]\nDescription=left\n[Service]\nExecStart=/bin/sleep infinity\n[Install]\nWantedBy=multi-user.target'
u docker.service $'[Unit]\nDescription=docker stand-in\n[Service]\nExecStart=/bin/sleep infinity'
u other.service $'[Unit]\nDescription=other\n[Service]\nExecStart=/bin/sleep infinity'
systemctl daemon-reload
systemctl enable --now saltbox_managed_web.service saltbox_managed_job.timer >/dev/null 2>&1
systemctl start docker.service other.service saltbox_managed_once.service saltbox_managed_restart.service
systemctl start saltbox_managed_crash.service || true
sleep 2
INNER
}

for rel in "$@"; do
  c=$prefix-${rel/./}
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 2g --cpus 2 --cgroupns=private --cap-add SYS_ADMIN \
    --security-opt apparmor=unconfined --security-opt seccomp=unconfined \
    --tmpfs /run --tmpfs /run/lock --tmpfs /tmp -e container=docker "ubuntu:$rel" bash -c '
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends systemd >/dev/null 2>&1
      mount -o remount,rw /sys/fs/cgroup
      exec /lib/systemd/systemd' >/dev/null
  booted=
  for _ in $(seq 1 120); do
    case "$(docker exec "$c" systemctl is-system-running 2>/dev/null || true)" in
      running|degraded) booted=1; break;;
    esac
    sleep 2
  done
  [ -n "$booted" ] || { echo "$c: systemd didn't boot" >&2; docker rm -f "$c" >/dev/null; exit 1; }
  docker exec -i "$c" bash -s < <(units)
  mkdir -p "$rel"
  docker exec "$c" systemctl list-units --type=service,timer --all --output=json > "$rel/list-units.json"
  docker exec "$c" systemctl list-unit-files --type=service --output=json > "$rel/list-unit-files.json"
  docker exec "$c" systemctl list-timers --all --output=json > "$rel/list-timers.json"
  docker exec "$c" systemctl show --timestamp=utc --property=Id,ActiveEnterTimestamp,StateChangeTimestamp \
    docker.service saltbox_managed_crash.service saltbox_managed_gone.service saltbox_managed_job.service \
    saltbox_managed_left.service saltbox_managed_once.service saltbox_managed_restart.service \
    saltbox_managed_web.service > "$rel/show.txt"
  docker exec "$c" systemctl --version | head -1
  docker rm -f "$c" >/dev/null
done
