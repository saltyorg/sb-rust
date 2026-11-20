#!/usr/bin/env bash
# The release workflow's shell logic (release spec, "Release notes and
# announcement" and "Saltbox's toolchain update"): a release's notes and
# its Discord announcement, in the form sb-go's build.yml gives them (its
# "Generate Release Notes" and "Notify Discord" steps), and the check that
# a published release may update Saltbox's toolchain.
# scripts/release.test.sh tests it.
#
#   scripts/release.sh notes TAG REPOSITORY
#       The notes in Markdown, on stdout, from the git repository in the
#       current directory, which must have TAG and the earlier tags.
#   scripts/release.sh discord TAG REPOSITORY NOTES_FILE TIMESTAMP
#       The Discord webhook's JSON payload, on stdout.
#   scripts/release.sh saltbox-gate TAG RELEASE_JSON VERSION_JSON
#       Whether the release may update Saltbox's toolchain, as GITHUB_OUTPUT
#       lines on stdout: `run=true` and `uv-version=<x>`, or `run=false`.
#       RELEASE_JSON is GitHub's release
#       (`GET /repos/{owner}/{repo}/releases/tags/{tag}`); VERSION_JSON,
#       which may be missing, the `sb version --json` of its
#       `sb_linux_amd64`.
#
# TAG is a release tag (`1.0.0`, `1.0.0-rc.1`), REPOSITORY `owner/name`
# (GITHUB_REPOSITORY), TIMESTAMP UTC (`2026-11-20T12:00:00.000Z`).
set -euo pipefail
shopt -s inherit_errexit

# Semver without leading zeros, as the release workflow's plan job reads it.
n='(0|[1-9][0-9]*)'
release_re="^$n\\.$n\\.$n(-rc\\.$n)?\$"
stable_re="^$n\\.$n\\.$n\$"

# Discord's embed description takes up to 4096 characters:
# https://discord.com/developers/docs/resources/message#embed-object-embed-limits
# The notes are cut at sb-go's 1800, which leaves the embed short.
description_max=1800

die() {
  echo "release: $*" >&2
  exit 1
}

usage() {
  die "usage: release.sh notes TAG REPOSITORY | discord TAG REPOSITORY NOTES_FILE TIMESTAMP | saltbox-gate TAG RELEASE_JSON VERSION_JSON"
}

