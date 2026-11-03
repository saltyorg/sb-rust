# Spec: `docker` (approved 2026-11-03)

Module of the approved capability map (`docker`). The first vertical slice
is `sb version` (done) → `sb docker ps` → `sb validate-config`, built as
`cli-core` → `term-ui` (both done) → `docker` + `config`. This spec covers
the module's shape and builds `sb docker ps`; `start`, `stop`, `restart`,
the container list for the log picker and the log streams are described
so the boundaries are right, and are built later (see Scope).

## Assumptions

1. **`sb docker ps` is sb-go's command with the reviewed changes.** The
   columns, layout, Traefik rule and binding format are sb-go's
   (`cmd/dockerPs.go`); proto-inline's review (maintainer, 2026-10-29)
   kept them and changed: status from an exit-code rule, parallel
   inspect, sorted cell contents, and the theme's colours. Its look is
   term-ui's reviewed table (Checkpoint B). Every other difference from
   sb-go is listed under "Differences from sb-go".
2. **bollard is the Engine API client** (capability map stack table;
   proto-musl built it for musl, proto-logs and proto-inline used it). It
   speaks to the daemon's socket; no `docker` binary is run.
3. **`ps` needs root,** as every sb command but `version` and completion
   (cli-core decision 3). It is the first root-needing command, so
   cli-core's relaunch path first runs end to end here (carried from
   cli-core Task 5, whose two equivalent mutants become catchable).
4. **No runner for `ps`** (decision 1): sb-go prints only the table;
   listing and inspecting take milliseconds (proto-inline: 9 ms).
5. **The rest of the module is not bollard alone.** sb-go's `start`,
   `stop` and `restart` check that the controller's unit file exists, then
   `systemctl is-active saltbox_managed_docker_controller.service` (5 s
   timeout), and call the Saltbox Docker controller's HTTP API on the
   loopback address sb-go's `layout.DockerControllerAPIURL` names (a job,
   then polling `job_status/<id>` every 5 s, at most 60 times; 10 s HTTP
   timeout, 1 MiB response cap). `logs` streams through bollard into the
   tui log viewer. The capability map's `docker` row is corrected
   accordingly (term-ui decision 6), below.

## Objective

`sb docker ps` lists every container (running or not) with its status, the
port Traefik routes to and its host port bindings, in sb's table look, so
a Saltbox user sees at a glance what runs, what is broken and what is
exposed. It matches sb-go's output for the same containers except where
listed, and it is the first root-needing, Docker-talking command, so it
proves the relaunch path, the async runtime, bollard on musl and the
logging levels end to end.

### `sb docker ps` behaviour

- **Arguments:** none (sb-go: `cobra.NoArgs`); one flag, `--json`
  (decision 2), a third deliberate extension of sb-go's command tree:
  added to the contract test's `EXTENSIONS` and to cli-core's spec
  ("Command tree"). `sb docker`'s and `sb docker ps`'s short and long help
  texts are sb-go's ("Manage Docker
  containers managed by Saltbox"; "List Docker containers with port
  mappings" and its long text); the contract test against sb-go's command
  tree checks the short texts (it compares `about` only), a snapshot test
  the long ones.
- **`sb docker` alone:** its help on stdout, exit 0, as sb-go (sb's
  existing handling of clap's missing-subcommand error already does this;
  a test pins it). With a global flag but no subcommand (`sb docker
  --color=never`) clap reports a missing subcommand, exit 2, as bare `sb
  --color=never` does today (a difference, below).
- **Daemon:** `DOCKER_HOST` with `unix://`, `tcp://` or `http://` is
  followed; unset or empty means bollard's default socket (as sb-go,
  which treats empty as unset). sb reads the variable itself (bollard's
  `connect_with_defaults()` would fail on an empty one), checks it, and
  connects with `Docker::connect_with_host`. bollard's default features
  (`http`, `pipe`; no TLS) are kept. `https://`, `ssh://`,
  `DOCKER_TLS_VERIFY` or `DOCKER_CERT_PATH` stop with an error naming the
  unsupported setting: without TLS compiled in, bollard would otherwise
  connect `tcp://` in plain text where sb-go uses TLS.
