# ADR-010: The rust-skills plugin is enabled and enforced in the repository

## Status

Accepted (maintainer, 2026-11-05); the reminder on every prompt and the rule that a
skill is loaded before every task are replaced by
[ADR-016](ADR-016-lighter-process.md), part 3

## Date

2026-11-05

## Context

The repository enforces the agent-skills plugin through
`.claude/settings.json` (`enabledPlugins`) and its hooks: a SessionStart
and a UserPromptSubmit reminder, plus CLAUDE.md's task → skill table.
The rust-skills plugin (`actionbook/rust-skills`, 2.1.0) adds
Rust-specific skills: ownership, concurrency, error handling, unsafe,
anti-patterns and the CLI domain. The maintainer had it on in their own
settings, but nothing in the repository asked for it, and the code built
so far was written without it. A whole-codebase review through its
skills (2026-11-05) found real problems, among them a signal race in
cli-core, so the maintainer asked for it to be enforced like agent-skills.

Enabling a plugin in the committed settings makes it the default for
anyone who clones this public repository and trusts the folder. The
plugin brings two things beyond its skills:

- **A prompt hook** that injects a "mandatory" routing and output format
  on every prompt (a "Reasoning Chain" section, a banner).
- **An MCP server**, `actionbook`, started with
  `npx -y @actionbookdev/mcp@latest`: an unpinned third-party npm package,
  fetched and run at each session start.

## Decision

`rust-skills@rust-skills` is enabled in `.claude/settings.json` next to
agent-skills. The reminder hook names it, and CLAUDE.md has a Rust task →
rust-skills table. Its skills add Rust guidance on top of the agent-skills
workflow; they replace none of it.

- The plugin's prompt-hook format doesn't apply: CLAUDE.md says the
  agent-skills workflow and its tables decide which skills load and how
  answers are written, and that `CONSTRAINTS.md` and CLAUDE.md win over
  any rust-skills suggestion.
- The unpinned MCP server is an accepted risk. It runs on the
  maintainer's machine already (their own settings enable the plugin).
  Nothing in sb-rust's code, build or checks uses it; sb-rust's own
  dependencies stay pinned and checked by `cargo deny`.
- As for agent-skills, the plugin's marketplace is registered in each
  contributor's own settings; the repository doesn't add it
  (`extraKnownMarketplaces`), so a clone without it simply doesn't load
  the plugin.

## Alternatives considered

- **Enforce, don't enable.** The hooks and CLAUDE.md require the skills,
  and each contributor enables the plugin themselves. Not chosen by the
  maintainer.
- **Pin the MCP server.** The server is declared by the plugin, not by
  the repository; pinning it means forking the plugin. Not worth it while
  nothing here uses the server.

## Consequences

- Anyone who trusts the folder and has the marketplace gets the plugin,
  its prompt hook and its MCP server.
- Revisit if the plugin's MCP server starts being used for sb-rust work,
  or if the plugin pins it: then pin or drop it here.
