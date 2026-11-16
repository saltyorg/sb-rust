# Spec: `tui` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`tui`, build step 6): sb's
full-screen screens, above `docker`, `host` and `config`. This spec covers
the module's first part, the log picker and viewer behind `sb logs` and
`sb docker logs`: one picker for Saltbox's systemd services and Docker
containers, one viewer for both, and the inline fallback when stdout
isn't a terminal. It is the first consumer of `sb-journal` (journal spec,
decision 8) and of sb-host's service list (host decision 1), and the
first screen in sb's binary, so it brings ratatui and crossterm in. The
editors (`sb edit`, `sb fact`) are the second part of this spec
("Editors: `sb edit` and `sb fact`", with its own plan); the
announcements viewer is a later section (Scope).

The behaviour comes from sb-go 35a7acc (2026-10-28), `cmd/logs.go`,
`cmd/dockerLogs.go` and `host/systemd.go`, redesigned as capability map
decision 3 asks, from proto-logs (reviewed 2026-10-29) and
`docs/ideas/log-picker.md`.

## Assumptions

1. **Both commands keep their names and take no arguments** (capability
   map decision 1; sb-go `cobra.NoArgs`, logs.go:27-37,
   dockerLogs.go:27-37). No source is named on the command line (journal
   decision 8; log-picker.md, "Not Doing").
2. **Both need root,** as every command but `version`, help and
   completion (cli-core decision 3). Journal files and the Docker socket
   are root's.
3. **The journal side is sb-host's journal API** (`page`, `drain`,
   `FollowWait`, the re-exported sb-journal types). There is no
   `journalctl` child (host decision 4, ADR-013). `Journal::open` and
   `follow` run under the caller's `spawn_blocking` (host journal.rs).
4. **The Docker side is bollard 0.21.1** through sb-docker (docker
   assumption 2). No `docker` binary is run.
5. **The runtime is the workspace's current-thread tokio** (crates/sb
   main.rs). proto-logs measured on multi-thread, so the groundwork slice
   measures input latency again.
6. **Units and containers are root's own data, but their log content is
   untrusted.** Any process on the host can write to the journal, and any
   container to its log. Bytes reach the screen only through term-ui's
   `DisplayText` (journal spec, "Display").
7. **Saltbox's Docker logs are json-file, 10 MiB × 3 per container**
   (Saltbox roles/docker/tasks/subtasks/daemon.yml:34-37), so a
   daemon-side read of one container is bounded by about 30 MiB.
   `docker_config_custom` can choose another driver; a driver without
   read support gives the daemon's error in the viewer.

## Objective

A Saltbox user who wants to see what a service or container is doing
gets from `sb logs` to the right log in a few keys, sees at a glance
what is broken, and can hand complete, plain text to a support request,
which Saltbox's support page asks for (saltyorg/docs
docs/community/support.md:34) and sb-go's screens can't give.

### The picker (`sb logs`, `sb docker logs`)

- **Entry points** (decided.md A-Q1): `sb logs` opens the picker on every
  kind. `sb docker logs` opens it filtered to containers. `Tab` cycles the
  kind filter (all, services, containers), so either filter can be
  cleared.
- **First load before the screen.** The service list and the container
  list load before the alternate screen opens (proto-logs measured about
  20 ms on a real install). Then:
  - `sb logs` with no services and no containers prints `No matching
    systemd services or Docker containers found.` and exits 0 (sb-go's
    message, extended to both kinds; logs.go:1089-1091).
  - `sb docker logs` with no containers prints `No containers found.`
    and exits 0 (sb-go: `No running containers found.`).
  - A failed service list ends `sb logs` with host's `failed to list
    systemd services: <cause>` in cli-core's error block, exit 1.
  - An unreachable Docker daemon ends `sb docker logs` as `sb docker ps`
    does (docker spec, "Errors"), exit 1. `sb logs` opens without
    containers, with the Containers section showing one notice row:
    `Docker unreachable: <cause>`.
- **Sections** (log-picker.md; proto-logs), in this order, each with its
  heading row, which the cursor skips:
  1. **Needs attention:** every source whose status has `attention` set.
     No cap (decided.md A-Q8). A source leaves it as soon as its status
     no longer has `attention` (A-Q9), while a container restarted by
     Docker less than 10 minutes ago keeps it, so a crash loop stays
     visible.
  2. **Services:** sb-host's `services(filters, token)` (host spec,
     "systemd services", with the motd spec's cancellation amendment)
     with the default filters: `saltbox_managed_*` plus `docker`,
     disabled leftovers included, `not-found` units dropped.
  3. **Containers:** running and stopped, from `connect::list` (docker
     spec; sb-go lists running ones only).

  A source under Needs attention is also under its kind. Empty sections
  are left out. Sources are sorted by name within a section.
- **Rows:** `name  <symbol> <status text>`, the name in the kind's role
  (`Service`, `Container`), the status in its role (`Ok`, `Problem`,
  `Warn`, `Idle`). The words carry the meaning without colour.
- **Service status** (proto-logs `classify_unit`, moved here as pure
  functions; host assumption 4 puts the picker's rule in `tui`):

  | Facts | Symbol and text | Role | Attention |
  |---|---|---|---|
  | active `failed` | `✗ failed 2h 5m ago` (`✗ failed` without a time) | Problem | yes |
  | sub `auto-restart` | `↻ restarting` | Problem | yes |
  | `active`/`running` | `✓ running · 2d 3h` (`✓ running` without a time) | Ok | no |
  | `active`/`exited` | `✓ finished` | Ok | no |
  | `active`/other sub | `✓ <sub>` | Ok | no |
  | `activating` | `◌ starting` | Warn | no |
  | `deactivating` | `◌ stopping` | Warn | no |
  | `inactive`, unit file `disabled`, no timer run due | `! disabled · not cleaned up` | Warn | yes |
  | `inactive` with a timer's next run | `✓ scheduled · next run in 3h` | Ok | no |
  | `inactive` otherwise | `○ stopped` | Idle | no |
  | any other active state | `◌ <state>` | Warn | no |

  "Ago" is now minus `StateChangeTimestamp` (an amendment to the host
  spec, below). Durations use host's `format_duration`, so the picker and
  motd write times alike.
- **Container status:** sb-docker's picker status (an amendment to the
  docker spec, below), built on `ps::status`, the module's one
  classification: `ps`'s text and role, with the time and restart
  details added (`✗ exited (code 3) · 2h ago`, `↻ running · restarted
  3× · up 4m`).
- **Live updates** (log-picker.md; proto-logs):
  - Services are read again every 5 s while the picker shows. A failed
    refresh keeps the last list and shows `couldn't refresh services:
    <cause>` in the footer until a refresh succeeds.
  - Containers are kept live by bollard's `events` (type `container`).
    An event marks the list stale; the next 4/s tick reloads it with
    `connect::list`, so a burst costs one reload. If the event stream
    ends or fails, the footer says `Docker events stopped; retrying`,
    and sb reconnects every 5 s, reloading once it is back.
  - The selection stays on the same source across reloads and filtering
    (by kind and name); if that source is gone, on the row now at its
    place.
- **Keys:** ↑/k and ↓/j move; PgUp/PgDn and Home/End jump; Enter opens the
  source; `/` starts the name filter (a ratatui-textarea line: Enter keeps
  it, Esc clears it), smart-case substring as the viewer's search; `Tab`
  cycles the kind filter; `q` and Esc (with no filter active) quit, exit
  0; Ctrl-C quits, exit 130.
- **Mouse** (map `tui` row; proto-facts' recorded layout rects plus
  `ListState::offset()`): the wheel moves the selection; a click selects
  a row, a click on the selected row opens it; clicks on headings are
  ignored.

### The viewer

- **Opening** a source shows its newest 500 lines, following (proto-logs;
  `journalctl -f`'s habit). Each open starts from the tail.
- **Lines:** `2026-11-13T10:04:05+0100 <origin>: <message>`, local time
  to the second (journalctl's `short-iso` without the host name;
  decided.md A-Q7). The origin is:
  - for a service, journalctl's: `SYSLOG_IDENTIFIER`, else `_COMM`, with
    `[SYSLOG_PID]`, else `[_PID]`, when either is set;
  - for a container, the stream: `stdout`, `stderr`, or `tty` for a
    container with a TTY.

  `t` hides and shows the time and origin; shown by default (sb-go,
  logs.go:1150). Local time comes from `/etc/localtime` through jiff's
  `TimeZone::tzif` (config decision 7); without it, UTC with `+0000`.
- **Text path** (research decision 10; journal spec, "Display"):
  - A message's bytes are cut to 64 KiB first; a cut line ends with the
    notice `… [N bytes not shown]` in the Muted role.
  - The bytes go through `DisplayText::from_bytes`, then ansi-to-tui
    into spans. A message with newlines becomes several screen lines of
    one entry.
  - Under `ascii`, `NO_COLOR` and `--color=never`, lines are built with
    colour off. Under `ansi256` and `ansi`, every RGB colour in a log
    span goes through `anstyle-lossy`, as the palette does (ADR-002
    decisions 3 and 5).
  - SGR 8 (conceal) and blink are kept (journal decision 2).
- **Viewer notices** are a variant of the source's events, never text in
  a log line (journal spec, "Display"). They draw as their own rows in
  the Muted role (errors in Problem):
  - `--- start of log ---` when nothing older matches;
  - `--- end of log ---` when nothing newer matches and the view isn't
    following;
  - `--- following ---` while following;
  - `container stopped; waiting for it to start again`, and `container
    started` when it does;
  - page and follow errors, each with its cause.
- **Scrolling** moves the view by screen rows, with no selection cursor
  (proto-logs):
  - ↑/k, ↓/j, the wheel (3 rows), PgUp/`u`, PgDn/`d`, Home/`g`, End/`G`.
  - Scrolling up pauses following; End, `G` or `f` resumes it, and so
    does reaching the bottom.
  - `w` toggles wrap, on by default. With wrap off, ←/→, Shift+wheel and
    a horizontal wheel scroll 8 columns, and the header shows the column.
  - Wrapped heights come from `Paragraph::line_count` (ratatui
    `unstable-rendered-line-info`, ADR-001), measured only for the rows
    the window needs, never the whole buffer (proto-logs' fix).
- **Paging:** within 5 screens of the oldest loaded line, with more
  older, the viewer asks for the page before it (sb-go's prefetch,
  logs.go:44-50); within 5 screens of the newest, with more newer and not
  following, the page after it. One request at a time per direction.
- **The buffer** holds at most 20,000 entries (sb-go) and 32 MiB of
  message bytes, whichever comes first. Past either, the end far from the
  view is dropped; the dropped side then has more again. Dropping the
  newest end stops following.
- **Search** (decided.md A-Q5): `/` opens a ratatui-textarea line. Enter
  searches the loaded buffer, smart-case (case-sensitive once the query
  has an upper-case letter), substring, over the plain text the screen
  shows (`DisplayText` with colour off, research decision 10). Every
  match is highlighted: in the `SearchHit` role under truecolor and
  `ansi256`, in reverse video under `ansi` and with colour off (decision
  23). `n` and `N` jump to the
  next and previous match and pause following; a search while following
  starts at the newest match (proto-logs' fix). The header shows `3/17`.
  Esc clears the search. The query resets when another source opens.
- **Follow:** `f` toggles it. Turning it on with the newest line not
  loaded loads the tail first.
- **Pasteable output** (decided.md A-Q3):
  - `s` saves the loaded buffer as plain text (colour off; time and
    origin as `t` shows them) to a new file in the run log's directory
    (`/var/log/sb`, root-only 0700, or `SB_LOG_DIR`), named
    `logs-<kind>-<name>-<YYYYMMDDTHHMMSSZ>.txt`, created with
    `create_new` and mode 0600. The name keeps only `[A-Za-z0-9_.@-]` of
    the source's name, others become `_`. The viewer then shows a notice:
    `saved the N lines loaded in the viewer to <path>. Review it for
    domains, IPs and tokens before sharing it.` (`1 line` for one). A
    failed save shows its cause.
  - `p` leaves the screen and writes the loaded buffer, plain, to stdout,
    then the same review line to stderr, and exits 0.
  - Both write only what is loaded: the notice says so, and PgUp loads
    more first. No automatic redaction (journal spec, Scope: its own
    decision). OSC 52 copying is later.
- **Leaving:** Esc goes back to the picker (the source's stream is
  stopped and its follow freed); `q` quits, exit 0; Ctrl-C quits, exit
  130.

### Sources

- **Services** (the journal adapter, research decisions 6 to 8):
  - One `Journal::open(Source::Local, release)` per run, under
    `spawn_blocking`; one query per source, `Journal::unit_query` (an
    amendment to the journal spec), so every page and the follow use the
    same query.
  - The tail is `Position::Tail` with n = 500; older is
    `Before(oldest_chosen)`, newer `After(newest_chosen)`, through
    sb-host's `journal::page`.
  - `Limits::with_output(4 MiB)` per page and per drain, not the 256 MiB
    default (Task 11 audit; san-I4).
  - **The cycle guard:** a forward page whose records are all already
    held adds nothing and ends forward paging, shown as `end of log`
    (journal Difference 20).
  - **Follow:** `follow(q, &end)` from the end of the page that holds the
    newest entry (the tail, or a forward page with no more newer), then
    the `FollowWait` loop with `drain` (journal spec, "Waiting for new
    entries"). A waited read is also retried on the 4/s tick (follow-L1).
  - An inotify limit at follow (`OpenError::Inotify` or `Watch`) is a
    notice, `can't follow: <cause>; f retries`; the view stays paged
    (follow-L4).
  - Read errors of a page or batch: the first shown as a notice with the
    count of the others, every one in the run log.