- **API version:** bollard 0.21.1 sends every request without a version
  prefix (its `Url::join` replaces the `/v1.53` it formats), which the
  daemon serves at its own current API version, so daemons older and newer
  than bollard's 1.53 both work; a fixed floor version wouldn't (Docker 29
  refuses anything below 1.40 by default). A newer daemon answers in its
  own, newer shape; bollard's models are mostly optional fields, which
  tolerate that. sb calls bollard's `version()` once after connecting and
  logs the daemon's `Version` and `ApiVersion` (bollard's
  `negotiate_version()` is not used: it discards the response and changes
  nothing on the wire). A failed `version()` call (unreachable, a 4xx or 5xx
  answer) ends the run as an unreachable daemon does: error block, exit 1. A
  good answer without `Version` or `ApiVersion` (both optional in bollard's
  model) logs them as unknown and the run goes on. `DOCKER_API_VERSION` is
  not read: with this bollard a pinned version never reaches the daemon (a
  difference, below). Requests keep bollard's 120 s timeout (sb-go has
  none). Evidence: docs/platform-notes/docker-engine-api.md (Checkpoint A).
- **Guarding that behaviour:** bollard is pinned to exactly 0.21.1
  (`=0.21.1`), since a compatible release could start prefixing versions;
  the fake Engine API records the paths it is sent and a test asserts
  they are unversioned, its failure message pointing here. A bollard
  upgrade re-checks what paths it sends and how a version would be
  chosen; if it prefixes them, sb chooses the version itself: the
  daemon's `ApiVersion` from an unversioned `/version` (or the
  `Api-Version` header every answer carries), capped at bollard's, passed
  as the `ClientVersion` to `connect_with_unix` / `connect_with_http`
  (`connect_with_host` takes none). A daemon whose minimum is above
  bollard's version needs a bollard upgrade either way.
- **Docker removing unversioned paths** (deprecated in moby's API
  description, "will be removed in a future release") would break this
  bollard against every daemon: each request would get the daemon's 4xx,
  shown in cli-core's error block, exit 1. The response then is a bollard
  release that prefixes versions or, until one exists, bollard's documented
  `Docker::with_request_modifier` adding the prefix. Without unversioned
  paths the daemon's version can't come from an unversioned `/version`; it
  would come from the `Api-Version` header, which the daemon sets on every
  answer including the 400 for a too-new version (whose message also names
  the maximum). Which of the two is read is decided then, with evidence. A
  fake Engine API variant that refuses unversioned paths shows that failure
  mode in a test.
- **Rows:** every container from the list endpoint (`all=true`), each
  inspected by ID (as sb-go) for state, health, exit code, labels and
  bindings, at most 16 at a time (`futures-util`'s `buffer_unordered`,
  already in the tree through bollard); sorted by name.
