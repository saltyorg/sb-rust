# sb-rust: agent instructions

## How to work (mandatory)

The `agent-skills@addy-agent-skills` plugin is enabled and enforced for this
repository (see the hooks in `.claude/settings.json`).

1. **Start every session** by invoking `agent-skills:using-agent-skills` via the
   Skill tool, before any other work.
2. **Before starting any task**, invoke the matching skill(s) from the table
   below *before* reading code, planning, or editing. Load the skill; do not
   paraphrase it from memory.
3. **Follow the loaded skill's steps in order**, including its verification
   step. A task is not done until verification passes with evidence.
4. **Switch skills when the phase changes** (implementation → tests → review →
   commit). Several skills usually apply to one task.
5. **State which skill(s) are in use** when starting a task. If no skill fits,
   say so in one line, then proceed.
6. **Surface assumptions** before non-trivial work. If unsure, stop and ask. Do
   not guess.

### Task → skill

| Situation | Skill |
|---|---|
| Ask is underspecified / unclear intent | `agent-skills:interview-me` |
| Rough idea needs refining | `agent-skills:idea-refine` |
| New project, feature, or significant change | `agent-skills:spec-driven-development` |
| No quality bar written down | `agent-skills:constraint-driven-development` |
| Have a spec, need tasks | `agent-skills:planning-and-task-breakdown` |
| Implementing code | `agent-skills:incremental-implementation` |
| Designing APIs / module boundaries / traits | `agent-skills:api-and-interface-design` |
| Library/crate usage where correctness matters | `agent-skills:source-driven-development` |
| High stakes or unfamiliar code | `agent-skills:doubt-driven-development` |
| Writing or changing logic, fixing bugs | `agent-skills:test-driven-development` |
| Something broke (build, test, runtime) | `agent-skills:debugging-and-error-recovery` |
| Reviewing a change | `agent-skills:code-review-and-quality` |
| Code works but is too complex | `agent-skills:code-simplification` |
| Untrusted input, auth, secrets, dependencies | `agent-skills:security-and-hardening` |
| Performance concerns | `agent-skills:performance-optimization` |
| Committing, branching, versioning | `agent-skills:git-workflow-and-versioning` |
| CI/CD pipelines | `agent-skills:ci-cd-and-automation` |
| Logging, metrics, tracing | `agent-skills:observability-and-instrumentation` |
| Docs and ADRs | `agent-skills:documentation-and-adrs` |
| Removing or migrating systems | `agent-skills:deprecation-and-migration` |
| Preparing a release / launch | `agent-skills:shipping-and-launch` |
| Context/rules setup, degraded output | `agent-skills:context-engineering` |

## Project artifacts

- **Specs before code:** `docs/specs/`. Tasks: `tasks/plan.md`, `tasks/todo.md`.
- **Significant decisions are ADRs** in `docs/decisions/`. Never delete an ADR;
  supersede it.
- Create these paths when first needed; they do not exist yet.

## Quality bar (never weaken silently)

- **`CONSTRAINTS.md` is the quality contract.** It does not exist yet: create it
  with `agent-skills:constraint-driven-development` before the first production
  code lands. Once it exists, read it before writing code and never weaken it to
  make a change pass.
- **Fix the code, not the lint.** `#[allow]` is forbidden. A truly justified
  exception uses `#[expect(lint, reason = "...")]`, needs a row in the
  `CONSTRAINTS.md` Exceptions table, and must be called out in review.
- **Never** lower thresholds, delete or skip tests, or relax the toolchain pin
  or lints without an ADR.

## Public repository rules

- **Never commit environment-specific details:** hostnames, IPs, domains, user
  names, machine-specific paths, credential sources or files, tokens, raw test
  output, or session logs.
- **Verified third-party behaviour** goes in `docs/platform-notes/`, with
  software versions and the method used, and no environment details.
- **Research** about other projects describes behaviours and lessons, not bug
  lists.
- **Live tests** read endpoints and credentials from environment variables only.
- Run `scripts/check-commit-messages.sh <base> HEAD` before handing back
  committed work; it enforces the Conventional Commits format.

## Running things

- **Resource limits:** cap heavy builds and every live test, for example
  `systemd-run --scope -p MemoryMax=... -p CPUQuota=...`. Run live tests against
  one server at a time.
- **Output:** keep it unbuffered so progress is visible.
- **Diagnosis:** when resources spike, measure (process list by memory,
  profiling) before attributing the cause.

## Precedence

The user's global instructions (`~/.claude/CLAUDE.md`) override any skill step
that conflicts with them, notably: Conventional Commits, Python validation
requirements, and the ban on remote mutations (push, PRs, tags, releases,
workflow runs, comments) without explicit per-action authorization. Skills that
describe pushing, opening PRs, or tagging stop at the local boundary and ask.
