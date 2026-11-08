#!/usr/bin/env bash
# proto-journal against journalctl, in a container per supported Ubuntu
# release, each with that release's own systemd (journald, journalctl,
# systemd-journal-remote). Unprivileged containers: no systemd as PID 1.
#
#   prototypes/proto-journal/compare.sh [RELEASE | IMAGE=RELEASE]...
#
# Default: every row of query.rs RELEASES (`proto-journal releases`).
# IMAGE=RELEASE runs another image under a row's rules, as a canary: for
# example 26.10=26.04 reads 26.10's journalctl with 26.04's `-u` rule, so
# a change there shows before the next LTS needs its row. Shared checks
# are in checks.sh; files from older releases are compat.sh's.
#
# Fixtures: gen-export's entries written by systemd-journal-remote under
# each writer variant the release supports (compact, keyed hash, xz, lz4,
# zstd), compared whole, by `-u demo.service`, one page back, and by
# field matches. Live: the release's systemd-journald run standalone,
# compared by tail and by follow across a rotation.
# Containers (sbj-<image>, or $SBJ_PREFIX-<image>) are removed on exit.
# Needs Docker and the musl target.
set -euo pipefail

cd "$(dirname "$0")/../.."
releases=("$@")

# Capped, as every heavy build and run here is.
systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% \
  cargo build -q --release --target x86_64-unknown-linux-musl -p proto-journal
bin=$PWD/target/x86_64-unknown-linux-musl/release/proto-journal
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
rel=$1
image=$2
want=$3
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd systemd-journal-remote >/dev/null 2>&1
remote=$(ls /usr/lib/systemd/systemd-journal-remote /lib/systemd/systemd-journal-remote 2>/dev/null | head -1)
echo "$image ($rel rules): $(journalctl --version | head -1)"
# shellcheck source=checks.sh
. /checks.sh
systemd_check "$image" "$rel" "$want"
mkdir -p /fx && cd /fx
/pj gen-export --entries 3000 > in.export
while read -r name env; do
  rm -f "$name.journal"
  # shellcheck disable=SC2086 # the variant's variables, word-split on purpose
  env $env "$remote" --output="/fx/$name.journal" - < in.export >/dev/null 2>&1 || { bad "$name: journal-remote failed"; continue; }
  f=/fx/$name.journal
  integrity "$name ($env)" --file "$f"
  fixture_checks "$name" "$rel" "--file $f" "--file $f"
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
# The flush must at least have created the persistent journal. Run
# standalone, journald may keep writing to /run until the rotation
# below; both readers read both directories, so the "local files" check
# still compares the same entries.
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
for arg in "${releases[@]}"; do
  image=${arg%%=*}
  rel=${arg#*=}
  want=$("$bin" releases | awk -v r="$rel" '$1 == r { print $2 }')
  if [ -z "$want" ]; then
    echo "$arg: $rel is not a row of query.rs RELEASES"
    status=1
    continue
  fi
  c=${SBJ_PREFIX:-sbj}-${image/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 4g --cpus 2 -v "$bin:/pj:ro" \
    -v "$PWD/prototypes/proto-journal/checks.sh:/checks.sh:ro" "ubuntu:$image" sleep 3600 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" "$image" "$want" < <(inner) || status=1
done
exit $status
