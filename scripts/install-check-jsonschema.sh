#!/usr/bin/env bash
# Installs check-jsonschema (CONSTRAINTS.md's Tools table), the workflows'
# linter, as an isolated, locked tool, as scripts/install-diff-cover.sh
# installs diff-cover: a virtual environment of its own on Python 3.12,
# with every package at the exact version and SHA-256 hashes of
# scripts/check-jsonschema/requirements.txt, then a link to its command in
# BIN_DIR.
# - Python 3.12 (`uv venv --python 3.12`): in CI, the one
#   actions/setup-python puts on PATH; on a host with no Python 3.12, uv
#   downloads its own CPython build (uv's default python-preference,
#   `managed`, takes a 3.12 the host has before downloading one:
#   https://docs.astral.sh/uv/concepts/python-versions/), refused unless
#   its SHA-256 matches the one uv ships for it (`HashMismatch`:
#   https://github.com/astral-sh/uv/blob/0.13.4/crates/uv-python-managed/src/downloads.rs).
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
# Usage: scripts/install-check-jsonschema.sh VENV_DIR BIN_DIR
set -euo pipefail

venv=$(realpath --canonicalize-missing -- "${1:?usage: install-check-jsonschema.sh VENV_DIR BIN_DIR}")
bin=$(realpath --canonicalize-missing -- "${2:?usage: install-check-jsonschema.sh VENV_DIR BIN_DIR}")
lock=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/check-jsonschema/requirements.txt

uv venv --no-config --no-project --clear --python 3.12 "$venv"
uv pip sync --no-config --python "$venv/bin/python" --require-hashes --only-binary :all: "$lock"
mkdir -p -- "$bin"
ln -sfn -- "$venv/bin/check-jsonschema" "$bin/check-jsonschema"
"$bin/check-jsonschema" --version
