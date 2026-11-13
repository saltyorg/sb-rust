#!/usr/bin/env bash
# The `journal` example (features `testing`) the journal scripts run, built
# the way sb ships (ADR-020, decision 5): through cross, in the image
# build/Dockerfile makes of the oldest supported release (Cross.toml), for
# x86_64-unknown-linux-gnu, in target/cross-gnu as check.sh's production
# build. It needs that release's glibc at most, so it runs on the host and
# in a container of every supported release, whichever host built it; not
# in an older release's (compat.sh's writers run it on the host instead).
#
# Sourced by compare.sh, compat.sh, follow.sh, seeds.sh and stress.sh,
# before their cd: `sbj_bin` sets `bin` to SBJ_BIN, a binary built already
# (CI builds it once), resolved to an absolute path, or else to a fresh
# build. Run as a script, it builds the example and prints its path (the
# workflow's build step). Needs cross and Docker (CONSTRAINTS.md, Tools).
#
# The build runs in cross's container, so a systemd-run scope around cross
# would not cap it: CROSS_CONTAINER_OPTS passes the caps to that container's
# `docker run` (cross 0.2.5's src/docker/shared.rs and CHANGELOG):
# https://github.com/cross-rs/cross/blob/v0.2.5/src/docker/shared.rs
# https://github.com/cross-rs/cross/blob/v0.2.5/CHANGELOG.md
# https://docs.docker.com/engine/containers/resource_constraints/

sbj_bin() {
  if [ -n "${SBJ_BIN:-}" ]; then
    if ! bin=$(realpath -e "$SBJ_BIN") || [ ! -x "$bin" ]; then
      echo "SBJ_BIN is not an executable file" >&2
      exit 2
    fi
    return
  fi
  local root target=x86_64-unknown-linux-gnu tool
  for tool in cross docker; do
    command -v "$tool" >/dev/null ||
      { echo "$tool is missing: see CONSTRAINTS.md, Tools (or set SBJ_BIN)" >&2; exit 2; }
  done
  root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
  (cd "$root" &&
    CARGO_TARGET_DIR=$root/target/cross-gnu CROSS_CONTAINER_OPTS='--memory 4g --cpus 2' \
      cross build -q --locked --release --target "$target" -p sb-journal --features testing --example journal)
  bin=$root/target/cross-gnu/$target/release/examples/journal
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  # Always a fresh build here; stdout carries only its path.
  SBJ_BIN=''
  sbj_bin >&2
  echo "$bin"
fi
