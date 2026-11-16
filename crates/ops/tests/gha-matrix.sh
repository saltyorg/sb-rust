#!/usr/bin/env bash
# `sb gha`'s container matrix (ops spec, Testing Strategy, "gha matrix";
# module build M81), sb-go's test-gha.yml rebuilt for sb: in a throwaway
# ubuntu:<release> container per supported release, one at a time,
#
#   - a `runner` user with a passwordless sudo policy, as GitHub's hosted
#     runners have;
#   - `/srv/git` made and given to `runner`, and a synthetic Saltbox
#     checkout of runner's (its `.python-version`, `.uv-version`,
#     `requirements/requirements-saltbox.txt`, a default for each of the
#     six configs, a hooks script) linked at `/srv/git/saltbox`, as the
#     workflow links its workspace there;
#   - `sb gha` run as `runner`, not with sudo, so sb relaunches itself
#     through sudo and `SUDO_USER` is `runner`;
#
# then test-gha.yml's checks: `runner:runner:600` on accounts.yml and
# settings.yml (here all six configs), `/srv/ansible/venv/bin/ansible
# --version`, `/usr/local/bin/uv`, saltbox.fact running, the hooks script
# run, and the root still the symlink.
#
#   SBG_SB=sb SBG_SALTBOX=dir [SBG_PREFIX=name] [SBG_DOCKER_OPTS=options] \
#     crates/ops/tests/gha-matrix.sh [RELEASE]...
#
# Default releases: 22.04 24.04 26.04. SBG_SB is an sb built without the
# `testing` feature (`cargo build --release -p sb`; a test build talks to
# the fake GitHub only) on a glibc no newer than the oldest release's.
# SBG_SALTBOX holds a Saltbox checkout's `.python-version`, `.uv-version`
# and `requirements/requirements-saltbox.txt` (public GitHub's, never a
# host's /srv).
#
# Each container, $SBG_PREFIX-<release>-<pid> (sbgha-... by default), is
# removed on exit and touches no other container or image. Docker owns
# its cgroup, so a systemd scope around this script doesn't cap it: it
# gets `--cpus 4 --memory 4g` with no swap and a pids limit, and
# SBG_DOCKER_OPTS adds `docker run`
# options, such as `--cpuset-cpus` to pin its cores. Inside it sb reaches
# Ubuntu's apt mirrors, the release metadata proxy, GitHub (uv, Python and
# saltbox.fact releases) and PyPI, as gha does on Saltbox's CI; the host's
# own trees are never mounted. Logs go to target/gha-matrix/<release>/;
# the last line per release is its result.
set -euo pipefail
[ -x "${SBG_SB:-}" ] || { echo "gha-matrix.sh: SBG_SB must name an executable" >&2; exit 2; }
saltbox=${SBG_SALTBOX:-}
for file in .python-version .uv-version requirements/requirements-saltbox.txt; do
  [ -f "$saltbox/$file" ] || { echo "gha-matrix.sh: SBG_SALTBOX has no $file" >&2; exit 2; }
done
sb=$(realpath "$SBG_SB")
saltbox=$(realpath "$saltbox")
cd "$(dirname "$0")/../../.."
prefix=${SBG_PREFIX:-sbgha}
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "gha-matrix.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
out=$PWD/target/gha-matrix

