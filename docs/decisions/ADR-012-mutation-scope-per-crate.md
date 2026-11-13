# ADR-012: Mutation test scope per crate

## Status

Accepted (maintainer, 2026-11-09, sb-journal plan Task 0)

## Date

2026-11-09

## Context

CONSTRAINTS.md requires that changed code's viable mutants are at least
80% caught, checked by `scripts/check.sh task`. Until now every crate's
tests judged every mutant (`--test-package` for each crate), because the
end-to-end tests in `sb` cover code in the library crates.

The sb-journal plan ports about 3,800 lines of non-test code into a new
crate. `cargo mutants --list` on the prototype gives about 640 mutants.
Task 0 timed one mutant in sb-host as `task` runs it:
- with every crate's tests: about 85 s (59 s rebuilding sb-host, sb and
  every test binary, then 25 s of tests);
- judged by its own crate's tests: 2 s.

At 85 s a mutant, Task 1 alone would take about 6 hours, and Tasks 2
and 3 about 3 hours each, against CONSTRAINTS.md's "a few minutes" for a
task check. sb's end-to-end tests cannot reach sb-journal until a
consumer exists; only sb-host's tests will, from the plan's Task 10.

## Decision

- **`check.sh task`** runs one cargo-mutants pass for each crate listed
  in `MUTATION_TESTERS`. A listed crate's mutants are judged by its own
  tests plus the tests of the crates named in its entry: the workspace
  crates whose tests reach it.
  - Every other crate keeps the old scope: one pass with every crate's
    tests.
  - The score is totalled over all passes, against the same 80% bar.
- sb-journal is listed in plan Task 1 with no extra testers. sb-host is
  added in Task 10, when its glue's tests reach sb-journal.

## Why this doesn't weaken the bar

- **Caught versus missed.** The narrower pass runs a subset of the tests,
  so it can't turn a missed mutant into a caught one. Here it can only
  lower the score.
- **Two edge cases where the score can rise:**
  - **A timeout becomes caught.** A test outside the narrow set (for
    example an `sb` end-to-end test) hangs on the mutant in the wide
    scope, which counts as not caught. The narrow set's own test then
    fails quickly, which counts as caught. Since ADR-021 a hung test
    is ended after 60 s and counts as failed, so the wide scope catches
    such a mutant too; the mutant's own timeout is the backstop.
  - **An unviable mutant becomes viable.** The mutant breaks only a
    dependent's build (for example through `impl Trait` auto-trait
    leakage, or different feature unification). The wide scope leaves it
    out of the score; the narrow scope builds it and can catch it.

  Both need another crate's code or tests to reach the listed crate.
  Until the plan's Task 10, nothing reaches sb-journal. From then on,
  only sb-host does, and sb-host is listed as a tester from that task.
  When an `sb` command first uses the crate, that task adds `sb` to the
  entry or accepts the slower pass.
- **A tester missing from the list** lowers the score: the mutants only
  its tests would catch count as missed. A plan task that makes a crate's
  tests reach a listed crate also adds it to the entry (for sb-journal,
  Task 10).
- **check.sh refuses a pass that gave no verdict.** That covers:
  - any cargo-mutants exit status other than 0, 2 or 3, including 4,
    where the baseline failed and nothing was tested;
  - outcomes without integer counts;
  - a `MUTATION_TESTERS` key or tester that isn't a crate.

  So a misspelt name can't produce a hollow 100%.

## Alternatives Considered

### Derive the testers from `cargo metadata`
- Pros: nothing to maintain.
- Cons: sb-journal's dependents are sb-host and sb, which brings back
  the 85 s mutants. It only helps crates nothing depends on.
- Rejected: no gain for the case that prompted this.

### Keep the scope and run four jobs
- Pros: no change; no list.
- Cons: Task 1 would still take about 1.5 h, and Tasks 2 and 3 about
  45-55 min each, at heavy CPU and memory. Every later change to
  sb-journal would cost the same.
- Rejected.

### Keep the scope and split the port into about 15 slices
- Pros: no change to the check.
- Cons: about 15 commits of an hour's mutation testing each, and later
  edits stay slow.
- Rejected.

### An all-crate mutation stage at each checkpoint
- This was first chosen as a safeguard for the list.
- Dropped (maintainer, 2026-11-09). The narrow pass can't catch a mutant
  the wide pass missed, since its tests are a subset. So a wide stage
  could fail a change that `task` passed only through the timeout and
  unviable edge cases above. It
  would cost hours per checkpoint for that.

## Consequences

- A sb-journal slice's mutation check takes minutes, not hours.
- The list only matters for speed and for avoiding false failures.
- CONSTRAINTS.md's Mutation row names the per-crate scope and this ADR.
