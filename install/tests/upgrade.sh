#!/usr/bin/env bash
# The upgrade every existing user takes: from sb-go to sb-rust, in a
# throwaway container (by hand, capped, as containers.sh; the release
# spec, "Release channels", and sb-go's final release that fetches
# sb-rust's newest release).
#
#   SBU_SB=sb SBU_SB_BETA=sb SBU_SB_NEXT=sb SBU_SB_NEXT_BETA=sb \
#     [SBU_PREFIX=name] [SBU_DOCKER_OPTS=options] [SBU_UPDATE_ARGS=flags] \
#     install/tests/upgrade.sh [RELEASE]
#
# SBU_SB is sb-rust as its 1.0.0 release: built from the tree under test
# with SB_VERSION 1.0.0, SB_CHANNEL release and SB_GIT_COMMIT set, no
# `testing` feature, on a glibc no newer than the release's (the host's
# 22.04 build runs on 24.04 and 26.04); SBU_SB_BETA the same with
# SB_CHANNEL beta; SBU_SB_NEXT and SBU_SB_NEXT_BETA the same as 1.0.1.
# SBU_UPDATE_ARGS adds flags to step 4's `sb update` (such as
# `--skip-self-update`, to try the rest while a tree's self-update looks
# elsewhere). Default release: 24.04 (26.04 too; 22.04 has no fresh
# setup).
#
# Inside the container, as root:
#  1. sb-go as users have it: the 0.1.3 release's sb_linux_amd64 from
#     GitHub, its digest checked against the release's (sb-go's own
#     check), at /usr/local/bin/sb; then `sb setup -b master` (Saltbox
#     from GitHub, its venv), accounts.yml filled in as a user has it,
#     the Saltbox user made, `sb completion` for bash, zsh and fish,
#     Sandbox cloned from GitHub, `sb list` (its tags cache), and root's
#     safe.directory entries as Saltbox's install adds them; then both
#     checkouts' origins moved to local bare copies of themselves,
#     Saltbox's one commit ahead (the stand-in remotes).
#  2. The fake GitHub (fake-github.py): /etc/hosts sends svm.saltbox.dev,
#     api.github.com, github.com and objects.githubusercontent.com to it,
#     its throwaway CA added to the system store. sb-go 0.1.3 has no URL
#     or environment override for its release lookup (layout's
#     SVMVersionProxyURL and the GitHub URL are constants), so the hosts
#     redirect is how sb-go's future repoint is simulated: in step 3 the
#     fake answers sb-go's own release list with sb-rust's (1.0.0, its
#     `sb_linux_amd64` and `sb_linux_amd64_beta`, sha256 digests, on
#     github.com); from step 4 on it answers sb-rust's list only. Anything
#     else goes to the real host.
#  3. sb-go's `sb self-update --yes`, through the proxy: sb is then
#     sb-rust 1.0.0's release asset.
#  4. sb-rust's first `sb update --migrations skip`: sb-rust's `sb list`
#     first reads the tags sb-go cached (the run log's `tags listed`,
#     cached, both repositories); the update runs to the end; its run log
#     is /var/log/sb/sb.log; Saltbox is at the stand-in remote's commit;
#     sb-go's completion files (sb.bash, _sb and sb.fish, where sb-rust
#     writes its own) are sb-rust's; and sb-go (its old binary) lists the
#     tags sb-rust cached after the pull as cached, rebuilding nothing.
#  5. sb-rust's own `sb self-update -y`, asking saltyorg/sb-rust through
#     the proxy, with 1.0.1, a 1.1.0-rc.1 prerelease and a 1.2.0-rc.1
#     whose prerelease flag is off listed, both rcs with a plain asset:
#     it takes 1.0.1's sb_linux_amd64, and reports the release channel.
#
# The container, $SBU_PREFIX-<release>-<pid> (sbupgrade-... by default),
# is removed on exit; it gets `--cpus 4 --memory 4g`, no swap and a pids
# limit; SBU_DOCKER_OPTS adds options such as `--cpuset-cpus`. Inside it
# reaches Ubuntu's mirrors, GitHub and PyPI; the host's own trees are
# never mounted but install/ (read-only) and the run's output directory,
# target/upgrade-tests/<release>/, where the logs go; the last line is
# the result.
set -euo pipefail
for var in SBU_SB SBU_SB_BETA SBU_SB_NEXT SBU_SB_NEXT_BETA; do
  [ -x "${!var:-}" ] || { echo "upgrade.sh: $var must name an executable" >&2; exit 2; }
  printf -v "$var" '%s' "$(realpath "${!var}")"
