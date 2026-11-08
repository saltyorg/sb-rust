#!/usr/bin/env bash
# proto-journal reading journal files while the release's own journald
# writes them heavily and rotates them, in an unprivileged container per
# supported Ubuntu release (journald standalone, as in compare.sh).
#
#   prototypes/proto-journal/stress.sh [RELEASE]...
#
# While four writers log as fast as they can and the files rotate by size
# and every half second, the reader takes snapshots (`tail`, filtered and
# not), runs decode-all and hash-check, and follows. Then, with journald
# idle, every check below must hold:
# - no read crashed (a panic exits 101) or hung;
# - every entry a snapshot printed is in journalctl's final view with the
#   same fields, cursor included: never wrong data;
# - a snapshot that reported no error has no gap in any writer's numbers;
#   one that did names what it skipped on stderr (a transient error);
# - the follower printed every entry, in journalctl's order, and nothing
#   else but the declined divergence below (an entry journald wrote again
#   across a rotation, whose first copy journalctl's match doesn't list);
# - a last snapshot of the idle journal equals journalctl's.
# Default: every row of query.rs RELEASES (`proto-journal releases`).
# Containers (sbj-stress-<release>, or $SBJ_PREFIX-stress-<release>) are
# removed on exit.
set -euo pipefail

cd "$(dirname "$0")/../.."
releases=("$@")
lines=${SBJ_STRESS_LINES:-20000}

systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% \
  cargo build -q --release --target x86_64-unknown-linux-musl -p proto-journal
