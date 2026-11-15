# motd's sources beside the tools sb-go ran

Verified 2026-11-14 for the `motd` module (modules plan Task M40; motd
spec, "Testing Strategy", "Container matrix", decision 6 and Differences
7, 8 and 9). Rerun 2026-11-15 with a closing session added (the
maintainer's decision on Difference 7).

**Software:** the `ubuntu:22.04`, `ubuntu:24.04` and `ubuntu:26.04`
images, amd64, with each release's own packages installed from its
archive:

| Release | systemd (logind, loginctl) | `last` | procps (`uptime`) |
|---|---|---|---|
| 22.04 | 249 (249.11-0ubuntu3; 249.11-0ubuntu3.23 at the rerun) | util-linux 2.37.2 | 3.3.17 |
| 24.04 | 255 (255.4-1ubuntu8; 255.4-1ubuntu8.18 at the rerun) | util-linux 2.39.3 | 4.0.4 |
| 26.04 | 259 (259.9-0ubuntu0.1) | wtmpdb 0.75.0 | 4.0.4 |

**Method:** `crates/motd/tests/matrix.sh`, one throwaway container per
release, booted with systemd as PID 1 in a private cgroup namespace (not
privileged: `CAP_SYS_ADMIN` only to remount its own cgroup and to mount,
`/sys` and `/proc/sys` read-only, no devices). Inside it, as root:

1. sshd started, and a synthetic user logged in over SSH three times:
   once logged out again, once logged out with a process left running
   (`nohup sleep 600 &`; its session stays, `closing`), and once still
   logged in.
2. A tmpfs mounted, and an anonymous volume bound a second time under a
   name with a space.
3. From step 1 on, an `/etc/update-motd.d/` script ran sb's `--sessions`
   and `--login`, `who` and `last` at each login, under pam_motd.
4. sb's `--distro`, `--kernel`, `--uptime`, `--sessions`, `--login`,
   `--disk` and `--gpu` lines compared with `lsb_release -ds`/`-cs`,
   `uname -r`, `uptime -p`, `who` (or logind's own table where utmp is
   gone), `last -w -n 5`, `df -H -x tmpfs -x overlay -x fuse.mergerfs -x
   fuse.rclone --output=target,pcent,size` with sb-go's `/dev`, `/sys`,
   `/proc` and `/run` rows dropped, and `lspci`'s display controllers
   with sb-go's exclusions.
5. `last -w -n 5`, loginctl's JSON and `loginctl show-session -p Id -p
   State` of every session written as the unit tests' fixtures
   (`crates/motd/tests/fixtures/logins/<release>/`), the kernel release
   and the boot time replaced by placeholders.
6. A command run over SSH without a terminal, and the counts again.

A loop file system was not mounted: the container has no devices.

## What the tools do

- **loginctl's JSON:** `loginctl list-sessions --output=json` writes a
  JSON array on 249 and 255. 259 takes `--output=json` without an error
  and writes its table; its JSON needs `--json=short`, which 249 and 255
  refuse (`unrecognized option`). sb picks the option by os-release's
  `VERSION_ID`: `--output=json` up to 24.04, `--json=short` after.
- **Session fields:** 249 writes `session`, `uid`, `user`, `seat`, `tty`;
  255 adds `state`, `idle` and `since`; 259 adds `leader` and `class`,
  and lists the user's service manager as a session of class `manager`
  beside the login (`user`). Its SSH session has no `tty`.
- **who:** on 22.04 and 24.04 it lists the one open login, as logind
  does. On 26.04 it lists nothing: utmp is no longer written.
- **last:** 26.04's base image has no `last`; it comes with the `wtmpdb`
  package. util-linux and wtmpdb write the same columns, with three
  changes in wtmpdb: the line `Day Mon D HH:MM - still logged in`
  (util-linux has no dash), the SSH login's terminal as `ssh` (util-linux:
  `pts/0`), and the trailer `wtmpdb begins ...` (util-linux: `wtmp
  begins ...`). Without `-w` both cut the user at 8 and the source at 16
  columns; with it they are whole. The first entry is the login still
  open (motd spec, Known limits).
- **Sessions beside who:** after the logins of step 1, sb's count
  (logind's sessions, the service manager's and the closing ones left
  out) equalled who's on 22.04 and 24.04 and logind's own open `user`
  rows on 26.04. A command run over SSH without a terminal (`ssh host
  cmd`) is a logind session but no utmp entry: with one running, who
  counted 1 (26.04: 0) and sb 2 on every release.
- **Closing sessions:** the login that logged out with a process left
  running stayed in logind's list as `closing` on all three releases
  (logind's states:
  https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html),
  and left utmp at the logout. Only 255 reports a session's `state` in
  the list. `loginctl show-session -p Id -p State <id>...` answers on
  every release with one block per session (`Id=`, `State=`), blocks
  apart by an empty line. Given an ID that isn't known, it prints the
  blocks before it, an error, and stops with exit 1.
- **At login, under pam_motd:** an update-motd.d script ran sb, `who`
  and `last` during each SSH login (Saltbox's 01-motd runs from the same
  place). logind already listed the login being made on every release,
  so sb counted it (and, at the third login, not the closing one); `who`
  counted nothing yet on 22.04 and 24.04 (sshd writes utmp after the PAM
  session opens; on 26.04 utmp isn't written).
  Last login on 22.04 and 24.04 was the previous login (wtmp gets the
  new one after pam_motd), and on 26.04 the login being made, `still
  logged in` (pam_wtmpdb runs before pam_motd).
- **The other sections:** Distribution, Kernel, Uptime and Disk Usage
  equalled the tools' text on all three releases (Difference 9), the
  tmpfs left out and the volume shown once. GPU equalled lspci's
  display controllers with sb-go's exclusions; the containers saw only
  controllers both sides exclude, so the names themselves rest on the
  unit tests of pciutils' forms.
