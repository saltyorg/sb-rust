# Spec: `docker` (draft)

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
4. **No runner for `ps`** (Open question 1): sb-go prints only the table;
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

- **Arguments:** none (sb-go: `cobra.NoArgs`). `sb docker`'s and `sb
  docker ps`'s short and long help texts are sb-go's ("Manage Docker
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
  followed; unset or empty means the default socket (as sb-go, which
  treats empty as unset). sb dispatches the scheme itself
  (`Docker::connect_with_unix` / `connect_with_http` with a
  `ClientVersion`), because `connect_with_defaults()` fixes bollard's API
  version and has no setter. bollard's default features (`http`, `pipe`;
  no TLS) are kept. `https://`, `ssh://`, `DOCKER_TLS_VERIFY` or
  `DOCKER_CERT_PATH` stop with an error naming the unsupported setting:
  without TLS compiled in, bollard would otherwise connect `tcp://` in
  plain text where sb-go uses TLS.
- **API version:** bollard prefixes every path with its version
  (`/v1.53/...`, `/version` included), and dockerd refuses a version above
  its own maximum, so bollard's `negotiate_version` fails on exactly the
  older daemons it is meant for. sb therefore connects at a floor version
  every supported daemon accepts, reads the daemon's `ApiVersion` from
  `/version`, and connects again at the lower of that and bollard's 1.53
  (the floor and the check against the Engine API documentation are the
  plan's first task). sb-go's client negotiates through `/_ping`, which
  carries no version. `DOCKER_API_VERSION`, when set, is used instead, as
  sb-go's `FromEnv`; one that doesn't parse is an error naming it. Requests keep bollard's 120 s timeout (sb-go has
  none).
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
  | Port Bindings | one per line, `ip:host->port/proto`, host and container ports padded within the cell so the arrows line up (when the table wraps a narrow column, wrapped lines may not); an unset IP shown as `0.0.0.0`, every IPv6 address in brackets (`[::]`, `[::1]`); bindings without a host port left out; ordered by container port number, then protocol, then IPv4 before IPv6, then address, then host port |

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
  | removing, or a state bollard doesn't know | the state | warn |

  The failed-start row is checked against the Engine API documentation
  and a real failed start in the plan's first task (synthetic fixture
  only); if Docker doesn't record it that way the row is dropped.
- **Table:** term-ui's `Table` (rounded, a rule between rows, border in
  the Border role, maximum column width 100 as sb-go), fitted to the
  terminal, natural width and plain when piped (`auto`); `--color`,
  `NO_COLOR` and `SB_COLOR_PROFILE` as everywhere. No containers: the
  headings alone, as sb-go.
- **Errors:**
  - Daemon unreachable, an unsupported `DOCKER_HOST` scheme or TLS
    setting, the version check or the list call failing: no table;
    cli-core's error block naming the address or setting and the cause;
    exit 1.
  - A container removed between list and inspect (404): left out, logged
    at DEBUG (Open question 3).
  - Any other inspect failure: the table of the rest, then one error,
    `failed to inspect N container(s)`, with a cause line per container
    naming its short ID (sb-go's `error inspecting container <id>: …`);
    exit 1.
- **Ctrl-C / SIGTERM:** cancels the requests; cli-core's quiet 130 / 143,
  no table, terminal as it was.
- **Logging** (run log, capability-map decision 11; term-ui decision 9,
  verbose by default): the daemon address, the negotiated API version,
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
- **A vanished container** is skipped (sb-go: an inspect failure, exit 1),
  if Open question 3 agrees.
- **`DOCKER_HOST`:** no TLS or ssh; such a setting is an error (sb-go's
  client supports TLS).
- **Requests:** inspected at most 16 at a time (sb-go: one after another);
  bollard's 120 s request timeout (sb-go: none).
- **Piped output:** sb's table is plain and at its natural width. sb-go's
  table library falls back to 80 columns when it can't read a terminal
  size (a pipe), so sb-go's piped table wraps at 80 columns, and it writes its
  bold headings and blue border codes even when piped (only the status
  colour turns off).
- **Errors** are cli-core's error block (sb-go: the bare error text).
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
- **Not here:** a JSON or plain output mode (Open question 2); a name
  filter (proto-inline had one; sb-go has none).

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
  tests/                  fake Engine API tests, live test (opt-in)
crates/sb/src/main.rs     the `docker` command group, runtime after parsing
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
  data), the table's look (snapshot), no containers.
- **Fake Engine API:** a test server on a Unix socket in a temporary
  directory answering `/version`, the list and the inspect endpoints from
  fixtures, reached through `DOCKER_HOST`; it drives the built `sb docker
  ps` end to end as root (as sb's other root tests: `SB_TEST_ROOT`,
  `SB_LOG_DIR` in the test's directory). Cases: rows; no containers; a
  vanished container; a failing inspect; an unreachable daemon; an old
  daemon (every path above its version refused with 400, `/version`
  included, as dockerd does, and its responses in that older version's
  shape, so bollard's models are shown to parse them); `DOCKER_TLS_VERIFY` set and an `https://`
  host (errors); an empty `DOCKER_HOST`; an
  inspect held open while the test sends SIGINT, then SIGTERM; piped and
  on a PTY; the log records. The server comes from a maintained crate
  (chosen in the plan, library first).
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
  are later work); dependencies beyond bollard, futures-util,
  tracing-log and a test server; any difference from sb-go not in the
  list above.
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
4. Unreachable daemon: one error block, exit 1, no table. A failing
   inspect: the table, then one error naming each container's short ID
   and cause, exit 1. A vanished container: left out, exit 0. An old
   daemon that refuses every path above its version: the table. A TLS or
   ssh `DOCKER_HOST` setting: an error naming it, exit 1, no request
   sent.
5. Not root: `sb docker ps` execs the stand-in sudo with cli-core's
   arguments and marker (fake-sudo test); cli-core Task 5's two
   equivalent mutants are caught.
6. SIGINT or SIGTERM while an inspect is held open: 130 or 143, quiet, no
   table, terminal restored (fake Engine API on a PTY).
7. The run log records the daemon address, the negotiated version, the
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

## Open Questions

1. **Runner for `ps`?** Recommended: no; the table alone, as sb-go. Then
   term-ui's carried runner criteria (the failure block and Ctrl-C endings
   in sb's own commands) move to `sb validate-config`. The alternative is
   a step line ("✓ containers read") above the table.
2. **JSON or plain output for scripts?** Recommended: not now (sb-go has
   none); add `--json` when a script needs it. The capability map says to
   decide per command.
3. **Vanished containers:** skip them (recommended) or report them as sb-go
   does?
4. **`DOCKER_HOST` through the sudo relaunch:** cli-core's relaunch keeps
   only `VISUAL`, `EDITOR`, `SB_COLOR_PROFILE`, `NO_COLOR` and the marker,
   so a non-root user's `DOCKER_HOST` and `DOCKER_API_VERSION` are dropped
   and the root run uses the default socket, as in sb-go. Recommended:
   keep sb-go's behaviour (Saltbox's Docker is local); the alternative adds
   both to the preserve list, with the carried sudoers `env_keep` check.