done
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$here/../.."
rel=${1:-24.04}
[[ $rel =~ ^(24|26)\.04$ ]] || { echo "upgrade.sh: $rel is not a release setup supports (24.04, 26.04)" >&2; exit 2; }
prefix=${SBU_PREFIX:-sbupgrade}
out=$PWD/target/upgrade-tests/$rel
rm -rf "$out"
mkdir -p "$out/assets/1.0.0" "$out/assets/1.0.1"
cp "$SBU_SB" "$out/assets/1.0.0/sb_linux_amd64"
cp "$SBU_SB_BETA" "$out/assets/1.0.0/sb_linux_amd64_beta"
cp "$SBU_SB_NEXT" "$out/assets/1.0.1/sb_linux_amd64"
cp "$SBU_SB_NEXT_BETA" "$out/assets/1.0.1/sb_linux_amd64_beta"
cp install/tests/fake-github.py "$out/fake-github.py"

container=""
# shellcheck disable=SC2317 # called by the EXIT trap
cleanup() { [ -z "$container" ] || docker rm -fv "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

cat >"$out/inner.sh" <<'EOS'
set -eEuo pipefail
rel=$1
export DEBIAN_FRONTEND=noninteractive
t=/sbtest/out
trap 'echo "FAIL: inner.sh line $LINENO exited $?" >&2' ERR
fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }
keep_log() { [ ! -d /var/log/sb ] || cp -r /var/log/sb "$t/sb-log-$1"; }
run() { # log cmd...: the command's output in $t/<log>.log, its tail on failure
  local log=$1 status=0
  shift
  "$@" >"$t/$log.log" 2>&1 </dev/null || status=$?
  keep_log "$log"
  [ "$status" = 0 ] || { tail -n 40 "$t/$log.log" >&2; fail "$log exited $status"; }
}
# The requests the fake had since `mark` (each phase's own).
mark() { wc -l <"$t/requests.log"; }
since() { tail -n "+$(($1 + 1))" "$t/requests.log"; }
apt-get update -qq >/dev/null
apt-get install -y -qq ca-certificates curl wget file bsdutils systemd git python3 openssl \
  bash-completion zsh fish >"$t/packages.log" 2>&1

# 1. sb-go as users have it: 0.1.3's release asset, its digest checked.
curl -fsSL https://api.github.com/repos/saltyorg/sb-go/releases/tags/0.1.3 >"$t/sb-go-0.1.3.json"
digest=$(python3 -c 'import json,sys; print(next(a["digest"] for a in json.load(open(sys.argv[1]))["assets"] if a["name"] == "sb_linux_amd64"))' "$t/sb-go-0.1.3.json")
curl -fsSL -o /tmp/sb-go https://github.com/saltyorg/sb-go/releases/download/0.1.3/sb_linux_amd64
[ "sha256:$(sha256sum /tmp/sb-go | cut -d' ' -f1)" = "$digest" ] || fail "sb-go 0.1.3's asset doesn't match its digest $digest"
install -m 0755 -o root -g root /tmp/sb-go /usr/local/bin/sb
go_version=$(sb version)
[[ $go_version == "Saltbox CLI version: 0.1.3 "* ]] || fail "sb-go is $go_version"
cp /usr/local/bin/sb "$t/sb-go"
pass "sb-go 0.1.3 installed, its digest checked ($go_version)"
run setup sb setup -b master
# accounts.yml filled in as a user has it: validation refuses its
# defaults for the user's domain and email.
sed -i -e '/^user:/,/^[^ ]/ s/^\(  domain:\).*/\1 upgrade-test.example.com/' \
  -e '/^user:/,/^[^ ]/ s/^\(  email:\).*/\1 admin@upgrade-test.example.com/' /srv/git/saltbox/accounts.yml
