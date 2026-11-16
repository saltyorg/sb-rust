#!/usr/bin/env bash
# `sb setup`'s container matrix (ops spec, Testing Strategy; module build
# M82), beside gha-matrix.sh: in a throwaway ubuntu:<release> container per
# release setup supports, one at a time,
#
#   - systemd installed first, for its `systemd-detect-virt`, which a
#     server has and the image doesn't;
#   - `sb setup -b master` run as root with no terminal on a fresh
#     container (no /srv/git/saltbox), so the clone is Saltbox's public
#     repository;
#   - then `sb setup` again, which the guard refuses, and `sb setup
#     --force` over the checkout, which fetches and resets it;
#
# then the checks: the checkout on master, accounts.yml and settings.yml
# root:root:600, `/srv/ansible/venv/bin/ansible --version`,
# `/usr/local/bin/uv`, saltbox.fact running, and the locale set.
#
#   SBS_SB=sb [SBS_PREFIX=name] [SBS_DOCKER_OPTS=options] \
#     crates/ops/tests/setup-matrix.sh [RELEASE]...
#
# Default releases: 24.04 26.04 (setup's list). SBS_SB is an sb built
# without the `testing` feature (`cargo build --release -p sb`) on a glibc
# no newer than the oldest release's.
#
# Each container, $SBS_PREFIX-<release>-<pid> (sbsetup-... by default), is
# removed on exit and touches no other container or image. Docker owns
# its cgroup, so a systemd scope around this script doesn't cap it: it
# gets `--cpus 4 --memory 4g` with no swap and a pids limit, and
# SBS_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus` to
# pin its cores. Inside it sb reaches Ubuntu's apt mirrors, GitHub (the
# Saltbox repository, uv, Python and saltbox.fact releases), the release
# metadata proxy and PyPI, as setup does on a server; the host's own
# trees are never mounted. Logs go to target/setup-matrix/<release>/; the
# last line per release is its result.
set -euo pipefail
[ -x "${SBS_SB:-}" ] || { echo "setup-matrix.sh: SBS_SB must name an executable" >&2; exit 2; }
sb=$(realpath "$SBS_SB")
cd "$(dirname "$0")/../../.."
prefix=${SBS_PREFIX:-sbsetup}
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "setup-matrix.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
out=$PWD/target/setup-matrix

container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Runs inside the container, as root, from /sbtest/out/inner.sh with no
# stdin: $1 the release. sb is /sbtest/sb. Its last line is
# `RESULT: <release> ...: passed; ...`.
inner() {
  cat <<'EOS'
set -euo pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
logs=/sbtest/out
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
apt-get update -qq >/dev/null
apt-get install -y -qq ca-certificates systemd >"$logs/packages.log" 2>&1
install -m 0755 /sbtest/sb /usr/local/bin/sb
root=/srv/git/saltbox

# A fresh server: setup clones. No terminal (setsid), no stdin.
status=0
setsid -w /usr/local/bin/sb setup -b master >"$logs/setup.log" 2>&1 </dev/null || status=$?
cp -r /var/log/sb "$logs/sb-log" 2>/dev/null || true
[ "$status" = 0 ] || { tail -n 60 "$logs/setup.log" >&2; fail "sb setup exited $status"; }
pass "sb setup on a fresh container"

[ -d "$root/.git" ] && [ ! -L "$root" ] || fail "no checkout at $root"
branch=$(git -C "$root" rev-parse --abbrev-ref HEAD)
[ "$branch" = master ] || fail "the checkout is on $branch"
for name in accounts.yml settings.yml; do
  got=$(stat -c '%U:%G:%a' "$root/$name")
  [ "$got" = "root:root:600" ] || fail "$name is $got"
done
ansible=$(/srv/ansible/venv/bin/ansible --version) || fail "the venv's ansible"
ansible=${ansible%%$'\n'*}
uv=$(/usr/local/bin/uv --version) || fail "uv"
fact=$("$root/ansible_facts.d/saltbox.fact" --version) || fail "saltbox.fact"
grep -q '^LANG=en_US.UTF-8' /etc/default/locale || fail "LANG not set in /etc/default/locale"
pass "the checkout, the configs, the venv, uv, saltbox.fact and the locale"

# Again: the guard refuses an existing installation, nothing run.
status=0
setsid -w /usr/local/bin/sb setup >"$logs/again.log" 2>&1 </dev/null || status=$?
[ "$status" = 1 ] || fail "a second sb setup exited $status"
grep -q "setup already exists at $root" "$logs/again.log" || fail "the guard's text is missing"
pass "the guard refuses a second setup"

# --force over the checkout: fetched and reset to master.
status=0
setsid -w /usr/local/bin/sb setup --force >"$logs/force.log" 2>&1 </dev/null || status=$?
[ "$status" = 0 ] || { tail -n 60 "$logs/force.log" >&2; fail "sb setup --force exited $status"; }
grep -q "Saltbox repository updated (master)" "$logs/force.log" || fail "--force didn't update the checkout"
pass "sb setup --force over the checkout"

# shellcheck disable=SC1091 # the container's own
echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed; $uv, $ansible, saltbox.fact $fact"
EOS
}

for rel in "${releases[@]}"; do
  rm -rf "${out:?}/$rel"
  mkdir -p "$out/$rel"
  inner >"$out/$rel/inner.sh"
  echo "setup-matrix.sh: $rel in $prefix-$rel-$$"
  # --rm and a bounded sleep: a run killed past its EXIT trap leaves no
  # container behind for long.
  # shellcheck disable=SC2086 # word splitting gives SBS_DOCKER_OPTS's options
  container=$(docker run -d --rm --init --name "$prefix-$rel-$$" \
    --label sb-rust.test=ops-setup-matrix \
    --cpus 4 --memory 4g --memory-swap 4g --pids-limit 4096 ${SBS_DOCKER_OPTS:-} \
    -v "$sb:/sbtest/sb:ro" \
    -v "$out/$rel:/sbtest/out" \
    "ubuntu:$rel" sleep 3h)
  status=0
  # An hour at most: a hung download ends the run, not only the 3 h sleep.
  timeout 3600 docker exec "$container" bash /sbtest/out/inner.sh "$rel" </dev/null 2>&1 \
    | tee "$out/$rel/summary.log" || status=$?
  docker rm -fv "$container" >/dev/null 2>&1 || true
  container=""
  [ "$status" = 0 ] || { echo "setup-matrix.sh: $rel failed; logs in $out/$rel" >&2; exit 1; }
  grep -q "^RESULT: $rel .*: passed;" "$out/$rel/summary.log" \
    || { echo "setup-matrix.sh: $rel ended without its result line; logs in $out/$rel" >&2; exit 1; }
done
echo "setup-matrix.sh: every release passed"
