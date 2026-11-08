#!/usr/bin/env bash
# Journal files from before an upgrade: a host upgraded from an older
# Ubuntu keeps its archived journal files, and journalctl reads them. Each
# OLD release's own systemd writes the files; each NEW (supported)
# release's journalctl then reads them, alone and merged with files its
# own systemd writes, next to proto-journal. Unprivileged containers: no
# systemd as PID 1.
#
#   prototypes/proto-journal/compat.sh [OLD... [-- NEW...]]
#
# Defaults: OLD 20.04 18.04 (systemd 245 and 237); NEW every row of
# query.rs RELEASES. A NEW of IMAGE=RELEASE is a canary, as in compare.sh.
#
# Stage 1, per OLD release: systemd-journal-remote writes gen-export's
# entries (compressed, and with --compress=no: the older journal-remote
# has no per-file compression or hash choice), and the release's
# systemd-journald, run standalone, writes a persistent journal across a
# rotation and is stopped cleanly, leaving archived and active files.
#
# Stage 2, per OLD and NEW pair:
# - each old fixture read alone (compare.sh's fixture checks);
# - an old fixture merged with a later generation of entries written by
#   NEW's journal-remote, read as one directory (`-D`);
# - an upgraded host: the old journal directory and machine id in place,
#   NEW's journald started on it and writing across a rotation, and the
#   local journal read by tail, `-u` and identifier.
#
# Containers (sbj-compat-..., or $SBJ_PREFIX-compat-...) are removed on
# exit; their files stay in target/journal-compat. Two runs in one tree
# must not overlap: they share both.
# Needs Docker and the musl target.
set -euo pipefail

