#!/usr/bin/env bash
# publish.sh, the commit mode, against a local Saltbox fixture and a fake gh
# on PATH: a prepared update is committed once with exactly its files; a
# master that moved, an unchanged update, a handover with another file, a
# .uv-version that isn't the release's and a missing token commit nothing.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf -- "$test_dir"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

saltbox="$test_dir/saltbox"
mkdir -p "$saltbox/.github" "$saltbox/requirements"
printf '3.12.13\n' > "$saltbox/.python-version"
printf '0.12.9\n' > "$saltbox/.uv-version"
printf '{"constraints": {"uv": "0.12.9"}}\n' > "$saltbox/.github/renovate.json"
printf '# baseline\n' > "$saltbox/requirements/requirements-saltbox.txt"
git -C "$saltbox" init --quiet --initial-branch=master
git -C "$saltbox" add .
git -C "$saltbox" commit --quiet -m 'test: initialize toolchain fixture'
base=$(git -C "$saltbox" rev-parse HEAD)

# prepare DIR: a handover as update.sh leaves it, for uv 0.12.10.
prepare() {
    local dir=$1
    mkdir -p "$dir/files/.github" "$dir/files/requirements"
    printf '%s\n' "$base" > "$dir/base-commit"
    printf 'true\n' > "$dir/changed"
    printf '3.12.14\n' > "$dir/files/.python-version"
    printf '0.12.10\n' > "$dir/files/.uv-version"
    printf '{"constraints": {"uv": "0.12.10"}}\n' > "$dir/files/.github/renovate.json"
    # uv's own lines: a header, pins, hashes, `# via` notes.
    printf '# updated\nfixture==1.0 \\\n    --hash=sha256:%064d \\\n    --hash=sha256:%064d\n    # via -r requirements/requirements-saltbox.in\n' 0 1 \
        > "$dir/files/requirements/requirements-saltbox.txt"
}

# The tree the commit must have: the base with the prepared files.
expected="$test_dir/expected"
git clone --quiet "$saltbox" "$expected"
prepare "$test_dir/reference"
cp -r "$test_dir/reference/files/." "$expected/"
git -C "$expected" add -A
tree=$(git -C "$expected" write-tree)

# The fake gh: the mutation, then the commit's metadata.
mkdir -p "$test_dir/bin"
cat > "$test_dir/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
[[ ${GH_TOKEN:-} == fixture-token ]] || { echo "gh without the token" >&2; exit 1; }
if [[ $2 == graphql ]]; then
    printf 'mutation\n' >> "$TEST_DIR/calls"
    cp "$4" "$TEST_DIR/request.json"
    jq -n '{data:{createCommitOnBranch:{commit:{oid:"cccccccccccccccccccccccccccccccccccccccc"}}}}'
elif [[ $2 == */commits/cccccccccccccccccccccccccccccccccccccccc ]]; then
    jq -n --arg base "$TEST_BASE" --arg tree "$TEST_TREE" '{parents:[{sha:$base}],commit:{
      tree:{sha:$tree},message:"chore(deps): update python toolchain",verification:{verified:true}}}'
else
    echo "Unexpected API request: $*" >&2
    exit 1
fi
GH
chmod 0755 "$test_dir/bin/gh"

run_publish() { # DIR [ENV...]
    local dir=$1
    shift
    : > "$test_dir/calls"
    env PATH="$test_dir/bin:$PATH" TEST_DIR="$test_dir" TEST_BASE="$base" TEST_TREE="$tree" \
        SALTBOX_TOKEN=fixture-token SALTBOX_REPOSITORY=saltyorg/Saltbox SALTBOX_CLONE_URL="file://$saltbox" \
        SB_RELEASE=1.0.0 UV_VERSION=0.12.10 INPUT_DIR="$dir" "$@" "$script_dir/publish.sh"
}
calls() { wc -l < "$test_dir/calls"; }

