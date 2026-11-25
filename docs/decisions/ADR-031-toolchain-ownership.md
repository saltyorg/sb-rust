# ADR-031: who owns which toolchain version

## Status

Accepted (the maintainer, 2026-11-25). Amends the release spec's
decision of 2026-11-20, "Saltbox's toolchain update", which ported sb-go's
update-saltbox-python job into sb-rust's release workflow.

## Date

2026-11-25

## Context

A Saltbox checkout pins three things sb reads: `.python-version` (the
exact Python), `.uv-version` (the oldest uv Saltbox accepts) and
`requirements/requirements-saltbox.txt` (the hashed lock). Until now
sb-go's release workflow wrote all three into Saltbox's master after
each release, with the Saltbox App's key, and the release spec moved that
job into sb-rust at 1.0.0. That made sb's repositories a writer of
another repository, held a key able to write to Saltbox in sb's release
workflow, and tied a change of Saltbox's Python to an sb release.

Saltbox's own branches differ: master is kept current, develop may move
Python for testing, and the pre-* branches predate the pins and carry
only an unhashed list of direct requirements.

## Decision

Whoever installs a version owns it: sb installs uv, and each Saltbox
branch's CI tests its Python and its lock.

- **D1:** sb owns uv only. Each release carries one uv (`UV_VERSION`),
  kept current by sb's own Renovate. `.uv-version` stays a minimum that
  sb checks.
- **D2:** each Saltbox branch owns its Python and lock. The minor is the
  major.minor of the branch's own `.python-version`; no automation
  changes the minor.
- **D3:** only master is automated. develop and other branches are
  changed by hand.
- **D4:** Saltbox's own workflow updates master: the Python patch,
  `.uv-version` (raised to the uv of the latest stable sb, read from that
  release's `sb version --json` `uv_version`) and the lock.
- **D5:** that update lands as a pull request opened with the Saltbox
  App's token, so Saltbox's CI runs on it, and GitHub merges it once CI
  passes.
- **D6:** sb-go and sb-rust write nothing to Saltbox. The
  update-saltbox-python action, its release jobs, the hand-run preflight
  and their use of the Saltbox App key are removed from both.
- **D7:** a checkout without `.python-version` makes sb use the newest
  3.12 patch its uv can install and install the requirements unhashed,
  as sb-go did before Saltbox pinned its Python. A missing `.uv-version`
  sets no minimum. A pin file that is present but malformed is still
  refused.
- **D8:** before installing, sb checks the pinned Python against its
  uv's download list, and says so in its own words when the uv can't
  install it, pointing at `sb self-update`.
- **D9:** sb-go gets the same changes: D6, D7 and D8.

`version --json`'s `uv_version` keeps its name and format: Saltbox's
workflow reads it (D4).

## Alternatives Considered

### Keep the updater in sb's release workflow (the 2026-11-20 decision)

- Pros: one place moves the uv minimum and the Python together with a
  release.
- Cons: sb's repositories hold a key that writes to Saltbox; a Python
  patch waits for an sb release; master's update skips Saltbox's own CI
  until after the commit; develop's and the pre-* branches' needs aren't
  met.
- Rejected: the repository that tests the Python and the lock should be
  the one that changes them.

### Saltbox pins nothing, sb picks the Python

- Pros: no pin files to keep current.
- Cons: a venv is no longer reproducible from the checkout, and a branch
  can't test a new Python before master takes it.
- Rejected: kept only as the fallback for branches that predate the pins
  (D7).

## Consequences

- The release workflow loses three jobs, the `saltbox` environment, the
  preflight workflow and every use of the Saltbox App's variable and
  secret; the gate loses the action's tests and the updater's tools.
- A Saltbox branch without pins works, with a notice on every run and no
  hashes: what it installs isn't reproducible. The manifest records the
  unhashed install, so the next pinned checkout rebuilds the venv.
- A branch that pins a Python newer than sb's uv knows stops before
  anything is installed, with "this Saltbox branch needs Python X.Y.Z,
  which this sb's uv (U.V.W) can't install; run sb self-update".
- Raising master's `.uv-version` is Saltbox's step, bounded by the latest
  stable sb's uv, so hosts on an older sb get the uv-minimum message and
  `sb self-update`, as before.
