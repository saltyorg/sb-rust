#!/usr/bin/env bash
# python-env's live acceptance (python-env spec, Testing strategy, "Live";
# module build M21), sb-go's acceptance-script pattern
# (scripts/test-python-isolation-container.sh and
# test-deadsnakes-cleanup-container.sh) rebuilt for sb: in a throwaway
# ubuntu:<release> container per supported release, one at a time,
#
#   - `sb update-venv` from nothing (uv, libpq-dev, Python, the venv and
#     saltbox.fact installed), run from a hostile environment as sb-go's
#     script does; then again, up to date, nothing changed;
#   - sb-go's `update-venv` on that tree: up to date, nothing changed
#     (sb-go accepts an sb-built tree and fact);
#   - a `.python-version` bump, rebuilt on the new Python;
#   - saltbox.fact changed by one byte at its released version, repaired
#     by `sb update-venv` (the digest rule);
#   - `sb reinstall-venv` (the Python reused), `sb reinstall-python` (a new
#     Python release) and `sb reinstall-facts`;
#   - sb-go's `reinstall-venv`, then `sb update-venv` adopting sb-go's
#     generation unchanged, then sb-go's `update-venv` again;
#   - on 22.04, before the first run: deadsnakes' PPA added, its
#     python3.12, -dev and -venv installed, and a venv on it as the active
#     venv directory (sb-go's deadsnakes script); afterwards the packages
#     are gone, the PPA's source file sits in
#     /var/backups/sb/deadsnakes/<run id>/ and the legacy venv was adopted.
#
# After each run: the active venv is a symlink, one generation and one
# Python release are kept, the Saltbox user owns the trees, the `ansible`
# wrapper runs, and no `sb-python-*` scratch directory is left.
#
#   SBP_SB=sb SBP_SBGO=sb-go SBP_SALTBOX=dir [SBP_PREFIX=name] \
#     [SBP_PYTHON_FROM=version] [SBP_DOCKER_OPTS=options] \
#     crates/python-env/tests/live.sh [RELEASE]...
#
# Default releases: 22.04 24.04 26.04. SBP_SB is an sb built without the
# `testing` feature (`cargo build --release -p sb`; a test build talks to
# the fake GitHub only) on a glibc no newer than the oldest release's.
# SBP_SBGO is sb-go built from its repository. SBP_SALTBOX holds a Saltbox
# checkout's `.python-version`, `.uv-version` and
# `requirements/requirements-saltbox.txt` (public GitHub's, never a host's
# /srv). The first run uses SBP_PYTHON_FROM (3.12.13 by default, sb-go's
# script's), the bump the checkout's own `.python-version`.
#
# Each container, $SBP_PREFIX-<release>-<pid> (sbpyenv-... by default),
# is removed on exit and touches no other container or image. Docker owns
# its cgroup, so a systemd scope around this script doesn't cap it: it
# gets `--cpus 4 --memory 4g`, and SBP_DOCKER_OPTS adds `docker run`
# options, such as `--cpuset-cpus` to pin its cores. Inside it sb and
# sb-go reach the release metadata proxy, GitHub (uv, Python and
# saltbox.fact releases), PyPI, Ubuntu's apt mirrors and, on 22.04,
# deadsnakes' PPA; the host's own trees are never mounted. Logs go to
# target/python-env-live/<release>/; the last line per release is its
# result with the versions it ran.
set -euo pipefail
# The inputs are read against the caller's directory, before the cd.
for var in SBP_SB SBP_SBGO; do
  [ -x "${!var:-}" ] || { echo "live.sh: $var must name an executable" >&2; exit 2; }
done
saltbox=${SBP_SALTBOX:-}
for file in .python-version .uv-version requirements/requirements-saltbox.txt; do
  [ -f "$saltbox/$file" ] || { echo "live.sh: SBP_SALTBOX has no $file" >&2; exit 2; }
done
sb=$(realpath "$SBP_SB")
sbgo=$(realpath "$SBP_SBGO")
saltbox=$(realpath "$saltbox")
cd "$(dirname "$0")/../../.."
prefix=${SBP_PREFIX:-sbpyenv}
from=${SBP_PYTHON_FROM:-3.12.13}
to=$(tr -d '[:space:]' <"$saltbox/.python-version")
[[ $from =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && $to =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && $from != "$to" ]] || {
  echo "live.sh: SBP_PYTHON_FROM ($from) must be an exact version other than the checkout's ($to)" >&2
  exit 2
}
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "live.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
out=$PWD/target/python-env-live