- **Containers** (the docker adapter; decided.md A-Q2): sb-docker's log
  API (an amendment to the docker spec), whose positions are a
  nanosecond timestamp plus an ordinal among entries with that same
  timestamp:
  - the tail: `tail=500`, no follow;
  - older: time windows with no tail, widened until the page is full or
    the window reaches the container's creation (then `start of log`);
  - newer: from the newest position's second, entries at or before it
    dropped, stopped after 500;
  - follow: `follow=true` from the newest position's second, entries at
    or before it dropped. When the stream ends because the container
    stopped, the viewer says so and waits for a `start` event for that
    name, resolving the name again (a re-created container has a new
    ID), then follows from the last position (research decision 19).
- **The log-source interface** (journal spec, Objective; research
  decision 7) is a message protocol, defined in `sb-tui`: each open
  source is a task that takes requests (`Tail`, `Older`, `Newer`,
  `Follow(bool)`) and sends events (`Page { lines, more_older,
  more_newer, end }`, `Followed(lines)`, `Notice`), each tagged with the
  source's generation, so events from a source the user left are dropped
  (proto-logs). A line carries its time, origin, message bytes and an
  opaque position. The journal and docker adapters live in `sb-tui`,
  because the map's arrows point down (`tui` above `docker` and `host`).
  A channel, not a trait: the viewer then runs against a fake source
  task in tests, and the two kinds need no shared async trait object.
  The plan settles names with agent-skills:api-and-interface-design;
  the names settled (M66: `Page { kind, lines, oldest, newest,
  more_older, more_newer }` in place of `end`) are in "Verification
  notes".

### The inline fallback (decided.md A-Q4)

- When stdout isn't a terminal (`sb logs | grep x`, `sb logs > file`),
  the picker draws on `/dev/tty`. After Enter, the screen closes and the
  chosen log streams plain to stdout: the newest 500 lines, then new ones
  as they come, as `journalctl -fu` and `docker logs -f` do in Saltbox's
  docs (docs/reference/logs.md:62-135). Time and origin are shown; colour
  follows `--color` (off by default, as any piped output).
