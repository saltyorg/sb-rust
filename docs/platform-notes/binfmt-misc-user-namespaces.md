# binfmt_misc in a user namespace: an arm64 root without the host's handlers

Verified 2026-11-25 for the CI rehearsal's arm64 runners
(`scripts/ci-rehearsal/rootfs.sh`, CONSTRAINTS.md "CI rehearsal").

**Software:** Linux 6.8 (Ubuntu 22.04's HWE kernel), Docker 29, QEMU
user-mode 10.2.1 (Ubuntu 26.04's `qemu-user`, static builds), util-linux
2.41 (`unshare --map-users`, `pivot_root`), the ubuntu-base 22.04, 24.04
and 26.04 amd64 and arm64 tarballs.

**Method:** in an `ubuntu:26.04` container with Docker's seccomp and
AppArmor profiles off and `systempaths=unconfined`, no added capability:
`unshare --map-users=0:100000:65536 --map-groups=0:100000:65536
--setuid=0 --setgid=0 --mount --pid --fork`, then `mount -t binfmt_misc`
inside it, listing the mount, writing
a qemu-aarch64 handler to its `register`, and running the arm64 root's
programs (`uname -m`, `dpkg --print-architecture`, apt, sudo, setpriv,
`unshare --mount --net --pid --mount-proc`) after `pivot_root` into it.
The host's `/proc/sys/fs/binfmt_misc` was listed before and after.

## What it does

- binfmt_misc mounted inside a user namespace is a new instance: it
  lists only `register` and `status`, none of the host's handlers, and a
  handler registered there runs that namespace's programs alone; the
  host's list is unchanged. (Linux 6.7 added this, "binfmt_misc: enable
  sandboxed mounts".) The same mount from an unprivileged user namespace
  on the host works too, and shows the same empty instance.
- The handler needs the `F` flag: qemu is opened when it is registered,
  from the container's root, so the arm64 root needs no x86_64 qemu of its
  own. With `C`, a setuid program (sudo) runs with its own credentials.
- With uids and gids 0-65535 mapped to 100000-165535, a root filesystem
  unpacked and provisioned inside the namespace is owned by those ids,
  and `nobody` (65534) exists. The namespace's root has every capability
  over the namespace's own mounts, network and PID namespaces and none
  over the host's: `/proc/sys/kernel/core_pattern` is not writable to it
  in the new `/proc`, and a host directory bound in read-only can't be
  written, nor remounted writable. A new `/proc` mounts only where the
  container's `/proc` isn't masked (`systempaths=unconfined`).
- `unshare --map-users` maps the ranges but leaves the caller's own uid
  as it was: host root, unmapped, is `nobody` there until `--setuid=0
  --setgid=0` makes it the namespace's root.
- `unshare --propagation private` needs `/` to be a mount point: in a
  chroot it fails with "cannot change root filesystem propagation:
  Invalid argument", so the root is bound to itself and entered with
  `pivot_root`, which also leaves only its own mounts in the mount table.
- On Ubuntu 26.04 `pivot_root` is in `util-linux-extra`, not
  `util-linux`.
- Checked on a host with `kernel.apparmor_restrict_unprivileged_userns`
  at 0, as Ubuntu 22.04 has it. Ubuntu 23.10 and later default it to 1,
  where a user namespace made without CAP_SYS_ADMIN gets no capabilities
  unless an AppArmor profile allows it; there the container's root would
  likely need CAP_SYS_ADMIN for the namespace's mounts (not verified).
