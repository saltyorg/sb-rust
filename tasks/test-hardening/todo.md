# Test hardening: tasks

Plan: [tasks/test-hardening/plan.md](plan.md) (decisions MD1-MD4 and
Q1-Q9, each task's findings, acceptance and files). Every task follows
CLAUDE.md and the plan's conventions: tests first where behaviour
changes, `scripts/check.sh fast` after edits and before each commit,
one `task` and one `full` based on the batch's start commit, the full
one with the amd64 rehearsal, at each batch's end (Q8, Q9), signed
Conventional Commits with
`Library-check:` and `Review:` lines. sb runs as root only with
`SB_LOG_DIR` in `target/` or a temporary directory. Test data is
invented; no test reads the host's data.

## Ahead of the batches

- [x] 2.6: the pci.ids comparison reads a pinned upstream copy, never the
      host's (the fix for CI run 45746849992).

## Batch 1: fix first

- [x] 1.1: one skip mechanism, and the gate's report of skips.
- [x] 1.2: the gate refuses a root run it asked for and didn't get.
- [x] 1.3a: every other skip goes through the helper.
- [x] 1.3b: the floor guard reports a new hand-rolled skip; the
      CONSTRAINTS.md bullet.
- [x] 1.4: `sb logs` tests are root-gated, and isolation.rs catches the
      next one.
- [x] 1.5: the systemd hangup test runs in `check.sh full` (Q2).
- [x] 1.6: sb-repo's tests ignore an inherited `GIT_DIR` and the host's
      git configuration (B1 and B2).
- [x] 1.7: "nothing reaches the shell" reads what the shell would read.
- [x] 1.8a: check.sh's skip lines and ci.yml's base for a push.
- [x] 1.8b: the rehearsal's base and `job.py`.
- [x] 1.9: the gate's tests run the real `node`.
- [x] 1.10: the floor guard's rules are tested against the real check.sh
      and CONSTRAINTS.md.
- [ ] Checkpoint: the rehearsal, `full <batch start>`, Q3's measurement
      recorded and its outcome applied.

## Batch 2: environment and tooling

- [x] 2.1: colour variables are scrubbed in one place.
- [x] 2.2: `SHELL` and the PTY's working directory are the test's.
- [x] 2.3: sb's own git reads no host configuration, in sb's tests and in
      the gate's sandbox.
- [x] 2.4: the configparser reference is frozen, from Saltbox's pinned
      Python.
- [x] 2.5: the disk log test uses invented filesystem figures.
- [x] 2.10: no test depends on the host's data, and the gate says so.
- [x] 2.7: no shared `no-*` paths between tests in one process.
- [x] 2.8: the rehearsal runs for what changes a runner's verdict (Q5).
- [x] 2.9a: `scripts/install-gitleaks.sh`, its test, check.sh and the
      floor-guard stubs.
- [x] 2.9b: the workflows, Renovate, CONSTRAINTS.md and the comments; the
      gate host's gitleaks from the release binary, Go kept (Q7).
- [x] Checkpoint: the rehearsal, `full <batch start>`.

## Batch 3: oracles that can't fail, and the closed-reader redesign

- [x] 3.1: the specs say the stream ends when the reader goes, with no
      new line needed.
- [x] 3.2: cli-core's reader-gone watch.
- [x] 3.3: piped `sb logs` ends on the watch; the in-process tests; the
      follow's first-batch case.
- [x] 3.4: the end-to-end closed-reader test owns its pipe.
- [x] 3.5: the systemd parallelism test sees half-serial calls.
- [x] 3.6: "no poll after the interrupt" can fail.
- [x] 3.7: bench's cancelled gather and the hung mounts, on a channel.
- [x] 3.8: errors are asserted by kind: journal corruption and
      cli-core's log refusals.
- [x] 3.9: the 150 ms drain is checked by the child's own clock.
- [ ] 3.10: tests that pass whichever way the race goes decide the race (exec's done; hangup's and update's open).
- [x] 3.11: weak assertions in sb's and completion's tests.
- [x] 3.12: tests that restate the code (Q6: kept as change detectors).
- [x] 3.13: the docker fakes surface their own failures.
- [x] 3.14: the shipped binary runs the tests that need no seam.
- [x] 3.15: nextest's LEAK status is shown in the release-build run.
- [ ] Checkpoint: the rehearsal, `full <batch start>`, the journal matrix
      and fuzz harness, piped `sb logs` in the maintainer's terminal.

## Batch 4: timing and hangs

- [ ] 4.1a: the coverage run ends a hung test; the release-build run
      under `cargo test` inside `timeout` (Q4); CONSTRAINTS.md.
- [ ] 4.1b: the hang-shaped tests bound themselves.
- [ ] 4.2: signal tests end on events and on bounds measured at the
      moment.
- [ ] 4.3: the double-click tests stretch the drain instead of retrying.
- [ ] 4.4: host's and docker's fixed delays wait for state.
- [ ] 4.5: ops' Ctrl-C follows the event it is meant to follow; the
      helper fails out loud.
- [ ] 4.6: thin PTY and motd margins.
- [ ] 4.7: negative waits become recorded order; the redundant sleeps go.
- [ ] 4.8: test orphans close their output, and a leak fails the
      release-build run.
- [ ] 4.9: the journal matrix scripts wait for state and stop the right
      process.
- [ ] Checkpoint: the rehearsal, `full <batch start>`, the journal matrix
      and fuzz harness, no orphaned processes.

## Batch 5: blind failures and style

- [ ] 5.1: exit-code asserts and `.status()` show the child's output.
- [ ] 5.2: wait loops show sb's state when they time out.
- [ ] 5.3: the tui PTY asserts say why.
- [ ] 5.4: bare `is_ok`, `is_err`, `is_some` and `is_none` checks keep
      their value.
- [ ] 5.5: lost failure reasons in cli-core, term-ui and ops.
- [ ] 5.6: the motd matrix fails with its cause.
- [ ] 5.7: style in sb's tests.
- [ ] 5.8: style in the crates.
- [ ] 5.9: the gate takes one view of what changed.
- [ ] Checkpoint: complete (the plan's last checkpoint), and the plan
      archived.
