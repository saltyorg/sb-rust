#!/usr/bin/env bash
# scripts/release.sh against a fixture repository: the previous-tag
# rule (stable after stable, rc after either, semver precedence, tags the
# commit doesn't contain and tags that aren't releases left out), the
# notes' exact text, the Discord payload (built by jq, so a subject's
# quotes and backslashes survive; the notes cut at 1800 characters; each
# channel's assets), and the gate before Saltbox's toolchain update (stable
# releases only). check.sh runs it in its release scripts' step.
#
#   scripts/release.test.sh
#
# Its last line is `PASS: ...`.
set -euo pipefail
shopt -s inherit_errexit

script="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/release.sh"
fixture=$(mktemp -d)
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The fixture's git ignores the user's and the system's configuration (no
# signing, no hooks), with a fixed identity and clock.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid
clock=1700000000
commit() {
  clock=$((clock + 60))
  GIT_AUTHOR_DATE="@$clock +0000" GIT_COMMITTER_DATE="@$clock +0000" \
    git commit --quiet --allow-empty -m "$1"
}
short() { git rev-parse --short "$1"; }

cd "$fixture"
git init --quiet --initial-branch=main
commit "feat: first"
c1=$(short HEAD)
git tag v0.9.0
git tag 01.0.0
commit 'fix: keep "quotes" and a \ backslash'
c2=$(short HEAD)
git tag -a -m "an annotated tag" 1.0.0-rc.1
commit "docs: three"
c3=$(short HEAD)
git tag 1.0.0-rc.2
git tag not-a-release
commit "feat: four"
c4=$(short HEAD)
git tag 1.0.0
git switch --quiet -c side
commit "feat: on a side branch"
git tag 1.0.1-rc.1
git switch --quiet main
commit "fix: six"
c6=$(short HEAD)
git tag 1.0.1-rc.9
commit "fix: seven"
c7=$(short HEAD)
git tag 1.0.1-rc.10
commit "feat: eight"
c8=$(short HEAD)
git tag 1.0.1

repo=saltyorg/sb-rust
notes() { "$script" notes "$1" "$repo"; }

# expect_notes TAG: the notes of TAG are stdin.
expect_notes() {
  local want got
  want=$(cat)
  got=$(notes "$1") || fail "notes $1 failed"
  [[ $got == "$want" ]] || fail "notes $1:"$'\n'"--- want"$'\n'"$want"$'\n'"--- got"$'\n'"$got"
}

# The first release: every commit, and the tag's history as the changelog.
expect_notes 1.0.0-rc.1 <<EOF
## What's Changed

### Commits in this release:

- fix: keep "quotes" and a \\ backslash ($c2)
- feat: first ($c1)

**Full Changelog**: https://github.com/$repo/commits/1.0.0-rc.1
EOF

# An rc after an rc.
expect_notes 1.0.0-rc.2 <<EOF
## What's Changed

### Commits since 1.0.0-rc.1:

- docs: three ($c3)

**Full Changelog**: https://github.com/$repo/compare/1.0.0-rc.1...1.0.0-rc.2
EOF

# The first stable release starts from no stable tag: the rcs, v0.9.0 and
# 01.0.0 (not release tags) don't count.
expect_notes 1.0.0 <<EOF
## What's Changed

### Commits in this release:

- feat: four ($c4)
- docs: three ($c3)
- fix: keep "quotes" and a \\ backslash ($c2)
- feat: first ($c1)

**Full Changelog**: https://github.com/$repo/commits/1.0.0
EOF

# An rc after a stable release; the side branch's 1.0.1-rc.1 isn't in it.
expect_notes 1.0.1-rc.9 <<EOF
## What's Changed

### Commits since 1.0.0:

- fix: six ($c6)

**Full Changelog**: https://github.com/$repo/compare/1.0.0...1.0.1-rc.9
EOF

# Semver precedence: rc.10 follows rc.9.
expect_notes 1.0.1-rc.10 <<EOF
## What's Changed

### Commits since 1.0.1-rc.9:

- fix: seven ($c7)

**Full Changelog**: https://github.com/$repo/compare/1.0.1-rc.9...1.0.1-rc.10
EOF

# A stable release starts from the previous stable one, past its rcs.
expect_notes 1.0.1 <<EOF
## What's Changed

### Commits since 1.0.0:

- feat: eight ($c8)
- fix: seven ($c7)
- fix: six ($c6)

**Full Changelog**: https://github.com/$repo/compare/1.0.0...1.0.1
EOF