cd "$(dirname "$0")/../.."
old=()
new=()
while [ $# -gt 0 ] && [ "$1" != -- ]; do
  old+=("$1")
  shift
done
[ $# -gt 0 ] && shift
new=("$@")
[ ${#old[@]} -gt 0 ] || old=(20.04 18.04)

# Capped, as every heavy build and run here is.
systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% \
  cargo build -q --release --target x86_64-unknown-linux-musl -p proto-journal
bin=$PWD/target/x86_64-unknown-linux-musl/release/proto-journal
[ ${#new[@]} -gt 0 ] || mapfile -t new < <("$bin" releases | cut -d' ' -f1)
checks=$PWD/prototypes/proto-journal/checks.sh
work=$PWD/target/journal-compat

containers=()
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# The packages and paths every release's container needs.
# shellcheck disable=SC2317 # called through run
setup() {
  cat <<'EOS'
set -u -o pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd systemd-journal-remote >/dev/null 2>&1
remote=$(ls /usr/lib/systemd/systemd-journal-remote /lib/systemd/systemd-journal-remote 2>/dev/null | head -1)
jd=$(ls /usr/lib/systemd/systemd-journald /lib/systemd/systemd-journald 2>/dev/null | head -1)
# shellcheck source=checks.sh
. /checks.sh
# The release's journald, standalone, until its persistent journal is up.
# Standalone, journald 237, 245 and 255 were seen to keep writing to /run
# after `journalctl --flush` until the next rotation. The flag a booted
# host's flush leaves makes them write to /var from the start, as after
# boot; the flush then has nothing to move.
start_journald() {
  mkdir -p /run/systemd/journal
  touch /run/systemd/journal/flushed
  "$jd" >/dev/null 2>&1 &
  jpid=$!
  sleep 2
  # Kept as a smoke test only: with the flag set it returns at once.
  journalctl --flush >/dev/null 2>&1 || bad "journalctl --flush"
  [ -f "/var/log/journal/$mid/system.journal" ] || bad "flush: no persistent system.journal"
}
# Entries like compare.sh's live ones: ANSI, long (compressed) and plain,
# with a rotation halfway.
write_entries() { # identifier count
  local i
  for i in $(seq 1 "$2"); do
    case $((i % 4)) in
      0) printf '\033[32mgreen %s\033[0m\n' "$i" ;;
      1) printf 'long %s %s\n' "$i" "$(head -c 3000 /dev/zero | tr '\0' x)" ;;
      *) printf 'plain %s\n' "$i" ;;
    esac | systemd-cat -t "$1" -p $((i % 8))
    if [ "$i" = $(($2 / 2)) ]; then
      rotate
    fi
  done
  sleep 1
}
# SIGUSR2 rotates on every release (systemd-journald(8)); 237's
# `journalctl --rotate` goes through PID 1 and fails in a container.
rotate() {
  local before after
  before=$(find "/var/log/journal/$mid" -name 'system@*.journal' | wc -l)
  kill -USR2 "$jpid"
  for _ in $(seq 1 50); do
    after=$(find "/var/log/journal/$mid" -name 'system@*.journal' | wc -l)
    [ "$after" -gt "$before" ] && return
    sleep 0.1
  done
  bad "rotate: no file archived ($before -> $after)"
}
# write_entries' numbered lines from one identifier in /var. Counted by
# line start: 249's journald was seen to split some 3000-byte lines from
# systemd-cat into several entries, a different number each run.
count_lines() { # identifier
  journalctl -D /var/log/journal -q -t "$1" -o cat --all | grep -c -E '^(plain|long|.\[32mgreen) [0-9]+'
}
files() { # dir: each journal file, as journalctl would meet it
  find "$1" -name '*.journal*' -type f | sort | sed 's|.*/|  file: |'
}
EOS
}

# Stage 1, in the old release's container: its files into /out.
# shellcheck disable=SC2317 # called through run
produce() {
  setup
  cat <<'EOS'
rel=$1
echo "$rel (writer): $(journalctl --version | head -1)"
mkdir -p /out/fx
/pj gen-export --entries 3000 > /tmp/in.export
for variant in compressed:--compress=yes plain:--compress=no; do
  name=${variant%%:*}
  "$remote" "${variant#*:}" --output="/out/fx/$name.journal" - < /tmp/in.export >/dev/null 2>&1 ||
    bad "$name: journal-remote failed"
  integrity "$rel $name" --file "/out/fx/$name.journal"
done
systemd-machine-id-setup >/dev/null 2>&1
mid=$(cat /etc/machine-id)
mkdir -p /var/log/journal
start_journald
write_entries sb-old 300
# A clean stop, as on the shutdown before an upgrade: the active file
# stays system.journal, for the newer journald to take over (it renames
# it to an archived .journal~ file, refusing to append to an older header).
kill -TERM "$jpid"
wait "$jpid" 2>/dev/null
n=$(count_lines sb-old)
[ "$n" = 300 ] || bad "old journald: $n of 300 lines in /var"
integrity "$rel live journal" --directory /var/log/journal
files /var/log/journal
cp -a /var/log/journal /out/journal || bad "copy out the journal"
# Last: stage 2 runs only for an old release whose machine id is there.
[ $fail = 0 ] && { cp /etc/machine-id /out/machine-id || bad "copy out the machine id"; }
exit $fail
EOS
}

# Stage 2, in a supported release's container, with stage 1's files at
# /old (read-only).
# shellcheck disable=SC2317 # called through run
upgrade() {
  setup
  cat <<'EOS'
was=$1
rel=$2
image=$3
want=$4
echo "$was files on $image ($rel rules): $(journalctl --version | head -1)"
systemd_check "$image" "$rel" "$want"
mkdir -p /fx /mix && cd /fx
for f in /old/fx/*.journal; do
  name="$was $(basename "$f" .journal)"
  integrity "$name" --file "$f"
  fixture_checks "$name" "$rel" "--file $f" "--file $f"
done
# Merged: a later generation of entries from this release's writer, and
# few enough that `-u` last 500 and the page back reach into the old file.
/pj gen-export --entries 300 --generation 1 > new.export
"$remote" --output=/mix/new.journal - < new.export >/dev/null 2>&1 || bad "merge: journal-remote failed"
cp /old/fx/compressed.journal /mix/old.journal || bad "merge: copy the old file"
integrity "$was+$rel merged" --directory /mix
fixture_checks "$was+$rel merged" "$rel" "-D /mix" "--directory /mix"
# Both files were read: the whole check had both generations.
n=$(journalctl -D /mix -q -o export | grep -c '^__CURSOR=')
[ "$n" = 3300 ] || bad "merge: journalctl read $n of 3300 entries"

# The upgraded host: the old journal and machine id, this release's
# journald on top.
cp /old/machine-id /etc/machine-id || bad "host: copy the machine id"
mid=$(cat /etc/machine-id)
mkdir -p /var/log/journal
cp -a "/old/journal/$mid" /var/log/journal/ || bad "host: copy the old journal"
# The old system.journal is already there, so start_journald's check
# proves nothing here: the new entries must be in /var.
start_journald
write_entries sb-new 200
# Stopped before reading, so nothing is written between the two sides.
kill -TERM "$jpid"
wait "$jpid" 2>/dev/null
# Every numbered line reached /var, and nothing stayed in /run.
n=$(count_lines sb-new)
[ "$n" = 200 ] || bad "new journald: $n of 200 lines in /var"
[ -z "$(ls /run/log/journal/*/ 2>/dev/null)" ] || bad "new journald: files left in /run"
integrity "$was->$rel host" --directory /var/log/journal
files /var/log/journal
journalctl -o json --all -q > e.json || bad "journalctl host whole"
/pj tail --release "$rel" -n 100000000 > a.json || bad "pj host whole"
check "$was->$rel host whole (local files)" e.json a.json
# 200 new entries and journald's own: the last 500 reach the old files.
journalctl -o json --all -q -n 500 > e.json || bad "journalctl host last 500"
/pj tail --release "$rel" -n 500 > a.json || bad "pj host last 500"
check "$was->$rel host last 500" e.json a.json
journalctl -o json --all -q -n 200 SYSLOG_IDENTIFIER=sb-old > e.json || bad "journalctl host old identifier"
/pj tail --release "$rel" -n 200 --match SYSLOG_IDENTIFIER=sb-old > a.json || bad "pj host old identifier"
check "$was->$rel host, old identifier" e.json a.json
journalctl -o json --all -q -n 400 SYSLOG_IDENTIFIER=sb-old SYSLOG_IDENTIFIER=sb-new > e.json || bad "journalctl host both identifiers"
/pj tail --release "$rel" -n 400 --match SYSLOG_IDENTIFIER=sb-old --match SYSLOG_IDENTIFIER=sb-new > a.json ||
  bad "pj host both identifiers"
check "$was->$rel host, both identifiers" e.json a.json
# `-u` on the local journal (not --file, so 26.04's coredump term applies
# as on a host): the old fixture in the machine's directory, with the
# merged generation beside it.
cp /old/fx/compressed.journal "/var/log/journal/$mid/remote-old.journal" || bad "host: copy the old fixture"
cp /mix/new.journal "/var/log/journal/$mid/remote-new.journal" || bad "host: copy the new fixture"
journalctl -u demo.service -o json --all -q -n 500 > e.json || bad "journalctl host -u"
/pj tail --release "$rel" -u demo.service -n 500 > a.json || bad "pj host -u"
check "$was->$rel host -u last 500 (local files)" e.json a.json
exit $fail
EOS
}

run() { # container image script args...
  local c=$1 image=$2 script=$3
  shift 3
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 4g --cpus 2 -v "$bin:/pj:ro" -v "$checks:/checks.sh:ro" "${mounts[@]}" \
    "ubuntu:$image" sleep 3600 >/dev/null
  docker exec -i "$c" bash -s -- "$@" < <("$script")
}

# Release numbers only: each names a directory the loop below deletes.
for o in "${old[@]}"; do
  [[ $o =~ ^[0-9]+\.[0-9]+$ ]] || { echo "$o: not a release"; exit 2; }
done

status=0
for o in "${old[@]}"; do
  rm -rf "${work:?}/$o"
  mkdir -p "$work/$o"
  mounts=(-v "$work/$o:/out")
  run "${SBJ_PREFIX:-sbj}-compat-${o/./}" "$o" produce "$o" || status=1
done
for o in "${old[@]}"; do
  [ -f "$work/$o/machine-id" ] || continue
  for n in "${new[@]}"; do
    image=${n%%=*}
    rel=${n#*=}
    want=$("$bin" releases | awk -v r="$rel" '$1 == r { print $2 }')
    if [ -z "$want" ]; then
      echo "$n: $rel is not a row of query.rs RELEASES"
      status=1
      continue
    fi
    mounts=(-v "$work/$o:/old:ro")
    run "${SBJ_PREFIX:-sbj}-compat-${o/./}-on-${image/./}" "$image" upgrade "$o" "$rel" "$image" "$want" || status=1
  done
done
exit $status
