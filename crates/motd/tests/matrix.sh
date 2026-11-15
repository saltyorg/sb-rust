#!/usr/bin/env bash
# sb motd's sources against the tools sb-go ran, in a systemd-booted
# container per supported Ubuntu release (motd spec, Testing, "Container
# matrix"; the results are docs/platform-notes/motd-sources.md).
#
#   [SBM_BIN=binary] [SBM_PREFIX=name] [SBM_FIXTURES=dir] \
#     crates/motd/tests/matrix.sh [RELEASE]...
#
# Default: 22.04 24.04 26.04. SBM_BIN is an sb built on the oldest
# release (default target/debug/sb). Each container (sbm-<release>, or
# $SBM_PREFIX-<release>) boots systemd, starts sshd, and a synthetic user
# logs in over SSH twice, once logged out and once still logged in; an
# update-motd.d script records what sb, who and last say under pam_motd
# during each login (notes, not checks). Then
# sb's lines are compared with lsb_release -ds/-cs, uname -r, uptime -p,
# who's count (logind's own count where utmp is gone), last -w -n 5,
# df -H with sb-go's filters (a tmpfs, an anonymous volume and a bind
# mount of it added), and
# lspci's display controllers (sb-go's BMC exclusions applied).
# SBM_FIXTURES gets each release's last and loginctl output for the unit
# tests, the kernel release replaced by a placeholder.
#
# The container is not privileged: it gets CAP_SYS_ADMIN and no AppArmor
# profile only so that its systemd can remount its own cgroup (private
# cgroup namespace) writable and the test can mount; /sys and /proc/sys
# stay read-only and no device is passed. It is capped at 2 CPUs and
# 2 GB, and removed on exit with its anonymous volume. Needs Docker and the ubuntu:<release>
# images (pulled on first use); the packages come from Ubuntu's archive.
set -euo pipefail

cd "$(dirname "$0")/../../.."
bin=${SBM_BIN:-target/debug/sb}
prefix=${SBM_PREFIX:-sbm}
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
[ -x "$bin" ] || { echo "no sb binary at $bin (cargo build -p sb)" >&2; exit 2; }

containers=()
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  for c in "${containers[@]}"; do docker rm -fv "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# The container's PID 1: the packages, then systemd.
boot='
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd systemd-sysv dbus libpam-systemd openssh-server \
  lsb-release pciutils procps >/dev/null 2>&1
# 26.04 moved last to wtmpdb, which its base image lacks.
if apt-cache show wtmpdb >/dev/null 2>&1; then apt-get install -y -qq wtmpdb >/dev/null 2>&1; fi
mount -o remount,rw /sys/fs/cgroup
exec /sbin/init'

