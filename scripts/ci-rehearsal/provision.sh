#!/usr/bin/env bash
# Provisions a CI rehearsal's root filesystem, as root inside it, the way
# GitHub's Ubuntu runner images are built for what ci.yml's task job
# relies on (scripts/ci-rehearsal.sh). Each part names the runner-images
# build script it follows:
# https://github.com/actions/runner-images/tree/main/images/ubuntu/scripts/build
# - the apt packages of the image's toolset (install-apt-vital.sh,
#   install-apt-common.sh): toolset-<release>[-arm64].json's vital,
#   common and cmd packages, read from runner-images' main branch when
#   the rehearsal image is built;
# - git from the git-core PPA, with `safe.directory = *` in
#   /etc/gitconfig (install-git.sh);
# - python3 with pip and venv (install-python.sh);
# - rustup's stable toolchain, minimal profile, with rustfmt and clippy,
#   in /etc/skel (install-rust.sh), so the runner user's home has it;
# - /opt/hostedtoolcache, and /opt and /usr/share open to everyone
#   (configure-environment.sh, configure-system.sh);
# - the runner user, uid 1001, with passwordless sudo, its home 0750.
# Not installed: the image's other toolchains (Docker, .NET, Java,
# browsers, ...), which the task job doesn't use, and its toolcache's
# preinstalled Node, Go and Python: the setup actions download the same
# builds (actions/node-versions, go-versions, python-versions).
# Besides the image: Node 24 for running the job's JavaScript actions (the
# runner's own `externals`), and python3-yaml for the job runner (job.py).
#
# Usage: provision.sh RELEASE ARCH   (22.04|24.04|26.04, amd64|arm64)
set -euo pipefail

release=${1:?usage: provision.sh RELEASE ARCH}
arch=${2:?usage: provision.sh RELEASE ARCH}
case $arch in
amd64) suffix='' node_arch=x64 ;;
arm64) suffix=-arm64 node_arch=arm64 ;;
*) echo "provision: unknown architecture $arch" >&2; exit 2 ;;
esac
export DEBIAN_FRONTEND=noninteractive
# A download that fails is tried again, as the images' own apt wrapper
# tries it again (configure-apt-mock.sh).
printf 'Acquire::Retries "10";\n' >/etc/apt/apt.conf.d/80-rehearsal-retries
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl jq gnupg software-properties-common

toolset=$(curl -fsSL --retry 3 \
  "https://raw.githubusercontent.com/actions/runner-images/main/images/ubuntu/toolsets/toolset-${release//./}${suffix}.json")
# A list gone from the toolset fails here (jq can't iterate null), not
# as an image with fewer packages.
names=$(jq -r '.apt.vital_packages[], .apt.common_packages[], .apt.cmd_packages[]' <<<"$toolset")
mapfile -t listed <<<"$names"
# A name with no installation candidate (24.04's `netcat`, a virtual
# package with two providers) is left out, and said.
packages=()
for p in "${listed[@]}"; do
  candidate=$(apt-cache policy "$p" | awk '/Candidate:/ { print $2 }')
  if [[ -n $candidate && $candidate != '(none)' ]]; then
    packages+=("$p")
  else
    echo "provision: $p has no installation candidate here, left out"
  fi
done
apt-get install -y --no-install-recommends "${packages[@]}"

add-apt-repository -y ppa:git-core/ppa
apt-get update
apt-get install -y --no-install-recommends git
add-apt-repository -y --remove ppa:git-core/ppa
printf '[safe]\n        directory = *\n' >>/etc/gitconfig

apt-get install -y --no-install-recommends python3 python3-dev python3-pip python3-venv python3-yaml
apt-get clean
rm -rf /var/lib/apt/lists/*

export RUSTUP_HOME=/etc/skel/.rustup CARGO_HOME=/etc/skel/.cargo
curl -fsSL --retry 3 https://sh.rustup.rs | sh -s -- -y --default-toolchain=stable --profile=minimal
"$CARGO_HOME/bin/rustup" component add rustfmt clippy
rm -rf "${CARGO_HOME:?}"/registry/*

mkdir -p /opt/hostedtoolcache
chmod -R 777 /opt /usr/share

# The runner user (GitHub's runners: uid 1001, in adm and users), its
# home 0750, as useradd makes it on Ubuntu (HOME_MODE in login.defs) and
# as the hosted runners have it.
useradd --create-home --uid 1001 --shell /bin/bash --groups adm,users runner
chmod 0750 /home/runner
printf 'runner ALL=(ALL) NOPASSWD:ALL\n' >/etc/sudoers.d/runner
chmod 0440 /etc/sudoers.d/runner

# Node 24, the runner's own for `using: node24` actions, checked against
# nodejs.org's SHASUMS256.txt for the latest 24.x.
node_dir=/opt/runner-externals/node24
dist=https://nodejs.org/dist/latest-v24.x
work=$(mktemp -d)
curl -fsSL --retry 3 -o "$work/SHASUMS256.txt" "$dist/SHASUMS256.txt"
file=$(awk -v a="linux-$node_arch.tar.xz" '$2 ~ "^node-v[0-9.]+-" a "$" { print $2 }' "$work/SHASUMS256.txt")
curl -fsSL --retry 3 -o "$work/$file" "$dist/$file"
(cd "$work" && grep -F " $file" SHASUMS256.txt | sha256sum -c -)
mkdir -p "$node_dir"
tar -xJf "$work/$file" -C "$node_dir" --strip-components=1
rm -rf "$work"
"$node_dir/bin/node" --version
