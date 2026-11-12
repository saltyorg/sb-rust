# Platform facts for exec: sudo, process groups, signals, dependencies

What the exec module's design leans on (docs/specs/exec.md; exec plan
Task 1), checked on 2026-11-04. Sources are upstream code and manuals at
the tags named; the kernel facts were confirmed by the test below.

Software: Ubuntu's sudo packages 1.9.9-1ubuntu2 (22.04) and
1.9.15p5-3ubuntu5 (24.04), and rust-sudo-rs 0.2.13-0ubuntu1 (26.04; the
archive's Sources lists); Linux 7.0; Rust 1.99.1; cross 0.2.5;
cargo-deny 0.20.2.

## sudo: `--preserve-env=list`

- **A preserved name is checked as a command-line `VAR=value`.** The
  front end reads each named variable and adds `NAME=value` to the list
  of `VAR=value` arguments (src/parse_args.c, `parse_env_list`, at
  SUDO_1_9_9 and SUDO_1_9_15p5; names that aren't set are skipped).
  sudoers checks that list only when `setenv` is off
  (plugins/sudoers/sudoers.c, `sudoers_check_common` at 1.9.15p5 and
  :659 at 1.9.9: `ISSET(ctx->mode, MODE_RUN) && !def_setenv` then
  `validate_env_vars`); under `env_reset` a name passes only if
  `env_check` (a value without `/` or `%`) or else `env_keep` covers it
  (plugins/sudoers/env.c, `env_should_keep`). `setenv` is on with
  `Defaults setenv`, for a command with the SETENV tag, or for one
  matched by `ALL` (sudo(8), "ENVIRONMENT"). A refused name refuses the
  whole command.
- **sudo-rs 0.2.13 does the same:** a requested variable `should_keep`
  rejects fails the whole command (src/sudo/env/environment.rs,
  `rejected_vars`, `Error::EnvironmentVar`); only `trust_environment`
  (SETENV) skips the check (src/sudo/pipeline.rs); `ALL` carries SETENV
  (src/sudoers/mod.rs).
- **For the maintainer, decided 2026-11-04 (cli-core):** sb's relaunch
  passed `SB_RELAUNCHED` this way, so under a rule naming specific
  commands without SETENV every relaunch was refused. The marker only
  guarded against sudo running sb as someone other than root (sudo runs
  the command as `runas_default` "if the -u option is not specified",
  sudoers(5)). sb now relaunches with `sudo -u root`: sudo runs it as
  root or refuses, so the relaunched run always passes sb's root check,
  and the marker is gone. The other preserved names (VISUAL, EDITOR,
  NO_COLOR, SB_COLOR_PROFILE) can still trip such a rule when set, close
  to sb-go, which preserved VISUAL and EDITOR.

## sudo: `use_pty` and signals

- **`use_pty` is on for every supported release:** sudo's default from
  1.9.14 (sudoers(5), 1.9.15p5: "This flag is on by default for sudo
  1.9.14 and above"); Ubuntu's stock sudoers sets `Defaults use_pty` on
  22.04 and 24.04 too (debian/etc/sudoers in the jammy and noble source
  packages); sudo-rs 0.2.13 defaults it on (src/defaults/mod.rs).
- **C sudo relays signals to sb's process group.** sudo relays the
  signals it receives to the command (sudo(8), "Signal handling"):
  SIGINT and SIGQUIT only when the command runs in a new pty or the
  signal came from a user process; never a signal the command itself
  sent; SIGKILL and SIGSTOP can't be relayed. Under `use_pty` the relay
  is `killpg(mc->cmnd_pid, signo)` (src/exec_monitor.c,
  `deliver_signal`, at SUDO_1_9_9 and SUDO_1_9_15p5): sb's whole group,
  so a child in sb's group gets the signal directly. Without a pty it is
  the pid only (src/exec_nopty.c). sudo-rs 0.2.13 relays to the pid only
  under `use_pty` too (src/exec/use_pty/monitor.rs: "FIXME: We should
  call killpg instead of kill").
- **Where a Ctrl-C comes from.** With sb's stdin and stdout on the
  user's terminal, sb runs in the foreground of sudo's pty and the
  terminal's Ctrl-C reaches sb's group from the kernel. When stdin or
  stdout isn't the user's terminal (`sb … | tee log`, `yes | sb …`),
  sudo runs the command in the background of its pty and leaves the
  user's terminal cooked (src/exec_pty.c, `CD_EXEC_BG`); a Ctrl-C then
  reaches sudo and is relayed as above, looking to sb like a signal from
  another process.
- **Consequence, decided 2026-11-04 (exec decision 2 revised):** sb never
  forwards SIGINT to an attached child: the terminal delivers it, as
  with POSIX `system()` ("The system() function shall ignore the SIGINT
  and SIGQUIT signals … while waiting for the command to terminate",
  POSIX.1-2024) and git's editor launch (editor.c,
  `launch_specified_editor`, git master read on 2026-11-04: SIGINT and
  SIGQUIT ignored around the editor, re-raised if it died of one). sb forwards SIGTERM to the
  child's pid (needed under sudo-rs, a duplicate under C sudo, harmless
  for most programs). Attached children assume sb is on the user's
  terminal; piped runs of such commands aren't supported (the install
  script runs `sb setup` attached), so the relayed Ctrl-C of a piped run,
  and sudo-rs's pid-only relay there, aren't designed for. sb doesn't
  need to tell where a signal came from, nor signal-hook's
  `extended-siginfo` feature (below).

## Doubled signals (a scope's stop, sudo's relay, a bash hangup)

Probed 2026-11-12 on Ubuntu 24.04: Linux 7.0, sudo 1.9.15p5-3ubuntu5
(C sudo, `use_pty` on), bash 5.2.21, util-linux 2.39.3 (`script`),
systemd 255.4, bpftrace 0.20.2. Method: every signal generated for the
target, a bash script that catches SIGTERM, SIGINT and SIGHUP so it
lives on to receive a copy, was recorded by the kernel's
`signal:signal_generate` tracepoint (bpftrace: time, sender, result),
which doesn't depend on how the target handles them. The target ran as
root through `sudo -n` from a non-root user with sudo rights, inside a
`systemd-run --scope` session scope, three runs per case (six for the
hangup).

- **A signal to the whole scope is doubled under sudo.** `systemctl kill
  --kill-whom=all` signals sudo and the command alike; sudo then relays
  its own copy, sometimes two (its monitor and its parent). SIGTERM: copies +0.02 to +0.26 ms
  after systemd's. SIGHUP: +0.01 to +0.13 ms. **SIGINT too:** +0.01 to
  +0.13 ms, two of three delivered separately (the third merged with the
  first while it was still pending). So a scope-wide SIGINT reaches sb
  twice under sudo.
- **A session scope's stop sends SIGTERM, then SIGHUP.** Stopping a
  scope with `SendSIGHUP=yes`, as logind's session scopes have (a reboot
  or shutdown during an SSH session, `loginctl terminate-session`,
  `KillUserProcesses`; here `systemctl stop` on a `systemd-run --scope -p
  SendSIGHUP=yes`), sends every process in it SIGTERM and then SIGHUP:
  +0.16 to +0.48 ms apart. Under sudo the command got five within
  0.2 ms: systemd's SIGTERM and SIGHUP and sudo's relays of both, in
  mixed order.
