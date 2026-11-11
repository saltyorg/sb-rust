#!/usr/bin/env bash
# sb-journal against journalctl, in a container per supported Ubuntu
# release, each with that release's own systemd (journald, journalctl,
# systemd-journal-remote). Unprivileged containers: no systemd as PID 1.
#
#   [SBJ_BIN=binary] crates/journal/tests/compare.sh [RELEASE | IMAGE=RELEASE]...
#
# Default: every row of rules.rs RELEASES (`journal releases`).
# IMAGE=RELEASE runs another image under a row's rules, as a canary: for
# example 26.10=26.04 reads 26.10's journalctl with 26.04's rules (the
# example gets `--release 26.04`), so a change there shows before the
# next LTS needs its row. Shared checks are in checks.sh; files from older
# releases are compat.sh's; following is follow.sh's.
#
# Fixtures: gen-export's entries written by systemd-journal-remote under
# each writer variant the release supports (compact, keyed hash, xz, lz4,
# zstd), compared whole, by `-u demo.service` (last 500, one page back,
# every page forward, the more_* flags, Difference 18's cursors), and by
# field matches; the default variant's pread counts against their bounds
# (success criterion 5). Boots: two boots in two files, their clocks
# overlapping or apart, and overlapping from two source machines
# (`_MACHINE_ID=`, 26.04's tail-entry machine), paged both ways
# (decision 11, Difference 20).
# Live: the release's systemd-journald run standalone, compared by tail.
# The coredump row: systemd-coredump installed, its user's coredump
# entries in the local journal, by `-u` (decisions 1 and 4).
# Containers (sbj-<image>, or $SBJ_PREFIX-<image>) are removed on exit.
# Needs Docker and the musl target.
set -euo pipefail

# The default variant's pread counts may not exceed these (success
# criterion 5; tasks/journal/plan.md, "The pread bound"):
# `-u demo.service`, 500 entries, the tail, the page before it and the first page forward.
# The figures were the same in every run (four per release; journal-remote
# keys each file's hashes at random, so a hash-chain collision could add
# a pread): the bounds are 26.04's, the highest, plus 1%.
PREADS_TAIL_MAX=1728
PREADS_BACK_MAX=2417
PREADS_FORWARD_MAX=1765

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
want=$3
max_tail=$4
max_back=$5
max_forward=$6
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd systemd-journal-remote >/dev/null 2>&1
# shellcheck source=checks.sh
. /checks.sh
remote=$(first_of /usr/lib/systemd/systemd-journal-remote /lib/systemd/systemd-journal-remote)
echo "$image ($rel rules): $(journalctl --version | head -1)"
coredump_uid
echo "  note  systemd-coredump user: ${cuid:-none}"
systemd_check "$image" "$rel" "$want"
mkdir -p /fx && cd /fx || exit 2
/sbj gen-export --entries 3000 > in.export
while read -r name env; do
  rm -f "$name.journal"
  # shellcheck disable=SC2086 # the variant's variables, word-split on purpose
  env $env "$remote" --output="/fx/$name.journal" - < in.export >/dev/null 2>&1 || { bad "$name: journal-remote failed"; continue; }
  f=/fx/$name.journal
  integrity "$name ($env)" --file "$f"
  fixture_checks "$name" "$rel" "--file $f" "--file $f" 1
  if [ "$name" = default ]; then
    echo "  note  default preads: tail $preads_tail, back $preads_back, forward $preads_forward"
    for p in "tail $preads_tail $max_tail" "back $preads_back $max_back" "forward $preads_forward $max_forward"; do
      read -r page got most <<<"$p"
      if [ -n "$got" ] && [ -n "$most" ] && [ "$got" -le "$most" ]; then
        ok "default preads $page: $got, at most $most"
      else
        bad "default preads $page: ${got:-none}, over the bound ${most:-none}"
      fi
    done
  fi
done <<'VARIANTS'
default SB_VARIANT=default
regular SYSTEMD_JOURNAL_COMPACT=0
unkeyed SYSTEMD_JOURNAL_KEYED_HASH=0
xz SYSTEMD_JOURNAL_COMPRESS=XZ
lz4 SYSTEMD_JOURNAL_COMPRESS=LZ4
zstd SYSTEMD_JOURNAL_COMPRESS=ZSTD
oldstyle SYSTEMD_JOURNAL_COMPACT=0 SYSTEMD_JOURNAL_KEYED_HASH=0 SYSTEMD_JOURNAL_COMPRESS=LZ4
VARIANTS