bin=$PWD/target/x86_64-unknown-linux-musl/release/proto-journal
# Default: every row of query.rs RELEASES, as compare.sh.
[ ${#releases[@]} -gt 0 ] || mapfile -t releases < <("$bin" releases | cut -d' ' -f1)

containers=()
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# Runs inside the container, as root, with the binary at /pj.
inner() {
  cat <<'EOS'
set -u -o pipefail
rel=$1 lines=$2
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd jq >/dev/null 2>&1
echo "$rel: $(journalctl --version | head -1), $lines lines per writer"
fail=0
bad() { fail=1; echo "  FAIL  $1"; }
ok() { echo "  ok    $1"; }

# Rotation by size as well as on demand; nothing vacuumed, so every entry
# a snapshot saw is still there at the end; no rate limit.
mkdir -p /etc/systemd/journald.conf.d /var/log/journal
cat > /etc/systemd/journald.conf.d/stress.conf <<'CONF'
[Journal]
Storage=persistent
SystemMaxFileSize=1M
SystemMaxUse=8G
SystemKeepFree=0
SystemMaxFiles=100000
RateLimitIntervalSec=0
CONF
systemd-machine-id-setup >/dev/null 2>&1
jd=$(ls /usr/lib/systemd/systemd-journald /lib/systemd/systemd-journald 2>/dev/null | head -1)
"$jd" >/dev/null 2>&1 &
sleep 2
journalctl --flush >/dev/null 2>&1 || bad "journalctl --flush"
mid=$(cat /etc/machine-id)
mkdir -p /st && cd /st

/pj tail --release "$rel" -f -n 0 --match SYSLOG_IDENTIFIER=stress > follow.all 2> follow.err &
pf=$!
for _ in $(seq 1 100); do
  echo "ready" | systemd-cat -t stress
  grep -q '"MESSAGE":"ready"' follow.all 2>/dev/null && break
  sleep 0.2
done

# Four writers: short lines, long compressible ones (compressed by
# journald), random ones and ANSI ones. Each numbers its lines.
pad=$(head -c 3000 /dev/zero | tr '\0' x)
(for i in $(seq 1 "$lines"); do echo "w1 $i"; done | systemd-cat -t stress) &
w1=$!
(for i in $(seq 1 "$lines"); do echo "w2 $i $pad"; done | systemd-cat -t stress) &
w2=$!
(for i in $(seq 1 "$lines"); do echo "w3 $i $(head -c 600 /dev/urandom | base64 -w0)"; done | systemd-cat -t stress) &
w3=$!
(for i in $(seq 1 "$lines"); do printf 'w4 %s \033[31mred\033[0m\n' "$i"; done | systemd-cat -t stress) &
w4=$!
(while kill -0 "$w1" "$w2" "$w3" "$w4" 2>/dev/null; do journalctl --rotate >/dev/null 2>&1; sleep 0.5; done) &
rot=$!

k=0 crashed=0 tools=0
# A panic exits 101; `timeout` exits 124 on a hang.
run() { # name, then the command
  local name=$1 rc
  shift
  timeout 60 "$@"
  rc=$?
  # Noted in a file: the caller redirects this function's output.
  if [ "$rc" -ge 100 ]; then crashed=1; echo "  $name exit $rc" >> crashes.txt; fi
}
while kill -0 "$w1" "$w2" "$w3" "$w4" 2>/dev/null; do
  k=$((k + 1))
  run tail /pj tail --release "$rel" -n 300 --match SYSLOG_IDENTIFIER=stress > "s.$k.json" 2> "s.$k.err"
  run tail /pj tail --release "$rel" -n 300 > "a.$k.json" 2> "a.$k.err"
  if [ $((k % 5)) = 0 ]; then
    tools=$((tools + 1))
    run decode-all /pj decode-all > /dev/null 2>&1
    run hash-check /pj hash-check > /dev/null 2>&1
  fi
done
wait "$w1" "$w2" "$w3" "$w4" "$rot"
sleep 3
kill "$pf" 2>/dev/null; wait "$pf" 2>/dev/null
files=$(find "/var/log/journal/$mid" -name '*.journal' | wc -l)
echo "  $k snapshot pairs, $tools tool runs, $files journal files"
[ -s crashes.txt ] && cat crashes.txt
[ "$crashed" = 0 ] && ok "no read crashed or hung" || bad "a read crashed or hung"
[ "$files" -gt 10 ] || bad "only $files files: the run didn't rotate enough"

journalctl -o json --all -q > final.json || bad "journalctl final"
# Every snapshot entry is in the final view, field for field.
jq -n --slurpfile final final.json '
  ($final | map({key: .__CURSOR, value: .}) | from_entries) as $m
  | [inputs | select($m[.__CURSOR] != .) | .__CURSOR] | length' s.*.json a.*.json > wrong.txt
wrong=$(cat wrong.txt)
total=$(cat s.*.json a.*.json | wc -l)
[ "$wrong" = 0 ] && ok "$total snapshot entries, all in journalctl's view with the same fields" \
  || bad "$wrong of $total snapshot entries differ from journalctl's view"

# Each writer's line numbers, as {w, n}: ANSI lines arrive as byte arrays.
numbers='[.[] | .MESSAGE | if type == "array" then implode else . end | strings
  | select(test("^w[0-9] ")) | split(" ") | {w: .[0], n: (.[1] | tonumber)}]'
# Gaps: per writer, a clean snapshot's numbers are consecutive. A number
# may repeat: journald can write an entry twice across a rotation, and
# journalctl shows both (the final checks below count them).
gaps=0 noisy=0 clean=0
for snap in s.*.json a.*.json; do
  if [ -s "${snap%.json}.err" ]; then noisy=$((noisy + 1)); continue; fi
  clean=$((clean + 1))
  g=$(jq -rs "$numbers"' | group_by(.w)
    | map(map(.n) | select((max - min + 1) != (unique | length))) | length' "$snap")
  [ "$g" = 0 ] || { gaps=$((gaps + 1)); echo "  gap in $snap"; }
done
[ "$gaps" = 0 ] && ok "no gaps in $clean clean snapshots" || bad "$gaps snapshots with gaps"
errs=$(cat ./*.err 2>/dev/null | sort | uniq -c | sort -rn | head -5)
echo "  $noisy snapshots reported an error${errs:+; the errors most often reported by any read:}"
[ -n "$errs" ] && echo "$errs" | sed 's/^/          /'

journalctl -o json --all -q SYSLOG_IDENTIFIER=stress | grep -v '"MESSAGE":"ready"' > fe.json
grep -v '"MESSAGE":"ready"' follow.all > fa.json
n=$(wc -l < fe.json)
distinct=$(jq -rs "$numbers"' | unique | length' fe.json)
[ "$distinct" = $((4 * lines)) ] || bad "journalctl has $distinct of $((4 * lines)) lines"
[ "$n" = "$distinct" ] || echo "  note  journald wrote $((n - distinct)) lines twice; journalctl shows both"
# The one divergence allowed (FINDINGS, declined): when a rotation cuts
# journald off while it links an entry, the entry is in its file's entry
# list but not in every field's, and journald writes it again as the next
# file's first. journalctl's match reads the field's list and shows only
# the second copy; the follow matches by the entry's items and shows both.
# Such an extra must be in journalctl's unfiltered view, and its twin
# (same seqnum id, boot, times and hash) in the matched one.
jq -c -n --slurpfile fe fe.json --slurpfile all final.json '
  def twin: .__CURSOR | sub(";i=[0-9a-f]+"; "");
  ($fe | map({key: .__CURSOR, value: true}) | from_entries) as $shown
  | ($fe | map({key: twin, value: true}) | from_entries) as $twins
  | ($all | map({key: .__CURSOR, value: true}) | from_entries) as $real
  | inputs
  | if $shown[.__CURSOR] or ($real[.__CURSOR] | not) or ($twins[twin] | not) then . else empty end' \
  fa.json > fa.kept.json
extra=$(($(wc -l < fa.json) - $(wc -l < fa.kept.json)))
# The filter above would hide a first copy printed twice: no cursor twice.
twice=$(jq -r .__CURSOR fa.json | sort | uniq -d | wc -l)
[ "$twice" = 0 ] || bad "follow printed $twice entries twice"
[ "$extra" = 0 ] || echo "  note  the follow showed $extra first copies of entries written again across a rotation that journalctl's match doesn't list"
if out=$(/pj compare fe.json fa.kept.json); then ok "follow: $(echo "$out" | tail -1)"; else bad "follow:"; echo "$out" | head -5; fi
[ -s follow.err ] && sort follow.err | uniq -c | sort -rn | head -3 | sed 's/^/  follow stderr: /'

journalctl -o json --all -q -n 300 > e.json
timeout 60 /pj tail --release "$rel" -n 300 > a.json 2> a.err
if out=$(/pj compare e.json a.json); then ok "idle tail: $(echo "$out" | tail -1)"; else bad "idle tail:"; echo "$out" | head -5; fi
[ -s a.err ] && bad "idle tail reported errors: $(head -1 a.err)"
exit $fail
EOS
}

status=0
for rel in "${releases[@]}"; do
  c=${SBJ_PREFIX:-sbj}-stress-${rel/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  # Capped: the writers run flat out.
  docker run -d --name "$c" --memory 4g --cpus 2 -v "$bin:/pj:ro" "ubuntu:$rel" sleep 7200 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" "$lines" < <(inner) || status=1
done
exit $status
