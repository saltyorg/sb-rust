# Implementation Plan: toolchain ownership (approved 2026-11-25)

Decisions: D1-D9 below (the maintainer, 2026-11-25), recorded in
[ADR-031](../../docs/decisions/ADR-031-toolchain-ownership.md). Specs:
[docs/specs/python-env.md](../../docs/specs/python-env.md) and
[docs/specs/release.md](../../docs/specs/release.md). Quality bar:
[CONSTRAINTS.md](../../CONSTRAINTS.md).

## Overview

sb owns uv. Each Saltbox branch owns its Python and its lock. Saltbox's
master is kept current by a workflow in Saltbox itself, and nothing in
sb's repositories writes to Saltbox any more. This plan is sb-rust's
part: the specs and the ADR, sb's fallback for a checkout without pins,
its own message when its uv can't install the branch's Python, and the
removal of the Saltbox updater from the release workflow.

## Decisions

- **D1:** sb owns uv only. Each release carries one uv (`UV_VERSION`),
  kept current by sb's own Renovate. `.uv-version` in Saltbox stays a
  minimum that sb checks.
- **D2:** each Saltbox branch owns its Python and lock. The minor is the
  major.minor of the branch's own `.python-version`; no automation
  changes the minor.
- **D3:** only master is automated. develop and other branches are
  changed by hand, since they may move Python for testing.
- **D4:** Saltbox's own workflow updates master: the Python patch,
  `.uv-version` (raised to the uv of the latest stable sb) and the lock.
- **D5:** the update lands as a PR opened with the Saltbox App's token, so
  Saltbox CI runs on it. GitHub auto-merges it when CI passes.
- **D6:** sb-go and sb-rust write nothing to Saltbox. The
  update-saltbox-python action, its release jobs, the hand-run preflight
  and their use of the Saltbox App key are removed from both. This amends
  the release spec's decision of 2026-11-20 ("Saltbox's toolchain
  update").
- **D7:** a checkout without `.python-version` (the pre-* branches) makes
  sb fall back to the newest 3.12 patch its uv knows, installing
  `requirements/requirements-saltbox.txt` unhashed, as sb-go did before it
  had pins. If `.uv-version` is also missing, there's no minimum to check.
- **D8:** sb says in its own words when its uv can't install the branch's
  Python: "this Saltbox branch needs Python X.Y.Z, which this sb's uv
  (U.V.W) can't install; run sb self-update". It checks uv's Python list
  before installing.
- **D9:** sb-go gets the same changes as sb-rust: D6, D7 and D8.

## Order across the projects

1. sb-rust, locally (R1-R5). It isn't released, so its timing is free,
   and it lands before sb-rust's first push.
2. sb-go: the same changes, then a stable release. From then on, nothing
   in sb writes to Saltbox's master.
3. Saltbox: its own workflow, run by hand once; its first PR passes CI
   and merges before its schedule is turned on.
4. Moving master to another Python minor is then an ordinary Saltbox PR.

Hosts on an older sb-go keep working throughout: they don't use the
updater, and master keeps its pins.

## Tasks

### R1: specs and ADR

- ADR-031, who owns which toolchain version: D1-D9 and why. Whoever
  installs a version owns it: sb installs uv, and the branch's CI tests
  its Python and lock.
- docs/specs/release.md: "Saltbox's toolchain update" is replaced by an
  amendment note pointing at ADR-031; the saltbox-gate, saltbox-prepare
  and saltbox-commit jobs, the preflight workflow, and the Saltbox App
  variable and secret go from the spec. `version --json`'s `uv_version`
  stays: Saltbox's workflow reads it.
- docs/specs/python-env.md: assumption 2 reworded; rules for D7 and D8
  with their exact messages.
- CHANGELOG: the user-visible changes (D7, D8).

Done when the specs match D1-D9 and nothing in docs/ still describes sb
writing to Saltbox, apart from the amendment note.

### R2: the fallback for a checkout without pins (D7)

- A missing `.python-version` gives the newest 3.12 patch the pinned
  uv's download list offers, installed unhashed; a missing `.uv-version`
  skips the minimum check; a present but malformed pin still fails with
  today's message.
- The requirements are installed with `uv pip install --requirement`,
  without `--require-hashes`; the manifest records the unhashed install,
  so moving between a pinned and a pin-less checkout rebuilds the venv.
  A manifest without the field counts as hashed.
- One line on every such run: "This Saltbox branch has no pinned Python;
  using Python 3.12.N, the newest 3.12 this sb's uv (U.V.W) provides".

Tests first: no pins at all; a Python pin without `.uv-version`; a
malformed pin; pin-less to pinned and back rebuilds; the manifest's
field. Reviewed by the code reviewer and the library reviewer.

### R3: sb's own message when its uv lacks the Python (D8)

- Before installing, `uv --no-config python list --only-downloads
  --all-versions --output-format json` through the isolated uv
  invocation, keeping CPython's default variant at an exact X.Y.Z.
- A pin not in the list fails with D8's message and installs nothing; a
  failing list call reports uv's own message.

Tests first: a listed pin installs as before; an unlisted one gives D8's
message and installs nothing; uv's list failing names uv's message.

### R4: remove the Saltbox updater (D6)

- `.github/actions/update-saltbox-python/`; release.yml's saltbox-gate,
  saltbox-prepare and saltbox-commit jobs and their `environment:
  saltbox`; `.github/workflows/saltbox-toolchain.yml`; check.sh's steps
  for them; `scripts/release.sh saltbox-gate` and its tests; the
  CONSTRAINTS.md rows and Renovate entries that exist only for them.
- Kept: `version --json`'s `uv_version`, with a test pinning its field
  name and format, because Saltbox's workflow reads it.
- The floor guard's removals each carry "D6: sb no longer writes to
  Saltbox" in the commit body. A blocking finding goes to the maintainer.

Done when nothing in .github/ or scripts/ refers to Saltbox's toolchain
or the App key, and the workflow linters and `scripts/check.sh full`
pass.

### R5: review and proof

- One code review over the tasks' commits, and a security review, since
  credentials leave the workflows.
- `scripts/check.sh full`, `scripts/check-commit-messages.sh`, then a
  rebuilt `target/release/sb`.
- A real check in throwaway containers against invented Saltbox
  checkouts: one with pins, one without, and one pinning a Python the uv
  lacks, with `SB_LOG_DIR` outside the host's log directory.
