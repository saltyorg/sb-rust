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
- **API version** (amended 2026-11-18: versioned paths, as Docker has
  deprecated unversioned ones). bollard 0.21.1 sends every request
  without a version prefix (its `Url::join` replaces the `/v1.53` it
  formats); sb puts the version back with bollard's documented
  `Docker::with_request_modifier`, prefixing each request's path with
  `/v<version>`. The version is chosen once, on connecting: the daemon is
  asked `/v1.53/version` (bollard's own version); a daemon that refuses
  that with a 400 (older than 1.53, or its oldest above it) is asked the
  unversioned `/version`, which such daemons serve. Every later request
  goes at the daemon's `ApiVersion` capped at bollard's 1.53, so daemons
  older and newer than bollard's both work, each answering in that
  version's shape. A daemon whose `MinAPIVersion` is above that version,
  or whose 400 for `/v1.53/version` says 1.53 is too old while it serves
  no unversioned path either (a Docker newer than this sb after the
  removal), is an error, `Docker's API at <address> starts at <min>,
  newer than sb's 1.53: a newer sb is needed`; a daemon that refuses both
  version calls otherwise is the error naming both refusals, and one that
  refused 1.53 without naming its own version an error saying so. The
  path is rebuilt from bollard's `http::Uri` parts, as bollard's
  `connect_with_custom_transport` example prefixes one; bollard's
  `negotiate_version()` isn't used (below). The run log gets the daemon's `Version` and
  `ApiVersion` and the version sb's requests use (bollard's
  `negotiate_version()` is not used: it discards the response and changes
  nothing on the wire). A failed version call (unreachable, a 4xx or 5xx
  answer) ends the run as an unreachable daemon does: error block, exit
  1. A good answer without `Version` or `ApiVersion` (both optional in
  bollard's model) logs them as unknown and the requests use bollard's
  1.53. `DOCKER_API_VERSION` is not read (a difference, below). Requests
  keep bollard's 120 s timeout (sb-go has none). Evidence:
  docs/platform-notes/docker-engine-api.md (Checkpoint A).
- **Guarding that behaviour:** bollard is pinned to exactly 0.21.1
  (`=0.21.1`): a release that prefixes versions itself would override
  the version sb chose (the modifier leaves a path that has a prefix
  already as it is), so an older daemon would refuse every request;
  `an_old_daemon_answers_at_its_own_version` fails if that happens.
  The fake Engine API records the paths it is sent and the tests assert
  them versioned, `/v1.53/` on a daemon that serves it and the daemon's
  own on an older one. A bollard upgrade re-checks what paths it sends
  and how a version would be chosen.
- **Rows:** every container from the list endpoint (`all=true`), each
  inspected by ID (as sb-go) for state, health, exit code, labels and
  bindings, at most 16 at a time (`futures-util`'s `buffer_unordered`,
  already in the tree through bollard); sorted by name.
- **Columns** (headings centred and bold, port columns right-aligned):

  | Column | Content |
  |---|---|
  | Container | the first name without its leading `/`; with no name, the short ID (12 characters, `unknown` when empty), as sb-go |
  | Status | the status rule below |
  | Traefik Port | values of labels containing `traefik.http.services` and `.loadbalancer.server.port`, not containing `-http`, with more than three dot-separated parts (sb-go's rule); empty values dropped, deduplicated, numeric values in numeric order then any others in text order, joined by `, `; in the table each value is one line (newline and carriage return as spaces) cut to 64 characters with `…`, and `--json` keeps it whole |
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
  --json` is: compact, keys in the order below, one trailing newline,
  every control character in a string escaped (`\n`, `\u001b`,
  `\u009b`), C1 and DEL included (serde_json escapes only U+0000 to
  U+001F; a label's 8-bit CSI or OSC would otherwise reach a terminal).
  An
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
  and death by the signal (`$?` 130 / 143; exec decision 7), no table,
  terminal as it was.
- **Logging** (run log, capability-map decision 11; term-ui decision 9,
  verbose by default): the daemon address, the daemon's API version,
  how many containers were listed and inspected, each failure with its
  cause, and the time taken. bollard logs through the `log` crate, which
  sb's tracing subscriber doesn't receive today; `tracing-log` (the
  tokio-rs bridge) forwards it. sb's own crates log at DEBUG and
  dependencies at WARN by default (carried from term-ui).

### Differences from sb-go (the complete list)

`ps`'s, below; `start`, `stop` and `restart`'s are their section's list
("Start, stop and restart", Differences from sb-go).

- **Status:** `exited` with code 0 is idle (sb-go: red); `exited` with a
  code names it, `(code N)` or `(out of memory)` (sb-go: bare `exited`);
  `running (unhealthy)` and `restarting` are problem (sb-go: yellow); a
  health status of none or empty reads `running`, ok (sb-go: `running
  (none)`, yellow); `created (failed to start)` if confirmed; created and
  paused are idle (sb-go: yellow). Colours come from the theme's roles,
  not sb-go's fixed 256-colour codes.
- **A container without a state** in its inspect (an artificial case;
  Docker sends one) reads `unknown` (warn); sb-go fails on it
  (maintainer, docker Checkpoint C).
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
    sb-go gives them `--verbose/-v`; dropped (decision 6).
  - *Amended 2026-11-13 by the tui spec:*
  - **The picker's container list** is `connect::list` (all containers,
    already). `Row` gains, from the inspect: `restart_count`
    (`RestartCount`), `created` (`Created`), `started_at` and
    `finished_at` (`State.StartedAt`, `State.FinishedAt`; Docker's zero
    time `0001-01-01T00:00:00Z` and an unparsable value read as `None`),
    as `jiff::Timestamp`. `--json`'s keys are unchanged.
  - **`picker_status(row, now)`** returns a symbol, a text, a role and an
    `attention` flag. The text is `ps::status`'s text (the one
    classification), followed by:
    - running: ` · up <duration>`; with a restart count above 0 and up
      less than 10 minutes, ` · restarted N× · up <duration>`, symbol
      `↻`, role Problem, attention;
    - exited, dead: ` · <duration> ago` from `finished_at`;
    - restarting: ` (N×)`.

    The symbol follows the role (`✓` ok, `✗` problem, `◌` warn, `○`
    idle), `↻` for restarting. Attention is role Problem, or the
    10-minute rule. Durations are formatted by a function the caller
    passes (the tui picker passes sb-host's `format_duration`), because
    sb-docker doesn't depend on sb-host (deny.toml).
  - **`logs.rs`, the log reads:** a `Position { time_ns: i128, ordinal:
    u32 }` and a `LogLine { position, stream (stdout, stderr, tty), bytes
    }`, with:
    - `tail(daemon, id, n)`: `tail=n`, `timestamps=true`, no follow;
      when `n` came back, the first entry's second is read again and
      the tail's first entries are numbered after those of that time it
      cut off, so their ordinals match every other read's (found in M63:
      a tail can begin inside entries sharing a time);
    - `older(daemon, id, before, n, created)`: windows of `since =
      floor(t - W)` (at least 1) and `until = ceil(t)`, W starting at
      60 s and doubling, keeping the entries before `before` and the last
      `n` of them, until `n` are found or `since` reaches `created` (then
      `more_older` is false);
    - `newer(daemon, id, after, n)`: `since = floor(t)`, the entries at
      or before `after` dropped, the stream dropped after `n + 1` (the
      extra one sets `more_newer`);
    - `follow(daemon, id, after)`: as `newer` with `follow=true` and no
      count, ending when the daemon ends the stream.

    Every read parses the RFC 3339 nanosecond prefix with jiff, filters
    the window itself too (moby's filter is one-sided), and counts
    ordinals among equal timestamps in the order the daemon sends them.
    A line without a parsable timestamp keeps the previous line's
    position with the next ordinal. Errors carry the daemon's message.
  - **`events.rs`:** `container_events(daemon)`, bollard's `events` with
    `type=container`, yielding the action (`create`, `start`, `die`,
    `destroy`, `rename`, `health_status`, others passed through) and the
    container's ID and name.
  - **A platform note** (docs/platform-notes/docker-logs.md) records the
    daemon probe: `tail` before `since`/`until`, the order of equal
    timestamps, 16 KiB partial lines, `until`'s inclusiveness, with
    Docker's version and the method.
  - **The fake Engine API** serves `/containers/{id}/logs` (multiplexed
    frames and a TTY stream, `tail` applied before `since` and `until` as
    moby does) and `/events`.
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
Try:     the production build of sb, run as root with SB_LOG_DIR set
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
  fake records the paths it was sent and the test asserts `/v1.53/version`
  refused, `/version`, then every request at the daemon's version); a
  newer daemon (a field bollard expects left out, an unknown field added);
  a daemon refusing unversioned paths (answered: every path versioned); an
  old one refusing them too (the error naming both refusals); a daemon
  whose oldest version is above bollard's (the newer-sb error); `DOCKER_TLS_VERIFY` set and an
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
   cli-core's "interrupted" block, death by the signal (`$?` 130 or 143),
   no table (with `--json`,
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
10. The production build (x86_64) runs `sb docker ps` against the
    fake Engine API and, by hand, a real daemon.
11. Binary size measured; bollard, hyper and tokio's network stack will
    cross the 5% tolerance, so an ADR records the new size (step by step)
    for the maintainer.

## Start, stop and restart (approved 2026-11-06)

The second plan of this module (Scope, "Later in this module"). It puts
`exec` in sb's binary, and reqwest in sb-docker.

### Objective

`sb docker start`, `sb docker stop [--ignore NAME]...` and `sb docker
restart [--ignore NAME]...` have the Saltbox Docker controller start or
stop the containers Saltbox manages, in its dependency order, and wait
for it to finish, under term-ui's runner: sb-go's behaviour, sb's look,
and a run log of what was asked and answered.

### Commands and help (the contract fixture, sb-go's texts)

| Command | Short | Long |
|---|---|---|
| `start` | Start Docker containers managed by Saltbox | Start Docker containers managed by Saltbox in dependency order. |
| `stop` | Stop Docker containers managed by Saltbox | Stop Docker containers managed by Saltbox in dependency order. |
| `restart` | Restart Docker containers managed by Saltbox | Restart Docker containers managed by Saltbox in dependency order. |

`--ignore` (a value flag, on `stop` and `restart` only): "Containers to
ignore during stop operation (can be specified multiple times)" (`restart`:
"... during restart operation ..."). Repeatable, and each value split on
commas, not trimmed, empty names dropped. cobra's `StringSlice` reads each
value as one CSV record (quotes stripped, a stray quote an error); Docker
names can't hold a comma or a quote (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`), so for
every real name the split is cobra's result. No positional arguments.
Needs root, as every command but `version`, completion and help.
`-v`/`--verbose` is a parse error (exit 2), as config's. *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*

### Steps and labels (term-ui's runner)

A step with no labels of its own in sb-go uses its running label for both
endings (sb-go's runner, `terminal/runner.go`), as here.

| Step | Running | Done | Failed |
|---|---|---|---|
| start (top) | Starting Docker containers | Docker containers started | Docker container start |
| stop (top) | Stopping Docker containers | Docker containers stopped | Docker container stop |
| restart (top) | Restarting Docker containers | Docker containers restarted | Docker container restart |
| service check | Checking Docker controller service | Docker controller service ready | Docker controller service check |
| request | Requesting Docker start job (stop: Requesting Docker stop job) | the same | the same |
| wait | Waiting for Docker start job (stop: Waiting for Docker stop job) | the same | the same |
| restart's stop | Stopping Docker containers | Stopped Docker containers | Stop Docker containers |
| restart's start | Starting Docker containers | Started Docker containers | Start Docker containers |

`start` and `stop`: the service check, the request, the wait, under the
top step. `restart`: the service check, then restart's stop (its request
and wait, with `--ignore`), then restart's start (its request and wait,
no `--ignore`); a failed stop starts nothing.

