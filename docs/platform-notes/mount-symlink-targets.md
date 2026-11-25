# A mount onto a symlink that leads nowhere

Verified 2026-11-26 for the dead-DNS test harness
(`crates/sb/tests/dead_dns/mod.rs`).

**Software:** util-linux 2.37.2 (Ubuntu 22.04), 2.39.3 (Ubuntu 24.04)
and 2.41.3 (Ubuntu 26.04), each in the `ubuntu:<release>` image, amd64,
on a Linux 6.8 kernel.

**Method:** a throwaway container per release (`docker run --rm` with
`CAP_SYS_ADMIN`, `CAP_NET_ADMIN` and AppArmor off), never the host's
`/etc`. Inside it, as root:

1. `/etc/resolv.conf` made the way a GitHub runner has it:
   systemd-resolved's link `../run/systemd/resolve/stub-resolv.conf`,
   its target a file (Docker's bind mount on it taken off first).
2. In a private mount namespace, `/run` covered with an empty read-only
   tmpfs, as `scripts/check.sh`'s test sandbox covers it.
3. There, in a further private mount namespace: `mount --bind FILE
   /etc/resolv.conf`.
4. There, instead: the harness's own script, an overlay on `/etc`
   whose upper layer (on a tmpfs) holds `resolv.conf` and
   `nsswitch.conf`; then `cat /etc/resolv.conf` and a read of
   `/etc/passwd`.

## What mount does

The same on all three releases:

- **The bind is refused:** `mount: /etc/resolv.conf: mount point is a
  symbolic link to nowhere.`, exit 32 (mount(8)'s "mount failure"). The
  kernel follows a symlink at a mount point, and in that namespace the
  link's target, under the covered `/run`, doesn't exist; nor can it be
  made there, `/run` being read-only. A test that binds onto a runner's
  `/etc/resolv.conf` in the sandbox ends this way, before sb starts.
- **Outside the covered `/run`** the same bind succeeds: mount(8) binds
  onto the link's target.
- **The overlay works:** `/etc/resolv.conf` reads as the upper layer's
  file, which hides the lower layer's link
  (<https://docs.kernel.org/filesystems/overlayfs.html#directories>),
  and the rest of `/etc` reads as before. The lower layer is `/etc`'s
  file system without the mounts on it: in a container, the image's
  `/etc/hosts`, not Docker's.
