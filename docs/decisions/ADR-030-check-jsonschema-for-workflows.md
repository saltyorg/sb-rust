# ADR-030: check-jsonschema checks the workflows in place of actionlint

## Status

Accepted (the maintainer, 2026-11-25).

**This loosens the gate.** actionlint's checks beyond the schema go:
expression syntax and types, `needs:` and matrix references, action
inputs, and shellcheck on each `run:` script.

## Date

2026-11-25

## Context

The Workflows row of CONSTRAINTS.md ran actionlint 1.7.12 on
`.github/workflows/`. Its releases lagged GitHub's syntax, and every
new feature the workflows used needed an entry in
`.github/actionlint.yaml` to pass: the 26.04 runner labels, the
workflow-level `cache-mode` key, and the `$/` self-repository action
form, which actionlint also read as a remote action without a ref, so it
didn't check those steps' inputs either. Each entry was a suppression the
floor guard had to watch, and each lasted until an actionlint release
caught up.

## Decision

- check-jsonschema 0.39.0 validates each workflow against GitHub's
  workflow schema and each action's `action.yml` against GitHub's action
  schema, both shipped inside it (`--builtin-schema
  vendor.github-workflows` and `vendor.github-actions`): no network and no
  settings file. 0.39.0 accepts every current workflow, the newer syntax
  above included.
- It is installed as diff-cover is: a hash-locked requirements file
  (`scripts/check-jsonschema/requirements.txt`), in a uv virtual
  environment of its own on Python 3.12
  (`scripts/install-check-jsonschema.sh`). In CI, actions/setup-python
  provides that Python, the same version on every Ubuntu release; on a
  host with no Python 3.12, uv downloads its own CPython build and checks
  its SHA-256.
- `scripts/workflow-schemas.test.sh` runs before the check, at the same
  levels: an invented workflow and composite action pass, and fail with a
  misspelt job key (`timeout-minuts`), `fail-fast: [nope]`,
  `cache-mode: sometimes` and `using: compost`. A check-jsonschema update
  that stopped refusing them fails the gate.
- actionlint, `.github/actionlint.yaml` and the shellcheck requirement go.
  shellcheck was in the Tools table only because actionlint ran it; the
  floor guard's rule against a new `# shellcheck disable=` directive in a
  workflow goes too, since nothing reads the directive there now.
- zizmor's row is unchanged.

## Alternatives considered

- **Keep actionlint and its settings.** Every new GitHub feature waits for
  an actionlint release, with a suppression in between.
- **Both linters.** The suppressions stay, which is the cost this
  decision removes.

## Consequences

- An expression error, a wrong `needs:` or an unknown action input shows
  only on GitHub, in the run, or in zizmor where an audit covers it.
- A `run:` script's shell isn't linted before GitHub runs it.
- The schemas are check-jsonschema's copies of SchemaStore's at its
  release; a GitHub feature newer than them fails the check until an
  update, which Renovate proposes.
