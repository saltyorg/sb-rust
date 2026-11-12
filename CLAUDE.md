# sb-rust: agent instructions

## How to work (mandatory)

The `agent-skills@addy-agent-skills` and `rust-skills@rust-skills` plugins
are enabled for this repository (see the hooks in `.claude/settings.json`;
rust-skills has its own table below).

1. **Start every session** by invoking `agent-skills:using-agent-skills` via the
   Skill tool, before any other work.
2. **When the phase of the work changes** (spec → plan → implementation →
   tests → debugging → review → commit → release), invoke the matching
   skill(s) from the table below. Load the skill; do not paraphrase it from
   memory. A skill loaded earlier in the session still covers its phase:
   don't load it again for the next task of the same kind (ADR-016).
3. **Follow the loaded skill's steps in order**, including its verification
   step. A task is not done until verification passes with evidence.
4. **State which skill(s) are in use** when a phase starts. If no skill fits,
   say so in one line, then proceed.
5. **Keep the process in proportion.** Time goes to sb's code and its tests.
   Work on the gate, the plans or the records needs a defect that affects
   sb or a request from the maintainer.
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

### Rust task → rust-skills skill

The `rust-skills@rust-skills` plugin is enabled too. It adds Rust-specific
guidance on top of the agent-skills workflow skill; it never replaces one.
Load one of these when a Rust question needs it:

| Situation | Skill |
|---|---|
| Unsure which, or a compile error | `rust-skills:rust-router` |
| Ownership, borrows, lifetimes | `rust-skills:m01-ownership` |
| Smart pointers, RAII, Drop | `rust-skills:m02-resource`, `rust-skills:m12-lifecycle` |
| Interior mutability, locks | `rust-skills:m03-mutability` |
| Generics, traits, dispatch | `rust-skills:m04-zero-cost` |
| Newtypes, builders, type state | `rust-skills:m05-type-driven` |
| Error types and propagation | `rust-skills:m06-error-handling`, `rust-skills:m13-domain-error` |
| Threads, async, tokio, signals | `rust-skills:m07-concurrency` |
| Domain modelling (Saltbox rules) | `rust-skills:m09-domain` |
| Performance | `rust-skills:m10-performance` |
| Crates, features, workspace | `rust-skills:m11-ecosystem` |
| Review for anti-patterns | `rust-skills:m15-anti-pattern` |
| Naming and style | `rust-skills:coding-guidelines` |
| CLI and terminal code | `rust-skills:domain-cli` |
| Any `unsafe` or FFI | `rust-skills:unsafe-checker` |
| Crate API docs | `rust-skills:docs` |

`CONSTRAINTS.md` and this file win over a rust-skills suggestion that
conflicts with them (for example `#[allow]` stays forbidden). The plugin's
own prompt hook injects a "mandatory" routing and output format (a
"Reasoning Chain" section, a banner); it doesn't apply here: the
agent-skills workflow and the tables above decide which skills load and
how answers are written.

## Library first (don't diverge from what dependencies already solve)

- Before writing glue for terminal, UI, input, event-loop, clipboard,
  process or I/O behaviour, check the dependency's official docs, recipes
  and examples (and its organisation's widget crates, e.g. ratatui's
  `tui-widgets`). Use `agent-skills:source-driven-development`.
- Follow the documented pattern and cite it in a code comment with the full
  URL. Never re-implement something a dependency already provides (event
  loops, input editing, clipboard, popups, widgets, ANSI handling).
- Diverging from the documented approach needs evidence: a measurement or a
  missing feature. Record it in a code comment ("why not X") and in the
  prototype's FINDINGS.md or an ADR.
- A pattern with no official source is marked `UNVERIFIED` in the code,
  with what it was checked against.
- Prefer crates that are actively maintained and widely used. A
  low-adoption crate needs a stated reason.
- Domain logic (Saltbox schema rules, file formats with Saltbox-specific
  constraints) is ours to write, but still goes through a library wherever
  one meets the constraint.
