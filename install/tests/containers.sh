#!/usr/bin/env bash
# install.sh in throwaway containers (ops spec, Testing Strategy, "install
# script"; module build M83): sb-go's test-install cases and the
# acceptance checks, in a fresh ubuntu:<release> container per release,
# one at a time:
#
#   - policy.sh, on a terminal (script(1)), so `sb setup`'s stand-in sees
#     the terminal it is attached to;
#   - run as a user other than root: refused, nothing done;
#   - piped into bash as documented, `--repair` with curl (and -v) and
#     with wget (without): the latest release found through the svm proxy
#     or GitHub, downloaded from GitHub's releases path, verified and
#     installed root:root 0755, and the lines piped after the script
#     still run (nothing read stdin);
#   - on a release setup supports, a fresh install of the sb under test,
#     piped into bash as documented with `-b master` and no terminal (so
#     sb's own output is plain): the script as it is, but for one line
#     sourcing a file that replaces the lookup and the download so the
#     binary is SBI_SB (verify, install and `sb setup -b master` are the
#     script's), `&& touch` running after it, the piped lines after it
#     too; the checkout on master, the venv, sb root's 0755, and no `-v`
#     reaching sb setup (its run log has no `--verbose given`);
#   - the documented form piped again over that install: refused, exit 1,
#     and the `&& ...` after it not run;
#   - on 22.04, the documented fresh form refused (setup's list).
#
#   SBI_SB=sb [SBI_PREFIX=name] [SBI_DOCKER_OPTS=options] \
#     install/tests/containers.sh [RELEASE]...
#
# Default releases: 22.04 24.04 26.04. SBI_SB is an sb built without the
# `testing` feature and with SB_VERSION, SB_GIT_COMMIT and SB_CHANNEL=release
# set (the script checks `sb version` against the tag and the channel), on a glibc no newer than 24.04's
# (22.04 only refuses a fresh install, so it never runs SBI_SB).
#
# Each container, $SBI_PREFIX-<release>-<pid> (sbinstall-... by default),
# is removed on exit and touches no other container or image. Docker owns
# its cgroup, so a systemd scope around this script doesn't cap it: it
# gets `--cpus 4 --memory 4g` with no swap and a pids limit, and
# SBI_DOCKER_OPTS adds `docker run` options, such as `--cpuset-cpus` to
# pin its cores. Inside it the script and sb reach Ubuntu's apt mirrors,
# the svm proxy, GitHub (sb-go's releases, the Saltbox repository, uv,
# Python and saltbox.fact) and PyPI; the host's own trees are never
# mounted, and the script never runs on the host. Logs go to
# target/install-tests/<release>/, handed back to the caller by the
# container's root (a caller that isn't root, such as a CI runner, can
# then remove them, as the next run does); the last line per release is
# its result.
set -euo pipefail
[ -x "${SBI_SB:-}" ] || { echo "containers.sh: SBI_SB must name an executable" >&2; exit 2; }
sb=$(realpath "$SBI_SB")
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../.."
prefix=${SBI_PREFIX:-sbinstall}
releases=("$@")
[ ${#releases[@]} -gt 0 ] || releases=(22.04 24.04 26.04)
for rel in "${releases[@]}"; do
  [[ $rel =~ ^[0-9]{2}\.[0-9]{2}$ ]] || { echo "containers.sh: $rel is not a release like 24.04" >&2; exit 2; }
done
out=$PWD/target/install-tests
owner=$(id -u):$(id -g)

container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# Paths under DIR not handed back to its caller: not its own, or a
# directory it can't write to, and find's errors on any it can't read.
# A root caller can remove anything, so only a caller that isn't root
# finds one.
not_removable() { # dir
  find "$1" \( ! -user "$(id -u)" -o -type d ! -writable \) -print 2>&1 || true
}

# Runs inside the container, as root, from /sbtest/out/inner.sh with no
# stdin: $1 the release. The script is /sbtest/install/install.sh, the sb
# under test /sbtest/sb. Its last line is `RESULT: <release> ...: passed`.
inner() {
  cat <<'EOS'
set -euo pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
logs=/sbtest/out
script=/sbtest/install/install.sh
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
apt-get update -qq >/dev/null
# The script's dependencies (curl or wget, file), script(1) for a
# terminal, and systemd for setup's virtualization check.
apt-get install -y -qq ca-certificates curl wget file bsdutils systemd >"$logs/packages.log" 2>&1

# The policy cases, on a terminal.
script -qec "SB_INSTALL_TEST_TERMINAL=yes bash /sbtest/install/tests/policy.sh" /dev/null \
  >"$logs/policy.log" 2>&1 </dev/null || { cat "$logs/policy.log" >&2; fail "policy.sh"; }
grep -q '^PASS: ' "$logs/policy.log" || { cat "$logs/policy.log" >&2; fail "policy.sh gave no PASS"; }
pass "policy.sh"

# Not root: refused before anything.
status=0
runuser -u nobody -- bash "$script" >"$logs/not-root.log" 2>&1 </dev/null || status=$?
[ "$status" = 1 ] || fail "as nobody: exit $status"
grep -q "must be run as root" "$logs/not-root.log" || fail "as nobody: $(cat "$logs/not-root.log")"
[ ! -e /usr/local/bin/sb ] || fail "as nobody: sb installed"
pass "refused as a user other than root"

# --repair piped into bash, with each download tool, the lines after the
# script still run: the real latest release, from GitHub.
for tool in curl wget; do
  verbose=()
  [ "$tool" = wget ] || verbose=(-v)
  rm -f /usr/local/bin/sb
  status=0
  { cat "$script"; echo 'echo "TRAILER RAN"'; } \
    | bash -s -- --repair --force-$tool "${verbose[@]}" >"$logs/repair-$tool.log" 2>&1 || status=$?
  [ "$status" = 0 ] || { tail -n 30 "$logs/repair-$tool.log" >&2; fail "repair with $tool exited $status"; }
  grep -q '^TRAILER RAN$' "$logs/repair-$tool.log" || fail "repair with $tool: the piped lines after the script didn't run"
  [ "$(stat -c '%U:%G %a' /usr/local/bin/sb)" = "root:root 755" ] || fail "repair with $tool: $(stat -c '%U:%G %a' /usr/local/bin/sb)"
  /usr/local/bin/sb version </dev/null | grep -q '^Saltbox CLI version: ' || fail "repair with $tool: sb version"
  ! ls /usr/local/bin/sb.install.* >/dev/null 2>&1 || fail "repair with $tool left a staged file"
done
released=$(/usr/local/bin/sb version </dev/null)
pass "--repair piped, with curl and wget ($released)"

case "$rel" in
  24.04 | 26.04) ;;
  *)
    # Setup's list: the documented fresh form refused, `&&` not run.
    status=0
    { cat "$script" | bash && touch /sbtest/out/after; } >"$logs/fresh.log" 2>&1 || status=$?
    [ "$status" = 1 ] || fail "fresh on $rel exited $status"
    grep -q "Unsupported Ubuntu version for fresh installation: $rel" "$logs/fresh.log" || fail "fresh on $rel: $(cat "$logs/fresh.log")"
    [ ! -e /sbtest/out/after ] || fail "fresh on $rel: && ran"
    pass "a fresh install refused on $rel"
    # shellcheck disable=SC1091 # the container's own
    echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed; $released"
    exit 0
    ;;
esac

# A fresh install of the sb under test, piped as documented: the script
# as it is but for one line, before its own parse_args, sourcing the
# replacements for the lookup and the download.
want=$(/sbtest/sb version </dev/null | sed -n 's/^Saltbox CLI version: \([^ ]*\) .*/\1/p')
cat >/sbtest/out/release.sh <<RELEASE
get_latest_version() { echo "v$want"; }
download_binary() { cp /sbtest/sb "\$TEMP_DIR/sb"; echo "\$TEMP_DIR/sb"; }
RELEASE
sed 's|^  parse_args "\$@"$|  source /sbtest/out/release.sh\n  parse_args "$@"|' "$script" >/sbtest/out/install-test.sh
[ "$(grep -c '^  source /sbtest/out/release.sh$' /sbtest/out/install-test.sh)" = 1 ] \
  || fail "the release replacement didn't go in"
rm -f /usr/local/bin/sb /sbtest/out/after
status=0
{ { cat /sbtest/out/install-test.sh; echo 'echo "TRAILER RAN"'; } | bash -s -- -b master \
  && touch /sbtest/out/after; } >"$logs/fresh.log" 2>&1 </dev/null || status=$?
cp -r /var/log/sb "$logs/sb-log" 2>/dev/null || true
[ "$status" = 0 ] || { tail -n 60 "$logs/fresh.log" >&2; fail "the fresh install exited $status"; }
[ -e /sbtest/out/after ] || fail "the fresh install: && didn't run"
grep -q '^TRAILER RAN$' "$logs/fresh.log" || fail "the fresh install: the piped lines after the script didn't run"
cmp -s /sbtest/sb /usr/local/bin/sb || fail "the installed sb isn't the one under test"
[ "$(stat -c '%U:%G %a' /usr/local/bin/sb)" = "root:root 755" ] || fail "sb is $(stat -c '%U:%G %a' /usr/local/bin/sb)"
grep -q "Saltbox installation completed" "$logs/fresh.log" || fail "no setup tree in the output"
branch=$(git -C /srv/git/saltbox -c safe.directory='*' rev-parse --abbrev-ref HEAD)
[ "$branch" = master ] || fail "the checkout is on $branch"
ansible=$(/srv/ansible/venv/bin/ansible --version) || fail "the venv's ansible"
ansible=${ansible%%$'\n'*}
if grep -rqs -- "--verbose given" /var/log/sb; then fail "-v reached sb setup"; fi
pass "a fresh install of the sb under test, piped with -b master, no terminal"

# Over that install, the documented form: refused, && not run.
rm -f /sbtest/out/after
status=0
{ cat "$script" | bash && touch /sbtest/out/after; } >"$logs/again.log" 2>&1 || status=$?
[ "$status" = 1 ] || fail "a second install exited $status"
grep -q "/srv/git/saltbox already exists" "$logs/again.log" || fail "a second install: $(cat "$logs/again.log")"
[ ! -e /sbtest/out/after ] || fail "a second install: && ran"
cmp -s /sbtest/sb /usr/local/bin/sb || fail "a second install changed sb"
pass "a second install refused, && not run"

# shellcheck disable=SC1091 # the container's own
echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed; $released; $ansible"
EOS
}