- The stream ends on Ctrl-C (cli-core's interrupted ending, death by
  SIGINT, `$?` 130), SIGTERM, or when the reader goes away (a broken
  pipe: cli-core's quiet exit 141). A stopped container is followed
  across its restart, as in the viewer; the notices go to stderr.
- With no terminal at all (no `/dev/tty`): cli-core's error block, `sb
  logs needs a terminal to choose a log`, exit 1. Nothing goes to stdout.

### Terminal, signals and the run log

- **Setup** follows ratatui's documented path: `ratatui::init` and
  `ratatui::restore` (https://docs.rs/ratatui/0.30.2/ratatui/fn.init.html)
  with mouse capture through crossterm's `EnableMouseCapture`. ratatui's
  panic hook chains with cli-core's, so a panic restores the terminal and
  still reaches the run log (hardening Task 17). On `/dev/tty` (the
  inline fallback) the same steps run on a `CrosstermBackend` over the tty
  file, because `ratatui::init` writes to stdout (a "why not" comment).
- **The event loop** is ratatui's async recipe
  (https://ratatui.rs/recipes/apps/terminal-and-event-handler/): a task
  reads crossterm's `EventStream` in `tokio::select!` and sends events on
  an unbounded channel; rendering runs on a 60 fps interval, only when
  something changed; a 4/s tick runs beside it (proto-logs' "root cause of
  the whole series of input bugs").
- **Leaving the screen** drains input for 150 ms after mouse capture is
  turned off, so late mouse codes don't reach the shell (proto-logs
  measured 600 bytes leaked without it, 0 with it). No documentation
  covers it: marked UNVERIFIED in the code, with that measurement.
- **Ctrl-C** is a key in raw mode: the screen closes and sb exits 130
  quietly, as sb-go (8ececcc). **SIGTERM and SIGHUP** close the screen
  through cli-core's restore hook (`Shutdown::on_forced_exit`, writing to
  the tty's own fd) and sb dies by the signal (cli-core). cli-L5 (a
  requested exit code swallowing a real error) gets its test here: a
  failure after Ctrl-C is still reported.
- **RLIMIT_NOFILE** (decided.md A-Q6): the logs commands raise the soft
  limit to min(hard, systemd's `HIGH_RLIMIT_NOFILE`, 262144) at start, as
  journalctl's `rlimit_nofile_bump` does (systemd v249 and v255
  src/journal/journalctl.c; the value from src/basic/def.h, checked at
  planning), through rustix `setrlimit`. A failure is logged and the run
  goes on.
- **The run log** records the command and entry filter, the counts
  listed, each source opened (kind and name), each page (direction,
  count, `more_*`, time taken), follow start and stop, every error and
  notice, saves (path and line count), and the exit. Never log content
  (capability map decision 11, "Never logged"): it can hold tokens. The
  picker's 5 s service refresh logs only a failure (when it starts or its
  cause changes), the recovery after it, and each service whose state
  changed, was added or went (maintainer, 2026-11-14; "Verification
  notes").

### Help texts

`sb logs` and `sb docker logs` keep sb-go's short texts, so the command
contract test passes unchanged ("Display logs of managed systemd
services"; "Display logs of Docker containers"). The long texts describe
the picker: `Shows Saltbox's systemd services and Docker containers, and
the log of the one you choose.` (`sb docker logs`: `... starting with
the Docker containers.`). A snapshot test pins them.

### Differences from sb-go (the complete list)

1. **One picker for both kinds:** `sb logs` lists containers too, and
   `sb docker logs` opens the same picker filtered to containers, a
   filter the user can clear (decided.md A-Q1).
2. **Stopped containers are listed** (sb-go: running only,
   dockerLogs.go:1092-1100), and the empty message reads `No containers
   found.`
3. **Needs attention** and the status words, symbols and roles above
   (sb-go: `✓ active/running • 2h 15m` per row, colour by a fixed code).
   Disabled leftover units are listed (host difference 6).
4. **Live picker:** services re-read every 5 s, containers by Docker
   events (sb-go: loaded once).
5. **A name filter** in the picker (sb-go disables the list's filter,
   logs.go:1126-1131) and a kind filter (`Tab`).
6. **Mouse** on both screens (sb-go: none).
7. **One line format:** local time to the second with the offset, no
   host name, journalctl's identifier and PID for services, the stream
   for containers (sb-go: the host name and `_SYSTEMD_UNIT` for
   services, UTC with nanoseconds for containers; logs.go:1018-1033,
   dockerLogs.go:656-664).
8. **The viewer opens following;** scrolling pauses it and reaching the
   bottom resumes it (sb-go: not following, and scrolling disabled while
   following).
9. **Wrap,** on by default, with `w` (sb-go: horizontal scroll only),
   and 8 columns per sideways step (sb-go: 10).
10. **Search** with highlighted matches (sb-go: none).
11. **Save and print-and-quit** (sb-go: no way to get the text out).
12. **Each open starts at the tail** (sb-go keeps a source's buffer for
    re-entry, logs.go:265-303).
13. **New lines arrive by sb-journal's follow and bollard's follow
    stream** (sb-go: `journalctl --cursor` and Docker `since` polling
    every 500 ms).
14. **Exact paging:** sb-journal's `more_*` flags and the cycle guard
    (sb-go: "at least 499 returned"); Docker pages back past the newest
    500 lines (sb-go's `until` with `tail` returns nothing there, the
    moby defect in the research) and never drops a line that shares the
    cursor's timestamp (sb-go drops all of them).
15. **No 64 KiB line limit on container logs** (sb-go fails the whole
    fetch); lines over 64 KiB are cut with a notice, journal lines too.
16. **A container's follow survives its restart** (sb-go: the stream
    ends).
17. **The inline fallback** when stdout isn't a terminal (sb-go draws its
    screen regardless).
18. **The soft RLIMIT_NOFILE is raised** as journalctl does (sb-go's Go
    runtime raises it to the hard limit).
19. **Errors** are cli-core's error block before the screen and notices
    on it (sb-go: `Error: failed to fetch logs: ...` in the viewer, bare
    text before it); malformed records are reported, not skipped
    silently (logs.go:1016-1019).
20. **Long help texts** describe the picker.

## Scope

- **In (this part):**
  - the `sb-tui` crate and the workspace entries for ratatui, crossterm,
    ansi-to-tui and ratatui-textarea;
  - the picker, the viewer, the two adapters and the inline fallback;
  - `sb logs` and `sb docker logs` in sb;
  - the amendments below, in their modules: sb-host's `services(filters, token)`
    with `StateChangeTimestamp`; sb-docker's `Row` fields, picker status, logs
    and events; `Journal::unit_query`;
  - the size ADR, deny.toml entries, the library-check hook entry and
    `MUTATION_TESTERS`;
  - a platform note on the Docker daemon's log reads (the probe below).
- **Later in this module, own sections and plans:**
  - **Editors:** `sb edit` and `sb fact` are specified in this spec's
    section "Editors: `sb edit` and `sb fact`" below, planned
    separately.
  - The announcements viewer (tui-markdown, tui-scrollview), with `ops`'
    `update`: it takes a list of `(repo, date, title, message)`, and has
    an inline form (tui-markdown's output on stdout, each announcement
    under its heading, the message through `DisplayText`) for stdout
    that isn't a terminal (ops spec, `update`, decisions 24 and 26).
- **Later for the viewer:** regex search; searching the whole journal
  (sb-journal's "budgeted, resumable search" slice); OSC 52 copy
  (crossterm `osc52`); journal priorities and unit events from
  sb-journal's later slices; light themes.
- **Not doing:** a source named on the command line; a Recent section;
  a merged multi-source view; the last error line per problem
  (log-picker.md, "Not Doing").

## Tech Stack

Versions checked 2026-11-13; "lock" means already in Cargo.lock (through
the prototypes, which are workspace members).

| Crate | Version | Why | Docs | Lock |
|---|---|---|---|---|
| `ratatui` | `=0.30.2`, `default-features = false`, features `crossterm_0_29`, `layout-cache`, `underline-color`, `unstable-rendered-line-info` | Screens; `Paragraph::line_count` for wrapped heights (ADR-001, a comment at each call site). No default `all-widgets`: the calendar widget pulls `time` | https://docs.rs/ratatui/0.30.2/ratatui/ , https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.line_count | yes |
| `ratatui-core` | `=0.1.2`, feature `anstyle` | `From<anstyle::Color>` for the palette and lossy-converted log spans (map decision 10), the version ratatui 0.30.2 uses (ADR-001 decision 1) | https://docs.rs/ratatui-core/0.1.2/ratatui_core/ | yes |
| `ratatui-widgets` | `=0.3.2` (through ratatui) | `List`, `ListState::offset()` for mouse rows | https://docs.rs/ratatui-widgets/0.3.2/ratatui_widgets/list/struct.ListState.html | yes |
| `crossterm` | `0.29.0`, feature `event-stream` | Raw mode, alternate screen, mouse, async `EventStream`; reads `/dev/tty` when stdin isn't a terminal | https://docs.rs/crossterm/0.29.0/crossterm/event/struct.EventStream.html | yes |
| `ansi-to-tui` | `8.0.1` (ratatui org) | SGR in log text to `Text` (on ratatui-core) | https://docs.rs/ansi-to-tui/8.0.1/ansi_to_tui/ | yes |
| `ratatui-textarea` | `0.9.3` (ratatui org), default features (`crossterm`) | The filter and search lines; reviewed in proto-logs. 0.9.4 (2026-10-31) is under two weeks old | https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/ | yes |
| `bollard` | `=0.21.1` (workspace) | `Docker::logs`, `Docker::events` | https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html#method.logs , https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html#method.events | yes |
| `jiff` | `0.2.38` (workspace) | RFC 3339 nanosecond parsing, local time through `TimeZone::tzif` | https://docs.rs/jiff/0.2.38/jiff/tz/struct.TimeZone.html#method.tzif | yes |
| `anstyle-lossy` | `1.1.5` (workspace) | RGB to 256 and 16 colours for log spans (ADR-002) | https://docs.rs/anstyle-lossy/1.1.5/anstyle_lossy/ | yes |
| `rustix` | `1.1.6` (workspace, `process`) | `setrlimit` for RLIMIT_NOFILE | https://docs.rs/rustix/1.1.6/rustix/process/fn.setrlimit.html | yes |
| `tokio` | `1.54.1` (workspace: `rt`, `time`; sb-tui adds `sync`, `macros`) | The loop's channels and `select!`, `spawn_blocking` | https://docs.rs/tokio/1.54.1/tokio/macro.select.html | yes |
| `futures-util` | `0.3.34` (workspace) | `StreamExt::next` on `EventStream` and bollard's streams | https://docs.rs/futures-util/0.3.34/futures_util/stream/trait.StreamExt.html | yes |
| term-ui, cli-core, host, docker | (workspace) | `DisplayText`, the theme's roles, `Shutdown`, exit codes, the run log; the service list and journal; the container list and logs | the module specs | — |

Tests only: `insta` with ratatui's `TestBackend`
(https://docs.rs/ratatui/0.30.2/ratatui/backend/struct.TestBackend.html);
`portable-pty` 0.9.0 and `vt100` 0.16.2 (both in the lock, through exec
and term-ui's `testing`) for the PTY smoke tests; sb-journal's and
sb-docker's `testing` features (crafted journal files, the fake Engine
API).

Rejected: `arboard` (no display on a server over SSH; OSC 52 later);
`tui-input` (single maintainer, and ratatui-textarea is already
reviewed); `regex` for search now (A-Q5: later, as a toggle).

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-tui -p sb-host -p sb-docker -p sb
Live:    SB_LIVE_DOCKER=1 SB_LOG_DIR=<scratch> systemd-run --scope -p MemoryMax=2G -p CPUQuota=200% cargo test -p sb-docker --test live
Probe:   the daemon probe in docs/platform-notes/docker-logs.md, against a throwaway container
Try:     the production build of sb, as root, SB_LOG_DIR set, in a real terminal: sb logs
```

## Project Structure

```
crates/tui/                sb-tui
  src/lib.rs               the two entry points (`logs`, `inline`)
  src/term.rs              terminal setup and restore, the event loop
  src/picker.rs            sections, rows, filters, selection, mouse
  src/status.rs            the service status rule (pure)
  src/viewer.rs            the buffer, windowed scrolling, paging, keys
  src/line.rs              line format, the record cap, spans, colour
  src/search.rs            smart-case matches
  src/save.rs              save and print
  src/source.rs            the source protocol
  src/source/journal.rs    the journal adapter, the cycle guard
  src/source/docker.rs     the docker adapter, follow across restarts
  src/testing.rs           a fake source task (`testing` feature)
crates/host/src/systemd.rs services(filters, token) (host spec), StateChangeTimestamp
crates/docker/src/ps.rs    Row's new fields, the picker status
crates/docker/src/logs.rs  positions, tail, older, newer, follow
crates/docker/src/events.rs container events
crates/journal/src/lib.rs  Journal::unit_query
crates/sb/src/main.rs      `logs`, `docker logs`
crates/sb/tests/logs*.rs   end to end on crafted journals and the fake Engine API
docs/platform-notes/docker-logs.md  the daemon probe
docs/decisions/            the size ADR
```

Gate bookkeeping (research decision 20):
- deny.toml: `sb-tui` with `wrappers = ["sb"]`; `sb-tui` added to the
  wrappers of `sb-cli-core`, `sb-term-ui`, `sb-host` and `sb-docker`.
- `.claude/hooks/library-check.sh`: `tui` added to its crate list
  (CLAUDE.md, Library first).
- check.sh's `MUTATION_TESTERS`: `sb` joins `sb-journal`'s entry
  (ADR-012 follow-up); `sb-tui`'s entry is `sb-tui sb`.

## Code Style

As docker's and host's: the rules are pure functions over parsed values,
tested first; the adapters wire libraries to them; no `#[allow]`; every
call into an unstable ratatui API names ADR-001.

```rust
/// Smart case (decided.md A-Q5): a query with an upper-case letter is
/// matched exactly, any other ignoring case. `plain` is what the screen
/// shows (`DisplayText` with colour off).
fn matches(plain: &str, query: &str) -> Vec<Range<usize>> {
    if query.chars().any(char::is_uppercase) {
        plain.match_indices(query).map(|(at, m)| at..at + m.len()).collect()
    } else {
        caseless_matches(plain, query)
    }
}
```

## Testing Strategy

Nothing touches the developer's host: no real journal, no real daemon,
no real systemctl.

- **Unit (most), in sb-tui:**
  - the service status rule, every row of its table, written failing
    against a stub first (proto-logs' tests move with it);
  - sections: Needs attention, sorting, empty sections, the kind and
    name filters, selection kept across reloads;
  - the line format (both kinds, `t` off, no time zone file, a message
    with newlines, a record over 64 KiB with its notice, a tab-heavy
    record), the colour profiles (no named colour under truecolor, no
    RGB under `ansi256`, nothing under `ascii`);
  - search: smart case, several matches in one line, a match across a
    span boundary, highlights, n/N, reset on a source switch;
  - the viewer against the fake source task: paging requests near both
    edges, the buffer caps by entries and by bytes, follow paused by
    scrolling and resumed at the bottom, notices, events of an old
    generation dropped;
  - the save file's name and mode, `create_new` refusing an existing
    file, the text written.
- **Snapshots:** `TestBackend` and insta for the picker (every status,
  every section, the filter, the Docker-unreachable row) and the viewer
  (wrap on and off, markers, search hits, a cut record), at 120×40 and
  80×24. A test checks that the selected row is one solid bar (proto-logs'
  test).
- **Frame cost:** a test counts the lines measured per frame on a buffer
  of 20,000 records of 5,000+ characters and asserts it doesn't grow with
  the buffer (proto-logs' "frame-time budget check"); a by-hand release
  timing at 240×70 is recorded in the size ADR, with a 5 ms budget per
  frame.
- **The journal adapter** on crafted journal files (sb-journal's
  `testing` writer, `Source::Directory`): tail, older and newer pages,
  `start` and `end` of log, the cycle guard on journal Difference 20's fixture
  (`forward_paging_cycles_where_the_order_does`), a follow that receives
  appended entries, an inotify failure through the testing seam, read
  errors as notices.
- **The docker adapter** on the fake Engine API, extended to serve
  `/containers/{id}/logs` (multiplexed frames, a TTY container's raw
  stream, a line over 64 KiB, entries sharing one timestamp, a log of
  several thousand lines to page back through) and `/events`: tail,
  older pages to the start, newer pages, follow, the stream ending on
  `die` and resuming on `start`, a re-created container with a new ID.
  The fake applies `tail` before `since` and `until`, as moby does, so a
  regression to sb-go's paging fails.
- **sb-host's `services(filters, token)`** as the host spec tests it: systemctl JSON
  fixtures from 249, 255 and 26.04's systemd, with `StateChangeTimestamp`
  in the `show` fixture.
- **End to end** (the built sb as root, `SB_TEST_ROOT`, `SB_LOG_DIR` in
  the test's directory, a stand-in `systemctl` first on `PATH`, crafted
  journal files through `SB_JOURNAL_DIR`, the fake Engine API through
  `DOCKER_HOST`):
  - on a PTY (portable-pty, the screen read through vt100): the picker's
    rows, Enter opens a log, an appended entry appears while following,
    Esc, `q` exits 0, Ctrl-C exits 130, the terminal restored (alternate
    screen left, raw mode off: the PTY's termios read back), no mouse
    codes reach the shell after `q`;
  - SIGTERM and SIGHUP on the screen: the terminal restored, death by
    the signal;
  - piped stdout: the picker on the PTY as `/dev/tty`, then the plain
    stream on the pipe, Ctrl-C ending it with 130, a closed reader
    ending it with 141;
  - no terminal (`setsid`, stdout a pipe): the error block, exit 1;
  - no services and no containers: the message, exit 0; a failing
    `systemctl`: the error block, exit 1;
  - `p`: the buffer on stdout, the review line on stderr, exit 0;
  - the run log holds the sources and pages and none of a log's content
    (a known marker string in a crafted entry never reaches it);
  - `--help` texts (snapshot); not root: the fake-sudo relaunch.
- **The daemon probe** (by hand, against a throwaway container, recorded
  as a platform note with Docker's version and the method): that `tail`
  applies before `since`/`until`; the order of entries with equal
  timestamps across reads; how a line over 16 KiB arrives with
  `timestamps=true`; that `until` is inclusive.
- **Input latency** (groundwork): key to screen change on the PTY,
  release build, against the harness's own baseline, recorded in the
  size ADR (proto-logs: about 14 ms on multi-thread).
- **Bars** (CONSTRAINTS.md): changed lines ≥ 90%, mutation ≥ 80%, the
  journal matrix after the `unit_query` change, `sb --help` and `sb
  version` within the 10 ms budget.

## Boundaries

- **Always:** bytes to the screen only through `DisplayText`; the
  documented ratatui and crossterm patterns, cited in code; read only
  from Docker and the journal; run sb as root only with `SB_LOG_DIR` in
  `target/` or a temporary directory; synthetic fixtures.
- **Ask first:** any dependency beyond the Tech Stack; any difference from
  sb-go not listed above; writing anywhere but the run log's directory
  and stdout; any write to Docker or systemd.
- **Never:** log a log's content; run `journalctl` or `docker`; commit
  real unit or container names, log lines or screenshots from a real
  host; restart real containers in a test; touch the Saltbox repository.

## Success Criteria

1. `sb logs` and `sb docker logs` open the picker on all kinds and on
   containers respectively, and `Tab` clears the filter (PTY test).
2. Every row of the service status table and of sb-docker's picker
   status has a test that failed against a stub first; the words alone
   tell the states apart (a snapshot with colour off).
3. Needs attention lists exactly the sources with `attention`, a
   container restarted 9 minutes ago stays and one restarted 11 minutes
   ago leaves, with no cap.
4. A container stopping or starting moves in the picker within one
   second of its event on the fake Engine API, without input; a service
   change appears within 6 s.
5. The viewer pages back to `start of log` and forward to `end of log`
   on a crafted journal and on the fake Engine API's multi-thousand-line
   log, with every entry exactly once (no gap, no duplicate, including
   entries sharing one timestamp), and stops on journal Difference 20's
   cycle.
6. A followed service shows an appended journal entry; a followed
   container shows a new line, then the stop notice on `die`, then new
   lines after `start`, also for a re-created container.
7. No escape sequence other than SGR reaches the screen (crafted entries
   with OSC, DCS, CSI cursor moves and 8-bit C1); a 1 MiB record shows
   64 KiB and its notice.
8. The frame-cost test holds, and the recorded release frame time at
   240×70 is within 5 ms.
9. `s` writes a 0600 file in the log directory with the loaded lines and
   shows the review notice; `p` writes them to stdout.
10. Piped stdout streams the chosen log plain after the picker on
    `/dev/tty`; with no terminal, the error, exit 1.
11. `q` exits 0, Ctrl-C 130, SIGTERM and SIGHUP by the signal, each with
    the terminal restored and no mouse codes left in the shell; a panic
    restores the terminal and is in the run log.
12. The run log records sources, pages and errors, and never a log's
    content.
13. The soft RLIMIT_NOFILE is raised in the logs commands only (a test
    reads it back in the child).
14. `scripts/check.sh full` passes. Binary size is measured and the size
    ADR records ratatui's and crossterm's cost; the production binary
    stays under the 45,711,520-byte ceiling.

## Decisions

1. **`sb logs` opens the picker on all kinds, `sb docker logs` filtered
   to containers, clearable** (decided.md A-Q1; journal decision 8).
2. **Docker positions are time windows with no tail,** a nanosecond
   timestamp plus an ordinal among equal timestamps, after a daemon probe
   (decided.md A-Q2).
3. **Save and print-and-quit, no automatic redaction, OSC 52 later**
   (decided.md A-Q3). The file goes in the run log's directory, already
   root-only (logrotate platform note), so it needs no directory of its
   own.
4. **The inline fallback runs when stdout isn't a terminal;** the picker
   draws on `/dev/tty`; with no terminal at all it fails clearly
   (decided.md A-Q4; capability map `tui` row).
5. **Search is smart-case substring with highlighted matches over the
   loaded buffer, reset on a source switch; regex later** (decided.md
   A-Q5).
6. **RLIMIT_NOFILE is raised as journalctl does, in the logs commands
   only** (decided.md A-Q6; journal decision 12's "match journalctl").
7. **One local-time line format without the host name; no Needs
   attention cap; a recovered source leaves at once, the 10-minute
   restart rule kept** (decided.md A-Q7 to Q9; shown to the maintainer as
   screenshots, plan decision 13).
8. **One picker, three sections, live updates, mouse** (log-picker.md;
   proto-logs' review, 2026-10-29; capability map decisions 3 and 4).
9. **The service status rule lives in `tui`,** as host assumption 4
   says; the container rule extends `ps::status` in sb-docker, as the
   docker spec says ("Later in this module").
10. **The log-source interface is a message protocol in `sb-tui`,** with
    both adapters there (research decision 7; the map's dependency
    direction). Names are settled with api-and-interface-design at
    planning.
11. **The cycle guard is the journal adapter's** (research decision 8;
    journal Difference 20).
12. **A 4 MiB output limit per page, a 64 KiB record cap, a buffer of
    20,000 entries and 32 MiB** (research decision 9; the Task 11 audit's
    condition, san-I4). The figures are checked by the fixture and
    frame-cost tests.
13. **Ctrl-C exits 130, `q` 0; SIGTERM and SIGHUP restore and die by the
    signal; ratatui's panic hook chains with cli-core's** (research
    decision 14; sb-go 8ececcc; hardening Task 17).
14. **The event loop is ratatui's async recipe on the current-thread
    runtime, journal reads under `spawn_blocking`** (research decision 12;
    capability map decision 8).
15. **Docker follow survives a restart by the `start` event, re-resolved
    by name** (research decision 19; log-picker.md's assumption that the
    event stream is enough).
16. **Follow-L1 and follow-L4 are handled in the viewer** (research
    decision 16): a waited read retried on the tick; inotify exhaustion a
    notice with `f` to retry.
17. **`Journal::unit_query` is added** (page-L5; journal plan decision 10
    named it, decision 18 deferred it for want of a consumer), and the
    adapter owns one query per source, so `follow` always gets its page's
    query.
18. **sb-go's short help texts stay** (capability map decision 1: the
    contract test compares them); the long texts describe the picker.
19. **`SB_JOURNAL_DIR` replaces the local journal with a directory,** read
    as root only and dropped by the sudo relaunch, for tests and
    debugging, as docker's `SB_DOCKER_CONTROLLER_URL` (docker spec,
    Differences). systemctl is replaced by a stand-in on `PATH`, as
    docker's tests do.
20. **The first load happens before the screen,** so the empty case
    prints sb-go's message and exits 0 (proto-logs measured 20 ms, under
    log-picker.md's 150 ms budget).
21. **Gate bookkeeping:** deny.toml, the library-check hook,
    `MUTATION_TESTERS`, and a size ADR when ratatui and crossterm enter
    the binary (research decision 20; ADR-004, 007 and 009 precedent). The
    ratchet rows wait for the complete binary (capability map, "When the
    binary is complete").
22. **No `-v`:** the run log has the detail (research decision 21; config
    decision 1).
23. **Search matches are reversed under 16 colours and with colour off**
    (the maintainer, 2026-11-14). Under `ansi` the
    `SearchHit` pick, yellow, turned a yellow span inside a match yellow
    on yellow; under `ascii`, `NO_COLOR` and `--color=never` the role has
    no colour, so matches weren't lit at all. Both now patch reverse video
    (ratatui's `Modifier::REVERSED`) over each span's own colours; under
    truecolor and `ansi256` a match keeps the `SearchHit` background. With
    colour off (anstream's choice for stdout, as sb's other output reads
    it) the screens take the plain theme, so lines are built with colour
    off as "Text path" says, also under `--color=never`, which crossterm
    doesn't know.

## Known limits

- **Docker positions assume a stable order of equal timestamps** across
  reads, which the probe checks. An entry the daemon wrote out of time
  order (stdout and stderr copied by separate goroutines) can land on the
  wrong side of a page boundary; moby's own `since`/`until` filter has the
  same one-sidedness (research, section 2).
- **Lines Docker splits at 16 KiB** show as separate rows, each with its
  own timestamp, unless the probe shows they can be joined reliably.
- **A TTY container's line has no length bound in bollard's decoder**
  before sb's cap; the daemon is root's own socket and the log is bounded
  by Saltbox's 30 MiB, so it is a note, not a threat. The decoder also
  looks for the newline from its buffer's start at each chunk, so a long
  line without one (a `\r`-only progress bar) costs time quadratic in its
  length, and the screen waits while it is read (M73's security pass).
- **An older Docker page holds its whole time window** before it is cut
  to 500 lines (sb-docker's `logs::older`): bounded by what the
  container logged in the window, at most Saltbox's 30 MiB of rotated
  log (M73's security pass).
- **Save and print give the loaded part only;** a complete log needs
  paging back first.
- **Services have no live signal:** a change shows within the 5 s
  refresh (no D-Bus dependency, proto-logs).

## Editors: `sb edit` and `sb fact`

This section of the spec covers the module's two editing commands:
`sb edit` (a picker, or one of six subcommands, then the user's
`$EDITOR`) and `sb fact` (the full-screen fact editor). `tui` comes after
`config` in the capability map's build order. `sb edit <file>` needs no
ratatui and comes first. The picker and the fact editor build on the
`tui` crate's base (terminal init and restore, the event loop, the
palette), which the log viewer section also needs. Whichever section's
plan comes first builds it. The facts data layer (parser, writer, locks,
drift) is `config`'s facts half (config spec line 6). This section
specifies it because the fact editor is its only user.

Sources: research brief F (spec-research/F-editors.md, which cites sb-go
as `file.go:line`), decided.md (F-Q1, the maintainer's answer of
2026-11-13), the capability map, the config, exec, term-ui and cli-core
specs, docs/ideas/fact-editor.md and prototypes/proto-facts/FINDINGS.md.

### Assumptions

1. **sb-go's commands and files.** `edit` keeps its bare picker and its
   six subcommands (`accounts`, `adv_settings`, `backup_config`,
   `hetzner_vlan`, `settings`, `inventory`). `fact` takes no arguments.
   Help texts are sb-go's except where a difference below changes them
   (capability map decision 1).
2. **Root.** Both commands need root (cli-core decision 3). cli-core's
   relaunch keeps `VISUAL` and `EDITOR` (root.rs:76-90).
3. **The Saltbox root** is cli-core's `saltbox_dir` (`SB_SALTBOX_DIR` or
   `/srv/git/saltbox`), as validate-config uses. The facts directory is
   `appdata_path(inventory).join("saltbox")`: `server_appdata_path` from
   localhost.yml when absolute, else `/opt` (sb-go
   layout/paths.go:18-58).
4. **`sb fact` edits Saltbox's per-app INI files,**
   `<appdata>/saltbox/<role>.ini`, which Saltbox's `saltbox_facts`
   module writes
   (https://raw.githubusercontent.com/saltyorg/Saltbox/master/library/saltbox_facts.py).
   It does **not** touch `saltbox.fact`, the executable Ansible local
   fact from saltyorg/ansible-facts, which belongs to `reinstall-facts`
   and `update-venv`.
5. **The facts dialect and session are sb-go's** (facts/document.go,
   facts/session.go, facts/apply.go). The screen is the reviewed
   settings page from docs/ideas/fact-editor.md and proto-facts, not
   sb-go's tree (decision 3 of the map: redesigned, functionality kept).
6. **No real host is touched in tests.** Fact files, locks and config
   files are fixtures in temp directories. The editor is a fake script.

### Objective

A Saltbox user changes their configuration with sb and learns at once if
Ansible will refuse it. They can find, copy, change and delete the
settings their apps generated, without reading INI files by hand. Neither
command can corrupt a file Saltbox owns or race Saltbox while it writes.

#### `sb edit` and `sb edit <file>`

- **Files**, relative to the Saltbox root (sb-go layout/layout.go:13-21),
  in the picker's order (sb-go cmd/edit.go:175-205):

  | Picker title | Shown name | Subcommand | Path |
  |---|---|---|---|
  | Accounts | accounts.yml | `accounts` | accounts.yml |
  | Settings | settings.yml | `settings` | settings.yml |
  | Advanced Settings | adv_settings.yml | `adv_settings` | adv_settings.yml |
  | Backup Settings | backup_config.yml | `backup_config` | backup_config.yml |
  | Hetzner VLAN Settings | hetzner_vlan.yml | `hetzner_vlan` | hetzner_vlan.yml |
  | Inventory Settings | localhost.yml | `inventory` | inventories/host_vars/localhost.yml |

  `motd.yml` and `providers.yml` stay out, as in sb-go.
- **Picker** (`sb edit` with no subcommand). A ratatui list screen with
  sb-go's prompt line, "Select a Saltbox configuration file to edit:",
  each item's title and shown name, and a key hint line. Keys: Up/Down
  and `k`/`j` move, Enter opens, `q` and Esc quit with exit 0. Ctrl+C
  quits with 130 (cli-core's `Shutdown`). Mouse: click selects, double
  click or Enter opens, and the wheel moves. Click targets come from the
  rects recorded while drawing (the map's `tui` row). It needs a terminal
  on stdin and stdout and refuses before drawing without one: `sb edit
  requires an interactive terminal; use sb edit <file>`, exit 1. On
  Enter, the picker leaves the alternate screen, turns the mouse off,
  drains pending input (the proto-facts and proto-logs lesson), and only
  then runs the editor. sb ends when the editor and the validation below
  end (sb-go cmd/edit.go:62-87).
- **Target checks** (sb-go `editorCommand`, cmd/edit.go:101-133). The
  path is followed through symlinks. A missing file fails with
  `configuration file does not yet exist: <path>`. A directory, FIFO or
  other non-regular file fails with `configuration path is not a regular
  file: <path>`. A symlink to a regular file is accepted (sb-go
  edit_test.go:47-57). The path is escaped for display (cli-core's rule).
- **Editor choice** (sb-go `preferredEditor`, cmd/edit.go:135-155):
  - The first non-empty one of `VISUAL` and `EDITOR`, split with
    `shlex::split` (quotes and backslashes; no shell, so no aliases,
    `$VAR` or `~`). A split that fails reports `invalid VISUAL environment
    variable` (or `EDITOR`), without echoing the value.
  - With neither set, the first of `sensible-editor`, `editor`, `nano`
    and `vi` that starts. exec's spawn error with `io::ErrorKind::NotFound`
    moves to the next name. With none found: `no editor found; set VISUAL
    or EDITOR to an executable editor`.
  - A named editor that doesn't start fails with `find editor "<program>":
    <cause>` (sb-go faf9887's wording).
  - The arguments are the variable's extra words, then the path.
- **Run.** exec's `Cmd::new(program)`, each extra word and the path as
  `value(..)` (shown as `[ARG]` in the log), run with `attached(&shutdown)`:
  sb's stdin, stdout and stderr, the inherited environment, nothing
  captured. A cancellation sends SIGTERM, then exec's grace period. A
  non-zero exit fails with `error opening editor: <exit status>`, exit 1,
  and skips the validation (the editor reported its own failure; `:cq`
  in vim is a deliberate abort).
- **Validation after the editor exits** (F-Q1, the maintainer's answer
  of 2026-11-13):
  - The edited file alone is validated with validate-config's exact
    rules: the YAML pre-pass (ADR-022), the parse, duplicate keys, then
    the file's schema (localhost.yml: duplicates and Ansible's acceptance
    only). It goes through sb-config's per-file step,
    `run::validate_file(step, settings, file, Checks::Skip)`, built on
    `check_file`, the one per-file check motd uses too (Amendments,
    below), so `sb edit` accepts a file exactly when `sb validate-config`
    does.
  - The credential network checks (Cloudflare, Docker Hub) are skipped.
    accounts.yml's structure errors are still reported, because they are
    rule issues. Its step gets an info notice: `credential checks skipped;
    sb validate-config runs them`.
  - Output is term-ui's runner, titled `sb edit`, with validate-config's
    step for that file ("Validating settings.yml" → "Validated
    settings.yml", failing as "Validation of settings.yml" with its
    issues in the failure block). The runner starts only after the editor
    has exited, because exec refuses `attached` while a runner draws.
  - **Clean:** exit 0.
  - **Issues:** the runner finishes, then term-ui's confirm prompt asks
    `Edit <shown name> again?` (default no). Yes runs the editor on the
    same file again, then validates again, as many times as the user
    says yes. No exits 1. The file stays as the editor saved it either
    way.
  - **Non-interactive answer:** `--no-edit-again` (on `edit` and each
    subcommand) reports the issues and exits 1 without asking. With no
    terminal and no flag, term-ui's prompt rule applies: an error naming
    `--no-edit-again`, exit 1, never a hang (term-ui criterion 10).
  - Ctrl+C at the prompt or during validation: 130, terminal restored.
- **Logging** (map decision 11): the file chosen, the editor's program name,
  its exit status, whether the file's bytes changed (compared in memory,
  never logged), and the
  validation's issue count. Never the file's content or an issue's text
  (an issue can quote a value). The editor's own output isn't logged
  (exec `attached`).

#### `sb fact`

- **Start.** stdin and stdout must both be terminals. Otherwise it fails
  with `sb fact requires an interactive terminal for input and output`,
  exit 1, before anything is opened (sb-go cmd/fact.go). It then opens
  the facts session, and only then draws, so an open error is sb's plain
  error block, exit 1: `open fact editor: <cause>`.
- **Session** (sb-config facts, sb-go facts/session.go):
  - **Directory.** The facts directory is opened from `/` one component
    at a time with `O_NOFOLLOW|O_DIRECTORY`, so a symlink anywhere in the
    path is refused. The fd is pinned, and every later operation is
    relative to it (`openat`, `statat` with `SYMLINK_NOFOLLOW`,
    `renameat`, `unlinkat`).
  - **Editor lock.** `.fact-editor.lock` (0640) is taken with a
    non-blocking `flock`. Held elsewhere: `another fact editor is
    active`. The name is sb-go's, so sb and sb-go exclude each other.
  - **Catalog.** Every `*.ini` file is read with `O_NOFOLLOW|O_NONBLOCK`,
    up to 1 MiB each (cli-core's cap). It must be a regular file and keep
    its dev and inode while it is read. One bad file fails the open,
    naming it (`catalog <role>: <cause>`). Roles are sorted, and the
    `DEFAULT` section is hidden.
  - **Role lock, at Save only** (decision 38). `<role>.ini.lock`, the
    lock Saltbox's `saltbox_facts` module takes ("shared with the
    Saltbox CLI"): created 0640, an exclusive `flock` polled every 50 ms
    for up to 30 s, which the user can cancel from a "waiting for
    Saltbox" dialog. It is taken for each role with staged changes when
    the user saves, in sorted role order, and released right after that
    role's commit (or its failure). Staging a change takes no lock, so
    an open `sb fact` never blocks a Saltbox run.
  - **Drift, checked at Save.** Once the role is locked, its bytes are
    compared with the snapshot taken at open. If they differ, or the
    file is gone, a "Before / On disk" dialog (tui-scrollview) offers
    Reload, which adopts the disk version and drops that role's staged
    changes for the user to make again, or Cancel, which keeps them
    staged and writes nothing for that role. Either way the role's lock
    is released before the dialog's answer is acted on, and the save
    carries on with the next role (decision 41).
- **Dialect** (sb-go facts/document.go, read as Saltbox's
  `configparser` reads it where the two differ, decision 39):
  - Input is UTF-8 without NUL. CRLF files stay CRLF. A carriage return
    without a line feed makes the file refuse to open (`catalog <role>:
    carriage return without a line feed at line <n>`): Saltbox reads the
    file with universal newlines, where a lone CR ends a line.
  - `#` starts a comment, Saltbox's only comment prefix. A comment or
    blank line ends a multi-line value. A line indented deeper than its
    key continues the value, even one that starts with `;`, as Saltbox's
    configparser reads it. Indents are counted in characters, and
    whitespace is Python's `str.isspace`.
  - A `;` line that doesn't continue a value (one at column 0, say) is a
    comment, as in sb-go, whose older writer produced them.
  - A malformed section, a duplicate section or a duplicate key is an
    error. Keys are split at the first `=`, so keys may contain brackets.
  - **Untouched lines are kept byte for byte.** An edited key keeps its
    own `key =` spacing. Multi-line values are written as `\n\t`
    continuations (Saltbox's writer's form). A comment always ends a
    value, so a value's lines hold no comment to keep.
  - A value with a line that has leading or trailing whitespace, or an
    empty inner line, is refused: "cannot be represented without loss".
    CR and NUL in a value are refused.
  - Every edit is checked by parsing the result again. Each set value
    must read back exactly, and every other fact must read as it did
    before. An edit that would change any other fact is refused with
    `Merged` ("the changes cannot be written without altering other
    facts in the file"). sb-go re-checks only the values it set.
- **Changes** (sb-go facts/apply.go): set a fact (change a key, or **add
  a key to an existing copy**), delete a fact, delete a copy (an INI
  section), delete an app (unlinks `<role>.ini`). Conflicting changes are
  refused: an edit under something deleted, or the same key twice.
  Renaming, new apps and new copies stay out, as in sb-go. Every change
  is checked before any role is locked (`Session::check`, which the
  screen also calls before staging), so a refused change writes nothing.
- **Save** (apply), per role in sorted order: take the role lock,
  check drift, prepare, commit, release the lock.
  - **Prepare:** check the role against its baseline, then create
    `.<role>-<rand>.tmp` relative to the pinned fd with
    `O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600, `fchown` it to the current
    file's owner, then `fchmod` it to that file's mode (owner first,
    because chown can clear setuid and setgid bits). Then write and
    `fsync` it.
  - **Commit:** check the baseline again (the bytes and the lock file's
    identity), `renameat` over the file, `fsync` the directory, then
    release the lock. There is no rollback. A failed role's temp file is
    unlinked and its lock released.
  - **Result:** each role is one of (decision 41):
    - **Applied:** its changes are on disk (or changed nothing).
    - **Unsynced:** its changes are on disk, but the directory sync
      failed, so they may not survive a crash. They count as applied
      (saving again is harmless). The save stops there (sb-go's "both
      Applied and Failed").
    - **Drifted, reloaded or kept:** the file changed on disk since the
      open and nothing was written for it. Reloaded drops its staged
      changes; kept leaves them staged. The save carries on with the
      next role.
    - **Failed,** with its cause. The save stops there.
    - **Unattempted:** not tried, because an earlier role failed or was
      unsynced, the lock wait was cancelled, or the save was stopped.
    The outcome dialog lists them, and unapplied changes stay staged in
    the changes panel.
- **Screen** (docs/ideas/fact-editor.md, proto-facts' reviewed design):
  - Each app is a heading with its settings beneath it and their values
    visible. Apps with several copies get a heading per copy.
  - A search box (ratatui-textarea) narrows the page as you type and
    opens matching apps. The page opens expanded when it fits, otherwise
    folded to app names (threshold settled in the manual check).
  - Buttons along the bottom, each showing its key and clickable: Edit,
    Add, Copy, Show/Hide, Delete/Keep, Save changes, Quit.
  - The "Unsaved changes" panel is always visible, with an undo per
    change. It sits on the right at 100 columns or more and at the bottom
    below that.
  - Delete works on a setting, a copy or an app. A note explains that
    Saltbox creates a new value on its next run.
  - Add asks for a key name, then its value (ratatui-textarea), under the
    selected copy. It refuses, before staging, a key that exists, an
    empty key, leading or trailing whitespace, `=`, a leading `#`, `;` or
    `[`, and control characters, newlines among them (sb-go's
    `validateKey`, Saltbox's reader's limits). Brackets inside a key are
    accepted, as sb-go and Saltbox accept them (`headers[Authorization]`;
    decision 40).
  - Dialogs (edit, add, save, quit with unsaved changes, outcome) use
    tui-popup. The right-click menu is drawn by sb, because tui-popup
    can't place a popup at the cursor (proto-facts FINDINGS). Every menu
    action has a key, and `m` or the Menu key opens the menu on the
    selected row.
  - Quit with staged changes opens the save dialog: Save, Discard or
    Cancel (sb-go's Apply/Discard/Return).
  - Mouse: click selects, the wheel scrolls, right-click opens the menu.
    Shift-drag is left to the terminal for native selection.
- **Secrets:**
  - A value is masked (dots of a fixed length) when its key name contains
    `pass`, `secret`, `token`, `key` or `api`, in any case (proto-facts
    `looks_secret`). Facts have no schema, so validate-config's secret
    paths don't apply. Masking too much is harmless.
  - **Show** reveals the selected value only, until the selection moves.
    **Copy** sends it through crossterm's OSC 52 `CopyToClipboard`
    without showing it. The status line says "if your terminal allows it;
    otherwise use Show", because success can't be confirmed.
  - The edit box for a masked value opens with the text hidden until
    Show is pressed in it.
  - Values are shown through term-ui's `DisplayText`, so control
    characters are escaped (as sb-go's `display()`).
  - **Never logged:** any fact value, masked or not (map decision 11 names
    "fact values"). The log records role, copy, key and change kind,
    lock waits, drift, and each role's outcome (the Result line above).
- **Exit codes:** 0 on quit (with or without a save), 1 on an open error
  or no terminal, 130 or 143 on Ctrl+C or SIGTERM with the terminal
  restored. A Ctrl+C with staged changes discards them, and an
  interrupted save finishes the role it is committing before it stops.

### Differences from sb-go (the editors; continuing the list above)

21. `sb edit` validates the edited file after the editor exits, with
    validate-config's rules and the credential checks skipped, and offers
    `Edit <file> again?`. A file with issues exits 1. sb-go doesn't
    validate and exits 0 (cmd/edit.go has no call).
22. `--no-edit-again` is new (the prompt's non-interactive answer).
23. The edit picker refuses to start without a terminal, with a message
    naming `sb edit <file>`. sb-go's bubbletea fails later on its own.
24. The picker is a ratatui list with mouse support (sb-go: bubbles `list`,
    keyboard only).
25. A cancelled editor gets SIGTERM, then exec's grace period. sb-go's
    `exec.CommandContext` sends SIGKILL at once (eadeca1).
26. An invalid `VISUAL` or `EDITOR` isn't echoed in the error (sb-go
    includes go-shellquote's error text).
27. `sb edit`'s help text drops "Editor wrappers should exec the foreground
    editor so cancellation can stop it", because SIGTERM reaches a wrapper
    that forwards it. It gains a line on the validation and
    `--no-edit-again`.
28. `sb fact` is the settings page (headings, search, buttons, changes
    panel, right-click menu) instead of sb-go's tree with an inspector.
    Its keys change with it. The help text's "Changes are staged for review
    before applying" becomes "Changes are listed in the changes panel until
    you save".
29. Credentials in facts are masked by key name, with per-value Show and
    Copy. sb-go shows every value in plain text (factui/view.go:37-49).
30. Fact files over 1 MiB fail the open, naming the file (sb-go reads them
    whole).
31. Copy to the clipboard (OSC 52) is new.
32. `sb fact` takes a role's `<role>.ini.lock` only while saving that
    role, checks for on-disk changes then, and releases the lock after
    the role's commit (decision 38). sb-go takes it at the role's first
    staged change and holds it until the editor closes
    (facts/session.go:288-380), so a concurrent Saltbox run fails on
    that role after 30 s.
33. An unquoted word that starts with `#` in `VISUAL` or `EDITOR` starts
    a comment: shlex 2.0.1 drops it and the rest of the line, so
    `EDITOR='vi #x'` runs `vi` alone. go-shellquote has no comments and
    passes `#x` to the editor. Quoted (`'#x'`) or inside a word (`a#b`),
    `#` is kept by both.
34. `sb edit`'s help text drops sb-go's "GUI clients may need a wait
    option and a working root GUI session": Saltbox runs on Ubuntu servers
    without a GUI (the maintainer, 2026-11-15).
35. An indented `;` line right after a value continues the value, as
    Saltbox's configparser reads it (its only comment prefix is `#`).
    sb-go reads it as a comment. A `;` line that doesn't continue a
    value is a comment in both (decision 39, 2026-11-15).
36. A fact file with a carriage return not followed by a line feed
    refuses to open, naming the line. sb-go reads the CR as part of the
    line; Saltbox's universal newlines end the line there (decision 39,
    2026-11-15).
37. An edit that would change how any other fact reads is refused with
    `Merged`. sb-go re-checks only the values it set (decision 39,
    2026-11-15).

### Scope

**In:** the edit picker, the six subcommands, editor choice and run, the
post-edit validation and prompt; `sb fact` with its session, dialect,
apply, screen, masking and mouse; sb-config's facts half and per-file
validation API.

**Later or out:**
- `motd.yml` and `providers.yml` in `edit` (a new feature, the
  maintainer's call).
- Renaming keys, new apps, new copies (sb-go has none).
- A "show all secrets" toggle (the idea's "Not Doing").
- Editing a temp copy visudo-style (F-Q1 option C, not chosen).

### Tech Stack

All of these are in Cargo.lock already, through the prototypes or other
crates. Each is confirmed at source in its task.

| Concern | Crate | Version | Why | Docs |
|---|---|---|---|---|
| Screens | `ratatui` (+ `unstable-rendered-line-info`, ADR-001) | 0.30.2 | stack table | https://docs.rs/ratatui/0.30.2/ratatui/ |
| Terminal, mouse, clipboard | `crossterm` with `event-stream`, `osc52` | 0.29.0 | stack table; `CopyToClipboard` | https://docs.rs/crossterm/0.29.0/crossterm/clipboard/struct.CopyToClipboard.html |
| Text inputs (search, edit, add) | `ratatui-textarea` | 0.9.3 | every input uses it (proto-facts, the Ctrl+H lesson) | https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/ |
| Dialogs | `tui-popup` (ratatui org) | 0.7.7 | proto-facts' verdict; **not in the stack table yet** (Amendments) | https://docs.rs/tui-popup/0.7.7/tui_popup/ |
| Long dialog text (drift, outcome) | `tui-scrollview` | 0.6.8 | stack row "small content" | https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/ |
| `VISUAL`/`EDITOR` split | `shlex` | 2.0.1 | stack row, replaces go-shellquote | https://docs.rs/shlex/2.0.1/shlex/fn.split.html |
| Locks and dir-relative I/O | `rustix` `fs` | 1.1.6 | `flock`, `openat`, `statat`, `renameat`, `unlinkat`, `fchown`, `fchmod`, `fsync`; already in the workspace | https://docs.rs/rustix/1.1.6/rustix/fs/fn.flock.html |
| Temp file names | `fastrand` | 2.5.0 | tempfile's own generator, already a workspace dependency | https://docs.rs/fastrand/2.5.0/fastrand/ |
| Prompt ("Edit again?") | `dialoguer` (term-ui's prompts) | 0.12.0 | term-ui's prompt row; **not in Cargo.lock**, added with term-ui's confirm, which `release` builds first (self-update) | https://docs.rs/dialoguer/0.12.0/dialoguer/struct.Confirm.html |
| Tests | `insta` + `TestBackend`; `portable-pty` for PTY smoke | 1.49.0 / 0.9.0 | already in sb | https://docs.rs/ratatui/0.30.2/ratatui/backend/struct.TestBackend.html |

Why not (recorded in the code, library-first rule):
- **tempfile's `Builder::tempfile_in(path)`** for fact saves: it resolves
  the path again, which loses the pinned, symlink-free directory fd. sb
  creates the temp file with `openat(O_CREAT|O_EXCL)` on the pinned fd
  instead. tempfile stays the choice where no fd is pinned.
- **rust-ini 0.21.3 and configparser 3.3.0:** both regenerate the file
  and drop comments (proto-facts FINDINGS). The line-preserving parser
  and writer are domain code.
- **cap-std / cap-tempfile:** they follow symlinks inside the directory,
  where sb-go refuses every one, and are a new dependency family for what
  rustix does already.
- **`which`** for the editor fallback: exec resolves `PATH` at spawn, and
  its not-found error moves to the next name. No lookup crate is needed.
- **The right-click menu** is drawn by sb: tui-popup's `move_to` works
  only after a first render and its size is frozen after that
  (proto-facts FINDINGS).

The `edit` log's "changed" field compares the file's bytes before and
after in memory (read up to cli-core's 1 MiB cap; a larger file logs
"unknown"). No hash crate is needed.

### Commands

```
Check: scripts/check.sh fast | task | full
Test:  systemd-run --scope -p MemoryMax=8G -p CPUQuota=400% cargo test -p sb-config -p sb-tui -p sb
Try:   the production build as root with SB_LOG_DIR set to a temporary directory,
       SB_SALTBOX_DIR pointing at a fixture set, and the inventory's
       server_appdata_path at a fixture facts directory
```

### Project Structure

```
crates/config/src/run.rs        check_file and validate_file: the per-file check and its step (Amendments)
crates/config/src/facts/        sb-config's facts half
  document.rs                   parser and line-preserving writer
  session.rs                    pinned dir, editor and role locks, catalog, drift
  apply.rs                      changes, prepare, commit, outcome
  secret.rs                     is_secret_key (the masking list)
crates/config/tests/fixtures/facts/  synthetic fact files (CRLF, continuations, comments)
crates/sb/src/edit.rs           sb edit: target checks, editor choice, run, validation, prompt
crates/tui/src/                 sb-tui (base shared with the log viewer)
  picker.rs                     the edit picker: returns the chosen file, draws nothing after
  fact/                         the fact editor screen: page, panel, dialogs, menu
crates/sb/tests/edit.rs         fake-editor tests through the binary
crates/sb/tests/fact.rs         PTY smoke test
```

The picker returns the chosen file and restores the terminal. sb's
`edit.rs` then runs the editor, so `sb edit <file>` and the picker share
one path and sb-tui never spawns a process.

### Code Style

```rust
/// The editor `VISUAL` or `EDITOR` names, split as a shell would split
/// words (no expansion; sb-go cmd/edit.go:135-155):
/// https://docs.rs/shlex/2.0.1/shlex/fn.split.html
/// `None` when neither is set, so the fallback list applies.
fn chosen_editor(visual: Option<&str>, editor: Option<&str>) -> Result<Option<Vec<String>>, EditError> {
    let Some((name, value)) = [("VISUAL", visual), ("EDITOR", editor)]
        .into_iter()
        .find_map(|(name, value)| value.filter(|v| !v.trim().is_empty()).map(|v| (name, v)))
    else {
        return Ok(None);
    };
    // The value isn't echoed: it can carry a path or a token.
    shlex::split(value)
        .filter(|words| !words.is_empty())
        .map(Some)
        .ok_or(EditError::InvalidVariable(name))
}
```

### Testing Strategy

No test touches a real Saltbox checkout, fact directory or terminal.
Root-only tests are gated by `SB_TEST_ROOT=1` and set `SB_LOG_DIR` under
the test's directory.

- **Editor choice (unit):** sb-go's edit_test.go cases. `VISUAL` before
  `EDITOR`, empty values skipped, quoted arguments, backslashes, a bad
  quote (and no echo of it), the fallback order with a fake `PATH` of
  stub scripts, none found.
- **Target checks (unit, temp dirs):** missing, a directory, a FIFO, a
  symlink to a regular file accepted, a dangling symlink, a control
  character in the path escaped.
- **Through the binary with a fake editor** (a shell script in a temp
  `PATH`, which records its argv and writes chosen bytes):
  - argv is the extra words, then the path;
  - `SB_SALTBOX_DIR` fixture sets: a clean edit exits 0; an edit that
    breaks the YAML shows the file's failure block and, with
    `--no-edit-again`, exits 1; without a terminal and no flag, the
    error names the flag;
  - accounts.yml with credentials: the notice says the checks were
    skipped, and a local fake HTTP server (sb-config's `testing::Fake`)
    sees no request;
  - a non-zero editor exit: `error opening editor`, exit 1, no
    validation step;
  - the run log holds the file, program, status, "changed" and issue
    count, and none of a known secret written into the file.
- **"Edit again?" on a PTY** (`portable-pty`): yes reruns the fake
  editor, which fixes the file the second time, then exit 0; no exits 1.
  Ctrl+C at the prompt: 130 and the terminal restored.
- **Root relaunch:** `EDITOR` survives sudo's relaunch (cli-core's
  existing relaunch test, extended with `VISUAL` and `EDITOR`).
- **Cancellation:** SIGTERM to sb while the fake editor sleeps: the
  editor gets SIGTERM, sb exits 143.
- **Picker:** `TestBackend` and insta snapshots at 80 and 40 columns;
  key and mouse events through the event handler (no terminal); a PTY
  test checks that the alternate screen and mouse capture are off and
  no mouse bytes leak before the fake editor starts.
- **Facts dialect (unit):** round-trip tests ported from sb-go's
  facts/*_test.go (CRLF, continuations, comments between keys, brackets
  in keys, `;` comments and indented `;` continuations, the lossless
  refusals, CR and NUL, a lone CR, duplicates, `DEFAULT`), checked
  against Saltbox's own reader (python3 configparser with
  saltbox_facts.py's settings). A property test: parse then write
  with no changes gives the same bytes. Files Saltbox's own writer
  produces (tab continuations, `True`/`False`) are fixtures, written by
  hand from saltbox_facts.py's rules.
- **Session (temp dirs):** a symlink in any path component refused; the
  editor lock held by a second session; a role lock held by a test
  holding `flock` as the Python module would, released after 100 ms
  (wait succeeds) or never (the 30 s timeout made injectable, so the
  test takes milliseconds); cancel while waiting; drift by changed bytes
  and by a deleted file; reload and release.
- **Apply (temp dirs):** owner and mode kept (run as root, with the
  fixture file owned by `nobody` and 0640); setuid not kept through
  chown; temp file 0600 and gone after a failure; sorted commits;
  baseline changed between prepare and commit gives Failed plus
  Unattempted; the directory fsynced (checked by the call order through
  a seam, not by crashing a disk).
- **Fact screen:** `TestBackend` and insta snapshots of the page, the
  folded page, search with and without matches, the changes panel at
  110 and 80 columns, every dialog, and the menu. Masking: a snapshot
  with secret keys shows dots, and no rendered buffer contains a known
  secret until Show. OSC 52: the emitted payload decodes to exactly the
  value.
- **Log:** a known fact value in a fixture never appears in the run log
  after browsing, Show, Copy, edit and save.
- **PTY smoke** (`portable-pty`): open, search, edit, save and quit
  against fixtures; the file has the intended bytes; the terminal is
  restored with no stray mouse bytes after `q`.
- **Manual real-terminal check** (by hand, two terminal emulators):
  mouse use, Shift-drag native selection under mouse capture, wide
  characters after overwrites, and the fold threshold. The maintainer
  reviews the screen before a verdict counts.

### Boundaries

- **Always:** keep untouched fact lines byte for byte; take Saltbox's
  `<role>.ini.lock` before writing a role; keep owner and mode; keep
  every fact value out of the log; validate through sb-config's one code
  path; restore the terminal before the editor starts.
- **Ask first:** adding `motd.yml` or `providers.yml` to `edit`; a new
  crate outside Cargo.lock; a term-ui API change beyond the prompts;
  changing the masking list; any change to how sb takes Saltbox's lock.
- **Never:** write a YAML config file from sb (the editor writes it);
  touch `saltbox.fact`; follow a symlink in the facts directory; run the
  credential network checks from `edit`; read or change a real Saltbox
  checkout or fact directory in tests or reviews.

### Success Criteria

15. `sb edit <sub>` runs the editor from `VISUAL`, then `EDITOR`, then the
    fallback list, with argv as specified, for each of the six files;
    each sb-go edit_test.go case passes.
16. The target checks fail with sb-go's texts for a missing file, a
    directory and a FIFO, and accept a symlink to a regular file.
17. After the editor exits 0, the file is validated: a fixture that
    validate-config accepts exits 0, and one it refuses shows the same
    issues as `sb validate-config` for that file (the same issue lines,
    compared in a test) and exits 1 with `--no-edit-again`.
18. No network request is made from `sb edit accounts`, with
    credentials set (fake server sees none), and the skip notice shows.
19. "Edit again?" reruns the editor on yes and exits 1 on no (PTY). With
    no terminal and no flag, the error names `--no-edit-again`.
20. A non-zero editor exit gives `error opening editor`, exit 1, and no
    validation.
21. The picker: snapshot matches; keys and clicks select and open; `q`
    exits 0; Ctrl+C exits 130; no terminal refuses with the stated
    message; no mouse bytes reach the editor or the shell (PTY).
22. `sb fact` without a terminal fails before opening anything.
23. Every ported sb-go dialect and apply case passes; unchanged files
    round-trip byte for byte; a written continuation uses `\t`.
24. The session refuses a symlinked path component; a second editor gets
    `another fact editor is active`; no role lock is held while changes
    are only staged (a test holding the lock as Saltbox's module would
    gets it at once); at Save a held role lock is waited for, times out,
    or is cancelled, and is released after the role's commit; drift
    shows its dialog.
25. Owner and mode are kept on save; a failed role leaves no temp file;
    outcomes are reported as Applied, Unsynced, Drifted (reloaded or
    kept), Failed and Unattempted.
26. Adding a key to an existing copy writes it, and the limits refuse
    bad names before staging.
27. Secret-looking keys render masked; Show reveals one value; Copy's
    OSC 52 payload is the value; no known fact value is in any log.
28. The manual two-emulator check is done and the maintainer has looked
    at the picker and the fact editor.

### Decisions (the editors; continuing the list above)

23. **The commands keep sb-go's names, the six subcommands and the bare
    picker,** with paths relative to the Saltbox root. Source: decided by
    recorded decisions (brief F D1; capability map decision 1).
24. **The picker is a ratatui list, then `$EDITOR`,** redesigned with
    mouse via recorded rects. The terminal is restored before the editor
    starts. Source: brief F D2 and D3 (the map's `tui` row and open item;
    sb-go 3cd52b3; exec's `attached` refusal).
25. **The editor is chosen as sb-go does,** split with shlex, run through
    exec's `attached` with the path as a `value`. A cancellation sends
    SIGTERM and the grace period. Source: brief F D3 (map decision 1; stack
    table's shlex row; exec spec).
26. **Target checks and their texts are sb-go's.** Source: brief F D4.
27. **The picker and `sb fact` refuse to start without a terminal,
    before opening anything; `sb edit <file>` leaves it to the editor.**
    Source: brief F D5 (term-ui criterion 10).
28. **The editors never write YAML; validation goes through
    validate-config's code path.** Source: brief F D6 (plan decision 38;
    ADR-022).
29. **`sb edit` validates the one file after the editor exits,** with
    validate-config's exact rules, the credential network checks skipped,
    the issues reported, and "edit again?" with `--no-edit-again` as its
    non-interactive answer. Source: the maintainer's answer of
    2026-11-13 (decided.md F-Q1).
30. **A file with issues makes `sb edit` exit 1** once the user declines
    to edit again. Source: follows from decision 29 and term-ui's runner
    (a failed step's command exits 1, term-ui criterion 4).
31. **The fact editor is the reviewed settings page.** Source: brief F D7
    (docs/ideas/fact-editor.md; proto-facts' verdict; map decision 4).
32. **Secrets in the fact editor are masked by key name; no fact value is
    ever logged.** Source: brief F D8 (term-ui decision 8; map decision
    11).
33. **The facts data layer is sb-config's facts half, with sb-go's
    session semantics:** pinned directory fd, `.fact-editor.lock`,
    Saltbox's `<role>.ini.lock` at 50 ms for up to 30 s (taken at Save
    only, decision 38), drift review, baseline recheck, sorted commits,
    fsync, and temp files on the pinned fd. Source: brief F D9 (map `config` row; map decision 1, on-disk
    compatibility with Saltbox's module).
34. **The INI dialect is sb-go's** (tab continuations, CRLF kept, the
    lossless check, untouched lines byte for byte). proto-facts' 4-space
    continuation is fixed. Source: brief F D10 (map decision 1).
35. **Adding a key to an existing copy is kept.** Source: brief F D11
    (sb-go factui/update.go:320,348, facts/apply.go:27; map decision 3).
    **For the maintainer:** docs/ideas/fact-editor.md's "Not Doing" says
    "Adding brand-new settings or renaming them: sb-go doesn't do this
    either". That is wrong. sb-go's `a` has added keys since b1945e4
    (2026-09-05). The idea doc needs that line corrected to "renaming
    them, or adding apps or copies".
36. **`edit` logs the file, the program name, the exit status, whether
    the bytes changed and the issue count, never content.** Source:
    brief F D12 (map decision 11).
37. **The manual real-terminal check is an acceptance item of the fact
    editor's last slice.** Source: brief F D13 (map open item "Manual
    real-terminal check"; standing practice that the maintainer reviews
    the screen).
38. **`sb fact` takes `<role>.ini.lock` only at Save,** checks for
    on-disk changes then, and releases the lock after that role's commit
    (difference 32). Reason: every sb command must work beside Saltbox's
    real use; sb-go's holding the lock while a change is staged makes a
    concurrent Saltbox run fail on that role after 30 s. Decided
    from recorded rules, 2026-11-13.
39. **Where sb-go's reader and Saltbox's differ, sb reads a fact file as
    Saltbox's configparser does** (saltbox_facts.py's settings): an
    indented `;` line right after a value is part of that value, a lone
    CR refuses the file, indents count characters and whitespace is
    Python's. A `;` line at column 0 stays a comment. An edit is
    refused with `Merged` when the result would read any other fact
    differently (differences 35-37). Reason: Saltbox is the file's
    reader, so an edit sb checks against any other reading could change
    a fact unseen. Decided after the code review,
    2026-11-15.
40. **Add refuses only a leading `[` among brackets;** brackets inside
    a key are accepted (`headers[Authorization]`), as sb-go's
    `validateKey` and Saltbox's reader accept them. The earlier text,
    "refuses `[` and `]`", was wrong; M55 builds to this one. Decided
    from sb-go and Saltbox's reader, 2026-11-15.
41. **The save's outcomes are Applied, Failed and Unattempted, plus
    Drifted (reloaded or kept) and Unsynced** (the Result line). A
    drift writes nothing for that role, and the save carries on with
    the next role; a failure or an unsynced role stops it. Unsynced is
    sb-go's "both Applied and Failed" under its own name. Decided after
    the code review, 2026-11-15.

### Known limits

- OSC 52 copy can't confirm success. The status line says so and points
  to Show.
- A commit across several roles has no rollback. A failure leaves the
  earlier roles applied, and the outcome dialog says which.
- A `;` line that doesn't continue a value (at column 0, say) is a
  comment to sb, as to sb-go. Saltbox's module refuses such a line, or
  reads it as a key when it holds an `=`. A file sb saves never gains
  one: sb writes no comments. An indented `;` line right after a value
  is read as Saltbox reads it (decision 39).
- Someone who can mount a FUSE file system with `allow_other` over a
  role file can make `sb fact` hang while it reads that file. That
  needs the Saltbox user, who usually has sudo anyway, so sb doesn't
  guard against it (2026-11-15, after the security
  review).
- The masking list is a guess by name. A secret under another name (for
  example `cookie`) shows until it is added to the list.
- `sb edit` validates the file after it is saved. A broken file is on
  disk until the user fixes it (F-Q1's option B, chosen over C).
- Drift is found at Save, perhaps long after the change was staged; the
  drift dialog then reloads (dropping that role's staged changes) or
  keeps them staged (decision 38).

## Verification notes

- **Input latency (groundwork, M61, 2026-11-13):** a key to the screen's
  change on a PTY, release build, current-thread runtime: a median of 9.2
  to 11.2 ms more than the harness's baseline over three runs, within one
  60 fps frame (proto-logs: about 14 ms on multi-thread). Measured by
  `crates/tui/tests/term.rs`'s `input_reaches_the_screen_within_a_few_frames`;
  the method and the binary's growth are in
  [ADR-023](../decisions/ADR-023-tui-binary-size.md).
- **The source protocol's names (M66, 2026-11-13, settled with
  agent-skills:api-and-interface-design):** `sb_tui::source` holds
  `Request` (`Tail`, `Older(Position)`, `Newer(Position)`,
  `Follow(bool)`), `Event` (`Page { kind, lines, oldest, newest,
  more_older, more_newer }`, `Followed(lines)`, `Notice`), `Line { time,
  origin, message, position }` with an opaque `Position`, `Generation`,
  `Tagged { generation, event }`, an `Outbox` per source and a
  `SourceHandle` whose drop aborts the task. The spec's `end` field
  became `kind` (`Tail`, `Older`, `Newer`): a journal page's `PageEnd`
  stays in the adapter, which follows from it, and the viewer needs to
  know which request a page answers. `oldest` and `newest` are the
  page's edges (`oldest_chosen`, `newest_chosen`), which can lie past
  the lines at an entry the page couldn't show, so `Older` and `Newer`
  carry the viewer's edge, not a line's. `Follow(true)` with no page
  holding the newest entry since the last follow reads the tail first.
- **The wake-up path (M66, used by M67):** each source holds, in its
  `Outbox`, an `Arc<tokio::sync::Notify>` the `Tui` owns, and calls
  `notify_one()` after each event it sends on the viewer's unbounded
  channel. `Notify` keeps one permit, so no wake is lost and a burst
  costs one. `run_loop` gains a wake arm after input (the token, then
  input, then the wake, so a busy follow can't starve input) and calls
  `App::wake() -> Flow` (default `Unchanged`), where the viewer takes
  every waiting event with `try_recv` and drops those of an old
  generation. Built in M67: `Tui::waker()` hands the `Arc<Notify>` out, and
  `Viewer::open` gives it to the source's `Outbox`.
- **The cycle guard's memory (M66):** the cursors sent by the tail,
  newer pages and follow batches, first in first out, at most 20,000,
  forgotten on `Tail` and on `Older`, and those sent after a `Newer`'s
  edge when that edge isn't the newest sent (the viewer dropped its
  newest end while following; the next follow then reads the tail
  first). Forgetting on `Older` too: paging
  back is when the viewer drops its newest end (by its 32 MiB byte cap
  before its 20,000 entries), and the newer pages that load it again
  would otherwise read as a cycle and end at a false `end of log` (the
  adapter's test `newer_pages_reach_the_end_with_every_entry_once`). A
  cycle back into entries an older page loaded shows them once more
  before the guard stops it, at most one lap.
- **The docker source and the Containers section (M69, 2026-11-13):**
  - `Notice` gains `ReadFailed(cause)` (`couldn't read the log:
    <cause>`, a page that couldn't be read: an empty page that ends
    paging that way), `Stopped` and `Started` (the spec's two container
    texts). Stopped and Started are drawn in Muted, the others in
    Problem; the follow's notices (`CantFollow`, `Stopped`, `Started`)
    go at the bottom even after an older page.
  - The adapter keeps the container's events stream open for the
    source's life (opened again after 5 s if it ends), so a restart
    quicker than the end of the log stream isn't missed: a `start` for
    the name seen while following, with no `die` after it, resumes the
    follow as soon as the stream ends. When the stream is opened again
    while waiting, the name is resolved through the container list (a
    `start` may have come while it was down, as in a dockerd restart),
    and a running container of that name is followed on.
  - A re-created container's older pages stop at the old container's
    creation when the new ID came from an event (it carries no creation
    time); the old container's log is gone with it, so this costs empty
    reads, not lines.
  - Healthcheck `exec_*` events don't reload the Containers section
    (they come on every check and change no row; `health_status`
    does).
  - When Docker was unreachable before the screen, the section stays
    that row for the run: nothing watches for the daemon's return.
  - A follow that must read the tail first and can't says
    `can't follow: <cause>; f retries` and leaves the buffer as it is;
    right after a failed tail (the viewer's opening asks for the tail,
    then the follow) it says `can't follow: the log couldn't be read; f
    retries` without reading again.
  - The Containers section's watch publishes on a tokio `watch`
    channel (only the newest list is kept while the viewer is open). A
    reload that fails keeps the last list and shows `couldn't refresh
    containers: <cause>` in the footer, as the services' refresh does.
  - `sb logs` with no services and Docker unreachable opens the picker
    with the Containers section's one row: the containers weren't
    seen, so `No matching ... found.` would not be true.
  - `sb docker logs` lists the containers only until `Tab` comes (M70).

- **The refresh's run log (maintainer, 2026-11-14):** the picker's 5 s
  read of the services runs with no tracing subscriber (tracing's
  `WithSubscriber::with_subscriber` with a `NoSubscriber`), so a refresh
  that changes nothing writes nothing: none of sb-exec's child records
  and none of sb-host's list records. The picker logs a failed read
  (`service refresh failed`; when the failure starts or its cause
  changes, next bullet) and each service whose state changed
  (`service state changed`, with all four: systemd's active state,
  sub-state, unit file state and state-change time), was added (`service
  listed`) or went (`service no longer listed`), outside the silenced
  read. A state-change time one of the two reads couldn't get (a failed
  or timed-out `systemctl show`) counts as unknown, not as a change, so
  one failed `show` doesn't log every service. The silence is a
  registered `Dispatch` of a `NoSubscriber`, built once, not
  `Dispatch::none()`, which would cache a callsite first hit inside the
  read as never enabled for the whole run. No sb-exec or sb-host API was
  added for it. Two consequences, accepted:
  - a refresh that succeeds drops its read's partial-read records
    (`systemd timers not read`, `systemd service times not read`); the
    first load before the screen is logged in full, so they appear once
    per run;
  - a task the read spawned wouldn't inherit the silence (tracing's
    default subscriber is per thread and per future); sb-host's list
    spawns none today. Nor does a record written while the read is
    dropped: the picker quit mid-read, sb-exec's group-kill record.
  `crates/tui/tests/refresh_log.rs` tests both directions, and
  `crates/sb/tests/logs.rs` counts the real read's records end to end.
- **A refresh that keeps failing (maintainer, 2026-11-14):** a failed
  read is logged (`service refresh failed`, with its cause) when the
  failure starts, or when its cause changes (the cause's text differs
  from the one last logged); a read that fails as the last one did logs
  nothing. When a read works again, one record, `service refresh
  recovered`, carries how many reads failed since the last good one
  (`failed_reads`), across cause changes. The footer still shows the
  newest cause on every failed read. `crates/tui/tests/refresh_log.rs`
  tests repeated failures, a changed cause and the recovery's count.
- **Search and sideways scrolling as built (M71, 2026-11-14):**
  - Search reads each message as the screen shows it with colour off,
    cut at the record cap, not the time and origin before it, nor the
    cut notice or the viewer's notices: journalctl's `--grep` matches the
    message alone. So `t` doesn't change what matches.
  - Smart case lowers with std's `char::to_lowercase`, each lowered byte
    mapped back to its character, so a match is a range of the shown
    text. A match across the log's colour spans is lit whole: the spans
    are split at its edges and patched with `SearchHit` as a background
    (ratatui patches whole spans only). Under `ansi`, `ascii`, `NO_COLOR`
    and `--color=never` a match is patched with reverse video instead
    (decision 23), so a log span in reverse video (SGR 7) inside a match
    looks as it did.
  - `/` opens the line on the search in effect, as the picker's filter
    does. Enter with an empty line clears the search; Esc in the line
    clears it; Esc outside it clears a search in effect first, then goes
    back to the picker.
  - "A search while following" is the view at the bottom: it starts at
    the newest match. Otherwise a search starts at the first match from
    the window's top, else the last above it. Enter moves the view only
    when the match is out of sight, so a search at the bottom keeps
    following; `n` and `N` always put the match's row at the window's top
    and pause following, and stop at either end (no wrap). A tail page
    (End, `G` or `f` with newer lines not loaded) reloads the buffer, and
    the search starts again at the newest match.
  - The header shows `/query 3/17`, `/query no matches`, or `/query
    -/17` once the current match has been dropped with its entry (the
    buffer's caps). The hints become `n/N next · Esc clear · ...` while a
    search is in effect; at 80 columns the hint row is cut at its end.
  - With wrap off, ←/→, Shift+wheel and a horizontal wheel step 8
    columns, with no right-hand stop (proto-logs); the header shows
    `wrap off · col N`; `w` resets the column. A jump to a match off to
    a side moves the column so the match sits near the window's middle,
    in steps of 8.
  - A match's row inside a wrapped entry is measured with
    `Paragraph::line_count` up to the end of its word (ADR-001); a word
    wider than the window can put it a row off (UNVERIFIED in the code).

- **Hardening as built (M73, 2026-11-15):**
  - Frame cost: `viewer/tests.rs`'s `a_frames_cost_doesnt_grow_with_the_buffer`
    counts the log lines measured per frame (`t` off and on, paging up,
    Home, wrap off and on, End) on 2,000 and on 20,000 records of 5,000+
    characters at 240×70: the same counts for both, under the test's
    bound of 2,000, and at most the lines in view for `t` at the bottom.
    The timing found a frame of such records at about 9 ms in release
    when every frame measured its window again (`Paragraph::line_count`
    wraps the whole record), so each line now keeps its height for the
    layout it was measured at (width, wrap, `t`), a frame measures only
    lines it hasn't (`a_frame_measures_only_lines_it_hasnt_measured`),
    and the paging check counts entries before it measures: each is a
    row at least, so five screens of them far from an end need no
    heights.
  - Success criterion 8 as measured ([ADR-023](../decisions/ADR-023-tui-binary-size.md)):
    every frame of the timing is within 5 ms, wrap on and off. M73
    left wrap off open: 5.6 ms a frame and 9.9 ms for the frame that
    turns it off, each building the styled text of every record in view
    (`DisplayText` over 5,000+ bytes each), the switch twice. The
    maintainer asked for 5 ms there too (2026-11-15, the follow-up
    "The viewer's wrap-off frame meets 5 ms"). Now, with wrap off, a
    line's height is its newlines counted from the bytes
    (`line::screen_lines`, `DisplayText` only when a string sequence or
    a C1 code could hide one); its text is built from only as much of
    the message as the window reaches, a screen more to the right
    (`line::within`); and each frame keeps the texts it drew for the
    next (`viewer.rs`'s `Drawn`), dropped when the format (theme, zone,
    `t`) or the search changes and built again for more rows or
    columns. The kept texts are bounded to what is on screen: one per
    entry in view, at most 4 MiB between them, outside the buffer's
    32 MiB. The frame that turns wrap off takes 3.1 ms, the other wrap-off
    frames 1.3 to 1.8 ms. `a_frames_cost_doesnt_grow_with_the_buffer`
    counts the texts built per frame too (the same on 2,000 and 20,000
    records, at most a row's worth each, and one or none for a row up
    and the steps sideways); `a_frame_builds_only_texts_it_hasnt_drawn`
    and `kept_texts_draw_what_new_ones_do` check the cache, and
    `line.rs`'s `a_window_draws_within_as_the_whole_text` and
    `screen_lines_are_the_texts_lines` the cut text and the count.
    Far to the right a wrap-off frame cost more (ratatui's
    `Paragraph::scroll` walks every character left of the column):
    4.9 ms at column 1,000, 14.2 ms at 4,000. Closed (the maintainer,
    2026-11-15): each screen line in view is handed to ratatui cut at
    the column (`columns.rs`), from marks kept with the text (each
    span's start, and a grapheme every 64 columns, or 64 graphemes of
    no width, outside printable ASCII), so a cut walks at most that far;
    a text that is the
    whole message is kept for every column. A step sideways or a row up
    takes 1.4 to 2.0 ms at columns 0, 1,000 and 4,000 alike.
    `a_frames_cost_doesnt_grow_with_the_buffer` times those columns and
    checks the texts built there match the first column's and that no
    line handed to ratatui is wider than the window and 64 columns;
    `columns.rs`'s `a_cut_draws_as_the_scrolled_line` checks the cut
    against `Paragraph::scroll` cell by cell. A frame that builds every
    text in view far right (a jump there, or a search, theme or `t`
    change there; a resize there makes the marks again) still grows
    with the column, since the message left
    of it is parsed for its styles: 5.7 to 5.8 ms at 4,000 on these
    records (17.8 before), more on CJK or colour-per-word text
    ([ADR-023](../decisions/ADR-023-tui-binary-size.md)).
  - Lines hold their caps (M73's security pass): an adapter builds each
    line with `Line::new`, which copies at most 64 KiB of the message
    and 256 bytes of the origin and counts the rest in `Line::cut` for
    the `… [N bytes not shown]` notice. A field many entries share is
    stored once in a journal file and charged once by a page's 4 MiB
    output limit, but each line held its own whole copy, so a page of
    entries sharing a large `SYSLOG_IDENTIFIER` or `MESSAGE` could hold
    gigabytes; it now holds at most 500 × (64 KiB + 256 B). The buffer's
    32 MiB counts origins as well as messages.
  - Colour profiles: `line.rs`'s `every_colour_on_a_line_fits_the_profile`
    feeds every SGR colour form (the 8, the bright 8, 256 colours, RGB, as
    foreground and background) with the cut notice and a search hit, and
    checks each profile: no RGB under `ansi256`, only the sixteen under
    `ansi`, none under `ascii`, and under truecolor sb's own colours RGB.
    The Testing Strategy's "no named colour under truecolor" is read as
    sb's own colours: a log's own named colour (SGR 31) is the user's
    terminal palette and passes as it came.
  - RLIMIT_NOFILE: `sb logs` and `sb docker logs` call
    `sb_tui::raise_open_files` first; the soft limit goes up to min(hard,
    262144) and never down, the hard limit unchanged (journalctl, as
    root, sets both; the hard limit is above 262144 on a stock Ubuntu
    host, so they agree there). The tests start sb under `ulimit -Sn 1024`
    and read its `/proc/<pid>/limits` while the picker shows.
  - follow-L1: the journal adapter drains on its own 4/s tick while
    following (tokio's `Interval`), beside the wake, the tick reset after
    every request and wake, so a read waited on gets a whole period; with
    nothing new a drain is a header read per open file. Its test writes a
    waited slot through a hard link in another directory, which the
    follow's watch doesn't see.
  - follow-L4: a follow that can't start shows its error with every
    cause (the system's error, which the notice left out before) and,
    for `EMFILE` creating the inotify instance or `ENOSPC` adding a
    watch, `the system's inotify limit (fs.inotify.max_user_instances)`
    (or `max_user_watches`) `may be reached`.

## Open questions

None. decided.md settled the brief's nine; decisions 18 to 20 follow
from recorded decisions. The editors' question (when `sb fact` takes a
role's lock) was decided on 2026-11-13 from recorded rules: decision 38.

## Amendments to other specs

The wording below was applied to those specs on 2026-11-13.

### host (docs/specs/host.md, "systemd services")

- In the record list, after "when the unit entered its active state;",
  add: "- when the unit last changed state (`StateChangeTimestamp`),
  for the log picker's "failed 2h ago" (tui spec);".
- Replace step 4 with: "4. One `systemctl show --timestamp=utc
  --property=Id,ActiveEnterTimestamp,StateChangeTimestamp` over the
  matched units, for both timestamps. `n/a`, an empty value or a missing
  property reads as unknown." *Amended 2026-11-13 (module build, M2):
  this read `--timestamp=unix`, which systemd 249 (22.04) rejects; the
  `unix` style came with v251 (systemd's v251 NEWS). The host spec's
  assumption 4 has the reason.*
- In "Failures", the second bullet becomes: "A failed `list-timers` or
  `show` leaves those fields empty (both timestamps for `show`), as
  sb-go ignores those errors."
- In Objective, "motd and tui: the systemd service list." becomes "motd
  and tui: the systemd service list; tui reads it again every 5 s while
  its picker shows."
- Decision 1's "the service list: `motd` and `tui`" gains: "built by the
  first of the tui and motd plans."

These go with the motd spec's amendment to the same section (the
caller's `CancellationToken`, and `list-units`, `list-unit-files` and
`list-timers` run concurrently before the one `show`), which this step 4
keeps: `show` still runs once, over the matched units, now for both
timestamps.

### docker (docs/specs/docker.md, "Later in this module")

Replace the two bullets "The container list for the tui log picker ..."
and "Container log streams the `tui` viewer consumes." with:

- **The picker's container list** is `connect::list` (all containers,
  already). `Row` gains, from the inspect: `restart_count`
  (`RestartCount`), `created` (`Created`), `started_at` and `finished_at`
  (`State.StartedAt`, `State.FinishedAt`; Docker's zero time
  `0001-01-01T00:00:00Z` and an unparsable value read as `None`), as
  `jiff::Timestamp`. `--json`'s keys are unchanged.
- **`picker_status(row, now)`** returns a symbol, a text, a role and an
  `attention` flag. The text is `ps::status`'s text (the one
  classification), followed by:
  - running: ` · up <duration>`; with a restart count above 0 and up
    less than 10 minutes, ` · restarted N× · up <duration>`, symbol `↻`,
    role Problem, attention;
  - exited, dead: ` · <duration> ago` from `finished_at`;
  - restarting: ` (N×)`.

  The symbol follows the role (`✓` ok, `✗` problem, `◌` warn, `○` idle),
  `↻` for restarting. Attention is role Problem, or the 10-minute rule.
  Durations are formatted by a function the caller passes (the tui
  picker passes sb-host's `format_duration`), because sb-docker doesn't
  depend on sb-host (deny.toml).
- **`logs.rs`, the log reads:** a `Position { time_ns: i128, ordinal:
  u32 }` and a `LogLine { position, stream (stdout, stderr, tty), bytes
  }`, with:
  - `tail(daemon, id, n)`: `tail=n`, `timestamps=true`, no follow;
  - `older(daemon, id, before, n, created)`: windows of `since =
    floor(t - W)` (at least 1) and `until = ceil(t)`, W starting at 60 s
    and doubling, keeping the entries before `before` and the last `n` of
    them, until `n` are found or `since` reaches `created` (then
    `more_older` is false);
  - `newer(daemon, id, after, n)`: `since = floor(t)`, the entries at or
    before `after` dropped, the stream dropped after `n + 1` (the extra
    one sets `more_newer`);
  - `follow(daemon, id, after)`: as `newer` with `follow=true` and no
    count, ending when the daemon ends the stream.

  Every read parses the RFC 3339 nanosecond prefix with jiff, filters the
  window itself too (moby's filter is one-sided), and counts ordinals
  among equal timestamps in the order the daemon sends them. A line
  without a parsable timestamp keeps the previous line's position with
  the next ordinal. Errors carry the daemon's message.
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

### journal (docs/specs/journal.md, "API (first slice)")

In the `impl Journal` block, after `open`, add:

```rust
    /// journalctl's `-u` for the release and source this journal was
    /// opened with: `Query::unit` without passing them again (page-L5).
    pub fn unit_query(&self, unit: &UnitName, coredump_uid: Option<Uid>) -> Query;
```

and after the paragraph on `UnitName`: "`Journal::unit_query` is the way
sb's callers build a unit query; `Query::unit` stays for the test tools.
A `follow`'s `from` must be the `end` of a page of the same query: the
caller keeps that rule (the tui spec's journal adapter owns one query per
source)."

### config (docs/specs/config.md; for the editors, shared with motd)

One per-file check, used by `sb edit`, by motd for motd.yml and by
validate-config itself. In "Behaviour", a new bullet after "Steps":

> **One file** (for `sb edit` and `sb motd`; tui and motd specs):
> - `check_file(settings, file) -> Result<Checked, FileError>` reads one
>   entry of `FILES` and its schema as validate-config does (cli-core's
>   `read_small` and 1 MiB cap, the YAML pre-pass, the parse, duplicate
>   keys, the schema rules and the warnings), with no network request.
>   `Checked` holds the `Report` (issues with positions, warnings, the
>   issue count) and the parsed tree, so a caller that needs the values
>   (motd's typed model) reads the file once. `FileError` is a read or
>   schema-load failure, worded as validate-config words it.
> - `run::validate_file(step, settings, file, checks)` draws
>   `check_file`'s result under the caller's step exactly as `run` does
>   for that file. `checks` is `Checks::Run` (validate-config and `sb
>   update`: accounts.yml's credential checks as concurrent sibling
>   steps) or `Checks::Skip`, which makes no request and adds the notice
>   `credential checks skipped; sb validate-config runs them` when the
>   file has credentials set. `run` calls it for each file with
>   `Checks::Run`, so the paths never differ. It returns the issue count
>   for the caller's log.

*As built (M48; config spec, "One file"):* `step` is the file's own
step, opened with `run::file_labels(file)`, so `sb edit` draws it at the
top level. With issues the step fails with `run::Issues`, which carries
the count (`Issues::count()`, read inside the step's body); the count is
also in `check_file`'s "config file checked" log record.

Project Structure gains: "`run.rs`: `check_file` and `validate_file`."
Success criteria gain: "13. `check_file` and `validate_file` give the
same issues for a file as `run` does (a test over the fixture sets), and
neither makes a request with `Checks::Skip`." Boundaries "Ask first":
"writing to any Saltbox file (the facts half)" becomes "writing to any
Saltbox file outside the facts half's specified saves (tui spec,
Editors)".

### capability map

- The `docker logs` and `logs` rows: "tui, docker" and "tui, host,
  journal" stay; sb-rust "specified (tui)", then "built" per slice.
- The stack table's ratatui row gains `ratatui-core` `anstyle` and the
  exact pins; `ratatui-textarea` stays at 0.9.3.
- The stack table gains a row "Dialogs | `tui-popup` (ratatui org) |
  0.7.7 | — | bubbletea overlays | proto-facts" (the editors).
- The "Atomic file saves" row gains: "fact files: a temp file on the
  pinned directory fd (`rustix::fs::openat`, `O_EXCL`), not tempfile,
  which re-resolves the path" (the editors).
- Commands table: `edit <file>`'s notes gain "validates the file after
  the editor exits (tui spec)"; `fact`'s notes gain "edits
  `<appdata>/saltbox/<role>.ini`, not `saltbox.fact`; the role lock only
  at Save (tui spec decision 38)".
- Open items: "Edit picker: not prototyped on its own" is closed by the
  editors' picker slice.

### docs/ideas/fact-editor.md (an idea doc, not a spec)

- "Not Doing": the add-key line, as in decision 35 (listed so it isn't
  missed).

## Suggested slices for the plan

0. **Groundwork (M), no user-visible behaviour:** the workspace entries
   (ratatui `=0.30.2` with its features, ratatui-core `=0.1.2` `anstyle`,
   crossterm `event-stream`, ansi-to-tui); the `sb-tui` crate; deny.toml,
   the library-check hook, `MUTATION_TESTERS`; terminal setup and restore
   through `ratatui::init`/`restore`, chained with cli-core's restore
   hook and panic path; the async event loop recipe on the current-thread
   runtime, with the PTY input-latency measurement; the size ADR.
1. **First vertical slice: `sb logs`, services only (M):**
   - host: `services(filters, token)` from the host spec with
     `StateChangeTimestamp` and motd's concurrency and cancellation
     amendment (whichever of the two plans comes first builds it);
   - journal: `Journal::unit_query`;
   - tui: the service status rule; the picker's Services section (no
     filter yet); the journal adapter (tail, older, newer, the cycle
     guard, the follow with `FollowWait`); the viewer (scrolling, wrap,
     paging, follow, markers, `t`), the line format and the record cap
     with the small limits; Esc, `q`, Ctrl-C 130;
   - sb: `sb logs`, `SB_JOURNAL_DIR`, the empty and failure cases;
     TestBackend snapshots and the PTY smoke test on crafted journals.
2. **Docker source (M):** the daemon probe and its platform note;
   `Row`'s fields and `picker_status`; `logs.rs` with the positions and
   the fake Engine API's logs endpoint; the docker adapter with follow
   across restarts; `events.rs`; the Containers section with live
   reloads; `sb docker logs`; the Docker-unreachable row.
3. **The unified picker (S):** Needs attention, the name filter
   (ratatui-textarea), the kind filter and `Tab`, mouse on both screens,
   the services' 5 s refresh.
4. **Search (S):** smart-case matches, highlights across spans, n/N,
   horizontal scrolling with wrap off.
5. **Pasteable output and the inline fallback (M):** `s` and `p`; the
   picker on `/dev/tty` with the plain stream; the no-terminal error.
6. **Hardening (S):** the frame-cost test and its timing; colour-profile
   guards on log spans; RLIMIT_NOFILE; follow-L1 and follow-L4; the
   security-auditor pass (journal files and container output are
   untrusted); the library-reviewer on each slice's diff.

**The editors' plan** (its own plan; `sb edit <file>` needs no ratatui
and can come before the groundwork above):

7. **`sb edit <sub>` direct, without validation** (the editors' first
   vertical slice; no ratatui). The six subcommands, target checks,
   editor choice, exec `attached`, exit codes, the log record.
   Fake-editor tests through the binary. Size: S.
8. **Per-file validation and the post-edit report.** sb-config's
   `check_file` and `validate_file` with `Checks`, `run` moved onto
   them, then `sb edit`'s validation step, term-ui's confirm prompt
   (built with `release` first in the build order; here if not yet),
   "edit again?" and `--no-edit-again`. Size: M.
9. **The edit picker.** The `tui` crate's base if the log viewer hasn't
   built it (slice 0: init, restore, panic hook with mouse off, the
   event loop with input drained, the palette), then the list with
   mouse, restore and hand-off to slice 7's path. Size: S (M with the
   base).
10. **sb-config facts: parser and writer.** document.rs with the ported
    round-trip corpus and the property test. Size: M.
11. **sb-config facts: session and apply.** Pinned directory, editor
    lock, role locks shared with Saltbox taken at Save (decision 38),
    drift, prepare and commit with owner and mode, outcomes. Size: M.
12. **Fact editor, read-only page.** Load, headings, search, folding,
    masking, Show, Copy. Snapshots. Size: M.
13. **Fact editor changes.** Edit, add key, delete at three levels, the
    changes panel with undo, save, quit, lock-wait, drift and outcome
    dialogs. Size: M (split edit/add from delete if it grows).
14. **Mouse, the right-click menu and the PTY smoke test,** then the
    manual two-emulator check and the maintainer's look. Size: M.