user=$(sed -n 's/^  name: *//p' /srv/git/saltbox/accounts.yml | head -n 1)
id "$user" >/dev/null 2>&1 || useradd -m "$user"
run completion sb completion
# Sandbox's checkout under the inventory's appdata path (/opt by default).
appdata=$(sed -n 's/^server_appdata_path: *//p' /srv/git/saltbox/inventories/host_vars/localhost.yml 2>/dev/null || true)
appdata=${appdata%%$'\n'*}
appdata=${appdata:-/opt}
run sandbox git clone -q -b master https://github.com/saltyorg/Sandbox "$appdata/sandbox"
run go-list sb list
[ -s /srv/git/saltbox/cache.json ] || fail "sb-go wrote no tags cache"
cp /srv/git/saltbox/cache.json "$t/cache-sb-go.json"
# sb's completion files, in each shell's directory that is there.
completion_files() {
  local dirs=() dir
  for dir in /etc/bash_completion.d /usr/share/bash-completion/completions \
    /usr/share/zsh/vendor-completions /usr/local/share/bash-completion/completions \
    /usr/local/share/zsh/site-functions /etc/fish/completions /usr/share/fish/vendor_completions.d; do
    [ ! -d "$dir" ] || dirs+=("$dir")
  done
  find "${dirs[@]}" -maxdepth 1 \( -name sb -o -name 'sb.*' -o -name _sb \) | sort
}
completion_files >"$t/completion-before.txt"
[ -s "$t/completion-before.txt" ] || fail "sb-go installed no completion file"
while read -r file; do
  if grep -q SB_COMPLETE "$file"; then fail "sb-go's $file is already sb-rust's"; fi
done <"$t/completion-before.txt"
# Root's safe.directory entries for both checkouts, as Saltbox's install
# adds them (roles/pre_tasks/tasks/subtasks/git.yml; repo spec,
# assumption 2): every Saltbox host past its first install has them.
git config --global --add safe.directory /srv/git/saltbox
git config --global --add safe.directory "$appdata/sandbox"
pass "sb-go's setup, completion ($(wc -l <"$t/completion-before.txt") files), tags cache and Sandbox"
# The stand-in remotes: each checkout's origin a local bare copy of it,
# Saltbox's one commit ahead.
for dir in /srv/git/saltbox "$appdata/sandbox"; do
  name=$(basename "$dir")
  git clone -q --bare "$dir" "/remotes/$name.git"
  git -C "$dir" remote set-url origin "file:///remotes/$name.git"
done
git clone -q /remotes/saltbox.git /remotes/work
git -C /remotes/work -c user.name=sb -c user.email=sb@example.invalid \
  commit -q --allow-empty -m "the stand-in remote moves on"
git -C /remotes/work push -q origin HEAD:master
pass "stand-in remotes, Saltbox's one commit ahead"

# 2. The fake GitHub.
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=sb-upgrade-ca \
  -keyout "$t/ca.key" -out "$t/ca.pem" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj /CN=github.com -keyout "$t/key.pem" -out "$t/srv.csr" 2>/dev/null
printf 'subjectAltName=DNS:svm.saltbox.dev,DNS:api.github.com,DNS:github.com,DNS:objects.githubusercontent.com\nbasicConstraints=CA:FALSE\n' >"$t/ext"
openssl x509 -req -in "$t/srv.csr" -CA "$t/ca.pem" -CAkey "$t/ca.key" -CAcreateserial -days 2 \
  -extfile "$t/ext" -out "$t/cert.pem" 2>/dev/null