### Behaviour (sb-go's `cmd/docker*.go`)

- **Service check:**
  - `stat` of the unit file
    `/etc/systemd/system/saltbox_managed_docker_controller.service`:
    missing is "the Docker controller service does not exist"; another
    stat error (ENOTDIR, ELOOP) is "error checking service status: stat
    <path>: <cause>" (Go's text names the path; Rust's error doesn't, so
    sb adds it).
  - Then `systemctl is-active saltbox_managed_docker_controller.service`
    (found on `PATH`, as sb-go), captured through exec with a 5 s
    timeout: exit 0 and stdout `active` after trimming whitespace is
    ready. A non-zero exit, a spawn failure, a timeout or another answer
    is "the Docker controller service is not running": a plain message,
    exec's outcome logged but not chained into the error, so the failure
    block shows sb-go's text alone and not term-ui's child ending.
- **Request:** `POST <base>/start` or `<base>/stop` with `ignore=<name>`
  per ignored name (query-encoded), `Content-Type: application/json`, no
  body. `<base>` is the loopback URL sb-go's `layout.DockerControllerAPIURL`
  names. The path is built segment by segment (a base with a path keeps
  it; `Url::join` would replace its last segment).
  - 200 with a JSON object whose `job_id` isn't blank (after trimming) is
    the job, polled with the ID as sent (sb-go).
  - Failures, each sb-go's text with the cause from reqwest or serde_json
    in place of Go's: "send Docker controller request: <cause>" (a
    transport error or the 10 s timeout); "docker controller request
    failed with status code: N"; "decode Docker controller response:
    <cause>"; "docker controller response is missing job ID".
