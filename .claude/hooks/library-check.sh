#!/usr/bin/env bash
# PostToolUse hook (Edit|Write): after a Rust source file changes in a crate
# that holds terminal, UI, input, process or I/O code, asks for the
# library-first check while the code is fresh. Silent for every other file:
# tests, the crates that are domain logic (config, journal), and anything
# that isn't Rust (ADR-016). Prototypes stay in: they are where a
# dependency's pattern is tried first.
set -euo pipefail

file=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty')
[[ "$file" =~ (^|/)(crates/(cli-core|docker|exec|host|release|repo|sb|term-ui|tui)|prototypes/[^/]+)/src/.*\.rs$ ]] || exit 0

msg="Library-first check (sb-rust) for ${file##*/}: for each non-domain piece you just wrote or changed (terminal, UI, input, event loop, scrolling, selection, layout, clipboard, process, I/O), name the dependency API or documented pattern it uses. If ratatui, the ratatui org's tui-widgets crates, crossterm, tokio, bollard or another dependency already provides it, switch to that now. Anything custom keeps a 'why not X' comment. Carry the outcome into the commit's Library-check: line."

jq -n --arg msg "$msg" \
  '{hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $msg}}'
