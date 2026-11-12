#!/usr/bin/env bash
# SessionStart hook: injects the agent-skills and rust-skills rule into
# Claude's context once a session. There is no reminder on every prompt:
# a skill is loaded when the phase of the work changes (ADR-016).
set -euo pipefail

msg='sb-rust: the agent-skills and rust-skills plugins are enabled for this repository. Start the session by invoking the Skill tool with agent-skills:using-agent-skills. After that, load the matching agent-skills:* skill from the CLAUDE.md table when the phase of the work changes (spec, plan, implementation, tests, debugging, review, commit, release), and the matching rust-skills:* skill when a Rust question needs it (rust-skills:rust-router when unsure which). A skill loaded earlier in the session still covers its phase; say which skills are in use when a phase starts, or that none fits. Global ~/.claude/CLAUDE.md rules take precedence over conflicting skill steps.'

jq -n --arg msg "$msg" \
  '{hookSpecificOutput: {hookEventName: "SessionStart", additionalContext: $msg}}'