- **Columns** (headings centred and bold, port columns right-aligned):

  | Column | Content |
  |---|---|
  | Container | the first name without its leading `/`; with no name, the short ID (12 characters, `unknown` when empty), as sb-go |
  | Status | the status rule below |
  | Traefik Port | values of labels containing `traefik.http.services` and `.loadbalancer.server.port`, not containing `-http`, with more than three dot-separated parts (sb-go's rule); empty values dropped, deduplicated, numeric values in numeric order then any others in text order, joined by `, ` |
  | Port Bindings | one per line, `ip:host->port/proto`, host and container ports padded to one width, so the arrows line up in the right-aligned column whatever the address (when the table wraps a narrow column, wrapped lines may not); an unset IP shown as `0.0.0.0`, every IPv6 address in brackets (`[::]`, `[::1]`); bindings without a host port left out; ordered by container port number, then protocol, then IPv4 before IPv6, then address (numerically), then host port |

- **Status rule** (bollard's state and health enums, so every state is
  handled; the text alone tells the states apart):

  | State | Text | Role |
  |---|---|---|
  | running, healthy | `running (healthy)` | ok |
  | running, starting | `running (starting)` | warn |
  | running, unhealthy | `running (unhealthy)` | problem |
  | running, no health check (none, empty or absent) | `running` | ok |
  | exited, out of memory | `exited (out of memory)` | problem |
  | exited, code not 0 | `exited (code N)` | problem |
  | created with a start error (`State.Error` set) | `created (failed to start)` | problem |
  | exited with 0, created, paused | the state | idle |
  | restarting, dead | the state | problem |
  | removing, stopping | the state | warn |
  | empty or absent | `unknown` | warn |

  The failed-start row follows moby's `daemon/start.go`: a failed first
  start leaves the container `created` with `State.Error` set and exit
  code 128 unless a more specific one was set (platform note).
  bollard's state and health enums have no catch-all, in the list's
  models as in the inspect's: a state or health word it doesn't know
  fails the whole list call (no table, the list-failure error, exit 1)
  or, met only in an inspect, that container's inspect (an inspect
  failure). Docker's words are a fixed set that changes with its API;
  a new one means a bollard upgrade.
- **Table:** term-ui's `Table` (rounded, a rule between rows, border in
  the Border role, maximum column width 100 as sb-go), fitted to the
  terminal, natural width and plain when piped (`auto`); `--color`,
  `NO_COLOR` and `SB_COLOR_PROFILE` as everywhere. No containers: the
  headings alone, as sb-go.
- **`--json`** (decision 2), written with serde_json as `sb version
  --json` is: compact, keys in the order below, one trailing newline. An
  array of the same containers in the same order, each:

  | Key | Value |
  |---|---|
  | `name` | the Container column (the short ID when unnamed) |
  | `id` | the full ID |
  | `state` | Docker's state word (empty when Docker gives none) |
  | `health` | Docker's health word; `null` without a health check (none, empty or absent) |
  | `exit_code` | a number, always (Docker reports 0 for a running container) |
  | `oom_killed` | `true` or `false`, always |
  | `status` | the Status column's text |
  | `role` | the status rule's role: `ok`, `warn`, `problem` or `idle` |
  | `traefik_ports` | the column's values, as strings on purpose (label values are text) |
  | `bindings` | the column's bindings, same ones and same order, each `{"host_ip", "host_port", "container_port", "protocol"}`: the address as Docker gives it (unset as `0.0.0.0`, no brackets), ports as numbers (`null` for one that isn't a number) |

  No containers: `[]`. Never coloured. Fields are only ever added, never
  renamed or removed (scripts depend on them). With failed inspects the
  JSON of the rest goes to stdout, the error block to stderr, exit 1;
  whenever there would be no table (the errors below, Ctrl-C), nothing is
  written to stdout.
