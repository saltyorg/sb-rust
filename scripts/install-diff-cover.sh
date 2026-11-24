#!/usr/bin/env bash
# Installs diff-cover (CONSTRAINTS.md's Tools table) as an isolated,
# locked tool: a virtual environment of its own on the system Python, with
# every package at the exact version and SHA-256 hashes of
# scripts/diff-cover/requirements.txt, then a link to its command in
# BIN_DIR.
# - The lock is `uv pip compile --universal --generate-hashes` of
#   requirements.in (its header has the command; Renovate's pip-compile
#   manager regenerates it); `--require-hashes` refuses any package
#   without a matching hash, or not pinned in the lock:
#   https://docs.astral.sh/uv/pip/compile/#locking-requirements
#   https://docs.astral.sh/uv/reference/cli/#uv-pip-sync--require-hashes
# - Wheels only (`--only-binary :all:`), so no package's build code runs.
# - `--no-config`: no uv.toml or pyproject.toml of the caller's is read.
# - Run again (after the lock moved), it recreates the environment
#   (`--clear`), and `pip sync` leaves exactly the lock's packages in it.
#
# Usage: scripts/install-diff-cover.sh VENV_DIR BIN_DIR
set -euo pipefail

venv=$(realpath --canonicalize-missing -- "${1:?usage: install-diff-cover.sh VENV_DIR BIN_DIR}")
bin=$(realpath --canonicalize-missing -- "${2:?usage: install-diff-cover.sh VENV_DIR BIN_DIR}")
lock=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/diff-cover/requirements.txt

uv venv --no-config --no-project --clear --python-preference only-system --python python3 "$venv"
uv pip sync --no-config --python "$venv/bin/python" --require-hashes --only-binary :all: "$lock"
mkdir -p -- "$bin"
ln -sfn -- "$venv/bin/diff-cover" "$bin/diff-cover"
"$bin/diff-cover" --version
