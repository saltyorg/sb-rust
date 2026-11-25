#!/usr/bin/env bash
# The workflows' linter (CONSTRAINTS.md, Workflows) refuses what it is
# there to catch: check-jsonschema with its built-in GitHub schemas, run as
# check.sh runs it, on an invented workflow and composite action. Each
# passes as written, the newer syntax the repository uses included (the
# workflow-level `cache-mode`, a `$/` self-repository action, the 26.04
# runner labels), and fails once one key or value is broken.
# check-jsonschema's built-in schemas:
# https://github.com/python-jsonschema/check-jsonschema/blob/0.39.0/docs/usage.rst#builtin-schema-choices
set -euo pipefail

dir=$(mktemp -d)
trap 'rm -rf -- "$dir"' EXIT
failed=0

cat >"$dir/workflow.yml" <<'EOF'
name: example
on: push
permissions: {}
cache-mode: none
jobs:
  build:
    runs-on: ${{ matrix.runner }}
    timeout-minutes: 10
    strategy:
      fail-fast: false
      matrix:
        runner: [ubuntu-26.04, ubuntu-26.04-arm]
    steps:
      - uses: $/.github/actions/example
      - run: echo example
EOF
cat >"$dir/action.yml" <<'EOF'
name: example
description: An invented composite action.
runs:
  using: composite
  steps:
    - shell: bash
      run: echo example
EOF

# expect NAME SCHEMA FILE [FROM TO WANT]: FILE as it is passes SCHEMA;
# with FROM replaced by TO, it fails with status 1 and WANT in the output.
expect() {
  local name=$1 schema=$2 file=$3 from=${4-} to=${5-} want=${6-} text out status=0
  text=$(<"$file")
  if [[ -n $from ]]; then
    if [[ $text != *"$from"* ]]; then
      echo "FAIL $name: '$from' is not in $(basename -- "$file")" >&2
      failed=1
      return
    fi
    text=${text/"$from"/"$to"}
  fi
  printf '%s\n' "$text" >"$dir/case.yml"
  out=$(check-jsonschema --builtin-schema "$schema" "$dir/case.yml" 2>&1) || status=$?
  if [[ -z $from ]] && ((status == 0)); then
    echo "ok $name"
  elif [[ -n $from ]] && ((status == 1)) && [[ $out == *"$want"* ]]; then
    echo "ok $name"
  else
    printf 'FAIL %s: status %s\n%s\n' "$name" "$status" "$out" >&2
    failed=1
  fi
}

w=vendor.github-workflows
a=vendor.github-actions
expect "the workflow passes" "$w" "$dir/workflow.yml"
expect "a misspelt job key fails" "$w" "$dir/workflow.yml" \
  'timeout-minutes: 10' 'timeout-minuts: 10' "'timeout-minuts' was unexpected"
expect "fail-fast as a list fails" "$w" "$dir/workflow.yml" \
  'fail-fast: false' 'fail-fast: [nope]' "is not of type 'boolean'"
expect "an unknown cache-mode fails" "$w" "$dir/workflow.yml" \
  'cache-mode: none' 'cache-mode: sometimes' "'sometimes' is not one of"
expect "the action passes" "$a" "$dir/action.yml"
expect "an unknown runs.using fails" "$a" "$dir/action.yml" \
  'using: composite' 'using: compost' "'compost' is not one of"
exit "$failed"
