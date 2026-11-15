#!/usr/bin/env bash
# The container side of crates/ansible/tests/container.sh: real Ansible,
# from pip, and sb, mounted at /usr/local/bin/sb, in a throwaway
# ubuntu:24.04. Mounts: /check (this directory, read-only), /detect-virt
# (sb's `systemd-detect-virt` stand-in: a Docker container isn't LXC) and
# /out (results). sb runs as root with SB_LOG_DIR in /out and the default
# playbook program, /usr/local/bin/ansible-playbook, which is the pip
# install's. Each check prints `ok` or `FAIL`; the exit code is 1 when
# any failed.
set -euo pipefail
export LANG=C.UTF-8 DEBIAN_FRONTEND=noninteractive

# Ansible from pip (the spec's setup), unless the run brings its own
# ansible-playbook (container.sh's SBA_DOCKER_OPTS, for a dry run).
if [ ! -e /usr/local/bin/ansible-playbook ]; then
  apt-get update -qq >/dev/null
  apt-get install -y -qq python3-venv git >/dev/null
  python3 -m venv /opt/ansible
  /opt/ansible/bin/pip install -q "ansible-core==${ANSIBLE_CORE:?}"
  ln -s /opt/ansible/bin/ansible-playbook /usr/local/bin/ansible-playbook
fi
{
  ansible-playbook --version | head -n 1
  python3 --version 2>&1 || true
  grep '^PRETTY_NAME=' /etc/os-release
  sb version
} >/out/versions.txt 2>&1 || true

# The Saltbox root (moved by SB_SALTBOX_DIR) and the appdata root its
# inventory names, with the Sandbox checkout under it.
mkdir -p /work/appdata
cp -r /check/saltbox /work/saltbox
cp -r /check/sandbox /work/appdata/sandbox
mkdir -p /work/saltbox/inventories/host_vars
echo 'server_appdata_path: /work/appdata' >/work/saltbox/inventories/host_vars/localhost.yml
export SB_SALTBOX_DIR=/work/saltbox SB_LOG_DIR=/out/sb-logs PATH=/detect-virt:$PATH
mkdir -p "$SB_LOG_DIR"

fails=0
ok() { printf 'ok   %s\n' "$1"; }
fail() {
  printf 'FAIL %s\n' "$1"
  fails=$((fails + 1))
}

# Each checkout a git repository with one commit: the tag cache is keyed
# by its HEAD (`sb list` below). A dry run has no git.
if command -v git >/dev/null; then
  for repo in /work/saltbox /work/appdata/sandbox; do
    git -C "$repo" init -q
    git -C "$repo" add -A
    git -C "$repo" -c user.name=sb-check -c user.email=sb-check commit -q -m stand-in
  done
else
  fail "no git: the checkouts have no HEAD for sb list"
fi

# The variable names a run without -e has, which `check` subtracts.
code=0
sb install baseline --no-cache >/out/baseline.out 2>&1 || code=$?
if [ "$code" -eq 0 ] && [ -s /out/baseline.json ]; then
  ok "baseline run"
else
  fail "baseline run: sb exited $code, see /out/baseline.out"
fi

# `e_case NAME WANT VALUE...`: `sb install check --no-cache -e VALUE...`,
# then what the play recorded must be WANT: the variables Ansible made of
# the values, as JSON with sorted keys. Ansible parses each value itself
# (ansible-core's load_extra_vars: `@` a file, `{` or `[` YAML, else
# key=value pairs split on spaces, the rest `_raw_params`), so a value
# that reached it whole parses as WANT, and a value split or rejoined on
# the way wouldn't (`hello big world` split on spaces would leave
# `_raw_params: world`). An empty value is the exception: Ansible skips
# it, so only the `Executing...` line shows it was passed. That line
# shows each value as given, `--extra-vars=<value>`, all on that one
# line, as sb-go's does (ansible spec, decision 17).
values=()
e_case() {
  local name=$1 want=$2 value code=0 got
  shift 2
  local args=() shown="--tags check"
  for value; do
    args+=(-e "$value")
    values+=("$value")
    shown+=" --extra-vars=$value"
  done
  shown="Executing Ansible playbook with command: /usr/local/bin/ansible-playbook saltbox.yml --become $shown"
  rm -f /out/check.json
  sb install check --no-cache "${args[@]}" >"/out/e-$name.out" 2>&1 || code=$?
  if [ "$code" -ne 0 ]; then
    fail "-e $name: sb exited $code, see /out/e-$name.out"
    return
  fi
  if ! got=$(cat /out/check.json 2>/dev/null); then
    fail "-e $name: the play recorded nothing, see /out/e-$name.out"
    return
  fi
  mv /out/check.json "/out/e-$name.json"
  if [ "$got" != "$want" ]; then
    fail "-e $name: Ansible got $got, want $want"
  elif ! grep -q -x -F -e "$shown" "/out/e-$name.out"; then
    fail "-e $name: the Executing line doesn't show the values as given, see /out/e-$name.out"
  else
    ok "-e $name"
  fi
}

e_case json '{"a": 1, "b": [2, 3]}' '{"a": 1, "b": [2, 3]}'
e_case quotes $'{"msg": "say \\"hi\\", ok", "q": "it\'s"}' \
  $'{"msg": "say \\"hi\\", ok", "q": "it\'s"}'
e_case unicode '{"name": "Zoë 名前 🚀"}' '{"name": "Zoë 名前 🚀"}'
e_case leading-dash '{"-x": "1"}' '-x=1'
e_case no-cache '{"_raw_params": "--no-cache"}' '--no-cache'
e_case file '{"from_file": "loaded"}' '@vars.yml'
e_case spaces '{"_raw_params": "big world", "greeting": "hello"}' 'greeting=hello big world'
e_case comma '{"list": "a,b"}' 'list=a,b'
e_case empty '{}' ''
e_case several '{"a": "1", "b": 2, "from_file": "loaded"}' 'a=1' '{"b": 2}' '@vars.yml'
e_case in-order '{"k": "second"}' 'k=first' 'k=second'