- **Errors:**
  - Daemon unreachable, an unsupported `DOCKER_HOST` scheme or TLS
    setting, the version check or the list call failing: no table;
    cli-core's error block naming the address or setting and the cause;
    exit 1.
  - A container removed between list and inspect (404): left out, logged
    at DEBUG (decision 3).
  - Any other inspect failure: the table of the rest, then one error,
    `failed to inspect N container(s)`, with a cause line per container
    naming its short ID (sb-go's `error inspecting container <id>: …`);
    exit 1.
- **Ctrl-C / SIGTERM:** cancels the requests; cli-core's "interrupted" block
  with 130 / 143, no table, terminal as it was.
- **Logging** (run log, capability-map decision 11; term-ui decision 9,
  verbose by default): the daemon address, the daemon's API version,
  how many containers were listed and inspected, each failure with its
  cause, and the time taken. bollard logs through the `log` crate, which
  sb's tracing subscriber doesn't receive today; `tracing-log` (the
  tokio-rs bridge) forwards it. sb's own crates log at DEBUG and
  dependencies at WARN by default (carried from term-ui).

### Differences from sb-go (the complete list)

- **Status:** `exited` with code 0 is idle (sb-go: red); `exited` with a
  code names it, `(code N)` or `(out of memory)` (sb-go: bare `exited`);
  `running (unhealthy)` and `restarting` are problem (sb-go: yellow); a
  health status of none or empty reads `running`, ok (sb-go: `running
  (none)`, yellow); `created (failed to start)` if confirmed; created and
  paused are idle (sb-go: yellow). Colours come from the theme's roles,
  not sb-go's fixed 256-colour codes.
- **Order inside cells:** Traefik ports and bindings sorted (sb-go
  iterates Go maps, so its order varies between runs).
- **IPv6:** every IPv6 address bracketed (sb-go brackets only `::`, so
  `::1:80->80/tcp` is ambiguous there).
- **A vanished container** is skipped (sb-go: an inspect failure, exit 1).
- **`--json`** exists (sb-go has no machine-readable `ps`).
- **`DOCKER_HOST`:** no TLS or ssh; such a setting is an error (sb-go's
  client supports TLS).
- **`DOCKER_API_VERSION`** is not read (sb-go pins its client to it).
- **Requests:** inspected at most 16 at a time (sb-go: one after another);
  bollard's 120 s request timeout (sb-go: none).
- **Piped output:** sb's table is plain and at its natural width. sb-go's
  table library falls back to 80 columns when it can't read a terminal size
  (a pipe), so sb-go's piped table wraps at 80 columns, and it writes its
  bold headings and blue border codes even when piped (only the status
  colour turns off).
- **Errors** are cli-core's error block (sb-go: the bare error text).
- **`sb docker help`:** no such subcommand, as in sb-go's docker group;
  sb's parse error, exit 2.
- **`sb docker <unknown>`:** cli-core's parse error, exit 2 (sb-go:
  `unknown command`, exit 1), as for every sb command; `sb docker` with
  only a global flag is a missing-subcommand error, exit 2 (sb-go: help,
  exit 0).

### Scope

- **In:** the `docker` crate with `ps`; `sb docker` and `sb docker ps`;
  sb's tokio runtime built after argument parsing, only for commands that
  need it (carried from term-ui); the `log` bridge and the default levels;
  the size ADR for bollard and the first use of term-ui's tables.
- **Later in this module, own plans:**
  - `start`, `stop`, `restart`: need `exec` (for `systemctl`) and an HTTP
    client; sb-go's `--ignore`; the unit-file check and timeouts above.
    sb-go gives them `--verbose/-v`, which the contract test will expect,
    while term-ui decision 9 dropped verbose output: decided in their plan.
  - The container list for the tui log picker (proto-logs: list plus a
    parallel inspect, kept live by bollard's event stream, with a richer
    status: restart count, restarted in the last 10 minutes). `ps`'s
    status function is the module's one container classification; the
    picker extends it rather than adding a second.
  - Container log streams the `tui` viewer consumes.
- **Not here:** a name filter (proto-inline had one; sb-go has none).

### Capability map correction (term-ui decision 6)

The `docker` row becomes: "ps, the container list (kept live by the event
stream) and log streams via bollard; start/stop/restart via the Saltbox
Docker controller's HTTP API after a `systemctl` check" — depends on
`cli-core`, `term-ui`, and from start/stop/restart on `exec` (for
`systemctl`) and the HTTP client the stack table names. `ps` needs only
`cli-core` and `term-ui`.

## Tech Stack