# A prepared update: one signed commit with exactly the four files.
prepare "$test_dir/ok"
out=$(run_publish "$test_dir/ok") || fail "publish failed: $out"
[[ $(calls) -eq 1 ]] || fail "mutations: $(calls), want 1"
jq -e --arg base "$base" '
    .variables.input.expectedHeadOid == $base and
    .variables.input.branch == {repositoryNameWithOwner:"saltyorg/Saltbox",branchName:"master"} and
    ([.variables.input.fileChanges.additions[].path] == [".python-version",".uv-version",".github/renovate.json","requirements/requirements-saltbox.txt"]) and
    (.variables.input.fileChanges.additions[0].contents | @base64d) == "3.12.14\n" and
    (.variables.input.fileChanges.additions[3].contents | @base64d | startswith("# updated\nfixture==1.0 \\\n"))
' "$test_dir/request.json" > /dev/null || fail "request: $(cat "$test_dir/request.json")"
[[ $out == *"Updated Saltbox master for sb 1.0.0 with Python 3.12.14 and uv 0.12.10" ]] || fail "output: $out"

# refused DIR [ENV...]: publish fails and commits nothing.
refused() {
    if run_publish "$@" > /dev/null 2>&1; then fail "publish accepted $*"; fi
    [[ $(calls) -eq 0 ]] || fail "publish committed for $*"
}

# Master moved since the preparation.
prepare "$test_dir/moved"
printf 'feat: elsewhere\n' > "$saltbox/other"
git -C "$saltbox" add other
git -C "$saltbox" commit --quiet -m 'test: move master'
refused "$test_dir/moved"
git -C "$saltbox" reset --quiet --hard "$base"

# A file outside the four.
prepare "$test_dir/extra"
printf 'x\n' > "$test_dir/extra/files/saltbox.yml"
refused "$test_dir/extra"
# A .uv-version other than the release's uv.
prepare "$test_dir/uv"
refused "$test_dir/uv" UV_VERSION=0.12.11
# A missing file.
prepare "$test_dir/missing"
rm "$test_dir/missing/files/.uv-version"
refused "$test_dir/missing"
# No token.
prepare "$test_dir/token"
refused "$test_dir/token" SALTBOX_TOKEN=
# A handover the prepare job's package code could have rewritten: an older
# Python, a renovate.json changed beyond constraints.uv, a lock with an
# index option, a URL or a short hash.
prepare "$test_dir/python"
printf '3.12.12\n' > "$test_dir/python/files/.python-version"
refused "$test_dir/python"
prepare "$test_dir/renovate"
printf '{"constraints": {"uv": "0.12.10"}, "automerge": true}\n' > "$test_dir/renovate/files/.github/renovate.json"
refused "$test_dir/renovate"
for line in '--extra-index-url https://example.invalid/simple' \
    'fixture @ https://example.invalid/fixture-1.0-py3-none-any.whl' \
    '    --hash=sha256:abc' '-e .'; do
    prepare "$test_dir/lock"
    printf 'fixture==1.0 \\\n    --hash=sha256:%064d\n%s\n' 0 "$line" > "$test_dir/lock/files/requirements/requirements-saltbox.txt"
    refused "$test_dir/lock"
    rm -rf -- "$test_dir/lock"
done

# Nothing to change (master has the release's uv and the files already):
# no commit.
printf '0.12.10\n' > "$saltbox/.uv-version"
printf '{"constraints": {"uv": "0.12.10"}}\n' > "$saltbox/.github/renovate.json"
git -C "$saltbox" commit --quiet -am 'test: uv already 0.12.10'
base=$(git -C "$saltbox" rev-parse HEAD)
prepare "$test_dir/same"
for file in .python-version .uv-version .github/renovate.json requirements/requirements-saltbox.txt; do
    cp -- "$saltbox/$file" "$test_dir/same/files/$file"
done
out=$(run_publish "$test_dir/same") || fail "an unchanged update failed: $out"
[[ $(calls) -eq 0 ]] || fail "an unchanged update committed"
[[ $out == *"already contains"* ]] || fail "output: $out"

echo 'PASS: publish (1 commit, 11 refusals, 1 unchanged)'
