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
# SBH_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus`.
#
# The run's own values never reach the fixtures. They become the invented
# capture's (Thu 2026-01-15): the units' timestamps become the release's
# stamp below, and each timer's next elapse (and time left) its invented
# value, which follow a boot at 10:00:00, 10:00:10 and 10:00:20 UTC; the
# timers are then sorted as systemctl sorts them. A timer with no invented
# value, a timer that has run, a field or a timestamp left unreplaced
# stops the capture. Needs jq.
set -euo pipefail
cd "$(dirname "$0")"
prefix=${SBH_PREFIX:-sbh-systemd}
command -v jq >/dev/null || { echo "capture.sh: jq not installed" >&2; exit 2; }

# The invented capture: <release>'s units' timestamp, and its timers' next
# elapses in microseconds.
invented() {
  case $1 in
    22.04) stamp='Thu 2026-01-15 10:00:03 UTC'
      next='{"systemd-tmpfiles-clean.timer":1768472100000000,"motd-news.timer":1768489200000000,"dpkg-db-backup.timer":1768521600000000,"saltbox_managed_job.timer":1768521600000000,"apt-daily.timer":1768543200000000,"apt-daily-upgrade.timer":1768545000000000,"e2scrub_all.timer":1768705800000000}' ;;
    24.04) stamp='Thu 2026-01-15 10:00:16 UTC'
      next='{"systemd-tmpfiles-clean.timer":1768472110000000,"motd-news.timer":1768482000000000,"dpkg-db-backup.timer":1768521600000000,"saltbox_managed_job.timer":1768521600000000,"apt-daily.timer":1768528800000000,"apt-daily-upgrade.timer":1768545300000000,"e2scrub_all.timer":1768705810000000}' ;;
    26.04) stamp='Thu 2026-01-15 10:00:29 UTC'
      next='{"systemd-tmpfiles-clean.timer":1768472120000000,"motd-news.timer":1768500000000000,"apt-daily.timer":1768518000000000,"dpkg-db-backup.timer":1768521600000000,"saltbox_managed_job.timer":1768521600000000,"apt-daily-upgrade.timer":1768545240000000,"e2scrub_all.timer":1768705820000000}' ;;
    *) echo "capture.sh: no invented values for $1" >&2; exit 2 ;;
  esac
}

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

for rel in "$@"; do invented "$rel"; done
c=
trap '[ -z "$c" ] || docker rm -f "$c" >/dev/null 2>&1 || true' EXIT
for rel in "$@"; do
  invented "$rel"
  c=$prefix-${rel/./}
  docker rm -f "$c" >/dev/null 2>&1 || true
  # shellcheck disable=SC2086 # SBH_DOCKER_OPTS is a list of options.
  docker run -d --rm --name "$c" --memory 2g --cpus 2 ${SBH_DOCKER_OPTS:-} --cgroupns=private --cap-add SYS_ADMIN \
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
  # A jq error (a value with no invented one) removes the file it cut
  # short, as the check on show.txt below does.
  docker exec "$c" systemctl list-timers --all --output=json | jq -c --argjson next "$next" '
    map(if keys != ["activates", "last", "left", "next", "passed", "unit"] then
        error("capture.sh: \(.unit) has fields with no invented value")
      elif .last != 0 or .passed != 0 then error("capture.sh: \(.unit) has run; capture again")
      elif .next == null then .
      else .next = ($next[.unit] // error("capture.sh: no invented next elapse for \(.unit)")) | .left = .next end)
    | sort_by(.next == null, .next, .unit)' > "$rel/list-timers.json" || {
    rm -f "$rel/list-timers.json"; echo "capture.sh: $rel: list-timers.json not written" >&2; exit 1
  }
  docker exec "$c" systemctl show --timestamp=utc --property=Id,ActiveEnterTimestamp,StateChangeTimestamp \
    docker.service saltbox_managed_crash.service saltbox_managed_gone.service saltbox_managed_job.service \
    saltbox_managed_left.service saltbox_managed_once.service saltbox_managed_restart.service \
    saltbox_managed_web.service |
    sed -E "s/[A-Z][a-z]{2} [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2} UTC/$stamp/" > "$rel/show.txt"
  # Only these lines may be left; anything else, or grep failing, removes
  # the file.
  grep -vxE "(Id=.*|[A-Za-z]+Timestamp=(n/a|$stamp)?)?" "$rel/show.txt" >/dev/null && st=0 || st=$?
  if [ "$st" -ne 1 ]; then
    rm "$rel/show.txt"; echo "capture.sh: $rel: a value in show.txt has no invented one" >&2; exit 1
  fi
  docker exec "$c" systemctl --version | head -1
  docker rm -f "$c" >/dev/null
done