cp "$t/ca.pem" /usr/local/share/ca-certificates/sb-upgrade-ca.crt
update-ca-certificates >/dev/null 2>&1
python3 - "$t" <<'PY'
import json, socket, sys
hosts = ["svm.saltbox.dev", "api.github.com", "github.com", "objects.githubusercontent.com"]
json.dump({h: socket.gethostbyname(h) for h in hosts}, open(sys.argv[1] + "/real.json", "w"))
PY
releases() { # repos tag... : the list answered for the repositories given, newest first
  python3 - "$t" "$@" <<'PY'
import hashlib, json, os, sys
dir, repos, tags = sys.argv[1], sys.argv[2], sys.argv[3:]
listed = []
for i, tag in enumerate(tags):
    # `tag!`: an rc whose prerelease flag is off (mislabelled).
    mislabelled = tag.endswith("!")
    tag = tag.rstrip("!")
    rc = "-" in tag
    assets = []
    for j, name in enumerate(["sb_linux_amd64", "sb_linux_amd64_beta"]):
        # An rc's assets are never to be taken: 1.0.0's stand in, so a
        # client that takes one fails the test's cmp.
        path = os.path.join(dir, "assets", "1.0.0" if rc else tag, name)
        data = open(path, "rb").read()
        assets.append({"id": 100 * (i + 1) + j, "name": name, "size": len(data),
                       "digest": "sha256:" + hashlib.sha256(data).hexdigest(),
                       "browser_download_url": f"https://github.com/saltyorg/sb-rust/releases/download/{tag}/{name}"})
    listed.append({"id": i + 1, "tag_name": tag, "draft": False,
                   "prerelease": rc and not mislabelled, "assets": assets})
tmp = os.path.join(dir, "releases.json.tmp")
json.dump({"repos": repos.split(","), "releases": listed}, open(tmp, "w"))
os.replace(tmp, os.path.join(dir, "releases.json"))
PY
}
releases sb-go,sb-rust 1.0.0
: >"$t/requests.log"
python3 "$t/fake-github.py" "$t" >"$t/fake.log" 2>&1 &
printf '127.0.0.1 svm.saltbox.dev api.github.com github.com objects.githubusercontent.com\n' >>/etc/hosts
proxied() { curl -sf "https://svm.saltbox.dev/version?url=https%3A%2F%2Fapi.github.com%2Frepos%2Fsaltyorg%2F$1%2Freleases"; }
for _ in $(seq 1 50); do proxied sb-go >/dev/null && break; sleep 0.2; done
proxied sb-go | grep '"1.0.0"' >/dev/null || fail "the fake's proxy doesn't answer sb-go's list"
curl -sf https://api.github.com/repos/saltyorg/sb-rust/releases | grep '"1.0.0"' >/dev/null || fail "the fake doesn't answer sb-rust's list"
pass "the fake GitHub answers sb-go's list through the proxy and sb-rust's directly"

# 3. sb-go's self-update to sb-rust 1.0.0, through the proxy.
at=$(mark)
run go-self-update sb self-update --yes
version=$(sb version)
[[ $version == "Saltbox CLI version: 1.0.0 "* ]] || fail "after sb-go's self-update: $version"
cmp -s /usr/local/bin/sb "$t/assets/1.0.0/sb_linux_amd64" || fail "the installed sb isn't 1.0.0's release asset"
since "$at" | grep '^svm.saltbox.dev .*saltyorg%2Fsb-go%2Freleases' >/dev/null || fail "sb-go didn't ask through the proxy"
if grep -q "trying GitHub directly" "$t/go-self-update.log"; then fail "sb-go fell back from the proxy"; fi
sb version --json >"$t/version.json"
pass "sb-go's self-update installed sb-rust ($version; $(tr -d '\n' <"$t/version.json"))"

# 4. sb-rust's first update. From here sb-go's list is no longer sb-rust's.
releases sb-rust 1.0.0
rm -rf /var/log/sb
run rust-list-before sb list
grep -E 'tags listed.*repo="?Saltbox"?.*cached=true|cached=true.*repo="?Saltbox"?' /var/log/sb/sb.log >/dev/null \
  || fail "sb-rust didn't read Saltbox's tags from sb-go's cache: $(grep 'tags listed' /var/log/sb/sb.log)"
grep -E 'tags listed.*repo="?Sandbox"?.*cached=true|cached=true.*repo="?Sandbox"?' /var/log/sb/sb.log >/dev/null \
  || fail "sb-rust didn't read Sandbox's tags from sb-go's cache: $(grep 'tags listed' /var/log/sb/sb.log)"
cmp -s /srv/git/saltbox/cache.json "$t/cache-sb-go.json" || fail "sb-rust's list rewrote sb-go's cache"
pass "sb-rust's list read sb-go's tags cache"
rm -rf /var/log/sb
# shellcheck disable=SC2086 # word splitting gives SBU_UPDATE_ARGS's flags
run rust-update sb update --migrations skip ${SBU_UPDATE_ARGS:-}
[ -s /var/log/sb/sb.log ] || fail "no run log at /var/log/sb/sb.log: $(ls -la /var/log/sb 2>&1)"
grep -q 'run started .*command="update"' /var/log/sb/sb.log || fail "the run log has no record of sb update's run"
[ "$(git -C /srv/git/saltbox rev-parse HEAD)" = "$(git -C /remotes/work rev-parse HEAD)" ] \
  || fail "Saltbox wasn't updated to the stand-in remote's commit"