# Runs inside the container, as root, with sb at /sb.
inner() {
  cat <<'EOS'
set -u -o pipefail
rel=$1
fail=0
check() { # name, sb's value, the tool's value
  if [ "$2" = "$3" ]; then echo "  ok    $1"; else
    echo "  FAIL  $1"; echo "        sb:   $2" | sed '2,$s/^/              /'
    echo "        tool: $3" | sed '2,$s/^/              /'; fail=1
  fi
}
# sb's value for one section, plain, its key and padding removed.
sb() {
  NO_COLOR=1 SB_LOG_DIR=/tmp/sb-log /sb motd "--$1" | sed -E -e '/^$/d' -e 's/^[A-Za-z][A-Za-z ]*:  +//' -e 's/^ +//'
}
systemctl is-system-running --wait >/dev/null 2>&1 || systemctl --failed --no-legend
echo "$rel: $(systemctl --version | head -1); last from $(dpkg -S "$(command -v last 2>/dev/null || echo /none)" 2>/dev/null | cut -d: -f1 || echo none)"

# sb as Saltbox runs it: from update-motd.d under pam_motd, during the
# login, beside what who and last say at that moment.
cat >/etc/update-motd.d/99-sbm <<'EOM'
#!/bin/sh
n=$(ls /var/tmp/sbm-at-login.* 2>/dev/null | wc -l)
{
  NO_COLOR=1 SB_LOG_DIR=/tmp/sb-log /sb motd --sessions --login
  echo "who: $(who | grep -c .)"
  echo "last: $(last -w -n 5 | grep -v '^reboot' | head -1)"
} >"/var/tmp/sbm-at-login.$n" 2>&1
EOM
chmod 755 /etc/update-motd.d/99-sbm

# Two logins over SSH: one logged out, one still logged in.
useradd -m -s /bin/bash sbuser
home=$(getent passwd sbuser | cut -d: -f6)
mkdir -p /root/.ssh "$home/.ssh"
ssh-keygen -q -t ed25519 -N "" -f /root/.ssh/id_ed25519
cp /root/.ssh/id_ed25519.pub "$home/.ssh/authorized_keys"
chown -R sbuser: "$home/.ssh"
chmod 700 "$home/.ssh"
systemctl start ssh.socket >/dev/null 2>&1 || true
systemctl start ssh >/dev/null 2>&1 || true
o="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
# shellcheck disable=SC2086
ssh -tt $o sbuser@127.0.0.1 "sleep 1" </dev/null >/dev/null 2>&1
# shellcheck disable=SC2086
(setsid ssh -tt $o sbuser@127.0.0.1 "sleep 600" </dev/null >/dev/null 2>&1 &)
for _ in $(seq 1 20); do [ -s /var/tmp/sbm-at-login.1 ] && break; sleep 0.5; done
sleep 1
for n in 0 1; do
  sed "s/^/  note  at login $((n + 1)) under pam_motd: /" "/var/tmp/sbm-at-login.$n" 2>/dev/null \
    || echo "  note  at login $((n + 1)): pam_motd ran no update-motd.d script"
done

# A tmpfs (skipped by sb-go's filter), and the volume at /sbm-volume
# bound again under a name with a space (one device twice).
mkdir -p /mnt/sbm-tmpfs /sbm-volume/source "/mnt/sbm bind"
mount -t tmpfs tmpfs /mnt/sbm-tmpfs
mount --bind /sbm-volume/source "/mnt/sbm bind"

check distro "$(sb distro)" "$(lsb_release -ds) ($(lsb_release -cs))"
check kernel "$(sb kernel)" "$(uname -r)"
up_sb=$(sb uptime); up_tool=$(uptime -p | sed 's/^up //')
[ "$up_sb" = "$up_tool" ] || { up_sb=$(sb uptime); up_tool=$(uptime -p | sed 's/^up //'); }
check uptime "$up_sb" "$up_tool"

# who reads utmp, which 26.04 no longer writes: there logind's own table,
# its login classes, is the reference.
who_count=$(who | grep -c .)
if [ "$who_count" -eq 0 ]; then
  who_count=$(loginctl list-sessions --no-legend | awk '$6 == "user"' | grep -c .)
fi
case $who_count in
  0) sessions="No active sessions" ;;
  1) sessions="1 active session" ;;
  *) sessions="$who_count active sessions" ;;
esac
check sessions "$(sb sessions)" "$sessions"

# The first sbuser line of last, the still logged in login, in sb-go's text.
first=$(last -w -n 5 | awk '$1 == "sbuser"' | head -1)
read -r user _ from day mon date time _ <<<"$first"
check login "$(sb login)" "$user at $day $mon $date $time (still logged in) from $from"
grep -q "still logged in" <<<"$first" || { echo "  FAIL  login: the first entry is not the open login"; fail=1; }

# df as sb-go ran it, its /dev, /sys, /proc and /run rows dropped.
df_rows=$(df -H -x tmpfs -x overlay -x fuse.mergerfs -x fuse.rclone --output=target,pcent,size 2>/dev/null \
  | tail -n +2 | awk '{size=$NF; pc=$(NF-1); $NF=""; $(NF-1)=""; sub(/ +$/, ""); print $0 "|" pc "|" size}' \
  | grep -Ev '^/(dev|sys|proc|run)(/|\|)' | sort)
