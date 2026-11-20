#!/usr/bin/env bash
# The action's prepare mode (release spec, "Saltbox's toolchain update"):
# finds the newest CPython of PYTHON_MINOR that the released sb's uv
# supports, preflights Saltbox's requirements on it, prints the diff of
# Saltbox's four toolchain files and leaves them, with the master commit
# they were made against, in OUTPUT_DIR for the commit mode (publish.sh).
# It holds no token: the preflight runs the packages' own code (their
# builds and entry points), so the write token lives in another job.
# Ported from sb-go's update.sh (4006309), which also committed; its
# dry run is this mode.

set -euo pipefail

uv_version=${UV_VERSION:?UV_VERSION is required}
: "${SB_RELEASE:?SB_RELEASE is required}"
output_dir=${OUTPUT_DIR:?OUTPUT_DIR is required}
if [[ ! "$uv_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid embedded sb uv version: $uv_version" >&2
    exit 1
fi
if [[ $output_dir != /* || -e $output_dir ]]; then
    echo "OUTPUT_DIR must be an absolute path that doesn't exist yet: $output_dir" >&2
    exit 1
fi
if [[ ! "$PYTHON_MINOR" =~ ^[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid Python minor: $PYTHON_MINOR" >&2
    exit 1
fi

# shellcheck disable=SC1091 # GitHub supplies GITHUB_ACTION_PATH.
source "$GITHUB_ACTION_PATH/managed-python.sh"
workdir=$(managed_python_workspace)
trap 'rm -rf "$workdir"' EXIT

archive="$workdir/uv.tar.gz"
# shellcheck disable=SC1091 # GitHub supplies GITHUB_ACTION_PATH at action runtime.
source "$GITHUB_ACTION_PATH/download.sh"
download_with_retry \
    "https://github.com/astral-sh/uv/releases/download/$uv_version/uv-x86_64-unknown-linux-gnu.tar.gz" \
    "$archive"
tar --extract --gzip --file "$archive" --directory "$workdir"
uv_bin=$(find "$workdir" -type f -path '*/uv' -print -quit)
if [[ -z "$uv_bin" ]]; then
    echo "The uv archive did not contain a uv binary" >&2
    exit 1
fi
chmod 0755 "$uv_bin"
if [[ "$(managed_python_command "$workdir" uv "$workdir" "$uv_bin" --version)" != "uv $uv_version"* ]]; then
    echo "Downloaded uv does not report version $uv_version" >&2
    exit 1
fi

catalog="$workdir/python-catalog.json"
managed_python_command "$workdir" uv "$workdir" "$uv_bin" python list "cpython@$PYTHON_MINOR" \
    --all-versions \
    --managed-python \
    --output-format json > "$catalog"

candidate=$(jq -r --arg minor "$PYTHON_MINOR" '
    [
      .[]
      | select(.implementation == "cpython")
      | select(.os == "linux")
      | select(.arch == "x86_64")
      | select(.libc == "gnu")
      | select(.variant == "default")
      | select(.version | startswith($minor + "."))
    ]
    | sort_by(.version_parts.major, .version_parts.minor, .version_parts.patch)
    | last
    | .version // empty
' "$catalog")
if [[ -z "$candidate" ]]; then
    echo "uv $uv_version did not report a compatible CPython $PYTHON_MINOR Linux x86_64 GNU build" >&2
    exit 1
fi

managed_python_command "$workdir" uv "$workdir" "$uv_bin" python install \
    --managed-python \
    --no-bin \
    --install-dir "$workdir/python" \
    "$candidate"
python_bin=$(managed_python_command "$workdir" uv "$workdir" "$uv_bin" python find \
    --managed-python \
    --no-project \
    --no-python-downloads \
    "$candidate")
if [[ "$(managed_python_command "$workdir" python "$workdir" "$python_bin" --version)" != "Python $candidate" ]]; then
    echo "Installed Python does not report version $candidate" >&2
    exit 1
fi

saltbox_clone_url=${SALTBOX_CLONE_URL:-"https://github.com/$SALTBOX_REPOSITORY.git"}
git clone --depth=1 --branch master "$saltbox_clone_url" "$workdir/saltbox"
base_commit=$(git -C "$workdir/saltbox" rev-parse HEAD)
current=$(tr -d '[:space:]' < "$workdir/saltbox/.python-version")
if [[ ! "$current" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Saltbox .python-version is invalid: $current" >&2
    exit 1
fi
current_uv=$(tr -d '[:space:]' < "$workdir/saltbox/.uv-version")
if [[ ! "$current_uv" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Saltbox .uv-version is invalid: $current_uv" >&2
    exit 1
fi
if [[ "$(printf '%s\n%s\n' "$current" "$candidate" | sort -V | tail -n 1)" != "$candidate" ]]; then
    echo "uv $uv_version reports Python $candidate, which would downgrade Saltbox Python $current" >&2
    exit 1
fi
if [[ "$(printf '%s\n%s\n' "$current_uv" "$uv_version" | sort -V | tail -n 1)" != "$uv_version" ]]; then
    echo "sb's uv $uv_version would downgrade Saltbox's uv requirement $current_uv" >&2
    exit 1
fi
candidate_lock="$workdir/requirements-saltbox.txt"
managed_python_command "$workdir" uv "$workdir/saltbox" "$uv_bin" pip compile \
    --python-version "$candidate" \
    --generate-hashes \
    --output-file "$candidate_lock" \
    requirements/requirements-saltbox.in
managed_python_command "$workdir" uv "$workdir" "$uv_bin" venv \
    --python "$python_bin" \
    --no-project \
    --no-python-downloads \
    "$workdir/preflight-venv"
managed_python_command "$workdir" uv "$workdir" "$uv_bin" pip sync \
    --python "$workdir/preflight-venv/bin/python" \
    --require-hashes \
    "$candidate_lock"
managed_python_command "$workdir" uv "$workdir" "$uv_bin" pip check --python "$workdir/preflight-venv/bin/python"
managed_python_command "$workdir" entrypoint "$workdir/preflight-venv" "$workdir/preflight-venv/bin/ansible" --version
managed_python_command "$workdir" entrypoint "$workdir/preflight-venv" "$workdir/preflight-venv/bin/certbot" --version
managed_python_command "$workdir" entrypoint "$workdir/preflight-venv" "$workdir/preflight-venv/bin/apprise" --version

update_toolchain_files() {
    printf '%s\n' "$candidate" > "$workdir/saltbox/.python-version"
    printf '%s\n' "$uv_version" > "$workdir/saltbox/.uv-version"
    managed_python_command "$workdir" python "$workdir/saltbox" "$python_bin" - "$uv_version" <<'PY'
import re
import sys
from pathlib import Path

path = Path(".github/renovate.json")
content = path.read_text(encoding="utf-8")
updated, count = re.subn(
    r'("constraints"\s*:\s*\{\s*"uv"\s*:\s*")[^"]+("\s*\})',
    rf"\g<1>{sys.argv[1]}\g<2>",
    content,
    count=1,
)
if count != 1:
    raise SystemExit("Unable to update constraints.uv in Saltbox Renovate configuration")
path.write_text(updated, encoding="utf-8")
PY
    # Saltbox CI regenerates this public command; keep launch isolation out of
    # the persisted header so its byte-for-byte lockfile check stays stable.
    managed_python_command "$workdir" uv "$workdir/saltbox" "$uv_bin" pip compile \
        --python-version "$candidate" \
        --generate-hashes \
        --custom-compile-command "uv pip compile --python-version $candidate --generate-hashes --output-file requirements/requirements-saltbox.txt requirements/requirements-saltbox.in" \
        --output-file requirements/requirements-saltbox.txt \
        requirements/requirements-saltbox.in
}

# shellcheck source-path=SCRIPTDIR source=commit.sh # GitHub supplies GITHUB_ACTION_PATH.
source "$GITHUB_ACTION_PATH/commit.sh"
update_toolchain_files
git -C "$workdir/saltbox" diff -- "${saltbox_toolchain_files[@]}"
changed=true
if git -C "$workdir/saltbox" diff --quiet -- "${saltbox_toolchain_files[@]}"; then
    changed=false
    echo "Saltbox already contains the requested Python toolchain state"
fi
mkdir -p -- "$output_dir/files"
printf '%s\n' "$base_commit" > "$output_dir/base-commit"
for file in "${saltbox_toolchain_files[@]}"; do
    mkdir -p -- "$(dirname -- "$output_dir/files/$file")"
    cp -- "$workdir/saltbox/$file" "$output_dir/files/$file"
done
printf '%s\n' "$changed" > "$output_dir/changed"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    echo "changed=$changed" >> "$GITHUB_OUTPUT"
fi
echo "Prepared Saltbox's toolchain for sb $SB_RELEASE: Python $candidate and uv $uv_version (changed: $changed)"