completion_files >"$t/completion-after.txt"
for file in /usr/local/share/bash-completion/completions/sb.bash /usr/local/share/zsh/site-functions/_sb \
  /etc/fish/completions/sb.fish; do
  grep -q SB_COMPLETE "$file" 2>/dev/null || fail "$file isn't sb-rust's: $(head -c 300 "$file" 2>&1)"
done
while read -r file; do
  grep -q SB_COMPLETE "$file" || fail "sb-go's $file is still there"
done <"$t/completion-after.txt"
pass "sb-rust's update ran, its run log is there, and sb-go's completion files are replaced"
cp /srv/git/saltbox/cache.json "$t/cache-sb-rust.json"
cmp -s "$t/cache-sb-go.json" "$t/cache-sb-rust.json" && fail "the update didn't rebuild the tags after the pull"
run go-list-after "$t/sb-go" list
grep -qx 'Saltbox tags: (cached)' "$t/go-list-after.log" || fail "sb-go didn't list Saltbox's tags from sb-rust's cache"
grep -qx 'Sandbox tags (prepend sandbox-): (cached)' "$t/go-list-after.log" \
  || fail "sb-go didn't list Sandbox's tags from sb-rust's cache"
cmp -s /srv/git/saltbox/cache.json "$t/cache-sb-rust.json" || fail "sb-go's list rewrote sb-rust's cache"
pass "sb-go read the tags cache sb-rust wrote"

# 5. sb-rust's own self-update follows the release channel.
releases sb-rust 1.2.0-rc.1! 1.1.0-rc.1 1.0.1 1.0.0
at=$(mark)
run rust-self-update sb self-update -y
version=$(sb version)
[[ $version == "Saltbox CLI version: 1.0.1 "* ]] || fail "after sb-rust's self-update: $version"
cmp -s /usr/local/bin/sb "$t/assets/1.0.1/sb_linux_amd64" || fail "sb-rust took another asset than 1.0.1's sb_linux_amd64"
since "$at" | grep '^svm.saltbox.dev .*saltyorg%2Fsb-rust%2Freleases' >/dev/null || fail "sb-rust didn't ask for saltyorg/sb-rust through the proxy"
if grep -q "trying GitHub directly" "$t/rust-self-update.log"; then fail "sb-rust fell back from the proxy"; fi
sb version | grep -x 'Release channel: release' >/dev/null || fail "sb version names no release channel: $(sb version)"
sb version --json >"$t/version-next.json"
grep -q '"channel":"release"' "$t/version-next.json" || fail "sb version --json: $(cat "$t/version-next.json")"
pass "sb-rust's self-update took 1.0.1's release asset over both rcs ($(tr -d '\n' <"$t/version-next.json"))"

# shellcheck disable=SC1091 # the container's own
echo "RESULT: $rel ($(. /etc/os-release && echo "$VERSION")): passed; $go_version -> ${version%%$'\n'*}"
EOS

echo "upgrade.sh: $rel in $prefix-$rel-$$"
# shellcheck disable=SC2086 # word splitting gives SBU_DOCKER_OPTS's options
container=$(docker run -d --rm --init --name "$prefix-$rel-$$" \
  --label sb-rust.test=upgrade-tests \
  --cpus 4 --memory 4g --memory-swap 4g --pids-limit 4096 ${SBU_DOCKER_OPTS:-} \
  -v "$PWD/install:/sbtest/install:ro" \
  -v "$out:/sbtest/out" \
  "ubuntu:$rel" sleep 3h)
status=0
# An hour at most; --foreground, so a Ctrl-C reaches docker exec too.
timeout --foreground 3600 docker exec -e SBU_UPDATE_ARGS="${SBU_UPDATE_ARGS:-}" "$container" \
  bash /sbtest/out/inner.sh "$rel" </dev/null 2>&1 | tee "$out/summary.log" || status=$?
docker rm -fv "$container" >/dev/null 2>&1 || true
container=""
[ "$status" = 0 ] || { echo "upgrade.sh: $rel failed; logs in $out" >&2; exit 1; }
grep -q "^RESULT: $rel .*: passed;" "$out/summary.log" \
  || { echo "upgrade.sh: $rel ended without its result line; logs in $out" >&2; exit 1; }
echo "upgrade.sh: passed"
