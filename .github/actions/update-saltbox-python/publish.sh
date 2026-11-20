#!/usr/bin/env bash
# The action's commit mode (release spec, "Saltbox's toolchain update"):
# commits the toolchain files the prepare mode (update.sh) left in
# INPUT_DIR to Saltbox's master, signed by GitHub as the App, if master is
# still the commit they were made against. It runs no package code (git,
# jq and gh only), so the write token never meets the preflight's
# packages. sb-go's update.sh did both halves in one job.

set -euo pipefail

token=${SALTBOX_TOKEN:?SALTBOX_TOKEN is required}
# Kept from the environment of everything this starts: commit.sh passes it
# to gh alone.
unset SALTBOX_TOKEN
input_dir=${INPUT_DIR:?INPUT_DIR is required}
uv_version=${UV_VERSION:?UV_VERSION is required}
: "${SB_RELEASE:?SB_RELEASE is required}"
repository=${SALTBOX_REPOSITORY:?SALTBOX_REPOSITORY is required}

fail() {
    echo "$*" >&2
    exit 1
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source-path=SCRIPTDIR source=commit.sh
source "$script_dir/commit.sh"

# The handover holds the base commit, the changed flag and the four files,
# nothing else, and the versions are versions.
base_commit=$(tr -d '[:space:]' < "$input_dir/base-commit")
[[ $base_commit =~ ^[0-9a-f]{40}$ ]] || fail "The prepared base commit is invalid: $base_commit"
unexpected=$(find "$input_dir/files" -mindepth 1 -printf '%P\n' |
    grep -vxF -e .github -e requirements "${saltbox_toolchain_files[@]/#/-e}" || true)
[[ -z $unexpected ]] || fail "The prepared update has files it may not commit: $unexpected"
for file in "${saltbox_toolchain_files[@]}"; do
    [[ -f "$input_dir/files/$file" && ! -L "$input_dir/files/$file" ]] || fail "The prepared update lacks $file"
done
python_version=$(tr -d '[:space:]' < "$input_dir/files/.python-version")
[[ $python_version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "The prepared .python-version is invalid: $python_version"
[[ $(tr -d '[:space:]' < "$input_dir/files/.uv-version") == "$uv_version" ]] ||
    fail "The prepared .uv-version isn't the released uv $uv_version"

workdir=$(mktemp -d)
trap 'rm -rf -- "$workdir"' EXIT
saltbox_clone_url=${SALTBOX_CLONE_URL:-"https://github.com/$repository.git"}
git clone --quiet --depth=1 --branch master "$saltbox_clone_url" "$workdir/saltbox"
head=$(git -C "$workdir/saltbox" rev-parse HEAD)
[[ $head == "$base_commit" ]] ||
    fail "Saltbox master moved from $base_commit to $head since the update was prepared; re-run the saltbox-prepare job (saltbox-commit follows)"

# The prepare job ran package code, which could have rewritten the
# handover after it was made, so its contents are checked against master
# here, where no package code runs (release spec, "Saltbox's toolchain
# update"):
# - Python and uv don't go down;
# - renovate.json is master's with constraints.uv set to the released uv
#   (as JSON: update.sh keeps master's formatting);
# - the lock has only uv's lines: comments, `name==version`, and
#   `--hash=sha256:` lines, so no index, URL or other option can point
#   pip anywhere but PyPI, where a hash that isn't the file's fails the
#   install.
saltbox="$workdir/saltbox"
newest() { printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1; }
current_python=$(tr -d '[:space:]' < "$saltbox/.python-version")
current_uv=$(tr -d '[:space:]' < "$saltbox/.uv-version")
[[ $(newest "$current_python" "$python_version") == "$python_version" ]] ||
    fail "The prepared Python $python_version would downgrade Saltbox's $current_python"
[[ $(newest "$current_uv" "$uv_version") == "$uv_version" ]] ||
    fail "The released uv $uv_version would downgrade Saltbox's $current_uv"
jq -e --arg uv "$uv_version" --slurpfile prepared "$input_dir/files/.github/renovate.json" \
    '(.constraints.uv = $uv) == $prepared[0]' "$saltbox/.github/renovate.json" > /dev/null ||
    fail "The prepared renovate.json changes more than constraints.uv"
lock="$input_dir/files/requirements/requirements-saltbox.txt"
odd=$(grep -nvE '^$|^ *#|^[A-Za-z0-9][A-Za-z0-9._-]*==[A-Za-z0-9.+!_-]+( \\)?$|^ +--hash=sha256:[0-9a-f]{64}( \\)?$' "$lock" | head -n 5 || true)
[[ -z $odd ]] || fail "The prepared lock has lines uv doesn't write: $odd"
for file in "${saltbox_toolchain_files[@]}"; do
    cp -- "$input_dir/files/$file" "$workdir/saltbox/$file"
done
if git -C "$workdir/saltbox" diff --quiet -- "${saltbox_toolchain_files[@]}"; then
    echo "Saltbox already contains the requested Python toolchain state"
    exit 0
fi
# What is committed, shown by this job (the prepare job's log is its own).
git -C "$workdir/saltbox" --no-pager diff --stat -- "${saltbox_toolchain_files[@]}"
git -C "$workdir/saltbox" --no-pager diff -- .python-version .uv-version .github/renovate.json

# expectedHeadOid checks the original master atomically with commit creation.
commit_toolchain_update "$workdir/saltbox" "$base_commit" "$repository" "$token" "$workdir"
echo "Updated Saltbox master for sb $SB_RELEASE with Python $python_version and uv $uv_version"
