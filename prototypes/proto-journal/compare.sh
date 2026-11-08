#!/usr/bin/env bash
# proto-journal against journalctl, in a container per supported Ubuntu
# release, each with that release's own systemd (journald, journalctl,
# systemd-journal-remote). Unprivileged containers: no systemd as PID 1.
#
#   prototypes/proto-journal/compare.sh [22.04 24.04 26.04]
#
# Fixtures: gen-export's entries written by systemd-journal-remote under
# each writer variant the release supports (compact, keyed hash, xz, lz4,
# zstd), compared whole, by `-u demo.service`, one page back, and by
# field matches. Live: the release's systemd-journald run standalone,
# compared by tail and by follow across a rotation.
# Containers are removed on exit. Needs Docker and the musl target.
set -euo pipefail

cd "$(dirname "$0")/../.."
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)

cargo build -q --release --target x86_64-unknown-linux-musl -p proto-journal
bin=$PWD/target/x86_64-unknown-linux-musl/release/proto-journal

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
rel=$1
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd systemd-journal-remote >/dev/null 2>&1
remote=$(ls /usr/lib/systemd/systemd-journal-remote /lib/systemd/systemd-journal-remote 2>/dev/null | head -1)
echo "$rel: $(journalctl --version | head -1)"
fail=0
bad() { # what failed
  fail=1
  echo "  FAIL  $1"
}
check() { # name expected actual
  # An empty expected side would pass against an empty actual one: every
  # check here is built to have entries, so empty means the harness broke.
  if [ ! -s "$2" ]; then
    bad "$1: no expected entries"
  elif out=$(/pj compare "$2" "$3"); then
    echo "  ok    $1: $(echo "$out" | tail -1)"
  else
    fail=1
    echo "  FAIL  $1:"; echo "$out" | sed 's/^/          /'
  fi
}
mkdir -p /fx && cd /fx
/pj gen-export --entries 3000 > in.export
while read -r name env; do
  rm -f "$name.journal"
  # shellcheck disable=SC2086 # the variant's variables, word-split on purpose
  env $env "$remote" --output="/fx/$name.journal" - < in.export >/dev/null 2>&1 || { bad "$name: journal-remote failed"; continue; }
  f=/fx/$name.journal
  hc=$(/pj hash-check --file "$f") || bad "$name: hash-check"
  echo " $name ($env): $(echo "$hc" | head -1 | sed 's/^[^:]*: //')"
  dc=$(/pj decode-all --file "$f") || bad "$name: decode-all"
  echo "  $(echo "$dc" | sed 's/, [0-9.]*m\?s$//')"
  echo "$dc" | grep -q ' 0 errors' || bad "$name: decode errors"
  journalctl --file "$f" -o json --all -q > e.json || bad "$name: journalctl whole file"
  /pj tail --release "$rel" --file "$f" -n 100000000 > a.json || bad "$name: pj whole file"
  check "$name whole file" e.json a.json
  journalctl --file "$f" -u demo.service -o json --all -q -n 500 > e.json || bad "$name: journalctl -u"
  /pj tail --release "$rel" --file "$f" -u demo.service -n 500 > a.json || bad "$name: pj -u"
  check "$name -u last 500" e.json a.json
  cur=$(head -1 e.json | sed 's/.*"__CURSOR":"\([^"]*\)".*/\1/')
  journalctl --file "$f" -u demo.service -o json --all -q -n 1000 | head -n -500 > e.json || bad "$name: journalctl page"
  /pj tail --release "$rel" --file "$f" -u demo.service -n 500 --before "$cur" > a.json || bad "$name: pj page"
  check "$name -u page back" e.json a.json
  # AND across fields, OR within one: _UID=0 co-occurs with both priorities.
  journalctl --file "$f" -o json --all -q -n 300 _UID=0 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl and/or"
  /pj tail --release "$rel" --file "$f" -n 300 --match _UID=0 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: pj and/or"
  check "$name field match, AND and OR" e.json a.json
  # Only an OR on one field: no single-value term to drive the walk.
  journalctl --file "$f" -o json --all -q -n 300 PRIORITY=3 PRIORITY=5 > e.json || bad "$name: journalctl or"
  /pj tail --release "$rel" --file "$f" -n 300 --match PRIORITY=3 --match PRIORITY=5 > a.json || bad "$name: pj or"
  check "$name field match, OR only" e.json a.json
  # A value carried twice by its entries: walks its entry array, which
  # systemd 249 writes with each such entry twice.
  journalctl --file "$f" -o json --all -q -n 300 SAME=again > e.json || bad "$name: journalctl repeated value"
  # Whether this writer kept the repeated item (journalctl then prints it
  # twice). journal-remote 249 was found not to, so the matrix doesn't
  # exercise repeated array links; file.rs's crafted-file tests do.
  if grep -q '"SAME":\["again","again"\]' e.json; then
    echo "  note  $name: the writer kept the repeated item"
  fi
  /pj tail --release "$rel" --file "$f" -n 300 --match SAME=again > a.json || bad "$name: pj repeated value"
  check "$name field match, repeated value" e.json a.json
