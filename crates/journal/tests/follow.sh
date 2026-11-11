#!/usr/bin/env bash
# sb-journal's follow against `journalctl -f`, in a container per
# supported Ubuntu release with that release's own systemd-journald run
# standalone (as compare.sh does; no systemd as PID 1).
#
#   [SBJ_BIN=binary] crates/journal/tests/follow.sh [RELEASE | IMAGE=RELEASE]...
#
# Default: every row of rules.rs RELEASES (`journal releases`); IMAGE=RELEASE
# runs another image under a row's rules, as compare.sh's canaries do.
# Scenarios, each with fresh followers started with `-n 0` and a marker
# handshake (markers are written until both followers have printed one),
# then numbered entries under the scenario's own identifier:
#   flush     volatile journald, then /var/log/journal and a flush mid-stream
#   flush-var the same, /var/log/journal there (empty) before the followers
#   paced     entries 20 ms apart; latency = receipt - __REALTIME_TIMESTAMP
#   burst     entries written back to back from one process
#   rotate    two `journalctl --rotate` mid-stream
#   vacuum    a rotation, then `journalctl --vacuum-files=1` mid-stream
#   restart   journald killed (SIGKILL), restarted, stopped (SIGTERM), restarted
#   overflow  followers stopped through writes, a rotation and an inotify
#             queue overflow, then resumed
#   split     root and uid 1000 at once: system and user-1000 journals
#   between-full, between-empty
#             sb-journal only (journalctl can't follow from a page's end):
#             a page of the newest 5 of 20 entries, or of none (a page
#             that matched nothing), then entries written between the page
#             and the follow's start (`follow --wait-for`), then after it
# Each follower's numbered entries are counted: lost (a number missing)
# and duplicated (a number printed twice), plus its CPU time. sb-journal
# must lose and double nothing (journal spec, Difference 2 and "Starting a
# follow"); journalctl's counts are printed for comparison (255 doubles
# flush-var's entries, 259 loses those after flush's mid-follow /var
# creation).
# Containers (sbj-follow-<image>, or $SBJ_PREFIX-follow-<image>) are
# removed on exit. Needs Docker and the musl target.
set -euo pipefail

# SBJ_BIN names a binary built already (CI builds it once). It is resolved
# before the cd below, so a relative path works too.
if [ -n "${SBJ_BIN:-}" ]; then
  if ! SBJ_BIN=$(realpath -e "$SBJ_BIN") || [ ! -x "$SBJ_BIN" ]; then
    echo "SBJ_BIN is not an executable file" >&2
    exit 2
  fi
fi
cd "$(dirname "$0")/../../.."
releases=("$@")

# Without SBJ_BIN, build it here, capped, as every heavy build and run here is.
if [ -n "${SBJ_BIN:-}" ]; then
  bin=$SBJ_BIN
else
  systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% \
    cargo build -q --release --target x86_64-unknown-linux-musl -p sb-journal --features testing --example journal
  bin=$PWD/target/x86_64-unknown-linux-musl/release/examples/journal