# Refused: a tag that isn't a release tag, one the repository hasn't, a
# repository that isn't owner/name, a wrong argument count.
refused() {
  local err
  if err=$("$script" "$@" 2>&1); then fail "accepted: $*"; fi
  [[ $err == release:* ]] || fail "no message for: $*: $err"
}
refused notes v1.0.0 "$repo"
refused notes 01.0.0 "$repo"
refused notes 1.0.0-beta.1 "$repo"
refused notes 9.9.9 "$repo"
refused notes 1.0.0 "not a repo"
refused notes 1.0.0
refused nonsense

# A first release with more commits than its notes list: the newest 100,
# a heading that says so, and the changelog link for the rest.
mkdir "$fixture/long"
(
  cd "$fixture/long"
  git init --quiet --initial-branch=main
  for i in $(seq 1 102); do commit "feat: change $i"; done
  git tag 1.0.0
  got=$(notes 1.0.0) || fail "notes of a long first release failed"
  want_head=$'## What\'s Changed\n\n### The newest 100 of this release\'s 102 commits:\n\n'"- feat: change 102 ($(short HEAD))"
  [[ $got == "$want_head"$'\n'* ]] || fail "long first release's head:"$'\n'"$(head -5 <<<"$got")"
  [[ $(grep -c '^- ' <<<"$got") == 100 ]] || fail "long first release lists $(grep -c '^- ' <<<"$got") commits, not 100"
  [[ $(grep '^- ' <<<"$got" | tail -1) == "- feat: change 3 ($(short HEAD~99))" ]] ||
    fail "long first release's last line: $(grep '^- ' <<<"$got" | tail -1)"
  [[ $got == *$'\n\n'"**Full Changelog**: https://github.com/$repo/commits/1.0.0" ]] ||
    fail "long first release's link: $(tail -1 <<<"$got")"
  # At the cap, every commit, under the plain heading.
  git tag 1.0.0-rc.1 HEAD~2
  got=$(notes 1.0.0-rc.1) || fail "notes of a 100-commit first release failed"
  [[ $(grep -c '^- ' <<<"$got") == 100 && $got == *"### Commits in this release:"* ]] ||
    fail "a 100-commit first release:"$'\n'"$(head -5 <<<"$got")"
)

# The Discord payload.
time=2026-11-20T12:00:00.000Z
notes 1.0.1 >"$fixture/stable.md"
payload=$("$script" discord 1.0.1 "$repo" "$fixture/stable.md" "$time") || fail "discord 1.0.1 failed"
releases=https://github.com/$repo/releases
jq -e --rawfile notes "$fixture/stable.md" --arg r "$releases" --arg time "$time" '
  .username == "GitHub"
  and .allowed_mentions == { parse: [] }
  and (.embeds | length) == 1
  and .embeds[0].title == "🚀 New Release: 1.0.1"
  and .embeds[0].description == ($notes | rtrimstr("\n"))
  and .embeds[0].url == "\($r)/tag/1.0.1"
  and .embeds[0].timestamp == $time
  and .embeds[0].color == 5814783
  and .embeds[0].footer.text == "sb-rust"
  and .embeds[0].fields == [
    { name: "📥 Download",
      value: "[sb_linux_amd64](\($r)/download/1.0.1/sb_linux_amd64)\n[sb_linux_arm64](\($r)/download/1.0.1/sb_linux_arm64)",
      inline: true },
    { name: "📝 Full Changelog", value: "[View Changes](\($r)/tag/1.0.1)", inline: true }
  ]' <<<"$payload" >/dev/null || fail "discord 1.0.1 payload: $payload"

# The quotes and the backslash of a subject arrive as text.
notes 1.0.0-rc.1 >"$fixture/rc.md"
payload=$("$script" discord 1.0.0-rc.1 "$repo" "$fixture/rc.md" "$time") || fail "discord 1.0.0-rc.1 failed"
jq -e --arg line "- fix: keep \"quotes\" and a \\ backslash ($c2)" '
  (.embeds[0].description | split("\n") | index($line)) != null
  and (.embeds[0].fields[0].value | test("sb_linux_amd64_beta\\)\n.*sb_linux_arm64_beta\\)$"))' \
  <<<"$payload" >/dev/null || fail "discord 1.0.0-rc.1 payload: $payload"

# Long notes: cut to 1800 characters (not bytes), then "...".
printf 'é%.0s' {1..2000} >"$fixture/long.md"
payload=$("$script" discord 1.0.1 "$repo" "$fixture/long.md" "$time") || fail "discord long failed"
jq -e '.embeds[0].description | length == 1803 and endswith("é...")' <<<"$payload" >/dev/null ||
  fail "long notes not cut to 1800: $(jq '.embeds[0].description | length' <<<"$payload")"
printf 'é%.0s' {1..1800} >"$fixture/edge.md"
payload=$("$script" discord 1.0.1 "$repo" "$fixture/edge.md" "$time") || fail "discord edge failed"
jq -e '.embeds[0].description | length == 1800' <<<"$payload" >/dev/null || fail "1800 characters were cut"

