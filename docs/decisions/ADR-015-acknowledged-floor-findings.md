# ADR-015: Acknowledged floor findings

## Status

Accepted (maintainer, 2026-11-11, hardening plan decision 6)

## Date

2026-11-11

## Context

CONSTRAINTS.md's Floor guard row asked for zero floor violations in the
diff, and `scripts/check.sh` fails on any finding of
`scripts/floor-guard.mjs`. Some findings are expected and correct:
- an assertion that moves to another test is reported as removed;
- a new `#[expect]` with its Exceptions row is reported as a new
  exception;
- since the hardening plan's Task 1 the guard reports every changed or
  removed line of code in its own scripts, because it can't tell whether
  a changed pattern got wider or narrower. Any edit to an existing line
  of `check.sh`, the guard, its tests or the commit message check is a
  finding.

With no way to accept such a finding, `check.sh full` was run with the
guard step skipped (the re-review's finding docs-H2). A skipped step
checks nothing, and leaves no record of what was accepted or why.

## Decision

- **One tracked file, `scripts/floor-acknowledged.txt`,** holds the
  acknowledged findings. An entry is one line of four fields with a tab
  between them: the rule, the file, the flagged line's text as the guard
  prints it, and a reason. Runs of white space in the line's text count
  as one space. Lines that start with `#` are comments.
- **An entry covers exactly one finding** with that rule, file and line
  text. Two findings need two entries, even when they are alike. No
  entry covers a file or a rule: each changed or removed line of a guard
  script needs its own entry, so a reviewer sees exactly what left the
  gate.
- **An entry counts only in a diff that adds it.** It is committed with
  the change it acknowledges. An entry from an earlier commit
  acknowledges nothing, so a later weakening of the same line needs a
  new entry, and a new review. A range run over several commits sees
  each finding and its entry together.
- **The guard prints** each acknowledged finding with its reason. It
  fails on any other finding, on an added entry that matches no finding
  (`acknowledgement-unmatched`) and on an entry without its four fields
  (`acknowledgement-malformed`). Adding an entry is not itself a finding.
- **The file's lifetime.** Entries are only ever added, never edited or
  removed, and nothing empties the file: it is the record of what was
  accepted, and a removal inside a checked range would cancel an entry
  that range needs. The guard reports a removed or edited entry
  (`acknowledgement-removed`). A merge takes the union of both sides'
  lines.
- **No entry acknowledges an entry.** `acknowledgement-unmatched`,
  `acknowledgement-malformed` and `acknowledgement-removed` can't be
  acknowledged. A wrong entry is corrected by adding a right one in the
  diff that needs it; the old entry stays, and is inert, since an entry
  from an earlier commit acknowledges nothing.
- **CONSTRAINTS.md's Floor guard row** now reads "zero unacknowledged
  floor violations", with a paragraph that names the file and this ADR.

## Why this is a loosening, and its limits

A finding can now pass the gate, which it could not before, so the rule
is looser and needs this ADR. What keeps it narrow:
- the acknowledgement is in the diff under review, beside the change,
  with a reason, and `check.sh` prints it on every run over that diff;
- it can't be reused: one entry, one finding, one diff (with the base
  the work started from; see the limits);
- it is as narrow as its finding. Most findings are one line of one
  file, a changed line of a tool configuration file included. Two rules
  are about a file by nature: `tool-config-added` (a new configuration
  file) and `gate-file-deleted` (a deleted or renamed gate file). One
  entry for either covers that whole file, so the review reads the file;
- the review of a commit that adds entries reads them (CLAUDE.md,
  "Fresh-context review of every change"; the `Review:` line says what
  was accepted).

Limits that remain:
- The guard prints the first 120 characters of a flagged line, and an
  entry is matched against that text. Two lines that differ only after
  that point read as the same finding; each still needs its own entry.
- A range in which an acknowledged change is later undone can't pass
  as a whole: the entry is still added, and now matches nothing.
  - For a finding outside the guard's scripts, the range is checked in
    two parts, or the undoing commit on its own.
  - For a line of a guard script that doesn't help: the undoing commit
    changes a guarded line again, which is a finding of its own and
    needs its own entry. Each commit then passes on its own, with its
    entry; the range holding both never does.
- "One entry, one finding, one diff" holds when the base is the commit
  the work started from. With an earlier base, a line that is dropped
  with its entry, restored and dropped again shows as one removal with
  one added entry: the first entry covers the second drop. `check.sh`
  is run with the base the change started from, and the review of each
  commit reads its own diff.
- The working tree's copy of the guard judges the diff, so a change to
  the guard is judged by the changed guard. Its changed lines are
  findings like any other, which is what puts them in front of the
  reviewer.

## Alternatives considered

### Skip the guard step when a finding is expected
- Pros: no change.
- Cons: every other finding in the same diff passes unseen, and nothing
  records what was accepted.
- Rejected: this is the practice docs-H2 found.

### A list of rule and file pairs passed to the guard
- Pros: short entries.
- Cons: one entry would cover every finding of that rule in the file,
  including ones nobody looked at.
- Rejected.

### A standing allow-list that stays in force
- Pros: an entry is written once.
- Cons: an old entry would acknowledge a new weakening of the same line.
- Rejected: an entry counts only in the diff that adds it.

### A marker comment on the flagged line
- Pros: the acknowledgement sits beside the code.
- Cons: a removed line has nowhere to carry a comment, and a marker in
  the code reads as a standing exception.
- Rejected.

## Consequences

- `check.sh full` passes with every step run when a diff's expected
  findings are acknowledged.
- A commit that changes many guard lines alike carries one entry for
  each line.
- The file grows by a few lines for each such commit and is never
  pruned.
- Parallel branches may add entries: a merge keeps both sides' lines.
