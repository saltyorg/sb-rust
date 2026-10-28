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

  facts      Fact editor (keyboard + mouse) on a copy of synthetic facts
  logs       Log viewer on a demo container and demo systemd unit (removed on exit)
  markdown   Announcements renderer (full-screen view, then inline, then termimad)
  runner     Task runner with spinners, live output and a failing step
  inline     docker ps style table, then username/password prompts
  complete   Shell with dynamic tag completion loaded (bash)

Each run builds the prototype first. Nothing touches real Saltbox files.
EOF
}

bin() {
  command -v cargo >/dev/null || { echo "cargo not found on PATH" >&2; exit 1; }
  cargo build -q -p "proto-$1"
  printf '%s/target/debug/proto-%s' "$root" "$1"
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
  facts)
    b="$(bin facts)"
    data="$(mktemp -d)"
    trap 'rm -rf "$data"' EXIT
    cp prototypes/proto-facts/fixtures/*.ini "$data/"
    intro "Fact editor (synthetic facts in a temp copy)" \
      "Look at: tree + inspector layout, masked secrets, the edit popup, the review popup." \
      "Keys:  ↑/↓ move   →/Enter expand   Enter on a fact edits (Enter saves, Alt+Enter newline, Esc cancels)" \
      "       r reveal secrets   d delete   u undo   a review + apply   q quit" \
      "Mouse: click selects, click again expands/edits, right-click a fact for actions, wheel scrolls." \
      "       Try Shift+drag to check native text selection still works." \
      "Resize the window and try a narrow one (<96 columns stacks the panes)."
    "$b" "$data"
    printf '\nChanges were written to the temp copy only. Diff vs the fixtures:\n'
    diff -r prototypes/proto-facts/fixtures "$data" || true
    ;;
  logs)
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
    intro "Log viewer (demo container + demo systemd unit, removed on exit)" \
      "Look at: the picker, streaming while following, pausing, search highlighting, switching source." \
      "Picker: ↑/↓ move   Enter open   q quit" \
      "Viewer: f follow on/off   ↑/↓ PgUp/PgDn scroll   / search (try: needle)   n/N next/prev match" \
      "        Esc back to picker (stops the stream)   q quit" \
      "Afterwards, try the inline design:  $b --inline $name   (Ctrl-C stops)" \
      "To see your real containers/services instead, run: $b"
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
