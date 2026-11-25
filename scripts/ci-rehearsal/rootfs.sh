#!/usr/bin/env bash
# The CI rehearsal's runner root (scripts/ci-rehearsal.sh): an Ubuntu
# root filesystem of either architecture, entered as root of a user
# namespace of its own inside the rehearsal's x86_64 container, so that
# nothing run in it, root through sudo included, is root of the host.
#
# - The user namespace maps uids and gids 0-65535 to 100000-165535 on the
#   host: its root has every capability over its own mounts, network and
#   PID namespaces (check.sh's sandbox, the tests' namespaces), none over
#   the host's; a host file it may write it writes as uid 100000. The
#   root filesystem is unpacked and provisioned inside it, so its owners
#   are those ids; `nobody` (65534) exists for the tests. A host file
#   bound in (the repository at /src) is read-only there, and its owner,
#   unmapped, shows as nobody.
# - arm64: a binfmt_misc handler sends every aarch64 ELF that is
#   executed to qemu-aarch64, registered in that namespace's own
#   binfmt_misc instance: one mounted inside a user namespace is new and
#   empty, and its handlers apply to that namespace's processes alone
#   (Linux 6.7 and later; docs/platform-notes/binfmt-misc-user-namespaces.md).
#   `F` opens qemu when the handler is registered, so it runs from the
#   container's root; `C` takes a setuid program's credentials from the
#   program, so sudo works. The magic and mask are qemu's own for
#   aarch64 (qemu's scripts/qemu-binfmt-conf.sh).
#   https://docs.kernel.org/admin-guide/binfmt-misc.html
#   https://www.qemu.org/docs/master/user/main.html
# - The root filesystem becomes the mount namespace's root (pivot_root),
#   so its mount table holds only its own mounts, as a container's does:
#   check.sh's sandbox remounts every mount it lists.
#
#   rootfs.sh fetch RELEASE ARCH ROOTFS   unpack the latest ubuntu-base
#                                         RELEASE ARCH tarball into
#                                         ROOTFS, checked against its
#                                         signed SHA256SUMS
#   rootfs.sh enter ARCH ROOTFS CMD...    run CMD as the namespace's root
#                                         inside ROOTFS, with /src and
#                                         each directory in
#                                         ROOTFS_BINDS (spaces between)
#                                         bound in read-only at the same
#                                         paths
set -euo pipefail

# The namespace's ids on the host, and its root as the process's user
# (unshare maps the ranges; without --setuid the caller, host root,
# unmapped, would be nobody there).
MAP=(--map-users=0:100000:65536 --map-groups=0:100000:65536 --setuid=0 --setgid=0)

usage() {
  echo "usage: rootfs.sh fetch RELEASE ARCH ROOTFS | rootfs.sh enter ARCH ROOTFS CMD..." >&2
  exit 2
}

# The ubuntu-base images: https://cdimage.ubuntu.com/ubuntu-base/releases/
fetch() {
  local release=$1 arch=$2 rootfs=$3 base work file
  base="https://cdimage.ubuntu.com/ubuntu-base/releases/$release/release"
  work=$(mktemp -d)
  curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/SHA256SUMS"
  curl -fsSL --retry 3 -o "$work/SHA256SUMS.gpg" "$base/SHA256SUMS.gpg"
  gpgv --keyring /usr/share/keyrings/ubuntu-archive-keyring.gpg "$work/SHA256SUMS.gpg" "$work/SHA256SUMS"
  # The latest point release: ubuntu-base-24.04.5-base-arm64.tar.gz
  # after 24.04.4's.
  file=$(sed -nE "s/^[0-9a-f]{64} \\*?(ubuntu-base-[0-9.]+-base-$arch\\.tar\\.gz)\$/\\1/p" "$work/SHA256SUMS" |
    sort -V | tail -n 1)
  [[ -n $file ]] || { echo "rootfs: no $arch tarball in $base/SHA256SUMS" >&2; exit 1; }
  echo "rootfs: $file"
  curl -fsSL --retry 3 -o "$work/$file" "$base/$file"
  (cd "$work" && grep -E " \*?$file\$" SHA256SUMS | sed 's/ \*/  /' | sha256sum -c -)
  # Unpacked by the namespace's root, so the files are its ids'.
  mkdir -p "$rootfs"
  chown 100000:100000 "$rootfs" "$work" "$work/$file"
  unshare "${MAP[@]}" -- tar -xzf "$work/$file" -C "$rootfs"
  rm -rf "$work"
}

# Inside the namespaces: the mounts, the handler, the new root, then CMD.
inner() {
  local arch=$1 rootfs=$2 d
  shift 2
  mount --bind "$rootfs" "$rootfs"
  mount -t proc proc "$rootfs/proc"
  mount --rbind /sys "$rootfs/sys"
  mount --rbind /dev "$rootfs/dev"
  if [[ $arch == arm64 ]]; then
    mount -t binfmt_misc binfmt_misc "$rootfs/proc/sys/fs/binfmt_misc"
    printf '%s' ':qemu-aarch64:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\xb7\x00:\xff\xff\xff\xff\xff\xff\xff\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/usr/bin/qemu-aarch64:FC' \
      >"$rootfs/proc/sys/fs/binfmt_misc/register"
  fi
  for d in /src ${ROOTFS_BINDS:-}; do
    [[ -d $d ]] || continue
    mkdir -p "$rootfs$d"
    mount --rbind "$d" "$rootfs$d"
    mount -o remount,bind,ro "$rootfs$d"
  done
  # The network's names, as Docker gives its containers.
  cp /etc/resolv.conf /etc/hosts /etc/hostname "$rootfs/etc/"
  mkdir -p "$rootfs/.old-root"
  cd "$rootfs"
  pivot_root . .old-root
  cd /
  umount -l /.old-root
  rmdir /.old-root
  exec "$@"
}

case ${1:-} in
fetch)
  (($# == 4)) || usage
  fetch "$2" "$3" "$4"
  ;;
enter)
  (($# >= 4)) || usage
  [[ $2 == amd64 || $2 == arm64 ]] || usage
  arch=$2
  rootfs=$(realpath -- "$3")
  shift 3
  exec unshare "${MAP[@]}" --mount --pid --fork --kill-child --propagation private -- \
    bash -c "set -euo pipefail; $(declare -f inner)"'; inner "$@"' inner "$arch" "$rootfs" "$@"
  ;;
*) usage ;;
esac