fi
[ ${#releases[@]} -gt 0 ] || mapfile -t releases < <("$bin" releases | cut -d' ' -f1)

containers=()
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# Runs inside the container, as root, with the binary at /sbj.
inner() {
  cat <<'EOS'
set -u -o pipefail
rel=$1
image=$2
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd >/dev/null 2>&1
echo "$image ($rel rules): $(journalctl --version | head -1)"
systemd-machine-id-setup >/dev/null 2>&1
mid=$(cat /etc/machine-id)
for jd in /usr/lib/systemd/systemd-journald /lib/systemd/systemd-journald; do
  [ -x "$jd" ] && break
done
fail=0
bad() { fail=1; echo "  FAIL  $1"; }

start_jd() {
  "$jd" >/dev/null 2>&1 &
  jdpid=$!
  for _ in $(seq 1 50); do
    [ -S /run/systemd/journal/stdout ] && echo probe | systemd-cat -t sbf-probe 2>/dev/null && return 0
    sleep 0.1
  done
  bad "journald did not start"
}
stop_jd() { # signal
  kill "-$1" "$jdpid" 2>/dev/null; wait "$jdpid" 2>/dev/null
}
cpu() { # pid: user+system clock ticks
  awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || echo "?"
}

# Start both followers on identifier $1, wait for both to print a marker.
followers() {
  tag=$1
  rm -f fe.all fa.all fa.err
  if [ "${2:-}" = stamp ]; then
    # bash's EPOCHREALTIME: no fork per line. Only for paced writes, as
    # `read` takes a pipe a byte at a time.
    # shellcheck disable=SC2016 # expanded by the bash it is given to
    stamp='while IFS= read -r l; do printf "%s %s\n" "$EPOCHREALTIME" "$l"; done'
    { journalctl -f -n 0 -o json --all -q "SYSLOG_IDENTIFIER=$tag" 2>/dev/null | bash -c "$stamp" > fe.all; } &
    { /sbj follow --release "$rel" --match "SYSLOG_IDENTIFIER=$tag" 2>fa.err | bash -c "$stamp" > fa.all; } &
  else
    journalctl -f -n 0 -o json --all -q "SYSLOG_IDENTIFIER=$tag" > fe.all 2>/dev/null &
    /sbj follow --release "$rel" --match "SYSLOG_IDENTIFIER=$tag" > fa.all 2>fa.err &
  fi
  sleep 0.2
  jf=$(pgrep -n -x journalctl); pf=$(pgrep -n -x sbj)
  for _ in $(seq 1 100); do
    echo "ready" | systemd-cat -t "$tag"
    grep -q '"MESSAGE":"ready"' fe.all 2>/dev/null && grep -q '"MESSAGE":"ready"' fa.all 2>/dev/null && return 0
    sleep 0.2
  done
  bad "$tag: followers never got ready"
}

# Wait until both have entry $1 (or 10 s), stop them, count. $2: scenario.
finish() {
  last=$1 name=$2
  for _ in $(seq 1 100); do
    grep -q "\"MESSAGE\":\"follow $last\"" fe.all && grep -q "\"MESSAGE\":\"follow $last\"" fa.all && break
    sleep 0.1
  done
  sleep 1
  jc=$(cpu "$jf"); pc=$(cpu "$pf")
  kill "$jf" "$pf" 2>/dev/null; sleep 0.3
  for who in fe fa; do
    grep -o '"MESSAGE":"follow [0-9]*"' "$who.all" | grep -o '[0-9]*' | sort -n > "$who.nums"
    got=$(sort -nu "$who.nums" | wc -l)
    dup=$(( $(wc -l < "$who.nums") - got ))
    lost=$(( $(wc -l < written) - $(comm -12 <(sort -u written) <(sort -u "$who.nums") | wc -l) ))
    extra=$(comm -13 <(sort -u written) <(sort -u "$who.nums") | wc -l)
    if [ "$who" = fe ]; then label=journalctl t=$jc; else label=sb-journal t=$pc; fi
    printf '  %-8s %-10s written %4d  lost %3d  dup %3d  extra %d  cpu %s ticks\n' \
      "$name" "$label" "$(wc -l < written)" "$lost" "$dup" "$extra" "$t"
    if [ "$who" = fa ] && { [ "$lost" != 0 ] || [ "$dup" != 0 ] || [ "$extra" != 0 ]; }; then
      bad "$name: sb-journal lost $lost, duplicated $dup, extra $extra"
    fi
  done
  [ -s fa.err ] && sed 's/^/    sb-journal stderr: /' fa.err | head -5
  return 0
}

# Write `follow N` for N in $1..$2 under $tag, one process per entry
# unless $3 = burst. Only entries systemd-cat accepted count as written.
write() {
  if [ "${3:-}" = burst ]; then
    seq "$1" "$2" | sed 's/^/follow /' | systemd-cat -t "$tag" && seq "$1" "$2" >> written
  else
    for i in $(seq "$1" "$2"); do
      echo "follow $i" | systemd-cat -t "$tag" 2>/dev/null && echo "$i" >> written
      [ -n "${pace:-}" ] && sleep "$pace"
    done
  fi
  return 0
}

mkdir -p /w && cd /w || exit 2

# flush: volatile first (no /var/log/journal), then persistent.
rm -rf /var/log/journal /run/log/journal
start_jd
: > written; followers sbf-flush
write 1 100
mkdir -p /var/log/journal
journalctl --flush >/dev/null 2>&1 || bad "flush"
[ -f "/var/log/journal/$mid/system.journal" ] || bad "flush: no persistent system.journal"
write 101 200
finish 200 flush

# flush-var: the same, with an empty /var/log/journal there before the
# followers start, so both watch it (journalctl doesn't watch a missing
# root directory). journald stays volatile until asked to flush (the
# "flushed" flag in /run).
stop_jd TERM
rm -rf /var/log/journal /run/log/journal /run/systemd/journal/flushed
mkdir -p /var/log/journal
start_jd
[ -f "/var/log/journal/$mid/system.journal" ] && bad "flush-var: journald not volatile"
: > written; followers sbf-flushvar
write 1 100
journalctl --flush >/dev/null 2>&1 || bad "flush"
[ -f "/var/log/journal/$mid/system.journal" ] || bad "flush-var: no persistent system.journal"
write 101 200
finish 200 flush-var

# paced, with receipt stamps for latency.
: > written; followers sbf-paced stamp
pace=0.02 write 1 100
finish 100 paced
for who in fe fa; do
  # receipt (s.us) minus __REALTIME_TIMESTAMP (us), in ms, per entry.
  grep '"MESSAGE":"follow ' "$who.all" | sed 's/^\([0-9]*\)[.,]\([0-9]*\) .*"__REALTIME_TIMESTAMP":"\([0-9]*\)".*/\1\2 \3/' |
    awk '{print ($1 - $2) / 1000}' | sort -n > "$who.lat"
  n=$(wc -l < "$who.lat")
  [ "$n" -gt 0 ] || { bad "paced: no latency samples"; continue; }
  p50=$(sed -n "$(( (n + 1) / 2 ))p" "$who.lat"); p95=$(sed -n "$(( (n * 95 + 99) / 100 ))p" "$who.lat"); max=$(tail -1 "$who.lat")
  if [ "$who" = fe ]; then label=journalctl; else label=sb-journal; fi
  printf '  latency  %-10s p50 %s ms  p95 %s ms  max %s ms (%d samples)\n' "$label" "$p50" "$p95" "$max" "$n"
done

# burst
: > written; followers sbf-burst
write 1 3000 burst
finish 3000 burst

# rotate
: > written; followers sbf-rotate
write 1 100 burst
journalctl --rotate >/dev/null 2>&1 || bad "rotate"
write 101 200
journalctl --rotate >/dev/null 2>&1 || bad "rotate"
write 201 300 burst
finish 300 rotate

# vacuum: archived files exist (rotate, flush); delete all but one.
: > written; followers sbf-vacuum
write 1 100
journalctl --rotate >/dev/null 2>&1
before=$(find /var/log/journal /run/log/journal -name '*.journal*' | wc -l)
journalctl --vacuum-files=1 >/dev/null 2>&1 || bad "vacuum"
after=$(find /var/log/journal /run/log/journal -name '*.journal*' | wc -l)
[ "$after" -lt "$before" ] || bad "vacuum: no file deleted ($before -> $after)"
write 101 200 burst
finish 200 vacuum

# restart: SIGKILL leaves system.journal online; the new journald
# renames it to a .journal~ and starts a new file.
: > written; followers sbf-restart
write 1 100
stop_jd KILL
start_jd
write 101 200
stop_jd TERM
start_jd
write 201 300 burst
finish 300 restart

# overflow: both followers stopped while entries are written and more
# events than the inotify queue holds are raised in the journal
# directory (files created and deleted: distinct names don't coalesce),
# then resumed. Both see IN_Q_OVERFLOW.
: > written; followers sbf-overflow
kill -STOP "$jf" "$pf"
write 1 100 burst
max=$(cat /proc/sys/fs/inotify/max_queued_events)
d=/var/log/journal/$mid
for i in $(seq 1 $(( max / 2 + 1000 ))); do : > "$d/junk.$i"; rm "$d/junk.$i"; done
journalctl --rotate >/dev/null 2>&1
write 101 200 burst
kill -CONT "$jf" "$pf"
write 201 300
finish 300 overflow

# split: root and uid 1000 writing at the same time. Persistent journald
# puts uid 1000's entries in user-1000.journal, so two files are written
# together, each raising its own (timer-coalesced) IN_MODIFY.
: > written; followers sbf-split
for i in $(seq 1 2 299); do
  echo "follow $i" | systemd-cat -t sbf-split && echo "$i" >> written
done &
w1=$!
for i in $(seq 2 2 300); do
  echo "follow $i" | setpriv --reuid 1000 --regid 1000 --clear-groups systemd-cat -t sbf-split && echo "$i" >> written
done &
w2=$!
wait "$w1" "$w2"
sleep 0.5
[ -n "$(find "/var/log/journal/$mid" -name 'user-1000*.journal')" ] || bad "split: no user-1000 journal"
last_n=$(sort -n written | tail -1)
finish "$last_n" split

# Wait (10 s at most) until journalctl shows $2 entries of identifier $1
# whose message starts with $3, so they are in the files.
in_journal() {
  for _ in $(seq 1 100); do
    [ "$(journalctl -q -o cat "SYSLOG_IDENTIFIER=$1" | grep -c "^$3")" -ge "$2" ] && return 0
    sleep 0.1
  done
  bad "$1: journald never showed $2 \"$3\" entries"
}

# between: $1 the identifier, $2 the page's size, $3 the entries before
# the page ("before N", not counted as follow entries). The page must show
# the newest min($2, $3) of them; then "follow 1..100" written between the
# page and the follow, which must come first, and 101..200 after it.
between() {
  tag=$1 n=$2 pre=$3 name=$4
  : > written; rm -f fa.all fa.err go
  for i in $(seq 1 "$pre"); do echo "before $i" | systemd-cat -t "$tag"; done
  [ "$pre" -gt 0 ] && in_journal "$tag" "$pre" before
  /sbj follow --release "$rel" --match "SYSLOG_IDENTIFIER=$tag" -n "$n" --wait-for go > fa.all 2>fa.err &
  pf=$!
  for _ in $(seq 1 100); do grep -q '^sb-journal: page read' fa.err && break; sleep 0.1; done
  grep -q '^sb-journal: page read' fa.err || bad "$name: the page was never read"
  write 1 100
  in_journal "$tag" 100 follow
  : > go
  write 101 200
  for _ in $(seq 1 100); do grep -q '"MESSAGE":"follow 200"' fa.all && break; sleep 0.1; done
  sleep 1
  pc=$(cpu "$pf")
  kill "$pf" 2>/dev/null; sleep 0.3
  shown=$(grep -o '"MESSAGE":"before [0-9]*"' fa.all | grep -o '[0-9]*' | tr '\n' ' ')
  want=$(if [ "$pre" -gt 0 ]; then seq $(( pre - (n < pre ? n : pre) + 1 )) "$pre" | tr '\n' ' '; fi)
  [ "$shown" = "$want" ] || bad "$name: the page showed [$shown], not [$want]"
  grep -o '"MESSAGE":"follow [0-9]*"' fa.all | grep -o '[0-9]*' | sort -n > fa.nums
  got=$(sort -nu fa.nums | wc -l)
  dup=$(( $(wc -l < fa.nums) - got ))
  lost=$(( $(wc -l < written) - $(comm -12 <(sort -u written) <(sort -u fa.nums) | wc -l) ))
  extra=$(comm -13 <(sort -u written) <(sort -u fa.nums) | wc -l)
  printf '  %-8s %-10s written %4d  lost %3d  dup %3d  extra %d  cpu %s ticks (page of %d)\n' \
    "$name" sb-journal "$(wc -l < written)" "$lost" "$dup" "$extra" "$pc" "$(echo "$want" | wc -w)"
  if [ "$lost" != 0 ] || [ "$dup" != 0 ] || [ "$extra" != 0 ]; then
    bad "$name: sb-journal lost $lost, duplicated $dup, extra $extra"
  fi
  grep -v '^sb-journal: page read' fa.err | sed 's/^/    sb-journal stderr: /' | head -5
  return 0
}
between sbf-between-full 5 20 between-full
between sbf-between-empty 10 0 between-empty
find /var/log/journal /run/log/journal -name '*.journal*' 2>/dev/null | sed 's/^/  file: /'
exit $fail
EOS
}

status=0
for arg in "${releases[@]}"; do
  image=${arg%%=*}
  rel=${arg#*=}
  if ! "$bin" releases | awk -v r="$rel" '$1 == r { found = 1 } END { exit !found }'; then
    echo "$arg: $rel is not a row of rules.rs RELEASES"
    status=1
    continue
  fi
  c=${SBJ_PREFIX:-sbj}-follow-${image/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 4g --cpus 2 -v "$bin:/sbj:ro" "ubuntu:$image" sleep 3600 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" "$image" < <(inner) || status=1
done
exit $status
