#!/usr/bin/env bash
# Injects the agent-skills enforcement rule into Claude's context.
# Usage: agent-skills-reminder.sh <SessionStart|UserPromptSubmit>
set -euo pipefail

event="${1:?hook event name required}"

if [ "$event" = "SessionStart" ]; then
  msg='MANDATORY (sb-rust): The agent-skills plugin is enforced in this repo. Before any other work, invoke the Skill tool with agent-skills:using-agent-skills. Then, for every task, invoke the matching agent-skills:* skill(s) listed in CLAUDE.md BEFORE reading code, planning, or editing, follow their steps in order including verification, and state which skill(s) you are using. If no skill fits, say so in one line. Global ~/.claude/CLAUDE.md rules take precedence over conflicting skill steps.'
else
  msg='Reminder (sb-rust): pick and invoke the matching agent-skills:* skill via the Skill tool before acting on this prompt, unless one already loaded this session still covers it. State which skill applies, or that none fits.'
fi

jq -n --arg event "$event" --arg msg "$msg" \
  '{hookSpecificOutput: {hookEventName: $event, additionalContext: $msg}}'