- bollard 0.21.1 (latest on crates.io, 2026-11), default features.
- tokio (current-thread runtime, as term-ui's demo); futures-util
  (`buffer_unordered`, already a bollard dependency).
- tracing-log, to forward `log` records into the run log.
- axum 0.8: an optional dependency behind `sb-docker`'s `testing` feature
  (the fake Engine API on a Unix listener); not in the release binary.
- term-ui's `Table`; cli-core's exit codes, error block, `Shutdown` and
  run log.

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-docker -p sb
Live:    SB_LIVE_DOCKER=1 SB_LOG_DIR=<scratch> systemd-run --scope -p MemoryMax=2G -p CPUQuota=200% cargo test -p sb-docker --test live
Try:     the production musl build of sb, run as root with SB_LOG_DIR set
```

## Project Structure

```
crates/docker/            sb-docker: ps (rows, status rule, Traefik rule,
  src/lib.rs              bindings, table); later start/stop/restart, the
  src/ps.rs               picker's list, log streams
  src/testing.rs          the fake Engine API (`testing` feature)
  tests/                  live test (opt-in)
crates/sb/src/main.rs     the `docker` command group, runtime after parsing
crates/sb/tests/          end-to-end tests on the fake Engine API
docs/specs/docker.md      this spec
docs/decisions/           the size ADR
```

deny.toml's wrapper lists: `sb-docker` added to `sb-cli-core`'s and
`sb-term-ui`'s, and its own entry allowing only `sb`.

## Code Style

As cli-core and term-ui: pure functions for the rules, tested first; the
command wires bollard to them.

```rust
/// Status text and role: sb-go's wording, an exit code deciding whether a
/// stopped container is a problem (proto-inline review).
pub fn status(state: &ContainerState) -> (String, Role) {
    match (state.status, health(state)) {
        (Some(ContainerStateStatusEnum::RUNNING), Some(HealthStatusEnum::HEALTHY)) => {
            ("running (healthy)".into(), Role::Ok)
        }
        // …
    }
}
```

## Testing Strategy

- **Unit (most):** the status rule (every row), the Traefik rule, the
  binding format and order (IPv6, missing host port, padding), display
  name and short ID (sb-go's `TestShortContainerID` cases), row building
  from bollard's model types filled from synthetic JSON fixtures (no host
  data), the table's look (snapshot), no containers, the `--json` output
  (exact string, with and without bindings and health).
- **Fake Engine API:** a test server on a Unix socket in a temporary
  directory answering `/version`, the list and the inspect endpoints from
  fixtures, reached through `DOCKER_HOST`; it drives the built `sb docker
  ps` end to end as root (as sb's other root tests: `SB_TEST_ROOT`,
  `SB_LOG_DIR` in the test's directory). Cases: rows; no containers; a
  vanished container; a failing inspect; an unreachable daemon; an old
  daemon (unversioned paths served at its own older version, versioned
  paths above it refused with 400, as dockerd does, and its responses in
  that version's shape, so bollard's models are shown to parse them; the
  fake records the paths it was sent and the test asserts they are
  unversioned); a newer daemon (a field bollard expects left out, an
  unknown field added); a daemon refusing unversioned paths (the error
  block, exit 1); `DOCKER_TLS_VERIFY` set and an
  `https://` host (errors); an empty `DOCKER_HOST`; an
  inspect held open while the test sends SIGINT, then SIGTERM; piped and
  on a PTY; the log records. The server is axum 0.8 on a Unix listener
  (platform note), behind `sb-docker`'s `testing` feature.
- **Relaunch:** cli-core's fake-sudo pattern (tests/relaunch.rs): `sb
  docker ps` run as `nobody` with a stand-in `sudo` first on `PATH`; the
  test asserts the arguments and the marker. A real sudo run would log to
  the host's `/var/log/sb` (sudo drops `SB_LOG_DIR`), so it stays a manual
  maintainer check.
- **Against sb-go:** once, by hand, sb-go pointed at the same fake Engine
  API (single label and binding per container, since Go's map order
  varies) and its output compared with sb's; differences must be the
  listed ones. Not a committed test.
- **Live (opt-in):** `SB_LIVE_DOCKER=1` runs read-only list and inspect
  against the local daemon, capped with systemd-run, one host at a time;
  nothing from it is committed.
- **Gates:** CONSTRAINTS.md as always (changed-line coverage ≥ 90%,
  mutation ≥ 80% on changed code, the ratchets).

## Boundaries

- **Always:** read only from Docker (version, list, inspect); run sb as
  root only with `SB_LOG_DIR` in `target/` or a temporary directory; synthetic
  fixtures.
- **Ask first:** any write to Docker or the controller (start/stop/restart
  are later work); dependencies beyond bollard, futures-util, tracing-log
  and axum (the `testing` feature only); any difference from sb-go not in
  the list above.
- **Never:** run the `docker` binary for `ps`; commit host details
  (container names, addresses, ports from a real host); touch the
  Saltbox repository.

## Success Criteria

1. Rows, columns, names, Traefik ports and binding lines follow sb-go's
   rules (unit tests per rule), differing only as listed; the manual
   comparison with sb-go on the fake Engine API shows no other difference.
2. Every row of the status table has a test, failing against a stub
   first; the text alone tells the states apart.
3. The table is term-ui's reviewed look (snapshot), fits 120, 70 and 50
   columns, and is plain at natural width when piped; `--color=never` and
   `NO_COLOR` write no colour. No containers: headings only.
3a. `--json` prints the documented shape (exact-string tests: key order,
   `null` health, an unnamed container, bindings), `[]` with no
   containers, never colour codes (even with `--color=always`); with a
   failed inspect the rest on stdout, the error on stderr, exit 1; with an
   unreachable daemon nothing on stdout, exit 1.
4. Unreachable daemon: one error block, exit 1, no table. A failing
   inspect: the table, then one error naming each container's short ID
   and cause, exit 1. A vanished container: left out, exit 0. An old
   daemon that refuses every path above its version: the table. A TLS or
   ssh `DOCKER_HOST` setting: an error naming it, exit 1, no request
   sent.
5. Not root: `sb docker ps` execs the stand-in sudo with cli-core's
   arguments and marker (fake-sudo test); cli-core Task 5's two
   equivalent mutants are caught.
6. SIGINT or SIGTERM while an inspect is held open: within seconds,
   cli-core's "interrupted" block, 130 or 143, no table (with `--json`,
   nothing on stdout). `ps` changes no terminal state (no runner), so
   there is nothing to restore: the runs are piped, which also checks
   stdout apart from stderr (amended in docker Task 8).
7. The run log records the daemon address, the daemon's version and API
   version (a fake daemon above bollard's 1.53, so logging bollard's capped
   1.53 instead fails the test), the
   counts and each failure; a dependency's WARN record reaches the log
   and its DEBUG record does not (positive and negative control).
8. A root run follows `DOCKER_HOST` (fake Engine API test).
9. `sb --help` stays within its ratchet and `sb version` within the 10 ms
   budget (no runtime built for them). `sb docker ps`'s median is recorded
   beside sb-go's on the same machine, for information (no bar).
10. The production musl build (x86_64) runs `sb docker ps` against the
    fake Engine API and, by hand, a real daemon.
11. Binary size measured; bollard, hyper and tokio's network stack will
    cross the 5% tolerance, so an ADR records the new size (step by step)
    for the maintainer.

## Decisions (maintainer, 2026-11-03)

1. **No runner for `ps`:** the table alone, as sb-go. term-ui's carried
   runner criteria (the failure block and Ctrl-C endings in sb's own
   commands) move to `sb validate-config`.
2. **`--json` now,** in the shape above.
3. **Vanished containers are skipped,** logged at DEBUG, exit 0.
4. **`DOCKER_HOST` through the sudo relaunch:** sb-go's behaviour;
   cli-core's preserve list is unchanged, so a non-root user's `DOCKER_HOST`
   and `DOCKER_API_VERSION` don't reach the root run (Saltbox's Docker is
   local).
