# ADR-029: The floor guard blocks gate changes only, with the maintainer's approval

## Status

Accepted (the maintainer, 2026-11-24). Supersedes
[ADR-015](ADR-015-acknowledged-floor-findings.md)'s acknowledgement
model: `scripts/floor-acknowledged.txt` is removed, and an exception is
the maintainer's approval in the commit message.

**This loosens the gate.** Removed assertions, removed or deleted tests
and changed gate-script lines no longer fail it: they are printed for the
code reviewer, and only review judges them.

## Date

2026-11-24

## Context

ADR-015 let a floor guard finding pass when the commit that caused it
added an entry for it to `scripts/floor-acknowledged.txt`. The history of
main up to this decision (892 commits) shows what the file became:

- **Size:** 1,056 entries, 307 KB. 155 commits (17%) added entries: a
  median of 3, a mean of 6.8, at most 74 in one commit.
- **What they acknowledged:** assertion-removed 706 (67%),
  gate-line-removed 228 (22%; 112 of them lines of the guard's own test
  file), wrappers-widened 49, tool-config-changed or -added 23,
  test-removed or -deleted 14, threshold-* 12, silenced-checker 7,
  check-disabled 7, new-exception 5, rule-removed 3, toolchain-changed 1,
  mutation-scope-narrowed 1.
- **Who acknowledged:** every entry was written in the commit that caused
  the finding, by that commit's author. None had a separate sign-off.
- **What they were:** refactors and decided removals (an assertion moved,
  reworded or replaced), with reasons often copied between entries. Some
  acknowledged tightenings, such as a new clippy `disallowed-methods`
  entry. The real loosenings in the file (the coverage floors and the
  binary size value of ADR-025, three `PREADS_*_MAX` bounds of ADR-019)
  were each decided in an ADR already.
- **What it cost:** about 4,250 lines across the guard, its tests and the
  file; a merge hot spot for parallel work; entries that went stale when
  code changed again, failing later runs; a range run that fails by
  design once an acknowledged line changes again; and one more step in
  every such commit, repeating what its body already said.

No acknowledgement changed an outcome. The part of the guard no other
check covers is the gate's own configuration: a check.sh threshold or
step, `-D warnings`, a lint table, deny.toml, a new `#[ignore]`,
`coverage(off)` or `mutants::skip`, a CONSTRAINTS.md number, the
toolchain pin, an `#[expect]` or other suppression. Those findings are
rare (about 108 of the 1,056 entries, 92 of them citing an ADR or the
maintainer's decision), and a silent change there does the most harm.

## Decision

- **Blocking:** the rules on the gate's configuration, its thresholds and
  the suppressions: `threshold-*`, `lint-removed`, `lint-weakened`
  (manifest lint tables, and a gate script's `-D <lint>` dropped or
  `-A <lint>` added), deny.toml's rules, `toolchain-changed`,
  `tool-config-*`, `clippy-rule-removed`, `gate-file-deleted`,
  `check-disabled`, `expected-failure-added`, `silenced-checker`,
  `test-made-easier`, `unfinished-work`, `new-exception`,
  `mutation-scope-narrowed`, `rule-removed` and `rule-changed`.
- **An exception needs the maintainer's approval.** The agent asks first,
  and only then writes the line, in the body of the commit that causes the
  finding:
  `Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)`.
  It covers every finding of that rule in that file in the checked diff.
  Before the commit exists, the gate is given the same line in the
  `FLOOR_APPROVED` environment variable. The guard prints each approved
  finding with its line; `scripts/check-commit-messages.sh` checks the
  line's form, and the guard fails on a `Floor-approved:` line in any
  other form.
- **Reported, never blocking:** `assertion-removed`, `test-removed`,
  `test-deleted` and `gate-line-removed` (a removed or changed line of
  check.sh, the guard or the commit message check). The guard prints them
  and exits 0. The reviewer hook tells the code reviewer to judge each
  against the commit message; the Floor rule that a removal needs a
  reason in the commit message stands.
- **Dropped:** the guard's own test file is no longer a gate file. Its
  changes are reviewed as code, and check.sh runs it.
- **The acknowledgement file is removed,** with the guard's reading of it.
  No tool reads it.

## Why this is a loosening, and its limits

A removed assertion or test, or a removed gate-script line, used to fail
the gate until acknowledged; now it passes, printed. Mutation testing
sees a removed assertion only when the code it tested is in the diff, and
coverage not at all, so for a test-only diff the code review is the only
check. The kept rules are where a silent change does the most damage,
and they now need the maintainer, not the author.

Limits:
- An approval is text the author writes. What makes it the maintainer's
  is the rule in CLAUDE.md and CONSTRAINTS.md that an agent asks before
  writing one; nothing can verify it.
- An approval covers a rule in a file for the whole checked diff: in a
  range run, an approval in one commit also covers a later finding of the
  same rule in the same file. Review reads each commit.
- The working tree's copy of the guard still judges the diff. A change to
  the guard is reported line by line, for review.

## Alternatives considered

### Remove the guard entirely
- Pros: about 4,250 fewer lines, no step in any commit.
- Cons: no deterministic check would remain on edits to the gate's
  configuration, thresholds and suppressions.
- Rejected.

### The maintainer signs off every acknowledgement
- Pros: real authority over every entry.
- Cons: hundreds of entries, up to 74 in one commit; it blocks unattended
  work and makes the sign-off a formality.
- Rejected: narrowing comes first, and then the approval covers only the
  rare blocking findings.

### A rule in the reviewer's brief only
- Pros: no files; review judges meaning, not text.
- Cons: not deterministic, and no record a tool can check.
- Rejected for the gate's configuration; kept for the reported findings.

## Consequences

- A refactor that moves assertions or rewrites gate-script lines commits
  without a second record of its reasons; the commit body carries them.
- A threshold, lint, deny.toml, toolchain or suppression change stops at
  the gate until the maintainer approves it. So do some tightenings the
  guard can't tell from a loosening: a new line in a tool configuration
  (a clippy `disallowed-methods` entry), or a CONSTRAINTS.md row that gains
  a requirement in the same direction as its number. Unattended work
  pauses there for the maintainer.
- No merge conflicts over an append-only file.

## Amendments

- 2026-11-24 (the maintainer): the guard also blocks the workflow
  linters' suppressions, `linter-config-changed` (a line of code added or
  removed in actionlint's or zizmor's settings, `.github/actionlint.yaml`
  or `.yml`, `.github/zizmor.yml` or `.yaml`, `zizmor.yml` or `.yaml`, or
  the file added or deleted) and `zizmor-ignore-added` (a
  `# zizmor: ignore[...]` comment added under `.github/`).
