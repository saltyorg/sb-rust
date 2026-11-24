# Implementation Plan: Stack prototypes

> Done. Each prototype was removed once its module was built (the
> maintainer, 2026-11-06): the findings are in `docs/prototypes/`, and the
> code, `scripts/try-prototype.sh` and `scripts/demo-log-sources.sh` are in
> git history at f5f05afc. The paths below are the prototypes' own.

## Overview

Before any module spec is locked, build eight throwaway prototypes. Each one
answers a single question about the proposed stack in
`docs/specs/capability-map.md`. Every prototype ends with a `FINDINGS.md`
verdict, and the verdicts update the capability map. Module specs come after.

## Architecture Decisions

- **Layout:** one Cargo workspace. Each prototype is a member crate under
  `prototypes/<name>/` with a `FINDINGS.md` next to its `Cargo.toml`.
- **Throwaway code:** prototype code is never promoted into modules. Modules
  are rewritten from the findings. This keeps the quality bar of real modules
  separate from spike speed.
- **Prototype bar:** builds with `cargo build`, and `cargo clippy` is clean
  under default lints. `#[allow]` stays forbidden (use
  `#[expect(..., reason)]`). No test-coverage requirement. `CONSTRAINTS.md` is
  written after the prototypes and before the first module code, as
  `CLAUDE.md` requires.
- **Real data:** prototypes may read the live Saltbox config, facts, and
  Docker/systemd at runtime, taking paths as arguments. Committed fixtures are
  synthetic only (public-repo rules).
- **Toolchain:** `rust-toolchain.toml` pins the stable release
  (1.99.1), edition 2024, with the x86_64 and aarch64 musl targets.

## FINDINGS.md format

```markdown
# Prototype: <name>
**Question:** <the one question>
**Crates tried:** <crate@version, ...>
**What was built:** <1–3 sentences>
**Result:** <observations, numbers, screenshots described in text>
**Verdict:** adopt | adopt with changes | reject → <alternative>
**Map impact:** <stack-table or module changes to make>
```

## Task List

See `tasks/prototypes/todo.md` for acceptance criteria and verification for each task.

### Phase 0: Foundation
- [x] Task 0: Workspace scaffold

### Phase 1: Highest-risk plumbing
- [x] Task 1: Static musl build
- [x] Task 2: YAML on real config
- [x] Task 3: Task runner and PTY

### Checkpoint A: plumbing verdicts reviewed

### Phase 2: Interactive screens
- [x] Task 4: Fact editor, keyboard
- [x] Task 5: Fact editor, mouse
- [x] Task 6: Log viewer streaming
- [x] Task 7: Announcements markdown

### Checkpoint B: screen verdicts reviewed

### Phase 3: Remaining surface
- [x] Task 8: Inline output kit
- [x] Task 9: Dynamic completions
- [x] Task 10: Update the capability map from the findings

### Checkpoint C: map ready for approval

### Phase 4: Follow-ups from module specs
- [x] Task 11: Own journal reader (`proto-journal`), from the host spec's
      decision 4

## Parallelization

After Task 0, Tasks 1–9 are independent of each other, apart from Task 5
building on Task 4. They can run in parallel worktrees if you want that.
Otherwise run them in the listed order, which goes riskiest first.

## Risks and Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A prototype reads real config containing secrets and output leaks into FINDINGS.md | High | Findings describe behaviour only. No raw output is pasted. |
| musl static build breaks with a chosen crate (e.g. TLS or PTY) | High | Task 1 goes first and includes reqwest, bollard and portable-pty. |
| ratatui-textarea or mouse handling is too limited for the fact editor | Med | Task 5 records exactly what is missing. A fallback is tui-input or a custom widget on ratatui primitives, without hand-rolled ANSI. |
| Prototypes drift into real implementation | Med | Each one answers its one question and stops. Code is never promoted. |

## Open Questions

- None blocking. Decisions made while prototyping go in each FINDINGS.md and
  then into the map.
- **Colour fallback:** truecolor is the default (capability map, decision 10).
  The `SB_COLOR_PROFILE` and `NO_COLOR` fallback is planned through
  `anstyle-query` and `anstyle-lossy` (or `termprofile`). It gets built, with
  an ADR, in the `term-ui`/`tui` module work, not in the prototypes.
