# Implementation Plan: `docker` — start, stop and restart (approved 2026-11-06)

Spec: [docs/specs/docker.md](../../docs/specs/docker.md), "Start, stop and
restart" (approved 2026-11-06, decisions 5-8). Controller facts:
[docs/platform-notes/docker-controller.md](../../docs/platform-notes/docker-controller.md).
Quality bar: [CONSTRAINTS.md](../../CONSTRAINTS.md). Earlier plans:
[tasks/exec/](../exec/plan.md) (done; its review backlog is carried in
[tasks/docker-control/todo.md](todo.md)), [tasks/config/](../config/plan.md),
[tasks/docker/](../docker/plan.md) (`ps`), [tasks/term-ui/](../term-ui/plan.md),
[tasks/cli-core/](../cli-core/plan.md), [tasks/prototypes/](../prototypes/plan.md).

## Overview

`sb docker start`, `stop [--ignore]` and `restart [--ignore]`: a service
check (the unit file, then `systemctl is-active` through exec), a job
requested from the Saltbox Docker controller over loopback HTTP, and its
status polled to the end, as term-ui steps with sb-go's labels and texts.
exec enters sb's binary here (its first user) and reqwest enters
sb-docker. Nothing in this plan ever talks to a real controller: every
test uses a fake one.

## Architecture decisions

- **Risk first: the HTTP client.** Task 1 builds the client and the job
  request against a fake controller first, because the spec's HTTP rules
  are where sb would silently differ from sb-go: reqwest proxies loopback
  where Go doesn't, follows redirects unless told not to, and reads whole
  bodies. Library-backed: reqwest's `no_proxy`, `redirect::Policy::none`
  and `RequestBuilder::timeout`; `Url::path_segments_mut` (with
  `pop_if_empty`, or a bare base gives `//start`) and `query_pairs_mut`
  for the URL; serde_json's `StreamDeserializer` for the first JSON value
  (`from_slice` refuses trailing data); clap's `value_delimiter(',')` for
  `--ignore`; the 1 MiB cap read chunk by chunk as
  config's `http::body` does (a few lines repeated: sb-docker can't
  depend on sb-config, and a shared HTTP crate for two callers isn't
  earning its keep yet; recorded in the code).
- **`ControlError`, typed** (the review backlog's "typed errors instead
  of anyhow in the public API (decide before start/stop/restart)"): one
  variant per failure in the spec, sb-go's texts as `Display` (thiserror),
  so tests match variants and the screen shows sb-go's words. A reqwest or
  serde_json cause is in the `Display` text only, not also a `#[source]`
  (term-ui's block shows the whole chain, so both would print it twice).
  Interrupted is `ControlError::Interrupted` (amended in Task 3): the
  service check takes the step's token, so exec stops `systemctl`
  gracefully (dropping its future would SIGKILL the group), and returns it
  when exec reports the run interrupted; the request and the wait run
  under `run_until_cancelled`, and on `None` the step adds decision 5's
  notice and returns it too. The runner decides the ending from the
  shutdown, never from the variant; only a request's or wait's `None`
  gives the notice, never a check's `Interrupted`.
  `ps`'s `anyhow` API stays as it is (its own change, still in the
  backlog). Fields stay private, set through `saltbox()` and builder
  setters for tests, so reqwest's and url's types don't enter
  sb-docker's API (M-DONT-LEAK-TYPES); `saltbox()` returns a `Result`
  (a client that can't be built fails before the runner, as
  validate-config's).
- **The controller's values are fields** (`Controller::saltbox()` gives
  sb-go's), so every timeout and the poll count is tested with short
  values and pinned once. One addition to the spec's shape: a
  `systemctl: PathBuf` field (`systemctl`, found on `PATH`, in
  `saltbox()`), so the service-check unit tests point at a stand-in
  without changing `PATH`, which is process-wide; the end-to-end tests
  still put a stand-in first on `PATH`, as the spec says.
- **Modules:** `control/{client,service,commands}.rs` from Task 1, and the
  fake controller in `testing/controller.rs` (testing.rs is already 754
  lines).
- **exec's forced-exit hook** (exec's `on_forced_exit`, which the binary
  must register once with its `Shutdown`) is registered here, the first
  time sb uses exec: deny.toml lets only sb-docker wrap sb-exec, so
  sb-docker re-exports the registration for sb's `main.rs` (Task 5).
- **Test safety:** the two overrides go into sb's shared test environment
  (`tests/common`'s `apply_sb_env`, enforced by `tests/isolation.rs`): a
  refused loopback URL and a unit file that doesn't exist, so no sb test
  can reach the real controller even if it sets nothing.
- **The fake controller** joins sb-docker's `testing` feature beside the
  fake Engine API: axum on a loopback TCP listener on a free port,
  answering from a script of responses and recording what it was sent
  (method, path, decoded query, headers).
- **Commands as one library function per command** (`control::start`,
  `stop`, `restart`) driving term-ui steps, tested on term-ui's
  in-memory runner with the fake controller, so sb's `main.rs` only
  parses, builds the controller and starts the runner (as
  validate-config).
- **Interrupts:** each step body runs its request or sleep under the
  step's token (`run_until_cancelled`, as config's checks); an
  interrupted request or wait adds decision 5's notice before ending.
  exec's capture already stops `systemctl` on the token.

## Task list

See [tasks/docker-control/todo.md](todo.md) for each task's acceptance criteria and
verification.

### Phase 1: The controller client

- [x] Task 1: `control.rs`'s client and the job request, on a fake
  controller
- [x] Task 2: the wait
- [x] Task 3: the service check

### Checkpoint A: the client is done

### Phase 2: The commands

- [x] Task 4: start, stop and restart as term-ui steps
- [x] Task 5: `sb docker start/stop/restart` in sb, end to end
- [x] Task 6: size, coverage and the spec's measurements (risk first:
  before the signal tests)
- [x] Task 7: signals end to end

### Checkpoint B: complete

Finished: the maintainer's by-hand runs passed and the plan closed on
2026-11-06 (c4408f8); [tasks/todo.md](../todo.md) carries its
follow-ups.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| exec's dependencies (portable-pty, rustix's process features) grow sb past the 5% size tolerance | Med | Task 7 measures it step by step; an ADR records the new value for the maintainer, as ADR-009 did for bollard |
| reqwest's defaults differ from Go's (proxy, redirects, body size) | High | Task 1 tests each with the environment and answers that would expose it (proxy variables set, a 307, a body over 1 MiB) |
| Tests sleeping through sb-go's real timeouts (5 s check, 10 s request, 5 min wait) make the suite slow | Med | Every value a `Controller` field; unit tests use milliseconds; only `saltbox()`'s values are pinned |
| A test reaching the real controller or unit file (they would start and stop the host's containers) | High | Unit tests never use `saltbox()`'s URL or unit file; `apply_sb_env` sets both overrides for every sb test; the fake sudo never runs sb; review briefs say so |
| The run log or the screen printing a controller's unexpected text unescaped | Low | term-ui's drawing and the run log's subscriber clean shown text; a job ID or status shown in an error goes through cli-core's `escape_controls` |
| sb-go's real timeouts in end-to-end runs (5 s check, 10 s request, 5 min wait) | Med | The timeouts and the poll limit are tested in unit tests only; end-to-end failures are answers that come at once |

## Open questions

None: the spec's were decided (decisions 5-9; 9: sb follows sb-go against today's controller, the controller's improvements are its Rust rewrite's).
