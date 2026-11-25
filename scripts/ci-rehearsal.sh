#!/usr/bin/env bash
# The CI rehearsal: ci.yml's `task` job on each of its runners, in
# containers made the way GitHub's runners are, so that what only a
# runner shows fails here and not on a push (the maintainer, 2026-11-25).
#
#   scripts/ci-rehearsal.sh [--rebuild] [--arch amd64|arm64] [RUNNER...]
#
# RUNNER: a runner label of the job's matrix (ubuntu-24.04, ...); by
# default its amd64 runners, which check.sh full runs too. arm64 is an
# opt-in, off by default, for reproducing an arm-specific problem:
# `--arch arm64` or an arm runner's label (ubuntu-24.04-arm). Its runs are
# emulated, hours long, and QEMU's quirks fail tests that pass on GitHub's
# native arm runners, which prove arm64 after a push
# (docs/platform-notes/binfmt-misc-user-namespaces.md; the maintainer,
# 2026-11-25). --rebuild makes the images again (they are kept as a
# cache, below).
#
# What it reproduces:
# - the runner: Ubuntu's release and architecture, GitHub's image's apt
#   packages, git, python3 and rustup
#   (scripts/ci-rehearsal/provision.sh), a non-root `runner` user with
#   passwordless sudo, its home /home/runner at 0750, the checkout at
#   /home/runner/work/sb-rust/sb-rust, the runner's variables;
# - the job: scripts/ci-rehearsal/job.py reads ci.yml and runs the job's
#   steps as the runner does, each `run` step as written and each action
#   from its repository at the pinned commit (install-action's binaries,
#   setup-node, setup-go, setup-python, rust-cache), so the tools come at
#   ci.yml's versions the way CI installs them, and check.sh task runs
#   with the command and environment ci.yml gives it, as root through
#   sudo, as a push to main does (base HEAD: every step);
# - the tree: HEAD with the working tree's changes (tracked, and
#   untracked but not ignored, as check.sh reads them) committed on top,
#   taken once at the start, so every runner checks the same tree; the
#   repository (and a worktree's common git directory) is bound in
#   read-only and cloned.
#
# Root: on amd64, the container's, as on a runner the VM's: the job's
# root through sudo, as the gate's own runs on this host are, with the
# capabilities its sandbox and tests use (CAP_SYS_ADMIN, CAP_NET_ADMIN,
# CAP_SYS_PTRACE) and AppArmor off, so the container is no boundary
# beyond the gate's own: root there can remount and write what root on
# the host can. That root also runs what the job downloads, which the
# gate's own runs don't: the pinned actions, install-action's binaries,
# the setup actions' builds, rustup, the PPA's git, each checked as CI
# checks it and no more. On arm64, a user namespace's whose ids are
# 100000-165535
# on the host (scripts/ci-rehearsal/rootfs.sh), with every capability
# over its own namespaces and none over the host's; its programs run
# under QEMU user-mode, through a binfmt_misc handler in that namespace's
# own instance, the host's handlers left as they are: everything
# emulated, builds included, several times slower than amd64, and
# timing-sensitive tests see that. What needs the initial namespace's
# capabilities differs there: a write clears a file's setuid bit, which
# root keeps (CAP_FSETID, Linux's setattr_should_drop_suidgid), so
# sb-config's owner_and_mode_are_kept_as_root fails on arm64 alone.
#
# What it can't reproduce: systemd (the container has none; check.sh's
# sandbox hides it in CI too); a VM's kernel (the host's is shared);
# on arm64, a VM's root (above); the runner image's other preinstalled
# software and its toolcache's prebuilt Node, Go and Python (the setup actions
# download the same builds); the GitHub token (actions run without one,
# so `github.token` is empty and API calls are anonymous); rust-cache's
# cache service (every run is a cold build); the post steps; the
# runner's CPU count and speed (the container's caps below), so the
# job's timeout-minutes is only printed.
#
# The containers run under Docker's own cgroups, outside a scope around
# this script, so they are capped here: each on the first three quarters
# of the host's CPUs, at the lowest CPU weight and I/O weight, with
# SB_REHEARSAL_CPUS CPUs as its quota (by default the CPU quota of the
# cgroup this script runs in, a systemd-run scope's, shared by the two
# containers; without one, a quarter of the host's),
# SB_REHEARSAL_MEMORY_GB of memory (the same, from its memory limit; a
# quarter of the host's, 32
# at most, by default) and no swap; two at once. Logs:
# target/ci-rehearsal/<runner>.log.
# Exit: 0 every runner passed, 1 one failed, 2 the rehearsal could not
# run.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow=.github/workflows/ci.yml
job=task
here=scripts/ci-rehearsal
out=target/ci-rehearsal
# The launcher image: arm64's x86_64 host side, and the reader of
# ci.yml.
LAUNCHER_FROM=ubuntu:26.04
LAUNCHER_PACKAGES="qemu-user ca-certificates curl gpgv ubuntu-keyring python3 python3-yaml util-linux util-linux-extra"
PARALLEL=2