# The boot fixtures need the machine id their headers carry.
systemd-machine-id-setup >/dev/null 2>&1
boot_checks "$rel" "$remote"

# Live: the release's own journald, run standalone.
mkdir -p /var/log/journal
jd=$(first_of /usr/lib/systemd/systemd-journald /lib/systemd/systemd-journald)
"$jd" >/dev/null 2>&1 &
sleep 2
journalctl --flush >/dev/null 2>&1 || bad "journalctl --flush"
# The flush must at least have created the persistent journal. Run
# standalone, journald may keep writing to /run; both readers read both
# directories, so the "local files" check still compares the same
# entries.
mid=$(cat /etc/machine-id)
[ -f "/var/log/journal/$mid/system.journal" ] || bad "flush: no persistent system.journal"
for i in $(seq 1 300); do
  case $((i % 4)) in
    0) printf '\033[32mgreen %s\033[0m\n' "$i" ;;
    1) printf 'long %s %s\n' "$i" "$(head -c 3000 /dev/zero | tr '\0' x)" ;;
    *) printf 'plain %s\n' "$i" ;;
  esac | systemd-cat -t sb-live -p $((i % 8))
done
# Both readers must see the same journal: wait until journald has taken
# in every line (under load it lags the writers), then a moment more for
# its own entries.
for _ in $(seq 1 120); do
  [ "$(journalctl -q -t sb-live -o cat | grep -cE '^(plain|long|.\[32mgreen) [0-9]+')" -ge 300 ] && break
  sleep 0.5
done
sleep 1
journalctl -o json --all -q -n 500 > e.json || bad "journalctl live"
sbj_tail "$rel" -n 500 > a.json || bad "sbj live"
check "live journald last 500 (local files)" e.json a.json
journalctl -o json --all -q -n 200 SYSLOG_IDENTIFIER=sb-live > e.json || bad "journalctl live identifier"
sbj_tail "$rel" -n 200 --match SYSLOG_IDENTIFIER=sb-live > a.json || bad "sbj live identifier"
check "live journald by identifier" e.json a.json

# Decision 4's coredump row: systemd-coredump installed, and the fixture
# with half its uid-1000 coredump decoys logged by that user, in the
# local journal. 26.04's journalctl takes that user's coredumps or
# root's, and sb-journal given --coredump-uid the same; 22.04's and
# 24.04's take root's only.
apt-get install -y -qq systemd-coredump >/dev/null 2>&1 || bad "coredump row: systemd-coredump doesn't install"
coredump_uid
if [ -z "$cuid" ] || [ "$cuid" = 0 ]; then
  bad "coredump row: no systemd-coredump user (${cuid:-none}); the local rule isn't checked"
else
  /sbj gen-export --entries 3000 --coredump-uid "$cuid" > cd.export
  "$remote" --output=/fx/coredump.journal - < cd.export >/dev/null 2>&1 || bad "coredump row: journal-remote failed"
  cp /fx/coredump.journal "/var/log/journal/$mid/remote-coredump.journal" || bad "coredump row: copy the fixture"
  journalctl -u demo.service -o json --all -q > e.json || bad "coredump row: journalctl -u"
  n=$(/sbj select e.json --coredumps-of "$cuid" | grep -c '^{')
  if [ "$want" -ge 257 ]; then
    [ "$n" -gt 0 ] || bad "coredump row: journalctl $want shows none of uid $cuid's coredumps"
  else
    [ "$n" = 0 ] || bad "coredump row: journalctl $want shows $n of uid $cuid's coredumps"
  fi
  sbj_tail "$rel" -u demo.service -n 100000000 > a.json || bad "coredump row: sbj -u"
  check "coredump row, -u with the systemd-coredump user's uid $cuid ($n of its coredumps)" e.json a.json
fi
files /run/log/journal
files /var/log/journal
exit $fail
EOS
}

status=0
for arg in "${releases[@]}"; do
  image=${arg%%=*}
  rel=${arg#*=}
  want=$("$bin" releases | awk -v r="$rel" '$1 == r { print $2 }')
  if [ -z "$want" ]; then
    echo "$arg: $rel is not a row of rules.rs RELEASES"
    status=1
    continue
  fi
  c=${SBJ_PREFIX:-sbj}-${image/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 4g --cpus 2 -v "$bin:/sbj:ro" \
    -v "$PWD/crates/journal/tests/checks.sh:/checks.sh:ro" "ubuntu:$image" sleep 3600 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" "$image" "$want" \
    "$PREADS_TAIL_MAX" "$PREADS_BACK_MAX" "$PREADS_FORWARD_MAX" < <(inner) || status=1
done
exit $status
