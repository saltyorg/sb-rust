# Implementation Plan: `cli-core`

Spec: [docs/specs/cli-core.md](../docs/specs/cli-core.md) (approved
2026-10-29, revised 2026-10-30). Quality bar: [CONSTRAINTS.md](../CONSTRAINTS.md).
Decisions: [ADR-001](../docs/decisions/ADR-001-unstable-feature-pins.md),
[ADR-002](../docs/decisions/ADR-002-colour-fallback.md),
[ADR-003](../docs/decisions/ADR-003-cli-core-ratchet-update.md),
[ADR-004](../docs/decisions/ADR-004-yaml-stack-binary-size.md). The prototype plan,
with its open items, is in [tasks/prototypes/](prototypes/plan.md).

## Overview

Build the frame every sb-rust command runs in: the `crates/` workspace, the
`sb` binary with `sb version`, and in `sb-cli-core` the startup steps (host
check hook, argument parsing, root relaunch only for commands that need it,
logging, colour, theme), cancellation, exit codes and error presentation.
When this is done, the first vertical slice's other commands
(`sb docker ps`, `sb validate-config`) need no startup code of their own.

## Architecture decisions

- **Crates:** `crates/cli-core` (package `sb-cli-core`, a library) and
  `crates/sb` (package `sb`, the binary). One crate per map module, named
  `sb-<module-id>`. cargo-deny's `bans.deny` + `wrappers` enforce the map's
  edges from the first crate (CONSTRAINTS.md, Architecture).
- **Host check without a dependency cycle:** `cli-core` takes the host
  support check as a startup hook. A small `crates/host` (package
  `sb-host`) is started now with only the OS release check (it needs
  neither `exec` nor `term-ui` yet); the rest of `host` comes in its own
  module spec. `sb` passes `sb_host::check_support` to `cli-core`.
- **Root decision per command:** each command declares whether it needs
  root; the default is yes. `--help`, `version` and completion don't.
  Every cli-core command is unprivileged, so the relaunch is tested through
  its decision and exec functions here and end to end with the first root
  command (`sb docker ps`).
- **Command tree grows with the modules:** cli-core adds only `version`
  and the hidden `help`. The contract test checks that every sb-rust
  command and flag exists in sb-go with the same name; full equality is the
  `ops` module's done criterion.
- **Palette in cli-core:** the role-named palette (from proto-theme) moves
  into `sb-cli-core::theme`, resolved once at startup for the colour
  profile (ADR-002) and the theme config. Screens and inline output take
  colours from it.
- **Libraries are confirmed at source in their task** (CLAUDE.md, library
  first) and cited in code: clap, colorchoice-clap, tokio and
  `tokio-util`'s `CancellationToken`, tracing + tracing-subscriber +
  tracing-appender, rustix, `toml` + serde, the YAML parser proto-yaml
  chose for the inventory, anstyle-lossy, ratatui-core's `anstyle`
  feature. Each new crate passes cargo-deny before it lands.
- **Testability:** each startup step is a function taking its inputs
  (environment, paths, euid) rather than reading globals, so unit tests
  need no root, no real `/var/log/sb` and no real `/opt/sb`. Tests that
  need root run only when `SB_TEST_ROOT=1`.

## Task list

Details, acceptance criteria and verification: [tasks/todo.md](todo.md).

### Phase 1: Foundation
- [x] Task 1: Workspace, `sb` binary and `sb version`
- [x] Task 2: Exit codes and the error block
- [x] Task 3: Cancellation on SIGINT and SIGTERM

### Checkpoint A: Foundation

### Phase 2: Startup
- [x] Task 4: Host support check hook (`sb-host`)
- [x] Task 5: Root relaunch for commands that need it
- [x] Task 6: Run log on the host

### Checkpoint B: Startup

### Phase 3: Colour and theme
- [x] Task 7: `--color` on every output path, including help
- [x] Task 8: Colour profiles (ADR-002)
- [x] Task 9: Theme config

### Checkpoint C: Complete
- [ ] Task 10: Startup wiring, contract test and success criteria
- [ ] Task 11: Help in sb-go's format (maintainer, Checkpoint C)
- [ ] Task 12: Parse errors in sb-go's layout

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| The 10 ms startup budget breaks once tokio, config and YAML reads land | Med | Measure from Task 1 (`check.sh task`); read the inventory and theme only when needed for the run, and build the tokio runtime after argument parsing |
| Signal tests interfere with the test harness | Med | One signal test per integration-test file (each is its own process); PTY smoke test with rexpect against the built binary |
| Root relaunch can't run end to end in cli-core (no root command yet) | Low | Unit-test the decision and exec construction with a fake `sudo` on `PATH`; end-to-end with `sb docker ps` |
| Exact pins (ADR-001) clash with other crates' requirements | Low | cargo-deny duplicates warn; resolve when it happens, recorded in the pin's commit |
| `/opt/sb` and `/var/log/sb` touched on this host by tests | High | Paths are injected; default-path tests run only with `SB_TEST_ROOT=1`, never by default (spec boundaries) |

## Open questions

None. Library choices are confirmed in their tasks; anything that changes
the spec goes back to the maintainer first.