# Every -e value in sb's run log too, in its start record's command,
# which the log quotes as tracing's Debug does (`\` and `"` escaped).
missing=0
for value in "${values[@]}"; do
  quoted=${value//\\/\\\\}
  quoted=${quoted//\"/\\\"}
  if ! grep -r -q -F -e "--extra-vars=$quoted" "$SB_LOG_DIR"; then
    fail "the -e value $value isn't in sb's run log as given"
    missing=1
  fi
done
[ "$missing" -eq 1 ] || ok "every -e value logged as given"

# sb's own listing against real Ansible: `sb list` finds no cache, so it
# rebuilds Saltbox's and Sandbox's entries with `--list-tags`, parses
# Ansible's listing and writes cache.json.
code=0
sb list >/out/list.out 2>&1 || code=$?
if [ "$code" -ne 0 ]; then
  fail "sb list: exit $code, see /out/list.out"
elif ! grep -q -w sonarr-config /out/list.out || ! grep -q -w after /out/list.out ||
  [ ! -s /work/saltbox/cache.json ]; then
  fail "sb list: the tags or cache.json are missing, see /out/list.out"
else
  ok "sb list: both caches rebuilt from Ansible's listing"
fi

# The Sandbox playbook runs when nothing interrupts it, so its marker's
# absence below means something.
rm -f /out/sandbox-ran
code=0
sb install sandbox-after --no-cache >/out/sandbox.out 2>&1 || code=$?
if [ "$code" -eq 0 ] && [ -e /out/sandbox-ran ]; then
  ok "sb install sandbox-after runs Sandbox's playbook"
else
  fail "sb install sandbox-after: exit $code, or no marker, see /out/sandbox.out"
fi

# Ctrl-C on a terminal: script(1) gives sb a PTY, and ^C written to it is
# the terminal's SIGINT to the foreground group, sb and Ansible. Ansible
# prints "User interrupted execution" and exits 99, which sb passes
# through (ansible spec, step 7 and difference 6), with exec's block and
# its hint, and the Sandbox playbook after it doesn't run.
rm -f /out/slow-started /out/slow-finished /out/sandbox-ran
mkfifo /tmp/keys
script -q -e -f -c 'sb install slow,sandbox-after --no-cache' /out/ctrl-c.typescript \
  </tmp/keys >/dev/null 2>&1 &
pid=$!
exec 3>/tmp/keys
for _ in $(seq 600); do
  if [ -e /out/slow-started ] || ! kill -0 "$pid" 2>/dev/null; then break; fi
  sleep 0.2
done
# The marker's task has ended; the next one starts its sleep.
sleep 2
printf '\003' >&3
for _ in $(seq 300); do
  kill -0 "$pid" 2>/dev/null || break
  sleep 0.2
done
hung=0
if kill -0 "$pid" 2>/dev/null; then
  # The rest ends with the container.
  hung=1
  kill -KILL "$pid" || true
fi
code=0
wait "$pid" || code=$?
exec 3>&-
typescript=$(sed 's/\x1b\[[0-9;]*m//g' /out/ctrl-c.typescript)
if [ "$hung" -eq 1 ]; then
  fail "Ctrl-C: sb didn't end within 60 s, see /out/ctrl-c.typescript"
elif [ ! -e /out/slow-started ]; then
  fail "Ctrl-C: the slow task never started, see /out/ctrl-c.typescript"
elif [ "$code" -ne 99 ]; then
  fail "Ctrl-C: sb exited $code, want Ansible's 99, see /out/ctrl-c.typescript"
elif [[ $typescript != *"User interrupted execution"* ]]; then
  fail "Ctrl-C: no \"User interrupted execution\" from Ansible"
elif [[ $typescript != *"exited with code 99"* ]]; then
  fail "Ctrl-C: exec's block doesn't name exit 99"
elif [[ $typescript != *"Ansible was interrupted."* ]]; then
  fail "Ctrl-C: no \"Ansible was interrupted.\" from sb"
elif [ -e /out/slow-finished ] || [ -e /out/sandbox-ran ]; then
  fail "Ctrl-C: the play went on, or Sandbox's playbook ran"
else
  ok "Ctrl-C: sb exited 99, the play stopped, Sandbox's playbook didn't run"
fi

# The --list-tags fixture: the listing sb's cache rebuild runs, in the
# Saltbox stand-in, with its ansible.cfg's force_color. That colours
# Ansible's warnings but not the listing, which ansible-core prints with
# a plain display.display(msg) (checked at 2.21.3):
# https://github.com/ansible/ansible/blob/v2.21.3/lib/ansible/cli/playbook.py
# sb strips escape codes all the same. It must hold no path or name of
# this container.
code=0
(cd /work/saltbox && ansible-playbook saltbox.yml --become --list-tags --skip-tags=always) \
  >/out/list-tags.txt 2>/out/list-tags.err || code=$?
if [ "$code" -ne 0 ]; then
  fail "--list-tags: exit $code, see /out/list-tags.err"
elif ! grep -q 'TASK TAGS: \[' /out/list-tags.txt; then
  fail "--list-tags: no TASK TAGS line in /out/list-tags.txt"
elif grep -q -F -e /work -e /out -e /opt -e /root -e "$HOSTNAME" /out/list-tags.txt; then
  fail "--list-tags: the output names this container's paths or host name"
else
  ok "--list-tags recorded"
fi

[ "$fails" -eq 0 ]