die() {
  echo "ci-rehearsal: $*" >&2
  exit 2
}

# The containers this run made, removed on exit whatever happened.
names=()
declare -A runner_sum=()
cleanup() {
  local n
  for n in "${names[@]}"; do docker rm -f "$n" >/dev/null 2>&1 || true; done
  if [[ -n ${tree:-} ]]; then rm -rf -- "$tree"; fi
}

# commit_image TAG NAME: the stopped container NAME as the image TAG
# (REPOSITORY:PREFIX<hash>); the repository's other tags with the same
# PREFIX and no further dash, older images of the same kind, go (left
# when Docker refuses: an image another one was made from).
commit_image() {
  local tag=$1 name=$2 repo prefix old
  docker commit "$name" "$tag" >/dev/null || die "could not save $tag"
  docker rm "$name" >/dev/null || true
  repo=${tag%%:*}
  prefix=${tag#*:}
  prefix=${prefix%"${prefix##*-}"}
  while IFS= read -r old; do
    if [[ $old == "$repo:$prefix"* && ${old#"$repo:$prefix"} != *-* && $old != "$tag" ]]; then
      docker rmi "$old" >/dev/null 2>&1 || echo "ci-rehearsal: left $old (in use)"
    fi
  done < <(docker images "$repo" --format '{{.Repository}}:{{.Tag}}')
}

ensure_launcher() {
  if ((rebuild == 0)) && docker image inspect "$launcher" >/dev/null 2>&1; then return; fi
  local name="sb-rehearsal-build-$$-launcher" log="$out/launcher.build.log"
  names+=("$name")
  echo "ci-rehearsal: making $launcher ($log)"
  docker run --name "$name" "${caps[@]}" -e DEBIAN_FRONTEND=noninteractive "$LAUNCHER_FROM" bash -c \
    "apt-get update && apt-get install -y --no-install-recommends $LAUNCHER_PACKAGES && rm -rf /var/lib/apt/lists/*" \
    >"$log" 2>&1 || {
    tail -n 30 "$log" >&2
    die "could not make $launcher"
  }
  commit_image "$launcher" "$name"
}

# ensure_runner RELEASE ARCH: sets `image` to the runner image, made
# first if missing (or --rebuild): amd64, ubuntu:<release> provisioned;
# arm64, the launcher with the runner's root filesystem at /rootfs,
# unpacked and provisioned in its user namespace.
ensure_runner() {
  local release=$1 arch=$2 name log status=0
  image="sb-rust-rehearsal/runner:$release-$arch-${runner_sum[$arch]}"
  if ((rebuild == 0)) && docker image inspect "$image" >/dev/null 2>&1; then return; fi
  name="sb-rehearsal-build-$$-$release-$arch"
  log="$out/ubuntu-$release-$arch.build.log"
  names+=("$name")
  echo "ci-rehearsal: making $image ($log)"
  if [[ $arch == amd64 ]]; then
    docker run --name "$name" "${caps[@]}" -v "$root/$here:/src:ro" "ubuntu:$release" \
      bash /src/provision.sh "$release" "$arch" >"$log" 2>&1 || status=$?
  else
    docker run --name "$name" "${caps[@]}" "${arm64_privileges[@]}" -v "$root/$here:/src:ro" "$launcher" \
      bash -c "bash /src/rootfs.sh fetch $release $arch /rootfs && bash /src/rootfs.sh enter $arch /rootfs bash /src/provision.sh $release $arch" \
      >"$log" 2>&1 || status=$?
  fi
  if ((status != 0)); then
    tail -n 30 "$log" >&2
    die "could not make $image"
  fi
  commit_image "$image" "$name"
}

# run_one LABEL: the job on LABEL in a container of its image. First the
# job runner's own tests, on that image's Python.
run_one() {
  local label=$1 name="sb-rehearsal-$$-$1" inner
  local test_cmd=(python3 -B -m unittest discover --start-directory "/src/$here" --pattern 'test_*.py')
  local job_cmd=(python3 -B "/src/$here/job.py" run "/src/$workflow" "$job" "$label" --src /src --rev "$rev"
    --patch "/src/$tree/worktree.patch" --untracked "/src/$tree/untracked.tar")
  inner="$(printf '%q ' "${test_cmd[@]}") && $(printf '%q ' "${job_cmd[@]}")"
  if [[ ${arch_of[$label]} == amd64 ]]; then
    docker run --rm --init --name "$name" --hostname rehearsal "${caps[@]}" "${amd64_privileges[@]}" \
      -v "$root:/src:ro" "${git_mount[@]}" "${image_of[$label]}" bash -c "$inner"
  else
    docker run --rm --init --name "$name" --hostname rehearsal "${caps[@]}" "${arm64_privileges[@]}" \
      -v "$root:/src:ro" "${git_mount[@]}" -e ROOTFS_BINDS="${git_common:-}" "${image_of[$label]}" \
      bash "/src/$here/rootfs.sh" enter arm64 /rootfs bash -c "$inner"
  fi
}

# finished PID [STATUS]: records the run PID's result: STATUS, when
# `wait -n` has reaped it already, else its own.
finished() {
  local pid=$1 status=${2:-} l
  if [[ -z $status ]]; then
    status=0
    wait "$pid" || status=$?
  fi
  l=${label_of[$pid]}
  result[$l]=$status
  took[$l]=$((SECONDS - started[$pid]))
  echo "ci-rehearsal: $l: $(verdict "$status") in $((took[$l] / 60)) min"
}

# 2 is job.py's "could not run" (a workflow it can't run, a failed
# checkout), 125 Docker's.
verdict() {
  case $1 in
  0) echo passed ;;
  2 | 125) echo "could not run ($1)" ;;
  *) echo "failed ($1)" ;;
  esac
}

# The whole run is a function, read before any of it runs: editing this
# file while a rehearsal runs (hours, for arm64) can't change that run.
main() {
  cd "$root"
  rebuild=0
  local wanted=() only=''
  while (($#)); do
    case $1 in
    --rebuild) rebuild=1 ;;
    --arch)
      [[ ${2:-} == amd64 || ${2:-} == arm64 ]] || die "--arch takes amd64 or arm64"
      only=$2
      shift
      ;;
    -h | --help)
      sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
      exit 0
      ;;
    -*) die "unknown option $1" ;;
    *) wanted+=("$1") ;;
    esac
    shift
  done

  [[ $(uname -m) == x86_64 ]] || die "runs on an x86_64 host (arm64 is emulated from there)"
  command -v docker >/dev/null || die "docker not installed"

  # The caps (CLAUDE.md, Running things), from this host's size.
  local cpus_all pinned quota mem_gb
  cpus_all=$(nproc --all)
  pinned=$((cpus_all * 3 / 4))
  ((pinned >= 1)) || pinned=1
  quota=$((cpus_all / 4))
  mem_gb=$(($(awk '/^MemTotal:/ { print $2 }' /proc/meminfo) / 1024 / 1024 / 4))
  # The caller's own limits, when it runs in a capped cgroup (cgroup v2's
  # cpu.max, "QUOTA PERIOD" or "max PERIOD", and memory.max), shared by
  # the containers at once: Docker's cgroups are outside it.
  # https://docs.kernel.org/admin-guide/cgroup-v2.html#cpu-interface-files
  local cg max period
  cg=/sys/fs/cgroup$(awk -F: '$1 == "0" { print $3 }' /proc/self/cgroup)
  if read -r max period <"$cg/cpu.max" 2>/dev/null && [[ $max =~ ^[0-9]+$ && $period =~ ^[1-9][0-9]*$ ]]; then
    quota=$((max / period / PARALLEL))
  fi
  if read -r max <"$cg/memory.max" 2>/dev/null && [[ $max =~ ^[0-9]+$ ]]; then
    mem_gb=$((max / 1024 / 1024 / 1024 / PARALLEL))
  fi
  ((quota >= 1)) || quota=1
  ((mem_gb <= 32)) || mem_gb=32
  quota=${SB_REHEARSAL_CPUS:-$quota}
  mem_gb=${SB_REHEARSAL_MEMORY_GB:-$mem_gb}
  [[ $quota =~ ^[1-9][0-9]*$ && $mem_gb =~ ^[1-9][0-9]*$ ]] ||
    die "SB_REHEARSAL_CPUS and SB_REHEARSAL_MEMORY_GB are whole numbers"
  ((mem_gb >= 4)) || die "a rehearsal needs 4 GB of memory at least"
  caps=(--cpuset-cpus="0-$((pinned - 1))" --cpus="$quota" --cpu-shares=2 --blkio-weight=10
    --memory="${mem_gb}g" --memory-swap="${mem_gb}g" --ulimit nofile=65536:65536)
  # amd64: the capabilities of the runner's root that the job uses:
  # check.sh's sandbox makes mounts and namespaces (unshare, mount, ip:
  # CAP_SYS_ADMIN and CAP_NET_ADMIN, with which Docker's seccomp profile
  # allows them), and the tests read root-run sb's /proc/<pid>/fd, which
  # sb, not dumpable as root, opens only to CAP_SYS_PTRACE (ptrace(2),
  # "Ptrace access mode checking"). AppArmor's docker-default profile
  # refuses mount whatever the capability.
  amd64_privileges=(--cap-add SYS_ADMIN --cap-add NET_ADMIN --cap-add SYS_PTRACE
    --security-opt apparmor=unconfined)
  # arm64: no capability added; the container's root runs rootfs.sh
  # alone, which makes the user namespace (unshare, and its mounts, which
  # Docker's seccomp and AppArmor profiles refuse), whose new /proc mounts
  # only where the container's isn't masked
  # (https://docs.docker.com/reference/cli/docker/container/run/#security-opt).
  arm64_privileges=(--security-opt seccomp=unconfined --security-opt apparmor=unconfined
    --security-opt systempaths=unconfined)

  # Images: named by the hash of what makes them, kept as a cache
  # (CONSTRAINTS.md, "Images the gate keeps"); an older one of the same
  # kind is removed when its successor is made.
  local sum
  sum=$(echo "$LAUNCHER_FROM $LAUNCHER_PACKAGES" | sha256sum)
  launcher="sb-rust-rehearsal/launcher:${sum:0:12}"
  # A runner image by what makes it: provision.sh, and for arm64 the
  # launcher and rootfs.sh too.
  sum=$(sha256sum <"$here/provision.sh")
  runner_sum[amd64]=${sum:0:12}
  sum=$({ echo "$launcher"; cat "$here/provision.sh" "$here/rootfs.sh"; } | sha256sum)
  runner_sum[arm64]=${sum:0:12}
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  mkdir -p "$out"
  ensure_launcher

  # The runners: job.py reads them from ci.yml.
  local matrix line label release arch
  mapfile -t matrix < <(docker run --rm "${caps[@]}" -v "$root:/src:ro" "$launcher" \
    python3 -B "/src/$here/job.py" runners "/src/$workflow" "$job")
  ((${#matrix[@]})) || die "$workflow's $job job lists no runners"
  declare -A release_of=() arch_of=() image_of=()
  local labels=()
  # amd64 unless arm64 is asked for.
  if [[ -z $only ]] && ((${#wanted[@]} == 0)); then only=amd64; fi
  for line in "${matrix[@]}"; do
    read -r label release arch <<<"$line"
    release_of[$label]=$release
    arch_of[$label]=$arch
    if [[ -z $only || $arch == "$only" ]]; then labels+=("$label"); fi
  done
  if ((${#wanted[@]})); then
    [[ -z $only ]] || die "--arch or runners, not both"
    for label in "${wanted[@]}"; do
      [[ -n ${release_of[$label]+x} ]] || die "$workflow's $job job has no runner $label (it has: ${!release_of[*]})"
    done
    labels=("${wanted[@]}")
  fi
  ((${#labels[@]})) || die "$workflow's $job job has no ${only:-} runner"

  # A linked worktree's .git names its common git directory: mounted too,
  # read-only at its own path (and bound into arm64's root), for the
  # clone.
  git_common=$(git rev-parse --path-format=absolute --git-common-dir)
  git_mount=()
  if [[ $git_common == "$root/.git" ]]; then
    git_common=''
  else
    git_mount=(-v "$git_common:$git_common:ro")
  fi

  # The tree, once for every runner: HEAD, the tracked changes' patch,
  # the untracked files (not ignored) in a tar; this run's own, removed on
  # exit.
  tree="$out/tree.$$"
  mkdir -p "$tree"
  rev=$(git rev-parse HEAD) || die "no HEAD"
  git diff --binary --no-ext-diff --no-textconv HEAD -- >"$tree/worktree.patch" || die "could not save the tree's changes"
  git ls-files -z --others --exclude-standard | tar --null --no-recursion -T - -cf "$tree/untracked.tar" ||
    die "could not save the untracked files"
  echo "ci-rehearsal: $rev with the working tree's changes ($(stat -c %s "$tree/worktree.patch") bytes of patch," \
    "$(tar -tf "$tree/untracked.tar" | wc -l) untracked files)"

  # The images first, one at a time (each a long download and install).
  for label in "${labels[@]}"; do
    ensure_runner "${release_of[$label]}" "${arch_of[$label]}"
    image_of[$label]=$image
  done

  # The runs, PARALLEL at once.
  declare -A started=() label_of=() result=() took=()
  local running=0 done_pid pid status
  for label in "${labels[@]}"; do
    names+=("sb-rehearsal-$$-$label")
    if ((running >= PARALLEL)); then
      status=0
      wait -n -p done_pid || status=$?
      finished "$done_pid" "$status"
      running=$((running - 1))
    fi
    echo "ci-rehearsal: $label: started, log $out/$label.log"
    run_one "$label" >"$out/$label.log" 2>&1 &
    started[$!]=$SECONDS
    label_of[$!]=$label
    running=$((running + 1))
  done
  for pid in "${!label_of[@]}"; do
    [[ -n ${result[${label_of[$pid]}]+x} ]] || finished "$pid"
  done

  # A failed runner is the verdict; one that could not run, with none
  # failed, is none (2).
  local failed=0 unrun=0
  echo
  echo "ci-rehearsal: $rev, job $job"
  for label in "${labels[@]}"; do
    status=${result[$label]}
    printf '  %-18s %-17s %4d min  %s\n' "$label" "$(verdict "$status")" "$((took[$label] / 60))" "$out/$label.log"
    case $status in
    0) ;;
    2 | 125) unrun=1 ;;
    *) failed=1 ;;
    esac
  done
  if ((failed == 0 && unrun)); then return 2; fi
  return "$failed"
}

main "$@"; exit
