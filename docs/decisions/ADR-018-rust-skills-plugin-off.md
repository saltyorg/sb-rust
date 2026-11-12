# ADR-018: The rust-skills plugin is switched off in the repository

## Status

Accepted (maintainer, 2026-11-12). Supersedes
[ADR-010](ADR-010-rust-skills-plugin.md).

## Date

2026-11-12

## Context

ADR-010 enabled `rust-skills@rust-skills` in `.claude/settings.json` and
accepted two things that come with it: a prompt hook that injects about
a hundred lines of "mandatory" routing and output format on most
prompts, which CLAUDE.md told the agent not to follow, and an unpinned
MCP server fetched at each session start.

Of the 53 skill invocations ADR-016 counted over the history's 294
commits, 3 were its skills. The hook's text was in the context of
nearly every prompt. Claude Code has no
setting that turns off one hook of an enabled plugin
(https://code.claude.com/docs/en/hooks.md: "There is no way to disable
an individual hook while keeping it in the configuration").

## Decision

`.claude/settings.json` sets `"rust-skills@rust-skills": false`, which
also overrides a contributor's user-level setting for this repository
(a local settings file can still switch it on).
CLAUDE.md's rust-skills table and the reminder hook's mention of it are
removed. Rust questions go to the standard library's and each crate's
documentation (`agent-skills:source-driven-development`), and the
reviewers keep Microsoft's Pragmatic Rust Guidelines.

## Alternatives considered

### Keep the plugin and its hook
- Pros: the skills stay one call away.
- Cons: the injected text on every prompt, for 3 uses.
- Rejected.

### Copy the few skills worth having into `.claude/skills/`
- Pros: no hook, no MCP server, the skills stay.
- Cons: copies that no longer follow upstream, for skills used 3 times.
- Rejected; it can be done later for a skill that is missed.

### A fork of the plugin without the hook
- Pros: everything else stays.
- Cons: a fork to keep in step with upstream.
- Rejected.

## Consequences

- No injected routing text, and the plugin's MCP server no longer
  starts for this repository.
- The whole-codebase review through its skills (2026-11-05) found real
  problems; a review of that kind can switch the plugin on for its own
  session.
