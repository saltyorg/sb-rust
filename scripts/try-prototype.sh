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
  runner       Task runner with spinners, live output and a failing step
  inline       docker ps style table, then username/password prompts
  complete     Shell with dynamic tag completion loaded (bash)

Each run builds the prototype (release mode) first. Nothing touches real Saltbox files.
EOF
}

bin() {
  command -v cargo >/dev/null || { echo "cargo not found on PATH" >&2; exit 1; }
  # Release builds: debug builds redraw ~10x slower and misrepresent how it feels.
  cargo build -q --release -p "proto-$1"
  printf '%s/target/release/proto-%s' "$root" "$1"
}

intro() {
  # $1 = title, rest = lines describing what to look at and the controls.
  printf '\n\033[1m%s\033[0m\n' "$1"
  shift
  printf '  %s\n' "$@"
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
      "Look at: the picker, streaming while following, pausing, search, switching source," \
      "         and how long lines, JSON and coloured output from real apps look." \
      "Picker: ↑/↓ or wheel move   Enter or click again to open   q quit" \
      "Viewer: ↑/↓ or wheel highlight a line (pauses)   Enter or click it to open it in full" \
      "        w wrap long lines on/off   f follow on/off   / search   n/N next/prev match" \
      "        right-click a line (or m) for the same actions   Esc back to picker   q quit" \
      "Inline design afterwards:  $b --inline <container-or-unit>   (Ctrl-C stops)"
    "$b"
    ;;
  logs-demo)
    b="$(bin logs)"
    name=sb-rust-proto-demo
    cleanup() {
      docker rm -f "$name" >/dev/null 2>&1 || true
      systemctl stop "$name.service" >/dev/null 2>&1 || true
    }
    trap cleanup EXIT
    docker run -d --rm --name "$name" alpine sh -c \
      'i=0; while true; do i=$((i+1)); printf "\033[32mINFO\033[0m line %s\n" $i; [ $((i%7)) = 0 ] && printf "\033[31mERROR\033[0m needle %s\n" $i; sleep 0.3; done' >/dev/null
    # shellcheck disable=SC2016 # $n is expanded by the unit's own sh, not here
    systemd-run --quiet --unit "$name" sh -c 'n=0; while true; do n=$((n+1)); echo "demo tick $n"; sleep 0.5; done'
    intro "Log viewer on a demo container + demo systemd unit (removed on exit)" \
      "Predictable output for checking search (try: needle) and switching sources." \
      "Picker: ↑/↓ or wheel move   Enter or click again to open   q quit" \
      "Viewer: ↑/↓ highlight   Enter open line   w wrap   f follow   / search   n/N   right-click menu" \
      "        Esc picker   q quit"
    "$b" --filter "$name"
    ;;
  markdown)
    b="$(bin markdown)"
    f=prototypes/proto-markdown/fixtures/announcements.yml
    intro "Announcements renderer (synthetic announcement)" \
      "Three renderings in a row: tui-markdown full-screen, tui-markdown inline, termimad inline." \
      "Full-screen keys: ↑/↓ PgUp/PgDn scroll   ←/→ previous/next announcement   q to continue" \
      "Look at: headings, bold/italic, inline code, links, lists, code block, quote wrapping, CJK/emoji."
    "$b" --mode view "$f"
    printf '\n\033[1m--- tui-markdown inline ---\033[0m\n'
    "$b" --mode inline "$f"
    printf '\n\033[1m--- termimad inline (rejected candidate, for comparison) ---\033[0m\n'
    "$b" --renderer termimad --mode inline "$f"
    printf '\nTo render the real Saltbox announcements: %s --mode view /srv/git/saltbox/announcements.yml\n' "$b"
    ;;
  runner)
    b="$(bin runner)"
    ansible=/srv/ansible/venv/bin/ansible-playbook
    [[ -x "$ansible" ]] || ansible=true
    intro "Task runner (simulated steps; the first runs ansible-playbook --version if present)" \
      "Run 1: spinners with a live tail, then a failing step that prints its last 10 lines." \
      "Run 2: a 30 s step - press Ctrl-C to see the interrupt handling." \
      "Run 3: the same steps piped through cat (plain output, no spinners)."
    "$b" --ansible "$ansible" --fail || true
    printf '\n\033[1m--- Run 2: press Ctrl-C during the long task ---\033[0m\n'
    "$b" --ansible "$ansible" --slow || true
    printf '\n\033[1m--- Run 3: piped ---\033[0m\n'
    "$b" --ansible "$ansible" --fail 2>&1 | cat || true
    ;;
  inline)
    b="$(bin inline)"
    intro "Inline output (no full-screen UI)" \
      "1. docker ps style table of your containers (resize narrow to see wrapping)." \
      "2. The same table with NO_COLOR=1." \
      "3. Restore-style prompts: try an empty or spaced username and a short password." \
      "   Nothing is restored; it only prints what it would do."
    "$b" ps
    printf '\n\033[1m--- NO_COLOR=1 ---\033[0m\n'
    NO_COLOR=1 "$b" ps
    printf '\n\033[1m--- prompts ---\033[0m\n'
    "$b" prompt || true
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