container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Runs inside the container, as root, from /sbtest/out/inner.sh with no
# stdin: $1 the release. sb is /sbtest/sb, the Saltbox files under
# /sbtest/saltbox. Its last line is `RESULT: <release> ...: passed; ...`.
inner() {
  cat <<'EOS'
set -euo pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
logs=/sbtest/out
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
apt-get update -qq >/dev/null
apt-get install -y -qq ca-certificates git sudo >"$logs/packages.log" 2>&1
install -m 0755 /sbtest/sb /usr/local/bin/sb

# GitHub's hosted runner: a `runner` user who may sudo without a password.
useradd -m -s /bin/bash runner
printf 'runner ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/runner
chmod 0440 /etc/sudoers.d/runner
home=$(getent passwd runner | cut -d: -f6)

# The workflow's checkout, runner's, and the six configs' defaults.
work=$home/work/saltbox
configs="accounts.yml adv_settings.yml backup_config.yml hetzner_vlan.yml providers.yml settings.yml"
mkdir -p "$work/requirements" "$work/defaults" "$work/bin/git"
cp /sbtest/saltbox/.python-version /sbtest/saltbox/.uv-version "$work/"
cp /sbtest/saltbox/requirements/requirements-saltbox.txt "$work/requirements/"
for name in $configs; do printf '%s: seed\n' "$name" >"$work/defaults/$name.default"; done
printf 'user:\n  name: seed\n' >"$work/defaults/accounts.yml.default"
printf 'touch "%s/hooks-ran"\n' "$work" >"$work/bin/git/init-hooks"
git -C "$work" init -q -b master
git -C "$work" add .
git -C "$work" -c user.name=fixture -c user.email=fixture.invalid commit -q -m "test: saltbox fixture"
chown -R runner:runner "$home/work"
# test-gha.yml: /srv/git made with sudo and given to runner, the
# workspace linked as the Saltbox root.
mkdir -p /srv/git
chown -R runner:runner /srv/git
runuser -u runner -- ln -s "$work" /srv/git/saltbox

# `sb gha` as runner, with no SUDO_* of its own (runuser, as the
# runner's own shell would): sb relaunches through sudo. The run log
# stays in the container (its /var/log/sb), copied out below.
status=0
(cd "$work" && runuser -u runner -- env HOME="$home" /usr/local/bin/sb gha) \
  >"$logs/gha.log" 2>&1 </dev/null || status=$?
cp -r /var/log/sb "$logs/sb-log" 2>/dev/null || true
[ "$status" = 0 ] || { tail -n 60 "$logs/gha.log" >&2; fail "sb gha exited $status"; }
pass "sb gha as runner"

[ -L /srv/git/saltbox ] || fail "the Saltbox root is no longer a symlink"
for name in $configs; do
  got=$(stat -c '%U:%G:%a' "/srv/git/saltbox/$name")
  [ "$got" = "runner:runner:600" ] || fail "$name is $got"
done
pass "the six configs are runner:runner:600"
# test-gha.yml's checks and the role jobs' first steps, as runner (they
# run without sudo there).
as_runner() { (cd "$home" && runuser -u runner -- env HOME="$home" "$@"); }
as_runner /srv/ansible/venv/bin/python3 --version >/dev/null || fail "the venv's python as runner"
ansible=$(as_runner /srv/ansible/venv/bin/ansible --version) || fail "the venv's ansible as runner"
ansible=${ansible%%$'\n'*}
[ -x /usr/local/bin/uv ] || fail "no /usr/local/bin/uv"
uv=$(as_runner /usr/local/bin/uv --version) || fail "uv as runner"
as_runner /usr/local/bin/uv pip freeze --python /srv/ansible/venv/bin/python >"$logs/freeze.log" \
  || fail "uv pip freeze as runner"
grep -qi '^ansible' "$logs/freeze.log" || fail "the freeze lists no ansible"
fact=$(as_runner /srv/git/saltbox/ansible_facts.d/saltbox.fact --version) || fail "saltbox.fact as runner"
[ -e "$work/hooks-ran" ] || fail "the hooks script didn't run"
pass "the venv, uv, saltbox.fact and the hooks"

# shellcheck disable=SC1091 # the container's own
echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed; $uv, $ansible, saltbox.fact $fact"
EOS
}

for rel in "${releases[@]}"; do
  rm -rf "${out:?}/$rel"
  mkdir -p "$out/$rel"
  inner >"$out/$rel/inner.sh"
  echo "gha-matrix.sh: $rel in $prefix-$rel-$$"
  # --rm and a bounded sleep: a run killed past its EXIT trap leaves no
  # container behind for long.
  # shellcheck disable=SC2086 # word splitting gives SBG_DOCKER_OPTS's options
  container=$(docker run -d --rm --init --name "$prefix-$rel-$$" \
    --label sb-rust.test=ops-gha-matrix \
    --cpus 4 --memory 4g --memory-swap 4g --pids-limit 4096 ${SBG_DOCKER_OPTS:-} \
    -v "$sb:/sbtest/sb:ro" \
    -v "$saltbox/.python-version:/sbtest/saltbox/.python-version:ro" \
    -v "$saltbox/.uv-version:/sbtest/saltbox/.uv-version:ro" \
    -v "$saltbox/requirements/requirements-saltbox.txt:/sbtest/saltbox/requirements/requirements-saltbox.txt:ro" \
    -v "$out/$rel:/sbtest/out" \
    "ubuntu:$rel" sleep 3h)
  status=0
  # An hour at most: a hung download ends the run, not only the 3 h sleep.
  timeout 3600 docker exec "$container" bash /sbtest/out/inner.sh "$rel" </dev/null 2>&1 \
    | tee "$out/$rel/summary.log" || status=$?
  docker rm -fv "$container" >/dev/null 2>&1 || true
  container=""
  [ "$status" = 0 ] || { echo "gha-matrix.sh: $rel failed; logs in $out/$rel" >&2; exit 1; }
  grep -q "^RESULT: $rel .*: passed;" "$out/$rel/summary.log" \
    || { echo "gha-matrix.sh: $rel ended without its result line; logs in $out/$rel" >&2; exit 1; }
done
echo "gha-matrix.sh: every release passed"