- **Enforcement:**
  - After an edit to a source file of a crate with terminal, UI, input,
    process or I/O code (`cli-core`, `docker`, `exec`, `host`, `sb`,
    `term-ui`) or of a prototype, a hook (`.claude/hooks/library-check.sh`)
    asks for this check. Answer it then, not at commit time. A new crate
    of that kind is added to the hook's list.
  - Before committing changes to terminal, UI, input, event-loop,
    clipboard, process or I/O code, run the `library-reviewer` agent
    (`.claude/agents/library-reviewer.md`) on the diff. Give it the range,
    not your reasoning. Fix REPLACE findings or record why not.
  - Every commit that changes Rust source carries a `Library-check:` line
    in its body: what is library-backed, what stayed custom and why, and
    whether the reviewer ran. `scripts/check-commit-messages.sh` rejects
    commits without it.

## Fresh-context review, by tier

A second pair of eyes catches what the author missed. The review is
sized to the change (ADR-016):

| What the change touches | Review |
|---|---|
| Only `docs/` and `tasks/` | None for the commit; read at the next checkpoint, or by the maintainer when a spec or ADR is approved. No `Review:` line. |
| Rust, the gate (`scripts/`, `CONSTRAINTS.md`, `CLAUDE.md`, `.claude/`, `.github/`, `deny.toml`, manifests), anything else | One `agent-skills:code-reviewer` run for the task, on its whole diff, in parallel with `scripts/check.sh full`. |
| Terminal, UI, input, event-loop, clipboard, process or I/O code | The `library-reviewer` as well. |
| Untrusted input, privileges, secrets, dependencies | `agent-skills:security-auditor` as well, when the spec or plan asks for it. |

- **One pass, one model.** A second pass on another model is for a
  checkpoint or the maintainer's request. Use
  `agent-skills:code-review-and-quality` when you review yourself or weigh
  the findings.
- **Reviewers also check Microsoft's Pragmatic Rust Guidelines**
  (https://microsoft.github.io/rust-guidelines/agents/all.txt). A
  `SubagentStart` hook (`.claude/hooks/reviewer-guidelines.sh`) caches it
  under `target/agent-guides/` and points `agent-skills:code-reviewer`,
  `agent-skills:security-auditor` and `library-reviewer` at it; it is not
  given to the authoring agent. Findings cite the guideline id (`M-...`);
  this file, `CONSTRAINTS.md` and the specs win where they conflict.
- **Brief it with the range and the goal, not your conclusions.** Tell it
  to edit nothing, to run sb only with `SB_LOG_DIR` in the scratchpad, and
  to report findings with file:line.
- **Weigh every finding against how sb runs on Saltbox.** Fix the ones
  that can happen in real use (test-first when it changes behaviour, per
  `agent-skills:test-driven-development`); decline the rest in the commit
  body with the reason. A finding doesn't become a plan of its own.
- **After fixing, re-run `scripts/check.sh full`.** Review the fix again
  only when it changes behaviour beyond what the finding spelled out.
- **A commit that changes a file outside `docs/` and `tasks/` carries a
  `Review:` line** in its body: which reviewers ran, on what diff (a
  commit range, or the working tree against a base), and what was fixed
  or declined with reasons. `scripts/check-commit-messages.sh` rejects
  such a commit without it. Keep it to what a reader needs: a few lines.

## Project artifacts

- **Specs before code:** `docs/specs/`. Tasks: `tasks/plan.md`, `tasks/todo.md`.
- **Significant decisions are ADRs** in `docs/decisions/`. Never delete an ADR;
  supersede it.
- Create these paths when first needed; they do not exist yet.

## Quality bar (never weaken silently)

- **`CONSTRAINTS.md` is the quality contract.** Read CONSTRAINTS.md before
  writing code. Do not weaken it to make a change pass. Run
  `scripts/check.sh fast` after edits, `task` before handing work back and
  `full` before committing. A change under `docs/` and `tasks/` only ends
  `task` and `full` after the fast checks (ADR-016).
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
- **Never run the `sb` binary as root without `SB_LOG_DIR`** pointing into
  `target/` or the session scratchpad: a root run logs to the host's
  `/var/log/sb`. This applies
  to subagents too: say it in every brief that may run sb. Tests and
  `scripts/check.sh` set it already.

## Precedence

The user's global instructions (`~/.claude/CLAUDE.md`) override any skill step
that conflicts with them, notably: Conventional Commits, Python validation
requirements, and the ban on remote mutations (push, PRs, tags, releases,
workflow runs, comments) without explicit per-action authorization. Skills that
describe pushing, opening PRs, or tagging stop at the local boundary and ask.
