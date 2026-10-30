#!/usr/bin/env bash
# Fails if any commit in BEFORE..AFTER breaks the commit message rules:
# - the subject is a Conventional Commit (lowercase description, no trailing
#   period);
# - a commit that changes Rust source (*.rs) has a "Library-check:" line in
#   its body saying which dependency APIs or documented patterns were checked
#   and what stayed custom and why (CLAUDE.md, "Library first"). This applies
#   from the commit that introduced the rule onward: a commit whose parent's
#   copy of this script doesn't mention it is exempt;
# - every commit has a "Review:" line in its body saying which review
#   subagents ran and what came of their findings (CLAUDE.md, "Fresh-context
#   review of every change"), from the commit that introduced the rule on,
#   the same way.
# With no usable BEFORE (first push), checks every commit reachable from AFTER.
set -euo pipefail
before="${1:-}"
after="${2:-HEAD}"
if [[ -z "$before" || "$before" =~ ^0+$ ]] || ! git cat-file -e "$before^{commit}" 2>/dev/null; then
  range="$after"
else
  range="$before..$after"
fi
pattern='^(feat|fix|docs|chore|refactor|test|ci|build|perf|style|revert)(\([a-z0-9-]+\))?!?: [^A-Z ](.{0,98}[^.])?$'
script=scripts/check-commit-messages.sh
status=0
while IFS= read -r line; do
  sha="${line%% *}"; subject="${line#* }"
  if [[ "$subject" =~ ^Merge\  ]]; then continue; fi
  if ! [[ "$subject" =~ $pattern ]]; then
    echo "Not a Conventional Commit: $sha $subject"
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
    && ! grep -qE '^Review: .+' <<<"$body"; then
    echo "Missing 'Review:' line: $sha $subject"
    status=1
  fi
done < <(git log --format='%h %s' "$range")
[[ $status -eq 0 ]] && echo "check-commit-messages: OK"
exit $status
