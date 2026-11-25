# Toolchain ownership: tasks

Plan: [tasks/toolchain/plan.md](plan.md). Every task follows CLAUDE.md:
tests first where behaviour changes, `scripts/check.sh fast` after
edits, `full` before landing, signed Conventional Commits with
`Library-check:` and `Review:` lines. sb runs as root only with
`SB_LOG_DIR` in `target/` or a temporary directory. Test data is
invented.

## R1: specs and ADR

- [ ] ADR-031: who owns which toolchain version (D1-D9).
- [ ] release.md: "Saltbox's toolchain update" replaced by an amendment
      note pointing at ADR-031; the Saltbox jobs, the preflight and the
      Saltbox App variable and secret gone.
- [ ] python-env.md: assumption 2 reworded; D7's and D8's rules and
      messages.
- [ ] CHANGELOG: D7 and D8.

## R2: no pins (D7)

- [ ] No `.python-version`: the newest 3.12 in uv's download list, with
      the notice line.
- [ ] No `.uv-version`: no minimum check.
- [ ] A malformed pin is still refused with today's message.
- [ ] Unhashed `uv pip install --requirement` for a pin-less checkout.
- [ ] The manifest's unhashed flag: pin-less to pinned and back
      rebuilds; an old manifest counts as hashed.
- [ ] Code reviewer and library reviewer.

## R3: the uv lacks the pinned Python (D8)

- [ ] The pin checked against uv's download list before installing.
- [ ] D8's message; nothing installed.
- [ ] A failing list call reports uv's stderr.

## R4: no Saltbox updater (D6)

- [ ] The action, the three release jobs, the preflight workflow, the
      `saltbox` environment's use.
- [ ] check.sh's steps, `release.sh saltbox-gate` and its tests.
- [ ] CONSTRAINTS.md rows and Renovate entries that exist only for them.
- [ ] A test pinning `version --json`'s `uv_version` field and format.

## R5: review and proof

- [ ] Code reviewer over the tasks; security auditor.
- [ ] `scripts/check.sh full` passes; commit messages checked.
- [ ] `target/release/sb` rebuilt; container runs against invented
      checkouts: pinned, pin-less, a Python the uv lacks.
