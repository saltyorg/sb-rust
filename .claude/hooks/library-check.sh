#!/usr/bin/env bash
# PostToolUse hook (Edit|Write): after a Rust file changes, asks for the
# library-first check while the code is fresh. Silent for other files.
set -euo pipefail

file=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty')
[[ "$file" == *.rs ]] || exit 0

msg="Library-first check (sb-rust) for ${file##*/}: for each non-domain piece you just wrote or changed (terminal, UI, input, event loop, scrolling, selection, layout, clipboard, process, I/O), name the dependency API or documented pattern it uses. If ratatui, the ratatui org's tui-widgets crates, crossterm, tokio, bollard or another dependency already provides it, switch to that now. Anything custom keeps a 'why not X' comment. Carry the outcome into the commit's Library-check: line."

jq -n --arg msg "$msg" \
  '{hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $msg}}'