- **Wait:** `GET <base>/job_status/<id>` (the ID one path segment,
  escaped) at once, then every 5 s, at most 60 times; no sleep follows
  the 60th.
  - `completed` ends the step; `pending` and `running` wait.
  - These end the wait at once: `failed` is "job failed"; another
    status is "unknown job status: X".
  - A failed poll (a transport error or timeout, reqwest's cause; "job
    status check failed with status code: N"; "decode job status
    response: <cause>"; "job status response is missing status") is
    asked again after the usual 5 s; three failed polls in a row
    (`FAILED_POLLS_IN_A_ROW`) end the wait with the last one's text
    (decision 10). Failed polls count toward the 60.
  - After the 60th: "timeout waiting for job completion".
- **HTTP:** reqwest (already in sb through config), each request limited
  to 10 s (`RequestBuilder::timeout`), at most 1 MiB of response read
  (sb-go's `LimitReader`: a body cut by the limit fails as a decode
  error; the first JSON value is read and trailing data ignored, as Go's
  `Decoder`; Go's case-blind keys and `null`s aren't copied: the
  controller writes neither), and
  no proxy: Go's `ProxyFromEnvironment` never proxies a loopback address;
  reqwest's system proxy (`HTTP_PROXY`, `http_proxy`, `ALL_PROXY`) has no
  loopback exception.
  - A small client in sb-docker, not config's `Http`: sb-docker can't
    depend on sb-config (deny.toml allows only `sb` to), and config's
    carries its proxy setting and credential masking, which the
    controller doesn't need.
  - No redirects: the controller never answers with one
    (docs/platform-notes/docker-controller.md; decision 8).
- **Ctrl-C or SIGTERM:** the current request or wait ends at once; the
  runner shows its interrupted ending and sb dies by the signal (`$?` 130
  or 143; exec decision 7). During the service check, exec stops
  `systemctl` (SIGTERM, then SIGKILL after exec's 5 s grace) and the
  interrupted outcome is passed on, not read as "not running". The
  controller's job is not cancelled (it has no cancel endpoint), so an
  interrupted request or wait adds an info notice under its step: "the
  controller may still be starting containers; `sb docker ps` shows
  where they are" ("stopping" for a stop; decision 5). The same notice
  follows every other ending without a final job status (decision 10):
  a request not answered or whose answer can't be read (it may have been
  accepted), a wait that gave up (its timeout, failed polls, an unknown
  status). A refused request and a failed job have none: their answers
  are final.
- **Restart's start:** a restart whose stop doesn't end done requests no
  start and says so under its step: "the start wasn't requested: <why>"
  (the stop job failed; the controller refused the stop; sb couldn't
  tell whether the stop finished; the run was interrupted), as does an
  interrupt that lands as the stop's last answer comes, which leaves the
  start's step skipped. A refused start after a done stop says "the
  containers were stopped and stay stopped: the controller refused the
  start". So once the stop is asked for, a restart ends with the start
  requested or such a line (docker-M2).
- **Run log:** the check's answer, each request (method, path, ignored
  names) and answer (status, job ID), each poll's status, the outcome.
  Nothing secret is involved.

