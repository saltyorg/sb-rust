#!/usr/bin/env bash
# Builds and launches one prototype with safe demo data, printing what it
# shows and how to drive it first. Run with no arguments for the list.
set -euo pipefail
# Make failures inside $(...) (e.g. a failed build) stop the script too.
shopt -s inherit_errexit

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

usage() {
  cat <<'EOF'
Usage: scripts/try-prototype.sh <prototype>

  facts        Fact editor as a settings page, on a copy of synthetic facts
  facts-large  The same with 60 synthetic apps, to see folding and search
  logs         Log viewer on your real containers and Saltbox services (read-only)
  logs-demo    Log viewer on a demo container and demo systemd unit (removed on exit)
  markdown     Announcements renderer (full-screen view, then inline, then termimad)
  complete     Shell with dynamic tag completion loaded (bash)

Each run builds the prototype (release mode) first. Nothing touches real Saltbox files.
EOF
}

bin() {
  command -v cargo >/dev/null || { echo "cargo not found on PATH" >&2; exit 1; }
  # Release builds: debug builds redraw ~10x slower and misrepresent how it feels.
  echo "Building proto-$1 (release)..." >&2
  cargo build -q --release -p "proto-$1"
  printf '%s/target/release/proto-%s' "$root" "$1"
}

intro() {
  # $1 = title, rest = lines describing what to look at and the controls.
  printf '\n\033[1m%s\033[0m\n' "$1"
  shift
  printf '  %s\n' "$@"
  # The prototypes are interactive: this waits for Enter, and they draw live
  # output and ask questions. Without a terminal (Claude Code's `!` prefix, a
  # pipe, cron) that input never comes, so say so instead of hanging.
  if [[ ! -t 0 ]]; then
    echo "try-prototype: needs an interactive terminal; run it in your own shell, not through a pipe or Claude Code's ! prefix" >&2
    exit 2
  fi
  printf '\nPress Enter to start (Ctrl-C to cancel)... '
  read -r _
}

