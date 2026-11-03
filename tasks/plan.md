# Implementation Plan: `config` — `sb validate-config` (approved 2026-11-03)

Spec: [docs/specs/config.md](../docs/specs/config.md) (approved
2026-11-03, decisions 1-6; 7-8 at Checkpoint A). Quality bar:
[CONSTRAINTS.md](../CONSTRAINTS.md).
Earlier plans: [tasks/docker/](docker/plan.md), [tasks/term-ui/](term-ui/plan.md)
and [tasks/cli-core/](cli-core/plan.md) (done; their open items are
carried in [tasks/todo.md](todo.md)), [tasks/prototypes/](prototypes/plan.md).

## Overview

Build `sb-config` with the validation half of the config module and wire
`sb validate-config` into sb: strict schema loading (serde-saphyr),
position-carrying config parsing with a duplicate-key pass (saphyr), sb-go's
rules and validators with every issue collected and sorted, the Cloudflare
and Docker Hub checks (reqwest), and the command as term-ui's first runner
command, which takes term-ui's carried criteria (the failure block and the
Ctrl-C endings in sb's own commands). term-ui gains a group mode that lets
every sibling finish (decision 3).

## Architecture decisions

- **Evidence first (risk first).** Task 1 pins Saltbox's schemas from the
  public upstream repository and checks what they use (rule keys,
  validators, `pattern` dialect and anchoring, `additionalProperties`
  forms, `validate_rclone_remote`), checks the new dependency trees
  against deny.toml, and picks the public-suffix and tz crates and the
  scalar-resolution approach. The maintainer reviews at Checkpoint A.
- **Pure core, thin I/O.** Schema loading, parsing, rules and sync
  validators are pure functions over fixtures (Tasks 4-7). The credential
  checks are a thin HTTP layer tested against a local fake (Tasks 8-9);
  the run over the file table is sb-config's (Task 10).
- **One crate, used only by sb.** `crates/config` (`sb-config`) depends on
  `sb-cli-core` and `sb-term-ui`; deny.toml's wrappers follow (map
  correction already made).
- **Fakes before the command.** Task 8 builds the HTTP fake (behind an
  `sb-config` `testing` feature, as sb-docker's) and Task 11 adds the basic
  end-to-end cases with the command, so every commit passes the coverage
  and mutation gates; Task 12 adds the rest.
- **The size ADR with the growth.** The new dependencies enter sb's binary
  when Task 11 links `sb-config`; that commit carries the ADR, attributing
  each. cargo-deny's gate reads sb's graph, so Tasks 4, 7, 8 and 9 run it
  on sb-config's own manifest until then.
- **Never the live checkout.** Every test and review uses fixtures through
  `SB_SALTBOX_DIR`; the real schemas come from the public upstream
  repository at a pinned commit.

## Task list

### Phase 1: Evidence and groundwork

- [x] Task 1: Schemas, dependencies, the musl build and crate choices
- [x] Task 2: term-ui: a group that lets every sibling finish
- [x] Task 3: One Saltbox root: `SB_SALTBOX_DIR`

### Checkpoint A: Evidence

- [x] Findings reviewed and the musl route picked by the maintainer
      (`cross`, ADR-008; timezones as Go); term-ui's amendment approved

### Added at Checkpoint A

- [x] Task 3a: term-ui: nested siblings drawn in tree order (before
      Task 4, the maintainer's call; nothing in Phase 2 depends on it)

### Phase 2: Validation

- [ ] Task 4: Crate and strict schema loading
- [ ] Task 5: Config parsing: positions, duplicate keys, scalars
- [ ] Task 6: The rules engine
- [ ] Task 7: The sync validators and secrets
- [ ] Task 8: The HTTP fake, the client and Docker Hub
- [ ] Task 9: The Cloudflare check
- [ ] Task 10: The validation run in sb-config
- [ ] Task 11: `sb validate-config` in sb (command, runner, basic
      end-to-end, size ADR)

### Checkpoint B: The command works

- [ ] `scripts/check.sh full`; the maintainer runs a release build on a
      real host's config

### Phase 3: Proof

- [ ] Task 12: End-to-end: the runner's look, failures, signals, the log
- [ ] Task 13: Success criteria, parity and measurements

### Checkpoint C: Complete

- [ ] All tasks done; spec success criteria met or carried by name;
      `scripts/check.sh full`

Details, acceptance criteria and verification: [tasks/todo.md](todo.md).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| A pinned schema uses something sb-go accepts loosely (a key, a pattern dialect sb can't compile) | High | Task 1 checks before code; the spec is amended at Checkpoint A if needed |
| yaml.v3's scalar resolution is hard to match from saphyr | Med | Task 1 confirms saphyr keeps the source text; Task 5 resolves scalars from it, a test at each edge |
| check.sh's plain musl build fails once aws-lc-sys needs a musl C compiler (proto-musl) | High | Task 1 confirmed it; the maintainer picked `cross` (ADR-008), which Task 11 puts into check.sh |
| reqwest, rustls, serde-saphyr, regex and the public-suffix list grow the binary a lot | Med | Task 1 measures each; Task 11's ADR attributes each; still far below sb-go |
| A developer's proxy carries test traffic (and test tokens) to a real proxy | Med | Unit clients built with proxies off; `common::sb()` removes the proxy variables |
| Cloudflare's retry and timeout behaviour makes tests slow or flaky | Med | Timeouts and backoff injectable; the fake answers or holds on demand |
| A term-ui API change breaks the runner's reviewed semantics | Med | Task 2 adds a mode beside the existing group; the existing tests stay unchanged |
| A test reads the real Saltbox checkout or calls the real services | High | `SB_SALTBOX_DIR`, `CLOUDFLARE_BASE_URL` and `SB_DOCKERHUB_URL` set by every test helper; no default reaches a real path or service in tests |

## Open questions

None (Checkpoint A answered Task 1's).