### Differences from sb-go (start, stop and restart)

- No `--verbose/-v` (config decision 1, term-ui decision 9; nothing
  passes it, decision 6): the job ID and the ignored names it showed are
  in the run log. The contract test allows a flag sb-go has and sb
  leaves out.
- An interrupted job gets the notice above (decision 5); sb-go says
  nothing.
- Every other ending without a final job status gets the notice too,
  and a restart that doesn't request its start says why (decision 10);
  sb-go says only the error.
- Up to three failed polls in a row are ridden out (decision 10); sb-go
  ends the wait at the first.
- sb's runner look and failure block, not sb-go's spinner and its outer
  `error: ...` wrapping; the texts above are the innermost cause.
- `SB_DOCKER_CONTROLLER_URL` and `SB_DOCKER_CONTROLLER_UNIT` replace the
  base URL and the unit file's path (tests and debugging; sb-go
  hard-codes both), as config's `SB_DOCKERHUB_URL`; the unit name given
  to `systemctl` stays fixed. Read as root only; sudo's relaunch drops
  them. A URL that doesn't parse is sb-go's "parse Docker controller URL:
  <cause>", before the runner starts, exit 1.
- Ctrl-C during the service check is the interrupted ending (sb-go:
  "not running"); a `systemctl` past its 5 s gets SIGTERM and exec's
  grace before SIGKILL (sb-go: SIGKILL at 5 s).