for rel in "${releases[@]}"; do
  rm -rf "${out:?}/$rel"
  mkdir -p "$out/$rel"
  inner >"$out/$rel/inner.sh"
  echo "containers.sh: $rel in $prefix-$rel-$$"
  # --rm and a bounded sleep: a run killed past its EXIT trap leaves no
  # container behind for long.
  # shellcheck disable=SC2086 # word splitting gives SBI_DOCKER_OPTS's options
  container=$(docker run -d --rm --init --name "$prefix-$rel-$$" \
    --label sb-rust.test=install-tests \
    --cpus 4 --memory 4g --memory-swap 4g --pids-limit 4096 ${SBI_DOCKER_OPTS:-} \
    -v "$sb:/sbtest/sb:ro" \
    -v "$PWD/install:/sbtest/install:ro" \
    -v "$out/$rel:/sbtest/out" \
    "ubuntu:$rel" sleep 3h)
  status=0
  # An hour at most: a hung download ends the run, not only the 3 h sleep.
  timeout 3600 docker exec "$container" bash /sbtest/out/inner.sh "$rel" </dev/null 2>&1 \
    | tee "$out/$rel/summary.log" || status=$?
  # Whatever the run's result: the container's root wrote the logs.
  docker exec "$container" chown -R "$owner" /sbtest/out || status=1
  docker rm -fv "$container" >/dev/null 2>&1 || true
  container=""
  left=$(not_removable "$out/$rel")
  [ -z "$left" ] || { printf 'containers.sh: %s: not removable by %s:\n%s\n' "$rel" "$owner" "$left" >&2; exit 1; }
  [ "$status" = 0 ] || { echo "containers.sh: $rel failed; logs in $out/$rel" >&2; exit 1; }
  grep -q "^RESULT: $rel .*: passed;" "$out/$rel/summary.log" \
    || { echo "containers.sh: $rel ended without its result line; logs in $out/$rel" >&2; exit 1; }
done
echo "containers.sh: every release passed"