case "${1:-}" in
  facts | facts-large)
    b="$(bin facts)"
    data="$(mktemp -d)"
    trap 'rm -rf "$data"' EXIT
    if [[ "$1" == facts-large ]]; then
      for i in $(seq -w 1 60); do
        printf '[app%s]\napi_key = synthetic-%s\nport = 8%s\n' "$i" "$i" "$i" >"$data/app$i.ini"
      done
      size="60 synthetic apps, to show how a big install starts folded"
    else
      cp prototypes/proto-facts/fixtures/*.ini "$data/"
      size="a few synthetic apps"
    fi
    intro "Fact editor: settings page with $size (temp copy)" \
      "Look at: apps as headings with values beside each setting, the search box," \
      "         the buttons along the bottom, and the unsaved-changes panel." \
      "Try:  type / and a name to search   click a setting, click again to edit" \
      "      right-click a setting, a copy or an app heading for its actions menu" \
      "      (keyboard: m opens the same menu; each item shows its shortcut key)" \
      "      Copy and Show on a secret   Delete a whole app   undo from the panel" \
      "Every button and menu item also has a key. Esc or q quits." \
      "Resize the window: under 100 columns the changes panel moves to the bottom."
    "$b" "$data"
    if [[ "$1" == facts ]]; then
      printf '\nChanges were written to the temp copy only. Diff vs the fixtures:\n'
      diff -r prototypes/proto-facts/fixtures "$data" || true
    fi
    ;;
  logs)
    b="$(bin logs)"
    intro "Log viewer on your real containers and Saltbox services (read-only)" \
      "Look at: the grouped picker and its status words/colours, streaming while following, pausing, search, switching source," \
      "         and how long lines, JSON and coloured output from real apps look." \
      "Picker: sections Needs attention, Services, Containers; status updates live" \
      "        ↑/↓ or wheel move   Enter or click again to open   / filter (Esc clears)   q quit" \
      "Viewer: ↑/↓, PgUp/PgDn or wheel scroll (scrolling up pauses; back at the bottom it follows)" \
      "        w wrap long lines on/off; with wrap off, ←/→ or shift+wheel scroll sideways" \
      "        f follow on/off   / search   n/N next/prev match   right-click (or m) menu" \
      "        Esc back to picker   q quit" \
      "Inline design afterwards:  $b --inline <container-or-unit>   (Ctrl-C stops)"
    "$b"
    ;;
  logs-demo)
    b="$(bin logs)"
    name=sb-rust-proto-demo
    cleanup() {
      docker rm -f "$name" "$name-crash" >/dev/null 2>&1 || true
      systemctl stop "$name.service" >/dev/null 2>&1 || true
    }
    trap cleanup EXIT
    docker run -d --rm --name "$name" alpine sh -c \
      'i=0; while true; do i=$((i+1)); printf "\033[32mINFO\033[0m line %s\n" $i; [ $((i%7)) = 0 ] && printf "\033[31mERROR\033[0m needle %s\n" $i; sleep 0.3; done' >/dev/null
    # Fails every few seconds and is restarted, to show "Needs attention".
    docker run -d --name "$name-crash" --restart on-failure alpine sh -c \
      'echo "starting"; sleep 4; echo "fatal: demo crash" >&2; exit 1' >/dev/null
    # shellcheck disable=SC2016 # $n is expanded by the unit's own sh, not here
    systemd-run --quiet --unit "$name" sh -c 'n=0; while true; do n=$((n+1)); echo "demo tick $n"; sleep 0.5; done'
    intro "Log viewer on a demo container + demo systemd unit (removed on exit)" \
      "Predictable output for checking search (try: needle) and switching sources." \
      "A third demo container crashes every few seconds, to show Needs attention live." \
      "Picker: ↑/↓ or wheel move   Enter or click again to open   / filter   q quit" \
      "Viewer: ↑/↓ or wheel scroll   w wrap (off: ←/→ or shift+wheel sideways)   f follow" \
      "        / search   n/N   right-click menu   Esc picker   q quit"
    "$b" --filter "$name" --service-prefix "$name"
    ;;
  markdown)
    b="$(bin markdown)"
    f=prototypes/proto-markdown/fixtures/announcements.yml
    before=prototypes/proto-markdown/fixtures/announcements-before.yml
    intro "Announcements (synthetic; one is new since the 'before' file)" \
      "Three renderings in a row: the full-screen viewer, the new ones printed inline (as after" \
      "sb update), then termimad inline for comparison." \
      "Viewer: list on the left (new ones marked ●, older ones tinted), announcement on the right." \
      "        ↑/↓ or click choose   PgUp/PgDn, space or wheel scroll   q to continue" \
      "Look at: the list, headings, bold/italic, inline code, links, lists, code, quote, table."
    "$b" --mode view --before "$before" "$f"
    printf '\n\033[1m--- tui-markdown inline (new announcements only) ---\033[0m\n'
    "$b" --mode inline --before "$before" "$f"
    printf '\n\033[1m--- termimad inline (rejected candidate, for comparison) ---\033[0m\n'
    "$b" --renderer termimad --mode inline --before "$before" "$f"
    printf '\nTo render the real Saltbox announcements: %s --mode view /srv/git/saltbox/announcements.yml\n' "$b"
    ;;
  complete)
    b="$(bin complete)"
    intro "Dynamic tag completion (a bash subshell with completion loaded)" \
      "Try:  proto-complete <TAB><TAB>" \
      "      proto-complete install s<TAB><TAB>" \
      "      proto-complete install plex,so<TAB>" \
      "      proto-complete install plex,sonarr,sandbox-<TAB><TAB>" \
      "Type exit to leave the subshell. For zsh: source <(COMPLETE=zsh proto-complete)"
    PATH="$(dirname "$b"):$PATH" bash --rcfile <(printf 'PS1="(proto-complete) \\w\\$ "\nsource <(COMPLETE=bash proto-complete)\n') -i
    ;;
  -h | --help | "")
    usage
    ;;
  *)
    usage
    exit 2
    ;;
esac
