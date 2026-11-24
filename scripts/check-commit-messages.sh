#!/usr/bin/env bash
# Fails if any commit in BEFORE..AFTER breaks the commit message rules:
# - the subject is a Conventional Commit: a type, an optional scope of
#   lower-case ASCII letters, digits and hyphens, then a description of 1
#   to 100 bytes (lengths are bytes: the script runs in the C locale, so a
#   non-ASCII character counts as two or more) that doesn't start with a
#   space, doesn't end with a period, and holds a letter or a digit
#   ("build: 1.2.3" passes, "fix: ." doesn't). The description's first
#   letter, after any leading non-letters (a backtick, a quote, a digit,
#   punctuation), is lowercase; proper nouns and identifiers later in it
#   may have capitals ("fix the `Foo` reader", "close ADR-001"). A
#   description with no letter has no first letter to check;
# - a commit that changes Rust source (*.rs) has a "Library-check:" line in
#   its body saying which dependency APIs or documented patterns were checked
#   and what stayed custom and why (CLAUDE.md, "Library first"). This applies
#   from the commit that introduced the rule onward: a commit whose parent's
#   copy of this script doesn't mention it is exempt;
# - every commit that changes a file outside docs/ and tasks/ has a
#   "Review:" line in its body saying which review subagents ran and what
#   came of their findings (CLAUDE.md, "Fresh-context review"; ADR-016),
#   from the commit that introduced the rule on, the same way. A commit of
#   documents only needs none: those are reviewed at the checkpoint;
# - a "Floor-approved:" line, the maintainer's approval of a blocking floor
#   guard finding (ADR-029), has the form
#   "Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)",
#   the date as YYYY-MM-DD. The guard matches it to its findings.
# A merge commit (two or more parents) is skipped, whatever its subject; any
# other commit is checked, a subject starting "Merge " included.
# With no usable BEFORE (first push), checks every commit reachable from AFTER.
set -euo pipefail
# One reading everywhere: bytes for lengths, and no locale's collation in a
# bracket expression.
export LC_ALL=C
before="${1:-}"
after="${2:-HEAD}"
if [[ -z "$before" || "$before" =~ ^0+$ ]] || ! git cat-file -e "$before^{commit}" 2>/dev/null; then
  range="$after"
else
  range="$before..$after"
fi
# The letters and digits are ASCII, spelt out: a range such as A-Z follows
# the locale's collation, which puts other letters (U+00DC, say) inside it
# in some locales and not in others. LC_ALL=C above settles that too; the
# lists keep the rules right if the script is run another way.
upper=ABCDEFGHIJKLMNOPQRSTUVWXYZ
lower=abcdefghijklmnopqrstuvwxyz
digits=0123456789
pattern="^(feat|fix|docs|chore|refactor|test|ci|build|perf|style|revert)(\\([$lower$digits-]+\\))?!?: [^ ].{0,99}\$"
# No capital before the description's first lower-case letter.
first_letter="^[^$upper$lower]*([$lower]|\$)"
approval="^Floor-approved: [^ ]+ [^ ]+ — [^ ].* \\(the maintainer, [$digits]{4}-[$digits]{2}-[$digits]{2}\\)\$"
script=scripts/check-commit-messages.sh
status=0
# The fields are split on the unit separator: a tab is white space to read,
# and a run of them would drop the root commit's empty parent list.
while IFS=$'\x1f' read -r sha parents subject; do
  read -ra parent_list <<<"$parents"
  if ((${#parent_list[@]} > 1)); then continue; fi
  description=${subject#*: }
  if ! [[ "$subject" =~ $pattern ]]; then
    echo "Not a Conventional Commit: $sha $subject"
    status=1
  elif [[ $description == *. ]] || ! [[ $description =~ [$upper$lower$digits] ]]; then
    echo "Not a Conventional Commit (the description ends with a period, or has no letter or digit): $sha $subject"
    status=1
  elif ! [[ $description =~ $first_letter ]]; then
    echo "Not a Conventional Commit (the description's first letter is a capital): $sha $subject"
    status=1
  fi
  # Read git's output first: under pipefail, `git ... | grep -q` can fail
  # when grep stops reading early and misjudge the commit.
  files=$(git diff-tree --no-commit-id --name-only -r "$sha")
  parent_script=$(git show "$sha^:$script" 2>/dev/null || true)
  body=$(git log -1 --format=%b "$sha")
  if grep -q '\.rs$' <<<"$files" \
    && grep -q 'Library-check:' <<<"$parent_script" \
    && ! grep -qE '^Library-check: .+' <<<"$body"; then
    echo "Missing 'Library-check:' line (commit changes Rust source): $sha $subject"
    status=1
  fi
  if grep -q 'Review:' <<<"$parent_script" \
    && grep -qvE '^(docs|tasks)/' <<<"$files" \
    && ! grep -qE '^Review: .+' <<<"$body"; then
    echo "Missing 'Review:' line (commit changes files outside docs/ and tasks/): $sha $subject"
    status=1
  fi
  while IFS= read -r line; do
    if ! [[ $line =~ $approval ]]; then
      echo "Malformed 'Floor-approved:' line (Floor-approved: <rule> <path> — <reason> (the maintainer, <date>)): $sha $line"
      status=1
    fi
  done < <(grep -E '^[[:space:]]*Floor-approved:' <<<"$body")
done < <(git log --format='%h%x1f%p%x1f%s' "$range")
[[ $status -eq 0 ]] && echo "check-commit-messages: OK"
exit $status