refused discord 1.0.1 "$repo" "$fixture/stable.md" "2026-11-20 12:00"
refused discord 1.0.1 "$repo" "$fixture/missing.md" "$time"
refused discord 1.0.1 "$repo" "$fixture/stable.md"

# The Saltbox toolchain gate runs only for a published stable release
# whose binary reports the release channel; everything else skips it.
release_json() { # TAG PRERELEASE DRAFT ASSET STATE
  jq -n --arg tag "$1" --argjson pre "$2" --argjson draft "$3" --arg asset "$4" --arg state "$5" \
    '{tag_name: $tag, prerelease: $pre, draft: $draft,
      assets: [{name: "sb_linux_amd64_beta", state: "uploaded"}, {name: $asset, state: $state}]}'
}
version_json() { # VERSION CHANNEL UV
  jq -n --arg v "$1" --arg c "$2" --arg uv "$3" '{version: $v, git_commit: "abc1234", uv_version: $uv, channel: $c}'
}
gate() { # TAG, then release_json's five, then version_json's three (none: no report)
  local tag=$1
  shift
  release_json "${@:1:5}" >"$fixture/release.json"
  rm -f "$fixture/version.json"
  if (($# > 5)); then version_json "${@:6:3}" >"$fixture/version.json"; fi
  "$script" saltbox-gate "$tag" "$fixture/release.json" "$fixture/version.json"
}
out=$(gate 1.0.0 1.0.0 false false sb_linux_amd64 uploaded 1.0.0 release 0.13.4 2>&1) ||
  fail "the gate failed on a stable release: $out"
[[ $out == $'run=true\nuv-version=0.13.4' ]] || fail "a stable release didn't pass the gate: $out"
# skipped WHY ARGS...: the gate says run=false, giving WHY.
skipped() {
  local why=$1 out err
  shift
  out=$(gate "$@" 2>"$fixture/err") || fail "the gate failed instead of skipping: $* ($(cat "$fixture/err"))"
  err=$(cat "$fixture/err")
  [[ $out == run=false ]] || fail "the gate ran for: $*: $out"
  [[ $err == *"$why"* ]] || fail "the gate skipped $* for: $err (want: $why)"
}
for tag in 1.0.0-rc.1 1.0.0-beta.1 1.0.0+build v1.0.0 1.0 01.0.0; do
  skipped "isn't a stable release tag" "$tag" "$tag" false false sb_linux_amd64 uploaded "$tag" release 0.13.4
done
# An rc as it is published: marked prerelease, `_beta` assets only.
skipped "isn't a stable release tag" 1.0.0-rc.1 1.0.0-rc.1 true false sb_linux_amd64_beta uploaded
skipped "marked prerelease" 1.0.0 1.0.0 true false sb_linux_amd64 uploaded 1.0.0 release 0.13.4
skipped "is a draft" 1.0.0 1.0.0 false true sb_linux_amd64 uploaded 1.0.0 release 0.13.4
skipped "isn't 1.0.0's" 1.0.0 0.9.9 false false sb_linux_amd64 uploaded 1.0.0 release 0.13.4
skipped "no uploaded sb_linux_amd64" 1.0.0 1.0.0 false false sb_linux_arm64 uploaded 1.0.0 release 0.13.4
skipped "no uploaded sb_linux_amd64" 1.0.0 1.0.0 false false sb_linux_amd64 starter 1.0.0 release 0.13.4
skipped "no sb version --json report" 1.0.0 1.0.0 false false sb_linux_amd64 uploaded
skipped "doesn't report 1.0.0" 1.0.0 1.0.0 false false sb_linux_amd64 uploaded 1.0.1 release 0.13.4
skipped "isn't on the release channel" 1.0.0 1.0.0 false false sb_linux_amd64 uploaded 1.0.0 beta 0.13.4
skipped "isn't on the release channel" 1.0.0 1.0.0 false false sb_linux_amd64 uploaded 1.0.0 none 0.13.4
# A stable release with no valid uv_version fails.
for uv in 0.13 ""; do
  if out=$(gate 1.0.0 1.0.0 false false sb_linux_amd64 uploaded 1.0.0 release "$uv" 2>&1); then
    fail "the gate passed uv_version '$uv': $out"
  fi
  [[ $out == release:*"no valid uv_version"* ]] || fail "uv_version '$uv': $out"
done

echo "PASS: release notes (6 tags, 10 refusals, a first release over the 100-commit cap and one at it), the Discord payload (2 channels, the 1800-character cut) and the Saltbox gate (1 run, 16 skips, 2 failures)"