- **On a terminal, not doubled.** A Ctrl-C typed into the user's terminal
  reaches the command once, from the kernel (sudo's `use_pty` passes the
  byte to the command's pty); a SIGTERM to the user's foreground group
  reaches it once, relayed by sudo (the command isn't in that group).
- **A hangup under an interactive bash is doubled, without sudo.** With
  `script -qec "bash --norc -i"` killed (its master closed), the job got
  SIGHUP twice, both sent by bash's process: bash re-sends SIGHUP to its
  jobs, then, as the session leader exits, the kernel hangs up the
  foreground group. Copies +0.13 to +0.24 ms apart (two of six merged
  while pending).

Consequence: within 250 ms of the first SIGTERM or SIGHUP sb read, sb
doesn't count another SIGTERM or SIGHUP, whichever came first, as a
second signal (cli-core's `REPEAT_WINDOW`; hardening plan decisions 2 and
9): the copies come within half a millisecond, a person signalling twice
on purpose takes far longer. Decided 2026-11-12: SIGINT stays outside
the window, so two Ctrl-C presses always force the exit; a scope-wide
SIGINT under sudo forces it too.

## Process groups (Linux)

Confirmed on Linux 7.0 by the script below, run as root in a new PID
namespace (`unshare -p -f --mount-proc bash kernel-facts.sh`):

1. **A process-group id isn't reused while a member uses it,** after the
   leader has exited and been reaped, both for a session (`setsid`, as
   portable-pty starts PTY children) and for a group that isn't a session
   (job control, as tokio's `process_group(0)` starts piped children).
   Method: `/proc/sys/kernel/ns_last_pid` set one below the group's id,
   then a new process: it gets the next free number. Control: once the
   member has exited, the same request gets the group's number.
2. **`kill(-pgid, 0)` succeeds while the group's only member is a
   zombie** (state `Z` checked in `/proc/<pid>/stat`). So "is the group
   empty" must reap or bound its wait: exec's escalation is bounded, and
   its tests reap grandchildren themselves.

```bash
#!/usr/bin/env bash
# Kernel facts exec's process-group handling leans on, checked inside a
# fresh PID namespace (run as root: unshare -p -f --mount-proc bash THIS):
# 1. A process-group id isn't reused while a member still uses it, even
#    after the leader has exited and been reaped (1: a session, 1b: a
#    group that isn't one).
# 2. kill(-pgid, 0) succeeds while the group's only member is a zombie.
set -euo pipefail
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# 1. A group whose leader exits at once, leaving a sleeping member.
setsid bash -c 'sleep 30 & echo $$ >'"$work/leader"'; exit 0' &
wait $!
pgid=$(cat "$work/leader")
member=$(pgrep -g "$pgid" sleep)
echo "leader $pgid reaped; member $member still in group $(ps -o pgid= -p "$member" | tr -d ' ')"
# Ask for the leader's number next: write the one before it as the last pid.
echo $((pgid - 1)) >/proc/sys/kernel/ns_last_pid
bash -c 'echo $$' >"$work/next"
next=$(cat "$work/next")
if [[ "$next" == "$pgid" ]]; then
  echo "FACT 1 FAILED: pid $pgid was reused while group $pgid has a member"
  exit 1
fi
echo "FACT 1 holds: asked for $pgid, got $next (number kept while group $pgid has a member)"
# Control: once the member is gone, the number is free again.
kill "$member"
while kill -0 "$member" 2>/dev/null; do sleep 0.05; done
echo $((pgid - 1)) >/proc/sys/kernel/ns_last_pid
bash -c 'echo $$' >"$work/next"
if [[ "$(cat "$work/next")" != "$pgid" ]]; then
  echo "CONTROL FAILED: $pgid not handed out after its group emptied"
  exit 1
fi
echo "control: with the member gone, asking for $pgid gives $pgid"

# 1b. The same for a group that isn't a session: a job in its own group
# (job control), as tokio's process_group(0) makes for piped children.
bash -c 'set -m; bash -c "sleep 30 & exit 0" & echo $! >'"$work/gleader"'; wait'
gpgid=$(cat "$work/gleader")
gmember=$(pgrep -g "$gpgid" sleep)
[[ "$(ps -o sid= -p "$gmember" | tr -d ' ')" != "$gpgid" ]] || { echo "1b: not group-only"; exit 1; }
echo $((gpgid - 1)) >/proc/sys/kernel/ns_last_pid
bash -c 'echo $$' >"$work/next"
if [[ "$(cat "$work/next")" == "$gpgid" ]]; then
  echo "FACT 1b FAILED: pid $gpgid reused while group $gpgid (not a session) has a member"
  exit 1
fi
echo "FACT 1b holds: group-only $gpgid kept, got $(cat "$work/next")"
kill "$gmember"
while kill -0 "$gmember" 2>/dev/null; do sleep 0.05; done
echo $((gpgid - 1)) >/proc/sys/kernel/ns_last_pid
bash -c 'echo $$' >"$work/next"
[[ "$(cat "$work/next")" == "$gpgid" ]] || { echo "CONTROL 1b FAILED"; exit 1; }
echo "control 1b: with the member gone, asking for $gpgid gives $gpgid"

# 2. A group whose only member is a zombie: its parent never waits.
bash -c 'setsid sleep 0.2 & echo $! >'"$work/zombie"'; exec sleep 5' &
holder=$!
sleep 1
zpgid=$(cat "$work/zombie")
state=$(awk '{print $3}' "/proc/$zpgid/stat")
echo "group $zpgid: its only member is in state $state"
[[ "$state" == Z ]] || { echo "FACT 2 not set up: member not a zombie"; exit 1; }
if kill -0 -- "-$zpgid" 2>/dev/null; then
  echo "FACT 2 holds: kill(-$zpgid, 0) succeeds with only a zombie in the group"
else
  echo "FACT 2 FAILED: kill(-$zpgid, 0) fails with a zombie member"
  exit 1
fi
kill "$holder"
wait "$holder" 2>/dev/null || true
```

## Signal origin (signal-hook 0.4.5), not needed after the revision

`iterator::exfiltrator::WithOrigin` reports `Cause::Kernel` for signals
whose `si_code` is `SI_KERNEL` and `Cause::Sent(Sent::User)` for `kill(2)`
(src/low_level/siginfo.rs). It needs the `extended-siginfo` feature,
which compiles C (`extended-siginfo-raw = ["cc"]`); a scratch crate
linking `SignalsInfo::<WithOrigin>` built through cross 0.2.5 for both
musl targets (cross's aarch64 linker wrapper prints its own shell trace
to stderr, a warning only). With decision 2 revised, sb doesn't use it.

## Library facts used

- portable-pty 0.9.0: `spawn_command` calls `setsid()`, so the child's
  group id is its pid, and resets SIGINT, SIGTERM and others to their
  defaults with the signal mask cleared (src/unix.rs); `Child::process_id()`
  gives the pid (src/lib.rs:141); `Child::kill` sends SIGHUP, then
  SIGKILL, to the pid only, and `clone_killer()`'s signaller sends SIGHUP
  only; a working directory that is unset or not a directory silently
  becomes `$HOME` (src/cmdbuilder.rs, `as_command`), so exec always sets
  and checks it; dropping the writer sends a newline and EOF.
- tokio 1.54.1: `Command::process_group(i32)` (src/process/mod.rs:790,
  unix); `kill_on_drop` sends SIGKILL to the pid only.
- rustix 1.1.6, `process` feature: `kill_process_group` and
  `test_kill_process_group` (src/process/kill.rs), `waitid` with
  `WaitId::Pgid` and `WaitIdOptions::NOWAIT` (src/process/wait.rs),
  `set_child_subreaper` (src/process/prctl.rs:866).

## Dependencies

A scratch crate (tokio with `rt`, `time`, `process`, `io-util`, `macros`;
portable-pty 0.9.0; signal-hook 0.4.5 with `extended-siginfo`; rustix with
`process`) linking `openpty`, `process_group(0)`,
`test_kill_process_group` and `SignalsInfo::<WithOrigin>`: compared with
sb's tree, apart from the spike's own `macros`, it adds portable-pty with
nix 0.28.0, filedescriptor 0.8.3, serial2 0.2.39, shell-words 1.1.1,
downcast-rs 1.2.1 and thiserror 1.0.69 (beside the workspace's thiserror
2, a duplicate deny.toml's `multiple-versions = "warn"` will report when
sb links it), and bumps log (already in sb's tree through tracing-log).
`cargo deny check` with the repository's deny.toml passes on that tree:
advisories, bans, licences and sources ok.