container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Runs inside the container, as root, from /sbtest/out/inner.sh with no
# stdin (a command reading it can't end the script early): $1 the
# release, $2 and $3 the two Python versions. sb is /sbtest/sb, sb-go
# /sbtest/sb-go, the Saltbox files under /sbtest/saltbox. Its last line
# is `RESULT: <release> ...: passed; <versions>`.
inner() {
  cat <<'EOS'
set -euo pipefail
rel=$1 from=$2 to=$3
export DEBIAN_FRONTEND=noninteractive
logs=/sbtest/out
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
apt-get update -qq >/dev/null
apt-get install -y -qq ca-certificates git gcc >"$logs/packages.log" 2>&1

# The Saltbox user and checkout sb and sb-go read.
useradd -m seed
checkout=/srv/git/saltbox
mkdir -p "$checkout/requirements"
cp /sbtest/saltbox/.uv-version "$checkout/"
cp /sbtest/saltbox/requirements/requirements-saltbox.txt "$checkout/requirements/"
printf '%s\n' "$from" >"$checkout/.python-version"
printf 'user:\n  name: seed\n' >"$checkout/accounts.yml"
git -C "$checkout" init -q -b master
git -C "$checkout" add .
git -C "$checkout" -c user.name=fixture -c user.email=fixture.invalid commit -q -m "test: saltbox fixture"
# Saltbox's pre_tasks add the checkout to root's safe.directory, so root's
# git reads it once the Saltbox user owns it (repo spec, assumption 2).
git config --global --add safe.directory "$checkout"

# sb-go's hostile caller: config files in the cwd and variables that
# would send uv, pip and Python elsewhere (the isolation must drop them).
mkdir -p /ambient/nested
printf 'invalid = [' >/ambient/uv.toml
printf 'invalid = [' >/ambient/pyproject.toml
printf '0.0.0\n' >/ambient/.python-version
printf 'UV_PYTHON=/missing\n' >/ambient/nested/.env
printf 'raise RuntimeError("ambient json imported")\n' >/ambient/nested/json.py
hostile() {
  (cd /ambient/nested && env \
    UV_CONFIG_FILE=/ambient/missing.toml UV_CACHE_DIR=/ambient/missing-cache \
    UV_PYTHON=/ambient/missing-python UV_PYTHON_INSTALL_DIR=/ambient/missing-install \
    UV_INDEX_URL=https://invalid.example PIP_INDEX_URL=https://invalid.example \
    PYTHONHOME=/ambient/missing-python PYTHONPATH=/ambient/nested \
    VIRTUAL_ENV=/ambient/missing-venv CONDA_PREFIX=/ambient/missing-conda \
    ANSIBLE_CONFIG=/ambient/missing-ansible.cfg \
    NO_COLOR=1 SB_LOG_DIR=/sbtest/out/sb-log "$@")
}
n=0
# One run of sb or sb-go: its output in $logs/<n>-<name>.log, kept in
# $last for the checks; a failed run fails the release.
run() {
  local name=$1; shift
  n=$((n + 1))
  last="$logs/$n-$name.log"
  echo "== $n $name: $*" >"$last"
  local status=0
  hostile "$@" >>"$last" 2>&1 </dev/null || status=$?
  [ "$status" = 0 ] || { tail -n 40 "$last" >&2; fail "$name exited $status"; }
}
said() { grep -qF -- "$1" "$last" || fail "$(basename "$last") lacks \"$1\""; }

fact=$checkout/ansible_facts.d/saltbox.fact
python() { readlink -f /srv/ansible/venv/bin/python3; }
wrappers() { grep -lsx '# Managed by sb-go Saltbox venv' /usr/local/bin/* | sort | xargs -r sha256sum; }
# Everything a reconcile may change: the active generation, the trees'
# entries, the manifest, the wrappers' bytes and the fact.
state() {
  readlink /srv/ansible/venv
  ls /srv/ansible/releases /srv/python/releases
  sha256sum "$(readlink -f /srv/ansible/venv)/../manifest.json" "$fact"
  wrappers
}
count() { find "$1" -mindepth 1 -maxdepth 1 | wc -l; }
healthy() {
  [ -L /srv/ansible/venv ] || fail "the active venv is not a symlink"
  [ "$(count /srv/ansible/releases)" = 1 ] || fail "releases: $(ls /srv/ansible/releases)"
  [ "$(count /srv/python/releases)" = 1 ] || fail "python releases: $(ls /srv/python/releases)"
  [ "$(/srv/ansible/venv/bin/python3 -I --version)" = "Python $1" ] || fail "not Python $1"
  for tree in "$checkout" /srv/ansible /srv/python; do
    [ "$(stat -c %U "$tree")" = seed ] || fail "$tree is not seed's"
  done
  (cd / && env -i PATH=/usr/bin:/bin /usr/local/bin/ansible --version) >/dev/null || fail "the ansible wrapper"
  [ -z "$(find /tmp -maxdepth 1 -name 'sb-python-*' -print -quit)" ] || fail "scratch left in /tmp"
}

# 22.04: what the old sb and pre-uv sb-go left behind (sb-go's
# test-deadsnakes-cleanup-container.sh): deadsnakes' PPA, its Python 3.12
# and a venv on it at the active path, a real directory.
legacy=(python3.12 python3.12-dev python3.12-venv)
if [ "$rel" = 22.04 ]; then
  {
    apt-get install -y -qq software-properties-common
    add-apt-repository -y ppa:deadsnakes/ppa
    apt-get install -y -qq "${legacy[@]}"
  } >>"$logs/packages.log" 2>&1
  apt-cache policy python3.12 | grep -qF ppa.launchpadcontent.net/deadsnakes/ppa \
    || fail "python3.12 is not from the PPA"
  mkdir -p /srv/ansible
  python3.12 -m venv /srv/ansible/venv
  case $(/srv/ansible/venv/bin/python3 --version) in "Python 3.12."*) ;; *) fail "the legacy venv" ;; esac
fi

run sb-update-venv /sbtest/sb update-venv
said "saltbox.fact not found. Proceeding with update."
said "saltbox.fact installed successfully"
uv=$(uv --version | cut -d' ' -f2)
[ "$uv" = "$(cat "$checkout/.uv-version")" ] || fail "uv $uv"
[ "$(uvx --version | cut -d' ' -f2)" = "$uv" ] || fail "uvx $(uvx --version)"
[ "$(dpkg-query -W -f='${db:Status-Abbrev}' libpq-dev)" = "ii " ] || fail "libpq-dev"
healthy "$from"
fact_sum=$(sha256sum <"$fact")
if [ "$rel" = 22.04 ]; then
  said "Removed old deadsnakes Python packages"
  for pkg in "${legacy[@]}"; do
    [ "$(dpkg-query -W -f='${db:Status-Abbrev}' "$pkg" 2>/dev/null || true)" != "ii " ] || fail "$pkg remains"
  done
  [ -z "$(grep -rl deadsnakes /etc/apt/sources.list.d || true)" ] || fail "a deadsnakes source remains"
  moved=$(find /var/backups/sb/deadsnakes -mindepth 2 -name 'deadsnakes*')
  [ -n "$moved" ] || fail "the source was not moved aside"
  [ "$(stat -c %a "$(dirname "$moved")")" = 700 ] || fail "the backup is not 0700"
  pass "deadsnakes packages removed, its source moved aside; the legacy venv replaced by a generation link"
fi
pass "update-venv from nothing on Python $from, from a hostile environment"

before=$(state)
run sb-update-venv /sbtest/sb update-venv
said "Ansible virtual environment was already up to date"
said "saltbox.fact is up to date"
[ "$(state)" = "$before" ] || fail "a healthy tree changed"
run sb-go-update-venv /sbtest/sb-go update-venv -v
said "Ansible virtual environment was already up to date"
said "saltbox.fact is up to date"
[ "$(state)" = "$before" ] || fail "sb-go changed sb's tree"
pass "a second update-venv changes nothing; sb-go accepts sb's tree and fact"

printf '%s\n' "$to" >"$checkout/.python-version"
run sb-update-venv /sbtest/sb update-venv
said "Ansible virtual environment update required"
[ "$(readlink /srv/ansible/venv)" != "$(echo "$before" | head -1)" ] || fail "no new generation"
healthy "$to"
pass ".python-version bump to $to"

before=$(state)
printf 'x' >>"$fact"
run sb-update-venv /sbtest/sb update-venv
said "does not match its release"
said "saltbox.fact reinstalled successfully"
[ "$(sha256sum <"$fact")" = "$fact_sum" ] || fail "the fact was not restored"
[ "$(state)" = "$before" ] || fail "the repair changed more than the fact"
pass "a fact changed by one byte is repaired by update-venv"

link=$(readlink /srv/ansible/venv) py=$(python)
run sb-reinstall-venv /sbtest/sb reinstall-venv
[ "$(readlink /srv/ansible/venv)" != "$link" ] || fail "reinstall-venv kept the generation"
[ "$(python)" = "$py" ] || fail "reinstall-venv replaced the Python"
healthy "$to"
link=$(readlink /srv/ansible/venv)
run sb-reinstall-python /sbtest/sb reinstall-python
[ "$(readlink /srv/ansible/venv)" != "$link" ] || fail "reinstall-python kept the generation"
[ "$(python)" != "$py" ] || fail "reinstall-python kept the Python"
healthy "$to"
run sb-reinstall-facts /sbtest/sb reinstall-facts
said "saltbox.fact reinstalled successfully"
[ "$(sha256sum <"$fact")" = "$fact_sum" ] || fail "reinstall-facts changed the fact"
pass "reinstall-venv, reinstall-python and reinstall-facts"

link=$(readlink /srv/ansible/venv)
run sb-go-reinstall-venv /sbtest/sb-go reinstall-venv -v
[ "$(readlink /srv/ansible/venv)" != "$link" ] || fail "sb-go kept the generation"
healthy "$to"
before=$(state)
run sb-update-venv /sbtest/sb update-venv
said "Ansible virtual environment was already up to date"
[ "$(state)" = "$before" ] || fail "sb changed sb-go's tree"
run sb-go-update-venv /sbtest/sb-go update-venv -v
said "Ansible virtual environment was already up to date"
said "saltbox.fact is up to date"
[ "$(state)" = "$before" ] || fail "sb-go changed its tree after sb"
pass "sb adopts sb-go's generation unchanged, and sb-go keeps it after sb"

ansible=$(/usr/local/bin/ansible --version | head -1)
# shellcheck disable=SC1091 # the container's own
echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed;" \
  "uv $uv, Python $from then $to," \
  "saltbox.fact $("$fact" --version), $ansible"
EOS
}

for rel in "${releases[@]}"; do
  rm -rf "${out:?}/$rel"
  mkdir -p "$out/$rel"
  inner >"$out/$rel/inner.sh"
  echo "live.sh: $rel in $prefix-$rel-$$"
  # --rm and a bounded sleep: a run killed past its EXIT trap leaves no
  # container behind for long.
  # shellcheck disable=SC2086 # word splitting gives SBP_DOCKER_OPTS's options
  container=$(docker run -d --rm --init --name "$prefix-$rel-$$" \
    --label sb-rust.test=python-env-live \
    --cpus 4 --memory 4g ${SBP_DOCKER_OPTS:-} \
    -v "$sb:/sbtest/sb:ro" -v "$sbgo:/sbtest/sb-go:ro" \
    -v "$saltbox/.python-version:/sbtest/saltbox/.python-version:ro" \
    -v "$saltbox/.uv-version:/sbtest/saltbox/.uv-version:ro" \
    -v "$saltbox/requirements/requirements-saltbox.txt:/sbtest/saltbox/requirements/requirements-saltbox.txt:ro" \
    -v "$out/$rel:/sbtest/out" \
    "ubuntu:$rel" sleep 3h)
  status=0
  docker exec "$container" bash /sbtest/out/inner.sh "$rel" "$from" "$to" </dev/null 2>&1 \
    | tee "$out/$rel/summary.log" || status=$?
  docker rm -fv "$container" >/dev/null
  container=""
  [ "$status" = 0 ] || { echo "live.sh: $rel failed; logs in $out/$rel" >&2; exit 1; }
  grep -q "^RESULT: $rel .*: passed;" "$out/$rel/summary.log" \
    || { echo "live.sh: $rel ended without its result line; logs in $out/$rel" >&2; exit 1; }
done
echo "live.sh: every release passed"