check_args() {
  local tag=$1 repo=$2
  [[ $tag =~ $release_re ]] || die "not a release tag: $tag"
  [[ $repo =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "not owner/name: $repo"
}

# previous_tag TAG: the release tag the notes start from, or nothing for the
# first release. For a stable TAG, the previous stable tag; for an rc, the
# previous tag of either kind. Previous: the highest by semver precedence of
# the tags below TAG that its commit contains, so a tag on a branch that
# TAG's commit doesn't contain is never one.
previous_tag() {
  local tag=$1 pattern=$release_re commit
  if [[ $tag =~ $stable_re ]]; then pattern=$stable_re; fi
  commit=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") || die "no tag $tag in this repository"
  local -a tags
  mapfile -t tags < <(git tag --merged "$commit")
  local -a candidates=()
  local t
  for t in "${tags[@]}"; do
    if [[ $t =~ $pattern && $t != "$tag" ]]; then candidates+=("$t"); fi
  done
  # Ranked as install.sh ranks: `-` becomes `~`, which GNU sort orders before
  # the release it precedes (1.0.0~rc.9 < 1.0.0~rc.10 < 1.0.0).
  # https://www.gnu.org/software/coreutils/manual/html_node/Version_002dsort-ordering-rules.html
  printf '%s\n' "${candidates[@]}" "$tag" | tr '-' '~' | sort -V | tr '~' '-' |
    awk -v tag="$tag" '!done && $0 == tag { print prev; done = 1 } !done && NF { prev = $0 }'
}

notes() {
  (($# == 2)) || usage
  local tag=$1 repo=$2 previous range
  check_args "$tag" "$repo"
  previous=$(previous_tag "$tag")
  range="refs/tags/$tag"
  echo "## What's Changed"
  echo
  if [[ -n $previous ]]; then
    echo "### Commits since $previous:"
    range="refs/tags/$previous..refs/tags/$tag"
  else
    echo "### Commits in this release:"
  fi
  echo
  git -c log.showSignature=false log --no-decorate --pretty=tformat:'- %s (%h)' "$range"
  echo
  if [[ -n $previous ]]; then
    echo "**Full Changelog**: https://github.com/$repo/compare/$previous...$tag"
  else
    # No compare view without a base: GitHub's own notes link the tag's
    # history for a first release.
    echo "**Full Changelog**: https://github.com/$repo/commits/$tag"
  fi
}

discord() {
  (($# == 4)) || usage
  local tag=$1 repo=$2 notes_file=$3 timestamp=$4 suffix=_beta
  check_args "$tag" "$repo"
  [[ -f $notes_file ]] || die "no notes file: $notes_file"
  [[ $timestamp =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{3})?Z$ ]] ||
    die "not a UTC timestamp: $timestamp"
  # The assets of the release's own channel: a stable release's plain
  # ones, a prerelease's `_beta` ones (release spec, "Release channels").
  if [[ $tag =~ $stable_re ]]; then suffix=; fi
  # Every value goes in as a jq argument, so quotes, backslashes and
  # newlines in commit subjects reach Discord as text:
  # https://jqlang.org/manual/v1.6/#invoking-jq
  jq -n --rawfile notes "$notes_file" --arg tag "$tag" --arg repo "$repo" \
    --arg timestamp "$timestamp" --arg suffix "$suffix" --argjson max "$description_max" '
    "https://github.com/\($repo)/releases" as $releases
    | ($notes | sub("\n+$"; "")) as $text
    | {
        username: "GitHub",
        allowed_mentions: { parse: [] },
        avatar_url: "https://github.githubassets.com/images/modules/logos_page/GitHub-Mark.png",
        embeds: [{
          title: "🚀 New Release: \($tag)",
          description: (if ($text | length) > $max then $text[:$max] + "..." else $text end),
          color: 5814783,
          url: "\($releases)/tag/\($tag)",
          timestamp: $timestamp,
          footer: { text: "sb-rust", icon_url: "https://github.com/saltyorg.png" },
          fields: [
            {
              name: "📥 Download",
              value: (["amd64", "arm64"]
                | map("sb_linux_\(.)\($suffix)" | "[\(.)](\($releases)/download/\($tag)/\(.))")
                | join("\n")),
              inline: true
            },
            {
              name: "📝 Full Changelog",
              value: "[View Changes](\($releases)/tag/\($tag))",
              inline: true
            }
          ]
        }]
      }'
}

# Saltbox's toolchain follows stable releases only, and never a prerelease
# (release spec, "Saltbox's toolchain update"; sb-go had no prereleases and
# no such check). The jobs that update Saltbox run only on `run=true`. It
# is `run=false` (with the reason on stderr) unless all of these hold:
# - TAG is a stable release tag, MAJOR.MINOR.PATCH and nothing else (no
#   `-` part, no build metadata, no `v`);
# - GitHub's release is TAG's, published (not a draft), not marked
#   prerelease, with an uploaded `sb_linux_amd64`;
# - that asset's `sb version --json` reports TAG and the release channel.
# A release that passes but reports no valid uv_version fails instead: the
# build's own check should have stopped it.
saltbox_gate() {
  (($# == 3)) || usage
  local tag=$1 release=$2 version=$3 uv
  skip() {
    echo "release: no Saltbox update: $*" >&2
    echo "run=false"
    exit 0
  }
  [[ $tag =~ $stable_re ]] || skip "$tag isn't a stable release tag (MAJOR.MINOR.PATCH)"
  [[ -f $release ]] || die "no release file: $release"
  jq -e --arg tag "$tag" '.tag_name == $tag' "$release" >/dev/null || skip "the release isn't $tag's"
  jq -e '.draft == false' "$release" >/dev/null || skip "$tag is a draft"
  jq -e '.prerelease == false' "$release" >/dev/null || skip "$tag is marked prerelease"
  jq -e 'any(.assets[]?; .name == "sb_linux_amd64" and .state == "uploaded")' "$release" >/dev/null ||
    skip "$tag has no uploaded sb_linux_amd64"
  [[ -f $version ]] || skip "no sb version --json report from sb_linux_amd64"
  jq -e --arg tag "$tag" '.version == $tag' "$version" >/dev/null || skip "sb_linux_amd64 doesn't report $tag"
  jq -e '.channel == "release"' "$version" >/dev/null || skip "sb_linux_amd64 isn't on the release channel"
  uv=$(jq -r '.uv_version | strings' "$version")
  [[ $uv =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "sb_linux_amd64 reports no valid uv_version: $uv"
  echo "run=true"
  echo "uv-version=$uv"
}

(($# >= 1)) || usage
command=$1
shift
case $command in
notes) notes "$@" ;;
discord) discord "$@" ;;
saltbox-gate) saltbox_gate "$@" ;;
*) usage ;;
esac