sb_rows=$(NO_COLOR=1 SB_LOG_DIR=/tmp/sb-log /sb motd --disk | grep 'used out of' | sed -e 's/^Disk Usage://' -e 's/^ *//' \
  | sed -E 's/^(.*[^ ]) +([0-9]+%) used out of +([^ ]+)$/\1|\2|\3/' | sort)
check disk "$sb_rows" "$df_rows"
grep -Eq '^(/sbm-volume|/mnt/sbm bind)\|' <<<"$sb_rows" || { echo "  FAIL  disk: the volume is missing"; fail=1; }
grep -q '^/mnt/sbm-tmpfs|' <<<"$sb_rows" && { echo "  FAIL  disk: the tmpfs is shown"; fail=1; }

# lspci's display controllers, named as sb-go took them, BMCs left out.
gpu_tool=$(lspci 2>/dev/null | grep -E 'VGA compatible controller|3D controller|Display controller' \
  | cut -d: -f3- | sed 's/^ *//' \
  | grep -viE 'ASPEED|Matrox MGA|Cirrus Logic|XGI|Silicon Motion|Hisilicon|ServerEngines|Nuvoton|Pilot' || true)
check gpu "$(sb gpu)" "$gpu_tool"

if [ -n "${SBM_FIXTURES_IN:-}" ]; then
  k=$(uname -r)
  # The kernel release and the boot time are the machine's: placeholders.
  scrub() {
    sed -E -e "s/$k/6.8.0-1-generic/g" -e "s/${k:0:16}/6.8.0-1-generic/g" \
      -e '/^reboot /s/[A-Z][a-z]{2} [A-Z][a-z]{2} [ 0-9][0-9] [0-9]{2}:[0-9]{2}/Sat Nov 14 23:00/' \
      -e 's/^(wtmp(db)? begins ).*/\1Sat Nov 14 23:00:00 2026/'
  }
  last -w -n 5 | scrub >"$SBM_FIXTURES_IN/last"
  case $rel in
    22.04 | 24.04) loginctl list-sessions --output=json ;;
    *) loginctl list-sessions --json=short ;;
  esac >"$SBM_FIXTURES_IN/loginctl.json"
fi
# A command over SSH without a terminal: logind lists it, utmp doesn't
# (a note for the platform note, not a check).
# shellcheck disable=SC2086
(setsid ssh $o sbuser@127.0.0.1 "sleep 600" </dev/null >/dev/null 2>&1 &)
for _ in $(seq 1 20); do [ "$(loginctl list-sessions --no-legend | grep -c sbuser)" -ge 3 ] && break; sleep 0.5; done
echo "  note  with a command-only SSH session: who $(who | grep -c .), sb: $(sb sessions)"
exit $fail
EOS
}

status=0
for rel in "${releases[@]}"; do
  name="$prefix-${rel/./}"
  containers+=("$name")
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker run -d --name "$name" --cpus 2 --memory 2g \
    --cgroupns=private --cap-add SYS_ADMIN --security-opt apparmor=unconfined \
    --tmpfs /run --tmpfs /run/lock -v /sbm-volume "ubuntu:$rel" sh -c "$boot" >/dev/null
  for _ in $(seq 1 300); do
    docker exec "$name" sh -c '[ "$(ps -o comm= -p 1)" = systemd ]' 2>/dev/null && break
    sleep 1
  done
  docker cp "$bin" "$name:/sb" >/dev/null
  env=()
  if [ -n "${SBM_FIXTURES:-}" ]; then
    mkdir -p "$SBM_FIXTURES/$rel"
    docker exec "$name" mkdir -p /fixtures
    env=(-e SBM_FIXTURES_IN=/fixtures)
  fi
  docker exec "${env[@]}" "$name" bash -c "$(inner)" inner "$rel" || status=1
  if [ -n "${SBM_FIXTURES:-}" ]; then
    docker cp "$name:/fixtures/." "$SBM_FIXTURES/$rel/" >/dev/null
  fi
  docker rm -fv "$name" >/dev/null 2>&1 || true
done
exit $status
