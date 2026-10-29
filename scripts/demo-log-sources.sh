#!/usr/bin/env bash
# Demo containers and systemd units covering every status the log picker can
# show. Nothing touches real Saltbox sources: services are named sbdemo-*
# (transient units, plus one runtime unit file under /run), containers are
# labelled sb-rust-demo=1. Both disappear on `down` (and the units on reboot).
#
#   scripts/demo-log-sources.sh up     create them
#   scripts/demo-log-sources.sh down   remove them
#   scripts/demo-log-sources.sh view   open the picker showing only them
set -euo pipefail
shopt -s inherit_errexit

PREFIX=sbdemo-
LABEL=sb-rust-demo=1
IMAGE=alpine:3
STOPPED_UNIT=/run/systemd/system/${PREFIX}stopped.service
LEFTOVER_UNIT=/run/systemd/system/${PREFIX}leftover.service

die() { echo "$*" >&2; exit 1; }
[[ $EUID -eq 0 ]] || die "needs root (systemd units and Docker)"

container() { docker run -d -q --label "$LABEL" --name "$PREFIX$1" "${@:2}" >/dev/null; }

# A unit is started with --no-block so units that never finish starting
# (the "starting" demo) don't hang this script.
unit() { systemd-run --quiet --no-block --unit "$PREFIX$1" "${@:2}"; }

up() {
  docker image inspect "$IMAGE" >/dev/null 2>&1 || docker pull -q "$IMAGE" >/dev/null

  # Containers: Needs attention
  container crashed "$IMAGE" sh -c 'echo "starting"; echo "fatal: demo crash" >&2; exit 3'
  container oom -m 16m --memory-swap 16m "$IMAGE" sh -c \
    'echo "allocating"; exec tail /dev/zero'
  container restarting --restart always "$IMAGE" sh -c 'echo "up briefly"; sleep 1; exit 1'
  container unhealthy --health-cmd false --health-interval 2s --health-retries 1 \
    "$IMAGE" sh -c 'while true; do echo "serving (health check fails)"; sleep 5; done'
  # Fails on its first run and then stays up, so it was restarted recently.
  container flapped --restart on-failure:3 "$IMAGE" sh -c \
    'if [ -f /tmp/ran ]; then echo "second run, staying up"; exec sleep infinity; fi
     touch /tmp/ran; echo "first run fails" >&2; exit 1'
  # Containers: running, busy and idle
  # shellcheck disable=SC2016 # expanded by the container's sh
  container running "$IMAGE" sh -c \
    'i=0; while true; do i=$((i+1)); echo "tick $i"; sleep 2; done'
  container health-starting --health-cmd true --health-interval 1h \
    "$IMAGE" sh -c 'echo "waiting for first health check"; exec sleep infinity'
  container paused "$IMAGE" sh -c 'echo "about to be paused"; exec sleep infinity'
  docker pause "${PREFIX}paused" >/dev/null
  container stopped "$IMAGE" sh -c 'echo "did its job"; exit 0'
  docker create -q --label "$LABEL" --name "${PREFIX}never-started" "$IMAGE" true >/dev/null

  # Services: Needs attention
  unit failed sh -c 'echo "boom" >&2; exit 2'
  unit restarting -p Restart=always -p RestartSec=30 sh -c 'echo "up briefly"; sleep 1; exit 1'
  # Services: running, busy and idle
  # shellcheck disable=SC2016 # expanded by the unit's sh
  unit running sh -c 'i=0; while true; do i=$((i+1)); echo "tick $i"; sleep 2; done'
  unit finished -p Type=oneshot -p RemainAfterExit=yes sh -c 'echo "one-shot done"'
  unit starting -p Type=notify -p TimeoutStartSec=infinity sh -c \
    'echo "never reports ready"; exec sleep infinity'
  unit timer --on-calendar='*:0/15' sh -c 'echo "timer run"'
  # Transient units are removed once inactive, so "stopped" needs a unit
  # file. It is enabled (runtime only, under /run) so it stays loaded like an
  # enabled Saltbox unit; systemctl only lists loaded units.
  cat >"$STOPPED_UNIT" <<'EOF'
[Unit]
Description=sb-rust demo: loaded but stopped
[Service]
ExecStart=/bin/true
[Install]
WantedBy=multi-user.target
EOF
  # A disabled unit file nothing starts: an app removed without cleanup.
  cat >"$LEFTOVER_UNIT" <<'EOF'
[Unit]
Description=sb-rust demo: disabled leftover
[Service]
ExecStart=/bin/true
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable --quiet --runtime "${PREFIX}stopped.service"

  echo "Demo sources are up. See them with: $0 view"
}

down() {
  local ids
  ids=$(docker ps -aq --filter "label=$LABEL")
  if [[ -n $ids ]]; then
    # shellcheck disable=SC2086 # one id per word
    docker rm -f $ids >/dev/null
  fi
  local units
  units=$(systemctl list-units "$PREFIX*" --all --plain --no-legend | awk '{print $1}')
  if [[ -n $units ]]; then
    # shellcheck disable=SC2086 # one unit per word
    systemctl stop $units 2>/dev/null || true
    # shellcheck disable=SC2086
    systemctl reset-failed $units 2>/dev/null || true
  fi
  systemctl disable --quiet --runtime "${PREFIX}stopped.service" 2>/dev/null || true
  rm -f "$STOPPED_UNIT" "$LEFTOVER_UNIT"
  systemctl daemon-reload
  echo "Demo sources removed."
}

view() {
  local root
  root=$(cd "$(dirname "$0")/.." && pwd)
  command -v cargo >/dev/null || die "cargo not found on PATH"
  (cd "$root" && cargo build -q --release -p proto-logs)
  "$root/target/release/proto-logs" --service-prefix "$PREFIX" --filter "$PREFIX"
}

case ${1:-} in
  up) up ;;
  down) down ;;
  view) view ;;
  *) die "usage: $0 up|down|view" ;;
esac
