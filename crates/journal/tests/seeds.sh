#!/usr/bin/env bash
# The fuzz harness's seeds (examples/fuzz.rs; journal spec, Testing
# Strategy): gen-export's entries written by systemd-journal-remote 249
# (22.04) and 255 (24.04), four writer variants each, so eight files. They
# are regenerated for each fuzz run and never committed.
#
#   [SBJ_BIN=binary] crates/journal/tests/seeds.sh DIR
#
# DIR receives <release>-<variant>.journal and is created if missing; files
# already there are replaced. Then:
#
#   systemd-run --scope -q -p MemoryMax=4G -p CPUQuota=200% -p OOMPolicy=continue \
#     cargo run -q -p sb-journal --features testing --example fuzz -- \
#     --work DIR/work DIR/*.journal
#
# Containers (sbj-seeds-<image>, or $SBJ_PREFIX-seeds-<image>) are removed
# on exit. Needs Docker, and cross unless SBJ_BIN is set (example.sh
# builds the example in sb's build image).
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: crates/journal/tests/seeds.sh DIR" >&2
  exit 2
fi
mkdir -p "$1"
out=$(realpath -e "$1")
# The example: SBJ_BIN, or a build in sb's build image (example.sh), before
# the cd below, so relative paths work too.
# shellcheck source=example.sh
. "$(dirname "$0")/example.sh"
sbj_bin
cd "$(dirname "$0")/../../.."

containers=()
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() {
  for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# Runs inside the container, as root, with the binary at /sbj and DIR at
# /out.
inner() {
  cat <<'EOS'
set -eu -o pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq systemd-journal-remote >/dev/null 2>&1
remote=/usr/lib/systemd/systemd-journal-remote
[ -x "$remote" ] || remote=/lib/systemd/systemd-journal-remote
cd /tmp
/sbj gen-export --entries 300 > in.export
while read -r name env; do
  f=/out/$rel-$name.journal
  rm -f "$f"
  # shellcheck disable=SC2086 # the variant's variables, word-split on purpose
  env $env "$remote" --output="$f" - < in.export >/dev/null 2>&1 || { echo "$rel $name: journal-remote failed" >&2; exit 1; }
  echo "$rel $name: $(stat -c %s "$f") bytes"
done <<'VARIANTS'
default SB_VARIANT=default
unkeyed SYSTEMD_JOURNAL_KEYED_HASH=0
xz SYSTEMD_JOURNAL_COMPRESS=XZ
lz4 SYSTEMD_JOURNAL_COMPRESS=LZ4
VARIANTS
# The files belong to the caller, not to the container's root.
chown "$2" /out/"$rel"-*.journal
EOS
}

for rel in 22.04 24.04; do
  c=${SBJ_PREFIX:-sbj}-seeds-${rel/./}
  containers+=("$c")
  docker rm -f "$c" >/dev/null 2>&1 || true
  docker run -d --name "$c" --memory 2g --cpus 2 -v "$bin:/sbj:ro" -v "$out:/out" \
    "ubuntu:$rel" sleep 1800 >/dev/null
  docker exec -i "$c" bash -s -- "$rel" "$(id -u):$(id -g)" < <(inner)
done
