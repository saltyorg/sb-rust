# ADR-016: Reviews by tier, path-aware checks and quieter hooks

## Status

Accepted (maintainer, 2026-11-12). Changed the same day: the plan id
check named in part 2 was retired by
[ADR-017](ADR-017-hardening-plan-cut.md), and the choice part 5 left
open was made by [ADR-018](ADR-018-rust-skills-plugin-off.md) (the
rust-skills plugin is off).

## Date

2026-11-12

## Context

The maintainer asked why progress was slow. A review of the hooks, the
checks and the history up to 619f25a found that the time went to review
rounds and to work on the process, and little of it to the checks:

- 294 commits, 121 of them without a line of Rust.
- About 300 review runs, some 31 hours of review time;
  99 runs and about 12 hours were on commits without Rust.
- The 12 commits before this one changed no Rust: plans, records and
  gate scripts, with 27 reviewer runs.
- `scripts/check.sh full` ran about 320 times; a typical run takes one
  to eight minutes. Warm, the floor guard's own tests take 23 seconds of
  every `fast` run, and `task` ran the crates' tests twice (36 seconds
  under `cargo test`, then again under `cargo llvm-cov`).
- Skills were invoked 53 times, 3 of them from rust-skills, against a
  rule and a reminder on every prompt that asked for one before every
  task.
- The library-first hook fired after every edit of every `.rs` file,
  tests and the journal's file format included.

CLAUDE.md asked for a fresh-context review of every change, "small or
obvious" ones included, a re-review of each fix, and a `Review:` line in
every commit. Recent commits were reviewed twice, on two models.

## Decision

### 1. Reviews by tier

| What the change touches | Review |
|---|---|
| Only `docs/` and `tasks/` (specs, ADRs, plans, records, notes) | None for the commit. The documents are read at the plan's next checkpoint, or by the maintainer when a spec or ADR is approved. No `Review:` line is needed. |
| Rust source, the gate (`scripts/`, `CONSTRAINTS.md`, `CLAUDE.md`, `.claude/`, `.github/`, `deny.toml`, manifests) or anything else | One `agent-skills:code-reviewer` run for the task, on the task's whole diff. A `Review:` line. |
| Rust in terminal, UI, input, event-loop, clipboard, process or I/O code | The `library-reviewer` as well, as before. |
| Untrusted input, privileges, secrets, dependencies | `agent-skills:security-auditor` as well, when the task's spec or plan asks for it. |

- **One pass, on one model.** A second pass on another model is for a
  checkpoint, or when the maintainer asks.
- **A fix is reviewed again only when it changes behaviour beyond what
  the finding spelled out.** Otherwise `scripts/check.sh` and the fix's
  test are its check.
- **Findings are weighed against how sb runs on Saltbox** before they
  become work. A finding that can't happen in real use is declined in
  the `Review:` line, with the reason.
- `scripts/check-commit-messages.sh` asks for the `Review:` line only on
  a commit that changes a file outside `docs/` and `tasks/`.

### 2. Path-aware checks

- **A change under `docs/` and `tasks/` only** ends `task` and `full`
  after the `fast` checks (and `full`'s commit messages), with a printed
  line. No build, test, measurement or dependency check reads those
  files. Any other file in the change, `CONSTRAINTS.md` and a README in
  a crate included, runs every step, and so does a run with nothing
  changed since the base.
- **The gate scripts' own tests** (the floor guard's and the plan id
  check's) run in `fast` when a file under `scripts/` changed or nothing
  changed, and are skipped with a printed line otherwise. The guard
  itself runs every time.
- **The crates' tests run once**, under `cargo llvm-cov`, which fails on
  a failing test as `cargo test` does. It runs no doc tests on a stable
  toolchain and no prototype, so `cargo test --doc` for the crates and
  `cargo test` for the prototypes follow it. Every test that ran before
  still runs.

No threshold changes.

### 3. Skills when the phase changes

A skill is loaded when the phase of the work changes (spec, plan,
implementation, tests, debugging, review, commit, release), and a skill
loaded earlier in the session covers its phase. The session-start
reminder stays; the reminder on every prompt goes.

### 4. The library-first hook fires where the rule applies

`.claude/hooks/library-check.sh` speaks only after an edit under `src/`
of `cli-core`, `docker`, `exec`, `host`, `sb` or `term-ui`, or of a
prototype. Tests, `config` and `journal` (domain logic and a file
format) get no reminder. The `library-reviewer` before a commit and the
`Library-check:` line are unchanged. A new crate with terminal, UI,
process or I/O code is added to the hook's list with its first commit.

### 5. Unchanged

- The reviewer guidelines hook (ADR-010's neighbour in
  `.claude/settings.json`).
- The rust-skills plugin and its own prompt hook (ADR-010). Claude Code
  has no setting that turns off one hook of an enabled plugin
  (https://code.claude.com/docs/en/hooks.md: "There is no way to disable
  an individual hook while keeping it in the configuration"), so
  silencing it means dropping the plugin, copying its skills into the
  repository, or keeping a fork. That choice is the maintainer's and is
  not made here.

## Why this is a loosening, and its limits

Parts 1 to 4 remove checks or reminders that ran before, so they need
this ADR (CLAUDE.md, "Quality bar").

- A document-only commit is no longer reviewed when it lands. A wrong
  statement in a spec or plan can stand until the checkpoint. The code
  written from it is still reviewed, and the maintainer approves specs
  and ADRs.
- A fix made after a review is trusted to its test and the checks.
- What counts as documents is two directories, by path. A file
  elsewhere, whatever its type, is not a document: `CONSTRAINTS.md`,
  `CLAUDE.md` and the scripts keep their review and every check.
- `check.sh` judges by the files that differ from the base. A run whose
  base is too recent to hold the code change sees documents only; the
  base is the commit the work started from, as it has always had to be
  for coverage and mutation.
- `cargo llvm-cov` builds a crate's example only when the example sets
  `test = true` (both journal examples do); `cargo test --workspace`
  built every example. clippy's `--all-targets` still checks them all.
- The library-first reminder no longer follows an edit in `config`,
  `journal` or a test; the `library-reviewer` before the commit does.
- A change to a gate script that breaks its tests is caught in the run
  that changes it, and not in later runs over other files.

## Alternatives considered

### Keep a review for every commit, on a cheaper model
- Pros: nothing goes unread.
- Cons: the cost was the rounds, each of which produces findings that
  become commits that need rounds; a cheaper model produces more of the
  findings that don't matter in use.
- Rejected.

### Review only at checkpoints
- Pros: the fewest runs.
- Cons: a defect in a task's code is built on for the rest of the
  phase; the reviews of cli-core, exec and the journal found real bugs
  at the task level.
- Rejected for Rust and the gate; chosen for documents.

### Skip the heavy stages by file type (`*.md` anywhere)
- Pros: more runs are short.
- Cons: `CONSTRAINTS.md` is the bar, and a crate's README or a snapshot
  can be read by a build or a test.
- Rejected: two directories nothing reads.

### Keep `cargo test --workspace` and run coverage beside it
- Pros: no change to a gate line.
- Cons: the crates' tests run twice in every `task` and `full`.
- Rejected.

## Consequences

- A document-only commit costs a `fast` run and no reviewer.
- A task's commit carries one `Review:` line from one pass.
- `fast` drops from about 25 seconds to a few, warm, when no script
  changed; `task` runs the crates' tests once.
- The floor guard reports each changed gate line of this commit; they
  are acknowledged in `scripts/floor-acknowledged.txt` (ADR-015).
- ADR-010's reminder on every prompt is gone; its rule that a matching
  skill is loaded before every task is replaced by part 3.