- No redirects followed (Go's client follows up to 10, re-sending the
  POST on 307 and 308): the controller never redirects (decision 8), as
  config's services.

### Shape

`sb-docker` gains `control.rs`:
- `Action` (start, stop);
- `Controller { base: Url, unit_file: PathBuf, check_timeout,
  request_timeout, poll_interval: Duration, body_limit: usize, max_polls:
  u32, http }` with `Controller::saltbox()` giving sb-go's values (a unit
  test pins them: 5 s, 10 s, 5 s, 1 MiB, 60), so each behaviour is tested
  with short values;
- `check_service`, `request(action, ignore)` and `wait(job)` as term-ui
  step bodies, failing with a `ControlError`, one variant per failure
  above, sb-go's texts as its `Display` (tests match variants).

sb-docker gains sb-exec (deny.toml already allows it) and reqwest; its
tokio, today behind `testing` only, becomes a plain dependency (`time`
for the poll's sleep). sb's `main.rs` builds the controller, with the two
overrides.

### Testing

- **Unit, on a fake controller** (axum on loopback, `testing` feature, as
  the fake Engine API; it records what it was sent and compares decoded
  query values):
  - a job requested and completed;
  - `--ignore` encoding: sb-go's
    `TestRequestDockerJobUsesContextAndEncodesIgnoredContainers` cases, a
    space and an ampersand;
  - a missing or blank job ID; non-200 on the request and on a poll; a
    transport failure on each;
  - `failed`, an unknown status, the 60-poll timeout (a short interval);
  - cancellation mid-wait: sb-go's
    `TestWaitForJobCompletionHonorsCancellation`;
  - a body over 1 MiB (cut: a decode error), trailing data after the
    JSON (ignored);
  - `HTTP_PROXY`, `http_proxy` and `ALL_PROXY` set, and the request still
    reaches the fake;
  - a 307 answer: "docker controller request failed with status code:
    307", its target never asked.
- **Service check:**
  - a stand-in `systemctl` that prints `active` (also with trailing
    whitespace), prints `inactive`, exits 3, prints `active` but exits 1,
    or sleeps past 5 s;
  - no `systemctl` on `PATH`;
  - the unit file present, missing, or behind a path through a regular
    file (ENOTDIR).
- **End to end:** the built sb as root, `SB_TEST_ROOT`, `SB_LOG_DIR` in
  the test's directory, the two overrides and a stand-in `systemctl`.
  - start; stop with `--ignore a,b --ignore c`; restart (stop then start;
    a failed stop starts nothing); `-v` refused (exit 2);
  - each failure's block, exit 1;
  - SIGINT and SIGTERM mid-wait, during a delayed request answer (both
    with decision 5's notice) and while a stand-in `systemctl` sleeps
    (the interrupted ending, no "not running" block): death by the
    signal, the terminal restored;
  - piped and on a PTY; the run log records; the relaunch through the
    fake sudo, which records its arguments and never runs sb (the
    relaunch drops the overrides, so a relaunched run would reach the
    real controller);
  - `--help` texts (snapshot).
- **Never against the real controller** from a test: it starts and stops
  the host's containers. A by-hand run on a Saltbox host is the
  maintainer's.
- **Each failure first:** every case above is written failing against a
  stub before its code.
- **Size and coverage:** exec (portable-pty, rustix's process features),
  the docker client's reqwest use and tokio's `time` measured against
  ADR-011's 11645408 bytes, and project coverage against its line and
  function values; an ADR if either moves past its tolerance.

### Success criteria

1. Each command's help, steps, labels, requests and texts are the ones
   above (unit tests per rule; the fake controller recording what it was
   sent).
2. Every service-check outcome above has a test, the interrupted one
   included.
3. Every request and wait answer and failure above has a test.
   `Controller::saltbox()`'s values are pinned (5 s check, 10 s request,
   5 s interval, 1 MiB, 60 polls, the first at once).
4. `--ignore` reaches only the stop, encoded; restart stops before it
   starts and starts nothing after a failed stop.
5. No proxy is used for the controller with `HTTP_PROXY`, `http_proxy`
   and `ALL_PROXY` set.
6. Ctrl-C and SIGTERM mid-wait: the interrupted ending with the
   notice, and death by the signal within a second; the terminal
   restored.
7. The run log records the check, the job ID, each poll and the outcome.
8. Not root: the fake-sudo relaunch with cli-core's arguments.
9. `sb --help` and `sb version` within their ratchets; size and coverage
   measured, an ADR past a tolerance.

### Known limits

- **A failed job's containers aren't named:** a job that reads `failed`
  names the containers that didn't reach the requested state (its
  `failed`, `outcomes` and `error`; docs/platform-notes/docker-controller.md).
  sb, like sb-go, says only "job failed"; naming them is a later
  difference.
- **sb can give up before the controller does:** the wait ends after
  about 5 minutes (59 sleeps of 5 s), while the controller's own job
  timeouts are 600 s for start and 300 s for stop, and a job can wait
  `pending` behind another. "timeout waiting for job completion" can
  come while the job goes on (as sb-go); the notice then says the
  controller may still be at work, and a restart says its start wasn't
  requested (decision 10).
- **The command-tree contract:** these commands leave out sb-go's `-v`,
  as validate-config does; a future full-equality contract test records
  them as dropped flags.

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

## Decisions (maintainer, 2026-11-06: start, stop and restart)

5. **An interrupted job gets a notice:** the controller can't cancel a
   job, so an interrupted request or wait says the controller may still
   be starting (stopping) containers and points at `sb docker ps`.
6. **No `--verbose/-v`:** nothing passes it to these commands; dropped,
   as config decision 1.
7. **The section approved** with these answers.
8. **No redirects:** the controller's source has none
   (docs/platform-notes/docker-controller.md), so sb follows none.
9. **Today's controller, sb-go's behaviour:** sb follows sb-go against
   the current (Go) controller. The richer feedback a review of the
   controller found (live per-container progress, queue position, a
   cancel endpoint, a version and features check) belongs to the
   controller's Rust rewrite, the maintainer's own work; sb takes it up
   then.

10. **Every ending without a final job status says so** (hardening plan
    decision 17, 2026-11-12; docker-M2): the "controller may still be
    starting (stopping) containers" notice on each, and a few failed
    polls in a row (three) ridden out before it; a restart whose stop
    doesn't end done says its start wasn't requested and why. Listed
    differences from sb-go.