done <<'VARIANTS'
default SB_VARIANT=default
regular SYSTEMD_JOURNAL_COMPACT=0
unkeyed SYSTEMD_JOURNAL_KEYED_HASH=0
xz SYSTEMD_JOURNAL_COMPRESS=XZ
lz4 SYSTEMD_JOURNAL_COMPRESS=LZ4
zstd SYSTEMD_JOURNAL_COMPRESS=ZSTD
oldstyle SYSTEMD_JOURNAL_COMPACT=0 SYSTEMD_JOURNAL_KEYED_HASH=0 SYSTEMD_JOURNAL_COMPRESS=LZ4
VARIANTS

# Live: the release's own journald, run standalone.
systemd-machine-id-setup >/dev/null 2>&1
mkdir -p /var/log/journal
jd=$(ls /usr/lib/systemd/systemd-journald /lib/systemd/systemd-journald 2>/dev/null | head -1)
"$jd" >/dev/null 2>&1 &
sleep 2
journalctl --flush >/dev/null 2>&1 || bad "journalctl --flush"
# The flush must have moved journald to persistent storage, or the
# "local files" check below reads only the runtime journal.
mid=$(cat /etc/machine-id)
[ -f "/var/log/journal/$mid/system.journal" ] || bad "flush: no persistent system.journal"
for i in $(seq 1 300); do
  case $((i % 4)) in
    0) printf '\033[32mgreen %s\033[0m\n' "$i" ;;
    1) printf 'long %s %s\n' "$i" "$(head -c 3000 /dev/zero | tr '\0' x)" ;;
    *) printf 'plain %s\n' "$i" ;;
  esac | systemd-cat -t sb-live -p $((i % 8))
done
sleep 1
journalctl -o json --all -q -n 500 > e.json || bad "journalctl live"
/pj tail --release "$rel" -n 500 > a.json || bad "pj live"
check "live journald last 500 (local files)" e.json a.json
journalctl -o json --all -q -n 200 SYSLOG_IDENTIFIER=sb-live > e.json || bad "journalctl live identifier"
/pj tail --release "$rel" -n 200 --match SYSLOG_IDENTIFIER=sb-live > a.json || bad "pj live identifier"
check "live journald by identifier" e.json a.json

journalctl -f -n 0 -o json --all -q SYSLOG_IDENTIFIER=sb-follow > fe.all 2>/dev/null &
jf=$!
/pj tail --release "$rel" -f -n 0 --match SYSLOG_IDENTIFIER=sb-follow > fa.all 2>fa.err &
pf=$!
# `-n 0` seeks to the end once the follower is up: write markers until
# both have printed one, so neither misses the numbered entries.
for _ in $(seq 1 100); do
  echo "ready" | systemd-cat -t sb-follow
  grep -q '"MESSAGE":"ready"' fe.all 2>/dev/null && grep -q '"MESSAGE":"ready"' fa.all 2>/dev/null && break
  sleep 0.2
done
for i in $(seq 1 200); do
  echo "follow $i" | systemd-cat -t sb-follow
  if [ "$i" = 100 ]; then
    before=$(find "/var/log/journal/$mid" -name 'system@*.journal' | wc -l)
    journalctl --rotate >/dev/null 2>&1 || bad "journalctl --rotate"
    after=$(find "/var/log/journal/$mid" -name 'system@*.journal' | wc -l)
    # The follow check is "across a rotation" only if a file was archived.
    [ "$after" -gt "$before" ] || bad "rotate: no file archived ($before -> $after)"
  fi
done
sleep 3
kill "$jf" "$pf" 2>/dev/null
wait "$jf" "$pf" 2>/dev/null
grep '"MESSAGE":"follow ' fe.all > fe.json; grep '"MESSAGE":"follow ' fa.all > fa.json
# Both followers must have every numbered entry: the same count alone
# would also hold if both had missed some.
n=$(wc -l < fe.json)
[ "$n" = 200 ] || bad "follow: journalctl -f printed $n of 200"
check "live follow across a rotation ($n entries)" fe.json fa.json
[ -s fa.err ] && sed 's/^/  follow stderr: /' fa.err
ls /run/log/journal/*/ /var/log/journal/*/ 2>/dev/null | sed 's/^/  file: /'
exit $fail
EOS
}

status=0
for rel in "${releases[@]}"; do
  c=sbj-${rel/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" -v "$bin:/pj:ro" "ubuntu:$rel" sleep 3600 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" < <(inner) || status=1
done
exit $status
