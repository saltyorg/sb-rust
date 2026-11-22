# Spec: `tui` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`tui`, build step 6): sb's
full-screen screens, above `docker`, `host` and `config`. This spec covers
the module's first part, the log picker and viewer behind `sb logs`: one
picker for Saltbox's systemd services and Docker containers, one viewer
for both, and the inline fallback when stdout isn't a terminal. It is the first consumer of `sb-journal` (journal spec,
decision 8) and of sb-host's service list (host decision 1), and the
first screen in sb's binary, so it brings ratatui and crossterm in. The
editors (`sb edit`, `sb fact`) are the second part of this spec
("Editors: `sb edit` and `sb fact`", with its own plan); the
announcement viewer is the third ("The announcement viewer", written
2026-11-15 from proto-markdown's reviewed design). *Amended 2026-11-18
(maintainer): `sb docker logs` is removed; `sb logs` is the one entry
point (decision 1).* It was built as the picker filtered to containers
(M69, M70); the text below describes `sb logs` alone. "Keys: one map
for every screen" (approved 2026-11-18) aligns the keys and hints of
every screen.

The behaviour comes from sb-go 35a7acc (2026-10-28), `cmd/logs.go`,
`cmd/dockerLogs.go` and `host/systemd.go`, redesigned as capability map
decision 3 asks, from proto-logs (reviewed 2026-10-29) and
`docs/ideas/log-picker.md`.

## Assumptions

1. **`sb logs` keeps its name and takes no arguments** (capability map
   decision 1; sb-go `cobra.NoArgs`, logs.go:27-37). No source is named
   on the command line (journal decision 8; log-picker.md, "Not Doing").
   *Amended 2026-11-18:* sb-go's `sb docker logs` (dockerLogs.go) is
   removed, its containers shown by `sb logs` (decision 1).
2. **`sb logs` needs root,** as every command but `version`, help and
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
gets from `sb logs` to the right log in a few keys and sees at a
glance what is broken. Complete, plain text for a support request,
which Saltbox's support page asks for (saltyorg/docs
docs/community/support.md:34), comes from `journalctl -u <unit>` or
`docker logs <name>`, or from `sb logs` with stdout piped (the inline
fallback); the viewer itself has no save or print (decision 24).

### The picker (`sb logs`)

- **Entry points** (decided.md A-Q1): `sb logs` opens the picker on every
  kind. `Tab` cycles the kind filter (all, services, containers).
  *Amended 2026-11-18 (maintainer):* sb-go's `sb docker logs`, which
  opened the picker filtered to containers, is removed (decision 1).
  `sb docker logs` is clap's unknown-subcommand error, exit 2, with the
  hint `for container logs, use 'sb logs': it shows Saltbox's services
  and Docker containers` (cli-core spec, "Help and parse errors").
- **First load before the screen.** The service list and the container
  list load before the alternate screen opens (proto-logs measured about
  20 ms on a real install). Then:
  - `sb logs` with no services and no containers prints `No matching
    systemd services or Docker containers found.` and exits 0 (sb-go's
    message, extended to both kinds; logs.go:1089-1091).
  - A failed service list ends `sb logs` with host's `failed to list
    systemd services: <cause>` in cli-core's error block, exit 1.
  - An unreachable Docker daemon doesn't end `sb logs`: it opens without
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
    `connect::list`, so a burst costs one reload. The tick is waited for
    only while the list is stale (*amended 2026-11-21*). If the event stream
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
    *Amended 2026-11-18 (the maintainer; "Keys: one map for every
    screen"):* Home/`g` jumps to the earliest entry of the whole log (the
    journal's first entry for the source, or the container log's start),
    not the oldest loaded line; End/`G` jumps to the newest and resumes
    following. Built on the viewer side after whole-log search lands.
  - Built (2026-11-18): Home/`g` go to the first line of the whole log
    and End/`G` to the newest, following, for every kind of source
    ("Home and End: the log's ends" below).
  - Built (2026-11-19, the key map): PgUp/`b`, PgDn/Space; `u` and `d`
    no longer page; ← → and `h` `l` sideways; `F` follows as End does;
    the keys in a footer row ("Keys: one map for every screen").
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
  The search also reads the rest of the log in the background
  ("Whole-log search, save and print" below).
- **Follow:** `f` toggles it. Turning it on with the newest line not
  loaded loads the tail first.
- **No save and no print** (decision 24): the viewer has no `s` and
  no `p`. A log's text comes from `journalctl -u <unit>` or `docker logs
  <name>`, as Saltbox's docs already say. OSC 52 copying is later.
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
- **Bracketed paste** is on while a screen is up (crossterm's
  `EnableBracketedPaste`), and off again on every way out (leaving, the
  drop, the panic hook, the forced exit's restore sequence). A paste
  comes as one `Event::Paste`: a screen with a text box open (the
  picker's filter, the viewer's search, the fact editor's boxes and
  search) puts it in whole through ratatui-textarea's `insert_str`, a
  one-line box with its line ends as spaces, a last line end dropped,
  and control characters other than a tab dropped (typing can't enter
  them, the screen wouldn't show them, and they would reach a fact
  file); with no box open a paste does nothing. So a pasted newline
  never stages a value or ends a line, and the text after it is never
  taken as keys (the maintainer, 2026-11-16, after M55).
- **The event loop** is ratatui's async recipe
  (https://ratatui.rs/recipes/apps/terminal-and-event-handler/): a task
  reads crossterm's `EventStream` in `tokio::select!` and sends events on
  an unbounded channel (proto-logs' "root cause of the whole series of
  input bugs"); a frame is drawn at most 60 times a second, only when
  something changed. *Amended 2026-11-21 (the maintainer, perf round 3):*
  the recipe's 60/s render interval and 4/s tick no longer run for every
  screen; they woke an idle screen 75-88 times a second. The loop arms
  its frame timer only while a frame is pending, on the same 60/s grid,
  and ticks a screen only when it asks (the picker's 5 s refresh, on the
  same 4/s grid). The viewer's first frame waits for its first page, a
  frame time at most, and is drawn as it comes, as the interval's tick
  often caught it. A key press's frame is drawn at once (a frame time
  after the last frame at the soonest), where the grid made it wait
  8.7 ms on average; the mouse, pastes and a source's lines keep the
  grid, so a wheel burst costs no extra frame.
- **Leaving the screen** clears it as the last step before the
  alternate screen is left (*amended 2026-11-19,* "Keys: one map for
  every screen", decision 6: "Blank on exit"). Before that, it drains
  input for 150 ms after mouse capture is
  turned off, so late mouse codes don't reach the shell (proto-logs
  measured 600 bytes leaked without it, 0 with it). No documentation
  covers it: marked UNVERIFIED in the code, with that measurement.
  *Amended 2026-11-21 (the maintainer, perf round 3, option C, 1 s
  cap):* the drain ends at the terminal's answer to a status query (DA1,
  written after the mouse is off), 1 s at most, read on the terminal by
  sb once crossterm's `EventStream` is gone (crossterm shows no such
  answer, and its own waits are fixed at 2 s); only a terminal that never
  answers waits the whole second. After a mouse report in the last
  second it drains 150 ms without a query, as before: mosh answers the
  query itself ahead of reports still on their way. An answer later than
  1 s reaches the shell.
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
  notice, and the exit. Never log content
  (capability map decision 11, "Never logged"): it can hold tokens. The
  picker's 5 s service refresh logs only a failure (when it starts or its
  cause changes), the recovery after it, and each service whose state
  changed, was added or went (maintainer, 2026-11-14; "Verification
  notes").

### Help texts

`sb logs` keeps sb-go's short text, so the command contract test passes
unchanged ("Display logs of managed systemd services"). The long text
describes the picker: `Shows Saltbox's systemd services and Docker
containers, and the log of the one you choose.` A snapshot test pins
it. (`sb docker logs`, removed 2026-11-18, kept sb-go's "Display logs of
Docker containers"; the contract test lists it as removed.)

### Differences from sb-go (the complete list)

1. **One picker for both kinds:** `sb logs` lists containers too
   (decided.md A-Q1). *Amended 2026-11-18 (maintainer):* `sb docker
   logs` is gone (decision 1): `sb docker logs` is a parse error, exit 2,
   with a hint naming `sb logs` (sb-go: its own container picker). The
   contract test lists it in `REMOVED`.
2. **Stopped containers are listed** (sb-go: running only,
   dockerLogs.go:1092-1100), and the containers filter's empty list reads
   `No containers found.`
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
11. *Removed 2026-11-19 (decision 24):* save and print-and-quit,
    which sb-go didn't have either.
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
  - `sb logs` and `sb docker logs` in sb (`sb docker logs` removed
    2026-11-18, decision 1);
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
  - The announcement viewer (tui-markdown, tui-scrollview), with `ops`'
    `update` and `sb announcements`: it takes a list of `(repo, date,
    title, message, new)`, and has an inline form (tui-markdown's output
    on stdout, each announcement under its heading, the message through
    `DisplayText`) for stdout that isn't a terminal (ops spec, `update`,
    decisions 24 and 26). Its section is "The announcement viewer"
    below.
- **Later for the viewer:** regex search; OSC 52 copy
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
`portable-pty` 0.9.0 and `vt100` 0.16.2 (both in the lock, through sb's
tests and term-ui's `testing`; exec's own PTY was removed, exec decision
15) for the PTY smoke tests; sb-journal's and
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
  src/stream.rs            the inline stream's lines, plain or in colour
  src/source.rs            the source protocol
  src/source/journal.rs    the journal adapter, the cycle guard
  src/source/docker.rs     the docker adapter, follow across restarts
  src/testing.rs           a fake source task (`testing` feature)
crates/host/src/systemd.rs services(filters, token) (host spec), StateChangeTimestamp
crates/docker/src/ps.rs    Row's new fields, the picker status
crates/docker/src/logs.rs  positions, tail, older, newer, follow
crates/docker/src/events.rs container events
crates/journal/src/lib.rs  Journal::unit_query
crates/sb/src/main.rs      `logs` (`docker logs` removed 2026-11-18)
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
    generation dropped.
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

1. `sb logs` opens the picker on all kinds, and `Tab` cycles the kind
   filter (PTY test). *Amended 2026-11-18:* `sb docker logs` is a parse
   error naming `sb logs` (decision 1).
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
9. *Removed 2026-11-19 (decision 24):* save and print.
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
   *Amended 2026-11-18 (maintainer): `sb docker logs` is removed.* `sb
   logs` already shows Saltbox's systemd services and Docker containers
   in one picker, so the docker subcommand goes; `sb docker` keeps `ps`,
   `restart`, `start` and `stop`. `sb docker logs` is clap's
   unknown-subcommand error with a hint naming `sb logs`; the contract
   test records it in `REMOVED` (capability map decision 1: a change of
   sb-go's command tree is recorded in the module spec). The picker's
   containers-first entry (`Picker::with_filter`) and its Services
   notice row (`with_no_services`), which only `sb docker logs` used, go
   with it; `Tab`'s containers filter stays.
2. **Docker positions are time windows with no tail,** a nanosecond
   timestamp plus an ordinal among equal timestamps, after a daemon probe
   (decided.md A-Q2).
3. *Superseded 2026-11-19 by decision 24* (decided.md A-Q3's save and
   print part): save and print-and-quit. OSC 52 copying stays later.
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
24. **No save and no print in the viewer** (the maintainer, 2026-11-19;
    supersedes decision 3's save and print, decided.md A-Q3). Getting a
    log's text out is `journalctl -u <unit>` or `docker logs <name>`,
    which Saltbox's docs already use, and the save file was root-only
    anyway (the run log's directory). `s`, `p`, the save module, the
    review notice and the whole-log export left with it; the inline
    stream for a piped stdout stays.

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
- **Services have no live signal:** a change shows within the 5 s
  refresh (no D-Bus dependency, proto-logs).

## Whole-log search, save and print (the log viewer)

Status: approved by the maintainer as drafted (2026-11-18, the six
decisions below). Save and print left the viewer on 2026-11-19
(decision 24 above), so decision 5 below is superseded and this
section's search is what was built. It extends
"The viewer" above (Search) and takes the journal
spec's "budgeted, resumable search" slice (journal spec, "Search").

### Objective

A search in `sb logs` or `sb docker logs` reaches back through the
whole of what the source holds: the unit's whole journal (as `journalctl
-u` with no `--since` would read it), or the container's whole log (what
the daemon still keeps). It runs in the background, finds matches
outside the loaded part, jumps to each in turn, and loads a match's
surroundings when it jumps there. The screen stays live while it runs.

**The rule:** viewing without a search costs what it costs today. No
scan runs, the match list is empty, and no frame does more work than it
does on main (measured, "Testing").

### Behaviour

- **Starting.** `/`, a query and Enter search the loaded buffer at once,
  as today (smart-case substring over the line as the screen shows it,
  `t`'s time and origin included when shown). The same Enter starts the
  whole-log search: older than the buffer, from its oldest edge to the
  start of the log, and, when the buffer doesn't reach the newest line,
  newer, from its newest edge to the end. Each line outside the buffer
  is matched exactly as a buffer line would be: the same `Line`, the
  same 64 KiB record cap, the same `DisplayText` with colour off, the
  same smart case.
- **Progress** shows in the header, after the count:
  - `3/17+  searching… 2 days back`: the age of the oldest entry the
    search has read, from when it started, in whole minutes (`40 min`),
    hours (`5 h`) or days (`2 days`). `+` while the count can still
    grow.
  - `3/17` alone when the whole log has been read.
  - `3/10000+  stopped at 10,000 matches` at the cap (below).
  - A read that fails ends that direction of the search with the
    source's usual notice row (`couldn't read the log: …`), and the
    header says `search stopped: read error`. An entry a journal side
    can't read doesn't end it, as it doesn't end paging: its read errors
    are one notice (`read error: …`, with the count of the others)
    before the side ends.
  - The progress's age is that of the oldest entry the search has read,
    whichever side read it.
- **`n` and `N`** go to the next newer and next older match, as today,
  across the unloaded part too:
  - Within the buffer, nothing changes.
  - To a match outside it, the viewer asks the source for the match's
    surroundings (a new `Request::Around(position)`): up to 250 entries
    before it, the match, and up to 249 after it. The buffer starts
    again with that page, as after a tail; paging, the start and end
    markers and the buffer caps work as usual from there. Following
    pauses, as `n` and `N` already do.
  - When the next match in that direction isn't known yet, because the
    search hasn't read that far, the key waits for it: the header shows
    `searching…`, and the view jumps when the match arrives. Another
    key cancels the wait.
  - Past the last match the key does nothing, as today (no wrap).
  - The count `3/17` numbers every match in log order, loaded or not.
- **Following.** A search while following reads older only. New lines
  are matched as they arrive, as today, and join the newest end of the
  match list. A search started while following begins at the newest
  match, as today. Back at the end (End, `G`, `f`) from a search that
  read the newer side, the search starts again as one while following,
  so the tail's matches and the newer side's never mix out of order.
- **Cancelling.** Esc clears the search and stops its read, as it
  clears the highlight today. A new query replaces the search and its
  read. Leaving the source stops it. `t` doesn't search again, as today:
  the match is of the line as shown when the search started, and Enter
  searches again. Counting the buffer again at `t` took 1.4 s on
  20,000 records of 5,000 characters (the frame-cost timing), where `t`
  costs 3 ms today.

### Home and End: the log's ends

Status: the maintainer's follow-up (2026-11-18). One rule for every
source the viewer shows, a service's journal and a container's log
alike.

- **Home/`g`** goes to the first line of the whole log, not the oldest
  line loaded:
  - When the buffer already holds the log's first line (`--- start of
    log ---` is loaded), the view moves to the top, as today.
  - Otherwise the viewer asks the source for the log's first page
    (`Request::Head`). The buffer starts again with it, as after a jump
    to a match, and following stops. Scrolling down from there loads
    newer pages as usual, and the start marker heads the view.
  - While the page is read, the header shows `going to the start…`.
    The read is one page, not the whole log (below), so it shows for a
    frame or two at most. Meanwhile no page of the old place is asked
    for, and End gives Home up: its page, when it comes, is dropped.
    The page starts at the first column (wrap off), the log's start.
  - Home then `n` during a whole-log search goes to the log's first
    match, once the older side has read to the start.
- **End/`G`** go to the newest line and follow, as today: when the
  newest line isn't loaded, the viewer reads the tail first (`Tail`,
  then `Follow`).
- **The header at the start:** while the start marker is in view and
  the view isn't following at the bottom (there a log whose start shows
  is short, and the header has nothing to add), the header says `at the
  start · <the first line's time>`, the time as the lines show it
  (local, to the second).
- **Journal:** the first page is sb-journal's new `Position::Head`: each
  file's first matching entry, found by its lists' heads (the match's
  DATA objects list their entries oldest first), then merged forward as
  a forward page merges, in the release's order between boots. No scan
  of the journal: the page reads what a forward page from its first
  entry would.
- **Docker:** the first page is sb-docker's new `logs::head`: one read
  from the container's creation (`since`), stopped after the page's
  500 lines and one more for `more`, the daemon's stream dropped there.
  It reads the log's start only.
- **A re-created container** (the source followed a `start` to a new
  ID): Home reaches the current container's first line, at its own
  creation. The old container's log went with it (`docker rm` removes
  the json-file log), and older pages already stop at the creation the
  source knows, so there's nothing older to reach.
- **During a whole-log search,** Home is a jump like `n` and `N`: the
  search's matches stay, numbered as before.

**Tests:**
- sb-journal: `Head` on one file, on 8 files of one boot each (300
  entries a file in the test; 25,000 in the timing) in the release's
  order (22.04, 24.04 and 26.04) against a scan's oldest entries, its
  preads against a tail page's, and with a query.
- sb-docker: `head` against `All`'s first entries, and a log shorter
  than a page.
- The viewer: Home loads the head and starts the buffer again, not
  following; Home with the start loaded only moves; the header's
  `going to the start…` and `at the start · …`; End resumes following.
- PTY, for a service and for a container (the fake Engine): Home shows
  the log's first line and the start header, End the newest line and
  `following`.
- **Timing:** the time to reach the start on the 200,000-entry, 8-file
  journal and on a container log of 30 MiB, recorded with the change.

### Budget, memory and threads

The runtime is current-thread tokio (Assumption 5): the screen and the
sources share one thread, so a search works in bounded steps and the
screen runs between them.

- **Journal.** Each side of a search is one sb-journal `Journal::scan`
  (journal spec, "Search") on the blocking pool (sb-host's
  `journal::scan`), off the screen's thread and beside the source's
  pages, which run on the pool's other threads, so paging and following
  don't wait for it. Each step of 5,000 entries (16 MiB at most) is
  matched there and sent as one event: its matches, and the time
  reached. A scan walks newest first, so the older side's matches come
  nearest first. The newer side's come farthest first: an `n` into it
  waits for that side to finish. On the screen's thread a step costs
  one event: its matches joined to the list.
- **Docker.** The Engine API has no search and no index:
  `/containers/{id}/logs` takes only `follow`, `stdout`, `stderr`,
  `since`, `until`, `timestamps` and `tail` (bollard 0.21.1's
  `LogsOptions`, bollard-stubs 1.53.1; Docker Engine API, ContainerLogs),
  and the daemon reads its json-file log from the start for any `since`
  (the platform note, "Both filters are one-sided"). So a search is one
  streamed read: from the container's creation to the buffer's oldest
  edge, and from its newest edge to the end when that's not loaded. Each
  line is matched as it arrives and dropped, and only the matches'
  positions are kept: sb holds one line at a time, at most the 64 KiB
  record cap. The daemon's read is bounded by Saltbox's json-file
  rotation, 10 MiB × 3 per container (Known limits). bollard's stream
  is decoded on the screen's thread, so the read yields to the screen
  after every 256 KiB of log text. A container's nearest older match is
  known only when the read reaches the buffer, so an `N` waits for the
  read to finish.
- **Matches kept:** at most 10,000 on each side of the buffer, each its
  position, time and count (about 200 bytes for a journal cursor, so
  about 2 MiB a side), with the buffer's own. They are the 10,000
  nearest the buffer: a side whose matches come nearest first stops at
  10,000; one whose matches come farthest first keeps the last 10,000
  as it goes. The header then shows `stopped at 10,000 matches`.
- **Frame budget:** the 5 ms per frame at 240×70 holds while a search
  runs. A frame draws only the window, as today, and the count and
  progress are a few header cells. The screen's per-step work is
  bounded (one event, or at most 256 KiB of Docker text matched).
- **The buffer's caps** (20,000 entries, 32 MiB) don't change. An
  `Around` page is 500 entries at most.

### Testing

- **Journal fixtures** big enough that matches lie outside the loaded
  500: crafted journals of 1,234 to 3,000 entries (the scan's steps, the
  adapter's sides, the PTY tests), two boots across two files in the
  page's order, and a step past its output limit losing no entry. As
  built, the 200,000-entry fixture stayed the step-cost measurement
  (journal spec, "Why one scan"); the 64 KiB cut is `Line::new`'s, the
  same for a line searched as for one shown, and is tested there.
- **Docker:** sb-docker's fake daemon serves logs of 300 entries with
  matches through them, entries sharing a time, and a log that can't be
  read (the search's side fails). The TTY
  decoding is the line decoder's, tested in sb-docker, the same for the
  scan as for pages.
- **Viewer tests** (fake source, `testing`): `n` and `N` across the
  unloaded part in both directions, the `Around` page and the buffer
  starting again, the wait for a match not yet read and its cancel,
  the cap, Esc and a new query stopping the read, following while
  searching, and the header's forms.
- **Frame cost:** the frame-cost test (lines measured per frame) runs
  with a search going and stays flat. The release timing at 240×70
  (size ADR) is taken before and after the change, with and without a
  search, and both are recorded. No-search frames must not get slower.
- **PTY tests:** `sb logs` on a journal fixture of 3,000 entries in a
  pseudo-terminal: search, `N` to a match 2,900 entries outside the
  window, its number, Esc. The cap is tested in the adapters' tests,
  with a small cap, not through the binary.

### Decisions (the maintainer, 2026-11-18, approved as drafted)

1. A search reads the whole log in the background; Enter starts it.
2. `n` and `N` jump outside the buffer, and the buffer starts again
   around the match (500 entries).
3. The progress form `searching… 2 days back`.
4. At most 10,000 matches, the nearest kept.
5. *Superseded 2026-11-19 (decision 24 above):* save and print wrote
   the whole log, cut at 256 MiB keeping the newest; Esc stopped a
   save.
6. Docker searches by one streamed read, with no daemon-side search to
   use; it's bounded by Saltbox's 30 MiB.

Built as (2026-11-18): decision 4's cap applies on each side of the
buffer ("Matches kept"), since the two sides are read apart and their
matches come in different orders; and the journal's side is one scan
per side, not budgeted calls ("Journal" above; journal spec, "Why one
scan").

## Editors: `sb edit` and `sb fact`

This section of the spec covers the module's two editing commands:
`sb edit` (a picker, or one of eight subcommands, then the user's
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
   `hetzner_vlan`, `settings`, `inventory`), and adds `motd` and
   `providers` (Difference 38), each also by its file's name (Difference
   39). Bare `fact` takes no arguments; its `get` and `list` are new
   (Difference 40, *amended 2026-11-22*).
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
  | Motd Settings | motd.yml | `motd` | motd.yml |
  | Providers | providers.yml | `providers` | providers.yml |

  The last two are sb's (maintainer, 2026-11-18; Difference 38). Not
  every host has motd.yml: the picker leaves it out when its file
  doesn't exist, and `sb edit motd` then fails with the target check's
  `configuration file does not yet exist: <path>`, exit 1, creating
  nothing. The six keep their behaviour, but for the default copy below.
  - **A missing file Saltbox ships a default for** (the maintainer,
    2026-11-20; config spec, decisions 10 and 11: sb-go's six but
    localhost.yml, and providers.yml; not motd.yml) is copied in from Saltbox's
    default before the target check, as validate-config copies it
    (sb-config's `run::copy_missing`): a runner titled `sb edit` shows
    one step, `Copying <file>.default`, with the config spec's notice,
    then the editor opens on the copy. With no default to copy (or a
    copy that fails, its warning shown), the target check fails as
    before. providers.yml is listed in the picker always.
  - **motd.yml** is validated as validate-config validates it, its
    schema included. A file still in its older format is migrated first,
    exactly as validate-config migrates it (sb-config's
    `validate_file_migrating`): the original is backed up to
    `SB_BACKUPS_DIR`, with the same notice. So both commands leave the
    file the same way.
  - **A file the editor removed** is the file's issue (`required config
    file not found`), offered again, as for the five required files. The
    file was there when the editor started (the target check), so `sb
    edit` checks motd.yml as required; validate-config skips a missing
    one, and localhost.yml stays skipped in both. A removed
    file Saltbox ships a default for gets the default again, with its
    notice, as validate-config would give it (*amended 2026-11-20*).
  - **providers.yml** holds Saltbox's DNS providers' credentials. It is a
    `vars_files` entry of saltbox.yml, read from the Saltbox root, with
    `defaults/providers.yml.default` when it's missing (Saltbox,
    saltbox.yml). It is checked as validate-config checks it
    (*amended 2026-11-20*, config spec decision 10): against
    `schema/providers.schema.yml`, or, while Saltbox has none, the
    YAML pre-pass, the parse and duplicate keys with the config spec's
    notice. Its step reads `Validating providers.yml`. The
    editor run and the log rules are the other files': the log has the
    file's name, its issue count and the step's record, never its
    content, and its issue lines name keys and positions, never values.
- **Picker** (`sb edit` with no subcommand). A ratatui list screen with
  sb-go's prompt line, "Select a Saltbox configuration file to edit:",
  each item's title and shown name, and a key hint line. Keys: Up/Down
  and `k`/`j` move, Enter opens, `q` and Esc quit with exit 0
  (*amended 2026-11-18,* "Keys: one map for every screen": the page and
  end keys, and `/` filtering by title or name, Esc clearing it first).
  Ctrl+C
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
  `shown_value(..)` (shown as given in the log; exec's decision 14,
  *amended 2026-11-15*), run with `attached(&shutdown)`:
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
    says yes. No exits 1, after validate-config's closing lines: the run
    log's `details:` pointer once (the step itself draws none), then
    `Fix with: sb edit <name>`. A file that couldn't be checked gets the
    pointer alone. The file stays as the editor saved it either way.
  - **Non-interactive answer:** `--no-edit-again` (on `edit` and each
    subcommand) reports the issues and exits 1 without asking. With no
    terminal and no flag, term-ui's prompt rule applies: an error naming
    `--no-edit-again`, exit 1, never a hang (term-ui criterion 10).
  - Ctrl+C at the prompt or during validation: 130, terminal restored.
- **Logging** (map decision 11): the file chosen, the editor's command
  (its program, extra words and the path: exec's child record),
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
    Add, Copy, Delete/Keep, Save changes, Keys, Quit (*amended
    2026-11-18:* Show/Hide gone, Keys added; "Keys: one map for every
    screen").
  - The "Unsaved changes" panel is always visible, with an undo per
    change. It sits on the right at 100 columns or more and at the bottom
    below that.
  - Delete works on a setting, a copy or an app. A note explains that
    Saltbox creates a new value on its next run: "<setting, copy or
    app>: deleted on save; Saltbox recreates it next run" on the
    status line, short enough for 80 columns (the maintainer,
    2026-11-16), and "Deleted values: Saltbox creates a new value next
    time it runs." in the save question.
  - Add asks for a key name, then its value (ratatui-textarea), under the
    selected copy. It refuses, before staging, a key that exists, an
    empty key, leading or trailing whitespace, `=`, a leading `#`, `;` or
    `[`, and control characters, newlines among them (sb-go's
    `validateKey`, Saltbox's reader's limits). Brackets inside a key are
    accepted, as sb-go and Saltbox accept them (`headers[Authorization]`;
    decision 40).
  - Dialogs (edit, add, save, quit with unsaved changes, outcome) use
    tui-popup, and so does the right-click menu, rendered without kept
    state into a rectangle of its own size by its row (M56's library
    review; proto-facts drew it by hand because a kept `PopupState`
    can't move a popup before its first render). It opens just under the
    row it acts on, or just over it when there is no room under it, so
    the row's name stays in sight (the maintainer, 2026-11-17). Every menu action has a key, and `m` or the Menu key
    opens the menu on the selected row.
  - Quit with staged changes opens the save dialog: Save, Discard or
    Cancel (sb-go's Apply/Discard/Return).
  - Mouse: click selects, the wheel scrolls, right-click opens the menu.
    Shift-drag is left to the terminal for native selection.
- **Values** (*amended 2026-11-18, the maintainer:* values are shown, not
  hidden): every value shows as it is, in the page, the
  boxes, the changes panel and the dialogs. There is no masking and no
  Show key. Until then a value was masked by its key's name, with a
  per-value Show.
  - **Copy** sends the selected value through crossterm's OSC 52
    `CopyToClipboard`. The status line says "Copied <key> (if your
    terminal allows it)", because success can't be confirmed.
  - Values are shown through term-ui's `DisplayText`, so control
    characters are escaped (as sb-go's `display()`).
  - **Never logged:** any fact value (map decision 11 names "fact
    values"). The log records role, copy, key and change kind, lock
    waits, drift, and each role's outcome (the Result line above).
  - **The key-name rule stays for text that leaves the screen.**
    sb-config's `facts::secret::is_secret_key` (`pass`, `secret`,
    `token`, `key`, `api`, `apprise`, `auth`, `credential`, `hook`,
    `hmac`, `claim`, and `kid` or `eab` as a whole word of the name;
    amended 2026-11-17 and 2026-11-18) marks validate-config's secret
    paths in its issue lines and the run log, and the query values of
    the URLs sb-config logs. "Keys: one map for every screen", "Secrets:
    where masking stays", lists every place.
- **Exit codes:** 0 on quit (with or without a save), 1 on an open error
  or no terminal, 130 or 143 on Ctrl+C or SIGTERM with the terminal
  restored. A Ctrl+C with staged changes discards them, and an
  interrupted save finishes the role it is committing before it stops.

#### `sb fact get` and `sb fact list`

*Added 2026-11-22* (the maintainer's decision on the design "Showing one
secret", decisions 43 to 46): a role can print the command to read a
secret instead of the secret, and a script can read a fact with
`$(…)`. Bare `sb fact` stays the editor.

```
sb fact get <app> <instance> <key> [--json]   # one value
sb fact get <app> <key> [--json]              # only when <app> has one instance
sb fact list [<app> [<instance>]] [--json]    # names only, never values
```

- **Words.** The app is the file (`<app>.ini`, Saltbox's role name),
  the instance its section, the key the fact, as `saltbox_facts` names
  them. The three-word form is canonical; the two-word form resolves
  only when the file holds exactly one instance. `get` reads any fact,
  secret or not.
- **The reader** (sb-config `facts::read::Store`): the session's
  directory rules without its locks. The facts directory is opened from
  `/` one component at a time without symlinks and pinned; each
  `<app>.ini` is opened relative to it with `O_NOFOLLOW|O_NONBLOCK`,
  must be a regular file that keeps its dev and inode, and is read up
  to 1 MiB; `DEFAULT` is hidden. It takes **no editor lock and no
  `<role>.ini.lock`**, so it works while `sb fact` is open and while
  Saltbox runs (Saltbox writes with `os.replace`, so one read is
  consistent). `get` reads only `<app>.ini`; `list` reads the files it
  lists, and one bad file fails it, naming it, as the editor's open
  does. An app name holding `/` or NUL names no file.
- **Output of `get`:** the value alone and `\n` on stdout, continuation
  lines joined by `\n`. To a pipe or a file the exact bytes go out; to
  a terminal the value goes through term-ui's `DisplayText` (colour
  off), as the editor shows values, so a value can't move the cursor or
  retitle the terminal. `--json` writes
  `{"app":…,"instance":…,"key":…,"value":…}` on one line, lossless; DEL
  and the C1 controls are written as `\u` escapes as well as the C0
  ones serde_json escapes, so the line is safe on a terminal and still
  the same JSON. No colour on stdout.
- **Output of `list`:** `<app> <instance> <key>` lines, sorted, narrowed
  by the app and the instance given; through `DisplayText` to a
  terminal. `--json` is an array of `{"app","instance","key"}` on one
  line. No value is ever listed. An empty store lists nothing (exit 0).
- **Exit codes and errors** (names only, escaped as cli-core's error
  block escapes them):

  | Case | Exit | stderr |
  |---|---|---|
  | found, not empty | 0 | nothing |
  | unknown app | 1 | `no facts for app 'plexx'; run sb fact list to see them` |
  | app with no instance | 1 | `app 'plex' has no instances` |
  | unknown instance | 1 | `app 'plex' has no instance 'plex2'; instances: plex, plex-4k` |
  | two-word form, several instances | 1 | `app 'plex' has 2 instances; name one: plex, plex-4k` |
  | unknown key | 1 | `plex/plex has no fact 'tokn'; facts: client_identifier, token` |
  | key present, value empty | 1 | `plex/plex token is empty` (Saltbox writes `""` as a placeholder; `--json` too) |
  | store missing, symlinked or unreadable | 1 | the facts layer's own text (`facts directory must be an existing directory without symlinks: …`, `facts file plex.ini must not be a symlink`, …) |
  | bad arguments | 2 | clap |

  `list` gives the unknown-app, unknown-instance and store rows.
- **Root, locks and the log.** Both need root through the existing
  relaunch (cli-core, `needs_root`) and never take the run lock. The run
  record holds `fact get` or `fact list`, the app, instance and key
  names (escaped) and the outcome. **No value reaches the run log, an
  error or a tracing field:** the error texts carry names only, and the
  value is held in a type whose `Debug` leaves it out.
- **Completion:** apps, instances and keys (completion spec, "Facts for
  `sb fact get` and `sb fact list`").

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
29. *Withdrawn 2026-11-18:* values show as they are, as in sb-go
    (factui/view.go:37-49). Copy is Difference 31.
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
38. `sb edit` also opens motd.yml ("Motd Settings", `sb edit motd`) and
    providers.yml ("Providers", `sb edit providers`), each left out of
    the picker when its file doesn't exist. sb-go's edit has neither
    (maintainer, 2026-11-18; decision 42). *Amended 2026-11-20:*
    providers.yml is listed always, and it and hetzner_vlan.yml are
    copied from Saltbox's default when missing (sb-go's edit fails on a
    missing hetzner_vlan.yml).
39. Each `sb edit` subcommand also takes its file's name as a hidden
    alias (`sb edit accounts.yml`, `sb edit adv_settings.yml`, ...), and
    `inventory` also takes `localhost` and `localhost.yml`, the
    inventory file's name: the names users type from the files. sb-go
    has no aliases; the contract test lists them as extensions.
40. `sb fact get <app> [<instance>] <key>` prints one fact's value and
    `sb fact list [<app> [<instance>]]` lists the facts' names
    (decisions 43 to 46, 2026-11-22). sb-go's `sb fact` takes no
    arguments (`cobra.NoArgs`) and has no way to read a fact without its
    editor; bare `sb fact` is the editor in both. The contract test lists
    both commands as extensions.

### Scope

**In:** the edit picker, the eight subcommands (motd and providers since
2026-11-18), editor choice and run, the
post-edit validation and prompt; `sb fact` with its session, dialect,
apply, screen and mouse; sb-config's facts half and per-file
validation API.

**Later or out:**
- Renaming keys, new apps, new copies (sb-go has none).
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
- **A kept `PopupState` for the right-click menu:** its `move_to` works
  only after a first render and its size is frozen after that
  (proto-facts FINDINGS). The menu renders with a fresh state each
  frame into a rectangle of its own size by its row instead, which
  tui-popup centres onto itself (M56's library review).

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
  secret.rs                     is_secret_key (the key-name rule for logged values)
crates/config/tests/fixtures/facts/  synthetic fact files (CRLF, continuations, comments)
crates/sb/src/edit.rs           sb edit: target checks, editor choice, run, validation, prompt
crates/tui/src/                 sb-tui (base shared with the log viewer)
  edit_picker.rs                the edit picker: returns the chosen file, draws nothing after
                                (picker.rs is the log picker's, built first)
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
  110 and 80 columns, every dialog, and the menu. Values: a snapshot
  with secret-named keys shows their values as they are (amended
  2026-11-18). OSC 52: the emitted payload decodes to exactly the
  value.
- **Log:** a known fact value in a fixture never appears in the run log
  after browsing, Copy, edit and save.
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
- **Ask first:** another file in `edit`; a new
  crate outside Cargo.lock; a term-ui API change beyond the prompts;
  changing the key-name rule for logged values; any change to how sb takes Saltbox's lock.
- **Never:** write a YAML config file from sb (the editor writes it);
  touch `saltbox.fact`; follow a symlink in the facts directory; run the
  credential network checks from `edit`; read or change a real Saltbox
  checkout or fact directory in tests or reviews.

### Success Criteria

15. `sb edit <sub>` runs the editor from `VISUAL`, then `EDITOR`, then the
    fallback list, with argv as specified, for each of the eight files;
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
27. Every value shows as it is, secret-named keys too (amended
    2026-11-18); Copy's OSC 52 payload is the value; no known fact value
    is in any log.
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
    exec's `attached` with the extra words and the path as `shown_value`s. A cancellation sends
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
32. **No fact value is ever logged.** Source: brief F D8 (term-ui
    decision 8; map decision 11). *Amended 2026-11-18 (the maintainer):*
    nothing is masked on screen; until then values were masked by key
    name.
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

42. **motd.yml and providers.yml are in `edit`** (the maintainer,
    2026-11-18, answering the "Later" item). motd.yml migrates as
    validate-config migrates it, so the two commands never leave it
    differently. providers.yml gets the schema-less check sb-config
    already has (localhost.yml's), the only YAML check there is for it.
    *Amended 2026-11-20* (config spec, decision 10): it gets
    validate-config's check, its schema included once Saltbox has it.
    Both are left out of the picker when missing, and refused by name
    (the target check) when asked for directly.
43. **`sb fact get` and `sb fact list` are subcommands of `sb fact`**
    (the maintainer, 2026-11-22, on the design "Showing one secret"),
    in `sb edit`'s shape: bare `sb fact` stays the editor. Plain
    positionals mirror `saltbox_facts`' role, instance and key, so every
    line `list` prints can be pasted after `sb fact get`. Generic, not
    secret-only: nothing has to be kept in step with Saltbox. Not
    `sb secret` (a top-level command suggesting a curated registry), not
    per-app helpers, not an `op://`-style joined reference.
44. **The value alone and a newline on stdout,** exact bytes to a pipe,
    `DisplayText` to a terminal, `--json` lossless, no `--no-newline`
    (an INI value can't end in a newline). It prints to a terminal
    without asking, as gh, op, pass and docker do: the user typed the
    command to see the value (the maintainer, 2026-11-22; decision 2 of
    2026-11-18 points the same way).
45. **Every miss exits 1,** an empty value included (printing an empty
    line with exit 0 would hide Saltbox's `""` placeholder from `$(…)`);
    the stderr text says which miss. Separate codes can come later
    without breaking anyone.
46. **Root through the relaunch, no locks.** One rule for every command,
    and the read is in the root run log; no editor lock and no role lock,
    because the role's line shows during an install, when the user may
    run the command in another terminal (the maintainer, 2026-11-22).

### Known limits

- OSC 52 copy can't confirm success. The status line says so.
- Blank on exit ("Keys: one map for every screen", decision 6) has two
  gaps. A panic on a screen on stdout leaves it through ratatui's own
  restore, which erases nothing (on the tty, sb's hook erases every row).
  And ratatui erases the display (ESC [2J) when the window is resized
  (ratatui-core 0.1.2 `terminal/resize.rs`), so under GNU screen or tmux
  without an alternate screen a frame shown before a resize, values
  included, can reach their scrollback.
- A commit across several roles has no rollback. A failure leaves the
  earlier roles applied, and the outcome dialog says which.
- A `;` line that doesn't continue a value (at column 0, say) is a
  comment to sb, as to sb-go. Saltbox's module refuses such a line, or
  reads it as a key when it holds an `=`. A file sb saves never gains
  one: sb writes no comments. An indented `;` line right after a value
  is read as Saltbox reads it (decision 39).
- `sb fact list` prints names as they are, one line each, space
  separated: a key or instance holding a space (Saltbox allows any but
  `=` and line breaks) can't be pasted after `sb fact get` without
  quoting. Completion leaves such names out.
- Someone who can mount a FUSE file system with `allow_other` over a
  role file can make `sb fact` hang while it reads that file. That
  needs the Saltbox user, who usually has sudo anyway, so sb doesn't
  guard against it (2026-11-15, after the security
  review).
- The key-name rule (for what is logged) is a guess by name. A secret
  under another name (for example `cookie`) in a config file is quoted
  in an issue line until it is added to the list. Every secret
  Saltbox's and Sandbox's roles stored through `saltbox_facts` on
  2026-11-18 is on it; a camel-case `eabKid` is not split, so its `kid`
  isn't a word.
- `sb edit` validates the file after it is saved. A broken file is on
  disk until the user fixes it (F-Q1's option B, chosen over C).
- Drift is found at Save, perhaps long after the change was staged; the
  drift dialog then reloads (dropping that role's staged changes) or
  keeps them staged (decision 38).

## The announcement viewer

*Written 2026-11-15 from proto-markdown's reviewed prototype*
(prototypes/proto-markdown/FINDINGS.md, "Viewer redesign (maintainer
review)", the maintainer's verdict of 2026-10-29, and its `src/main.rs`),
at the maintainer's request (2026-11-15: the earlier prototype's menu through all
announcements).
This is the section the module build's M87 waited on (plan open
question 1). The inline form is built (M74, `markdown::write_inline`);
this section is the full-screen form.

### Objective

A Saltbox user sees, right after `sb update`, which announcements are
new, or with `sb announcements` every one that applies (none called new),
each labelled with its repository; reads each one whole without
scrolling past its end, and can still read the ones they missed before.
*Amended 2026-11-17.*

### Input

- A list of entries, each `(repo, date, title, message, new)`: the
  repository's name, the date and title as the file wrote them, the
  Markdown message, and whether it is new. ops decides which entries
  apply and which are new, by sb-go's rules (ops spec, "Announcements":
  `date|title` identity against the file before, the required folder and
  file; no before file means every entry is new). The viewer adds no
  rule of its own.
- Unlike sb-go's viewer, which showed only the new entries, the list
  holds every applicable entry of the file, the older ones too, so missed
  ones can still be read (FINDINGS, "What counts as new").
- The callers: `sb announcements` on a terminal, with every applicable
  entry of Saltbox's and Sandbox's, none marked new, when at least one
  applies (the maintainer, 2026-11-17; ops difference 45); `sb update`
  on a terminal when there are new ones (M87), those marked. With stdout
  not a terminal both print the inline form instead: `sb announcements`
  every applicable entry, `sb update` the new ones (ops spec, decision
  24). *Amended 2026-11-17.*

### Layout

- **The list**, newest first by the date's text (a stable sort: on equal
  dates the order given, Saltbox's before Sandbox's), in a rounded,
  bordered block titled ` Announcements `, with `· N new ` after it when
  any are new. Every row names its repository between the date and the
  title (the maintainer, 2026-11-17: both repositories share the list,
  so each says whose it is), the same for both callers. A row is
  `● <date>  <repo>  <title>` for a new entry: the marker in the Ok role,
  the date and the repository Muted, the title in Text, bold. An older
  entry is `  <date>  <repo>  <title>`, all in the Faint role, without
  the marker. With none marked new (`sb announcements`, which never calls
  anything new: the maintainer, 2026-11-17), no title count, no legend,
  and every row ` <date>  <repo>  <title>`, the date and title in Text,
  the repository Muted: faint means older than a new one. The
  selected row is the Selected roles' bar (reverse video with colour
  off, decision 23's rule). *Amended 2026-11-17.*
- **The selected announcement** fills the rest, in a rounded block of
  its own whose title names the repository (` Saltbox `, ` Sandbox `):
  a first line `<date> · <title>` (date Muted, the dot Faint, the title
  Accent, bold), with a ` NEW ` badge (OnAccent on Ok, bold) when the
  entry is new, then the message as tui-markdown renders it with the
  inline form's style sheet (proto-markdown's reviewed `SaltboxSheet`;
  heading markers and code fences hidden in colour, kept with colour
  off). Text wraps at the pane's width.
- **The viewer opens on the newest new entry**, or the newest one when
  none is new.
- **Width:** at 70 columns and wider the list is on the left, as wide as
  its widest row (`ListItem::width`, display width) plus its border, at
  least 24 columns and at most 40% of the screen; a longer title is cut
  in the list and shown whole in the pane (decided in the prototype's
  review: keep the cap, keep titles short). Below 70 columns the list
  goes above the announcement, as tall as its rows plus its border, at
  most a third of the screen.
- **The footer** is the key hints, as the log screens' (`↑↓
  announcement · PgUp/PgDn scroll · q close`), and `● new` when any are
  new.
- **No announcement** (an empty list) reads `No announcements apply to
  this system.` in the pane; the callers don't open the viewer then.

### Scrolling

- **tui-scrollview 0.6.8** (ratatui org) scrolls the pane. It clamps
  while rendering: content that fits never scrolls, and the furthest
  position puts the last line on the last row (tui-scrollview
  `scroll_view.rs`, `render`). Its scrollbar shows only when the text
  doesn't fit, and it gives page up and down. The pane's block style
  colours the scrollbar (Faint), since tui-scrollview's `Scrollbar` has
  no style of its own (FINDINGS; the prototype's library review).
- It draws the whole announcement every frame. That was why the log
  viewer rejected it; an announcement is a few dozen lines. The content
  is capped at 2,000 rows (a longer one is cut there), so a pathological
  file can't make each frame's buffer huge.
- The content's height comes from `Paragraph::line_count` (ratatui's
  `unstable-rendered-line-info`, ADR-001), measured again one column
  narrower when a scrollbar is needed.
- The list moves with `ListState::select_next` and `select_previous`.
  `select_next` clamps only when the list renders, so the viewer clamps
  the selection before comparing, and a move past the last entry is no
  move (the prototype's library-review bug). Choosing another entry
  starts it at its top.

### Keys and mouse

- ↑/`k`, ↓/`j`, and ←/`h`, →/`l` (sb-go's; *amended 2026-11-18,* "Keys:
  one map for every screen"): the previous and next entry. PgDn/Space, PgUp/`b`: a page
  of the announcement. Home/`g`, End/`G`: its top and bottom. `q` and
  Esc close the viewer (exit 0); Ctrl-C closes it with 130, as the log
  screens.
- **Mouse:** a click on a list row selects it; a click on the list's
  border selects nothing (the clickable area is `Block::inner`). The
  wheel moves the selection over the list and scrolls the announcement
  three rows elsewhere. ratatui's `List` has no hit-testing, so the row
  is `ListState::offset()` plus the row inside the block, as the picker
  maps it ("The picker", Mouse).

### Terminal, text and colour

- The screen runs on the module's `Tui` and `App` (the event loop and
  the restore above), on stdout. Its theme is the run's, or the plain
  one with colour off (`NO_COLOR`, `--color=never`), as the log screens'
  (`screen_theme`).
- A message reaches the screen as in the inline form: `DisplayText` with
  colour off before tui-markdown, and each rendered span's text through
  it again (character references decode to any code point). The date,
  title and repository go through `DisplayText` on one line.
- Colours are the palette's roles in the run's profile; syntect's RGB
  highlighting is brought down to the profile as log spans are
  (ADR-002). With colour off no colour is drawn at all; the bold and
  italic stay.

### Run log

The screen logs that it opened (the number of entries and of new ones)
and how it closed, never a title or a message.

### Differences from sb-go (the announcement viewer; continuing the list above)

34. Every applicable announcement is listed, the new ones marked and the
    older ones faint; sb-go's viewer showed the new ones only.
    *Amended 2026-11-17:* marked so for `sb update`; `sb announcements`
    marks none. Each row names its repository; sb-go's viewer, one
    repository's, didn't.
35. A list beside (or, below 70 columns, above) the selected
    announcement, newest first, opening on the newest new one; sb-go
    paged through the new ones in date order.
36. An announcement that fits doesn't scroll, and one that doesn't stops
    at its last line (tui-scrollview); mouse selection and wheel
    scrolling are new.

### Testing Strategy

- **Unit, in sb-tui, on `TestBackend`:** a short announcement doesn't
  scroll (PageDown, End and the wheel leave it at the top); a long one
  stops with its last line on the last row; choosing another entry
  starts it at its top; a move past the last entry keeps the scroll;
  the marker, the tint and the `N new` title, and none of them unmarked;
  each row's repository, with snapshots of both repositories marked
  and not; it opens on the newest new
  entry; a click selects a row and a click on the border selects
  nothing; the list as wide as its longest title (display width) and the
  40% cap; the narrow layout below 70 columns; the scrollbar's colour;
  no named colour under truecolor, no RGB under `ansi256`, nothing under
  `ascii`; a message's control characters and character references
  never reach a cell; the 2,000-row cap.
- **End to end, in sb:** `sb announcements` on a PTY opens the viewer,
  `q` exits 0 and Ctrl-C 130, the terminal restored both times; piped,
  the inline form.
- **Look:** screenshots through `sb announcements` in tmux at 120×40 and
  80×24, plain and colour, from synthetic fixtures (new and older
  entries, a long one that scrolls, a short one that doesn't, the
  narrow layout, the piped output).

### Decisions (the announcement viewer; continuing the list above)

39. **The design is proto-markdown's reviewed viewer** (2026-10-29),
    unchanged but for the repository named in the pane's title (the
    list may hold Saltbox's and Sandbox's entries from `update`) and the
    2,000-row cap. Source: FINDINGS, "Viewer redesign (maintainer
    review)".
40. **tui-scrollview for the pane, ListState for the list,** not our own
    scroll maths (CLAUDE.md, "Library first"). Source: FINDINGS,
    "Scrolling comes from libraries".

### Known limits

- The pane is drawn whole every frame, and cut at 2,000 rows.
- A title longer than the list's 40% is cut in the list.
- With colour off, new and older rows differ only by the marker.

## The restore screen

*Added 2026-11-17 (the maintainer, 2026-11-16 and 2026-11-17; ops spec,
"`sb restore` (hidden)" and decision 32, which say what it shows and
does).* The module `restore`: one `App` on the module's `Tui`, three
modes.

- **Form:** three ratatui-textarea lines (`Username` shown,
  `Password` and `Repeat password` masked with `set_mask_char('•')`),
  no undo history; Enter, `\n`, `\r` and Ctrl-M move on (ratatui-textarea's
  single-line example, as the picker's filter line); the checks' text
  under the fields; Esc cancels. Centred with `Rect::centered`.
- **Run:** a `List` with a `ListState`, a row per file: the name padded
  to 20, then `waiting`, `fetching…`, `DONE`, `IGNORED` or `FAIL` with its
  reason.
- **Viewer,** once the summary comes: the announcement viewer's layout
  and list, shared through the module `split` (the menu beside the pane
  from 70 columns, above it below; the list movement clamped at once; a
  click's row from `ListState::offset()`), a check line above, a
  Continue button below. The pane is a tui-scrollview `ScrollView` of a
  wrapped `Paragraph`, as the announcement's, with a line cursor: each
  line's first row from `Paragraph::line_count`, the offset moved to
  keep the cursor's rows in sight. Each line goes through term-ui's
  `DisplayText` (colour off) and shows as it is (*amended 2026-11-18,
  the maintainer:* no masking, so no `s` or `a`; "Keys: one map for
  every screen"). Focus is
  the menu, the content or Continue (Tab, Shift-Tab); the focused part's
  border or button is the accent's. The selected row is the Selected
  roles' bar, reverse video with colour off (decision 23's rule).
- **The fetch** is the caller's future, given the answers once the form
  is submitted, run beside `Tui::run` in one `tokio::select!` and
  reporting through an unbounded channel and the screen's waker; it is
  dropped when the screen closes, the updates still queued taken first.
  The screen returns its rows, the summary and how it ended; ops prints
  the final list.

## Keys: one map for every screen

*Approved 2026-11-18, with the maintainer's answers to its five
questions ("Decisions" at the end).* The maintainer's decision of
2026-11-18: the screens' keys are "properly aligned across the board". Each key has one meaning on every screen, a
hint row fits on every screen, and `?` shows every key. The motd layout
and `sb logs <name>` were declined and are not part of this section.

It covers every screen built on the `tui` crate: the log picker and
viewer (`sb logs`), the edit picker (`sb edit`), the
fact editor (`sb fact`), the announcement viewer (`sb update`, `sb
announcements`) and the restore screen (`sb restore`). The whole-log
keys from "Whole-log search, save and print" (approved
2026-11-18) are counted as landed.

### Objective

A Saltbox user who learns a key on one screen can use it on the next.
No key that saves or deletes on one screen does something else on
another. No screen hides a value, so no key reveals one. At 50 columns
(a phone over SSH) every screen still shows how to get help and how to
leave, and `?` lists the rest.

### Today's keys (main at 75953b9, plus whole-log search)

Read from the code (`crates/tui/src/picker.rs`, `viewer.rs`,
`edit_picker.rs`, `fact/{mod,keys,mouse,dialog}.rs`, `announcements.rs`,
`restore.rs`) and checked against this spec's sections. "—" means the
key does nothing there.

| Key | Log picker | Log viewer | Edit picker | Fact editor | Announcements | Restore | |
|---|---|---|---|---|---|---|---|
| ↑ ↓, k j | move | a row | move | move | entry | field (form, ↑ ↓ only); menu or line (review) | |
| PgUp PgDn | a page | a page | — | a page | a page of text | a page (file pane) | **G** |
| `u` / `d` | — | page up / down | — | undo / **delete** | — | — | **C** |
| `b` / Space | — | — | — | — | page up / down | page up / down (file); Space presses Continue | **G** **C** |
| Home End | first, last | top, bottom (End follows) | — | first, last | top, bottom | first, last | **G** |
| `g` `G` | — | top, bottom | — | — | top, bottom | first, last | **G** |
| ← → | — | sideways (wrap off) | — | fold, unfold | — | back to menu, into file | **C** |
| Enter | open | — | open | fold, or edit | — | next field, submit; into file; Continue | |
| Tab | kind filter | — | — | leaves the search box | — | next field; next part | |
| Shift-Tab | — | — | — | — | — | previous field or part | **G** |
| `/` | filter | search | — | search | — | — | **G** |
| `n` `N` | — | next, previous match | — | — (dialogs: `n` = no) | — | — | |
| Esc | clears the filter, else quits | clears the search, else back to the picker; stops a save first (whole log) | quits | clears the search box; on the page **quits even with a search in effect** | closes | form: cancels; menu: ends; file: back to menu | **G** |
| `q` | quits | quits | quits | quits (asks with changes) | closes | menu: ends; file pane: — | **G** |
| Ctrl-C | 130 | 130 | 130 | 130 | 130 | 130 | |
| `s` | — | **saves the log** | — | **shows a secret** | — | **shows the line's secret** | **C** |
| Ctrl+S | — | — | — | edit box: **shows the masked text** | — | — | **C** |
| `v` | — | — | — | saves | — | — | **G** |
| `c` | — | — | — | copies a value; drift and waiting dialogs: **cancel** | — | **continue** | **C** |
| `a` | — | — | — | adds a setting | — | **shows every secret** | **C** |
| `r` | — | — | — | drift dialog: reload | — | — | |
| `p` `f` `t` `w` | — | print, follow, time, wrap | — | — | — | — | |
| `e` `m` Menu | — | — | — | edit, menu | — | — | |
| `y` | — | — | — | dialogs: yes | — | — | |
| `?` | — | — | — | — | — | — | **G** |

**C** marks a conflict (one key, different meanings), **G** a gap (one
action on different keys, or missing where it applies). The ones that
matter:

1. **`s` reveals a secret on two screens and saves on a third.** Pressed
   from the viewer's habit, `s` in `sb fact` or the restore screen shows
   a token, which is bad while sharing a screen for support. sb-go's
   fact editor saved on `s` and Ctrl+S (sb-go `factui/update.go:256`),
   so an sb-go user's habit does the same.
2. **Ctrl+S reveals** the text in the fact editor's edit and value boxes.
   It is sb-go's save key there, and k9s's save key in its log view.
3. **`d` and `u`** page in the viewer (in sb-go, the bubbles
   viewport's half-page keys, never in its hint row) and delete and undo
   in the fact editor (sb-go's `d` too).
4. **`c`** copies in the fact editor, continues in the restore screen
   and cancels in two of the fact editor's dialogs. **`a`** adds in the
   fact editor and shows every secret in the restore screen.
5. **Page up** is `u` in the viewer and `b` in the announcement viewer
   and the restore screen; `g`/`G` are missing from three screens;
   the edit picker has no page keys at all.
6. **Esc** clears a search before it quits in the picker and the viewer,
   but quits the fact editor's page with a search in effect (sb-go's
   fact editor cleared the search, `factui/update.go:262`).
7. **No screen has `?`.** sb-go's edit picker had it (bubbles `list`'s
   `ShowFullHelp`, whose help row sb-go left on).
8. **Labels:** the fact editor's edit box says `Enter save` for staging,
   while `Save (v)` writes the file; the announcement viewer says
   `q close`, the others `q quit`.

**Hint rows cut at 80 columns or narrower** (measured from the snapshots
and the hint strings):

| Screen | Hint row | Width | Cut |
|---|---|---|---|
| Log viewer | in the header, after the name, kind and state | keys alone 76; with `app  service  following` 102 | at 80 for any name: `s save`, `p print` and `q quit` are never visible (the 80×24 snapshot ends `Esc back · q q`). With `saltbox_managed_rclone_vfs`, cut at 120 too |
| Log picker | footer | 51; 70 with a filter in effect; trouble text goes first | at 50, `q quit`; at 80 with `Docker events stopped; retrying`, the last two hints |
| Fact editor | the button row | 77 with `Save`, 85 with `Save changes` | below 78: `Quit (q)` first, then `Save (v)` |
| Restore, review | footer | 79 (file focused) | below 80: `c continue`; at 50, `s`, `a` and `c` |
| Restore, form | footer | 63 | at 50: `Esc cancel` |
| Announcements | footer | 52 with `● new` | at 50: the `● new` legend |
| Edit picker | footer | 29 | none |

### Rules

1. **One meaning per key.** A key that does nothing on a screen does
   nothing there; it never does something else. One class is
   directional (rule 7).
2. **Text boxes take printable keys.** In the picker's filter line, the
   viewer's search line, the fact editor's boxes and the restore form,
   every printable key types (`?`, `q`, `s`, `r` included). Only Enter,
   Esc, Tab, the arrows, the box's own Ctrl and Alt keys (ratatui-textarea's
   table) and Ctrl-C act. F1 opens the keys overlay from a box.
3. **A dialog asks one question.** Its keys are its buttons, each printed
   on its button: Enter for the highlighted one, Esc for Cancel, `y`
   and `n` for a yes-or-no question. Nothing on the page under it acts.
   A letter the page uses means the same kind of thing in the dialog
   (the quit question's `d` discards).
4. **No screen hides a value** (decision 2): the fact editor and the
   restore screen show every value as it is, so there is no reveal key
   and no masked box. Masking stays where text leaves the screen: the run
   log, and output meant for sharing (Secrets, below).
5. **Esc goes back one step:** it closes the open thing (the overlay, a
   menu, a dialog, a text line), or clears a search or filter. With
   nothing to close, it leaves the screen: the viewer
   to its picker, any other screen as `q` does.
6. **`q` leaves the screen.** Its hint says what follows: `q quit` when
   sb ends, `q close` when the command goes on (`sb update` after the
   announcements). With staged changes the fact editor asks first, as
   today.
7. **Arrows point.** ← and →, and `h` and `l` with them, move in the
   direction they show on what the screen holds (decision 3): the
   viewer's text sideways, the fact editor's tree in and out, the split
   screens' list to the previous and next entry. All three are sb-go's,
   and so are `h` `l` in the viewer (bubbles `viewport`) and the
   announcement viewer.
8. **sb-go's keys stay** wherever sb-go had the screen, unless two of
   them conflict ("sb-go's keys" below).

### The key map

**Everywhere a screen has a list or text:**

| Key | Meaning | Change |
|---|---|---|
| ↑ ↓, `k` `j` | move one: the selection, or the text by a row | — |
| PgUp PgDn, `b` Space | a page up, down | `b` and Space join the picker, the viewer, the edit picker and the fact editor (less's keys, and sb-go's viewer's and announcement viewer's); Space no longer presses the restore screen's Continue |
| Home End, `g` `G` | first, last of what the screen shows; in the log viewer, Home and `g` the earliest entry of the whole log (the journal's first entry for the source, or the start of the container's log), not the oldest loaded line, and End and `G` the newest, following again (the maintainer, 2026-11-18; the jump is built once whole-log search lands) | `g` `G` join the pickers and the fact editor; Home End and the page keys join the edit picker |
| ← →, `h` `l` | the direction they point (rule 7) | `h` `l` come back (sb-go's, in the viewer and the announcement viewer) and join the fact editor; restore's ← → move to the previous, next file, as the announcement viewer's |
| Enter | open or do the selected thing | — |
| Tab, Shift-Tab | next, previous part: the picker's kind, the restore screen's focus, the fact editor's search box and page | Shift-Tab joins the picker (the kind backwards) |
| `/` | find: filter a list, or search a text | the edit picker gets the log picker's filter line (sb-go's edit picker had `/`) |
| `n` `N` | next, previous match | — |
| Esc | back one step (rule 5) | the fact editor's page clears a search before it quits |
| `q` | leave the screen (rule 6) | the restore screen's file pane too; `c` no longer continues |
| Ctrl-C | stop sb, exit 130 | — |
| `?`, F1 | the keys overlay | new; F1 also from a text box |

**Screen keys,** each on the screens where it applies and nowhere else:

| Key | Meaning | Screens | Change |
|---|---|---|---|
| `s`, Ctrl+S | save | fact editor: the staged changes (the save question) | was `v`, and `s` showed a secret; `s` and Ctrl+S are sb-go's; Ctrl+S no longer reveals in the boxes. The log viewer has no save since 2026-11-19 (the maintainer: save and print go), so `s` saves on one screen only |
| `c` | copy the selected value (OSC 52) | fact editor | — |
| `e` `a` `d` `u` | edit, add, delete or keep, undo | fact editor | — (decision 1) |
| `m`, Menu | the row's menu | fact editor | — |
| `f`, `F` | follow on, off | viewer | `F` is new: less's follow key, which `journalctl`'s pager users know |
| `w` | wrap on, off | viewer | — |
| `t` | time and origin on, off | viewer | — |

**Dialogs** (rule 3): the save question `y`/Enter, `n`/Esc; the quit
question `y`/Enter, `d`, `n`/Esc; waiting for Saltbox, `Cancel (Esc)`
(was `c`, which copies); the drift dialog, `Reload (r)` as today (sb-go's
key, and `r` means nothing else now that no key reveals) and `Cancel
(Esc)` (was `c`); the outcome, `OK (Enter)`, closed by Enter, Esc or `q`
as today. The fact editor's edit box reads `Enter keep · Alt+Enter new
line · Esc cancel`, so "save" means the file only.

**The whole-log keys** fit as they are: another key ends `n`'s wait for
a match, as that section says. The log viewer's save (`s`, Ctrl+S) and
print (`p`) are gone (the maintainer, 2026-11-19).

### Hints

- **A footer row on every screen,** the last row. The viewer's keys move
  there from its header, which keeps the name, kind, follow state and
  the search's count and progress (`3/17+  searching… 2 days back`).
  The header cuts the name with `…` first, so the state and the count
  stay. The viewer shows one log row fewer for it.
- **The fixed tail,** right-aligned on every screen: `? keys · q quit`
  (`q close` where rule 6 says, `F1 keys · Esc cancel` in the restore
  form, where `?` and `q` type). It is never dropped, so `?` is in the
  same place on every screen.
- **The screen's hints,** left-aligned, in the screen's order of
  importance. When the row is too narrow, the last ones go, whole: a
  hint is never cut. sb-go's hint row (bubbles `help`'s
  `ShortHelpView`) also dropped whole items, from the end, with an `…`
  ([help.go](https://github.com/charmbracelet/bubbles/blob/v2.2.1/help/help.go),
  `shouldAddItem`). sb drops by importance and shows no `…`, because
  the tail already points at the overlay.
- **Trouble and notices** keep their places: the picker's trouble text
  leads the footer and is never dropped (cut with `…` only when it alone
  is wider than the room the tail leaves). A text line (filter, search)
  takes the footer's place while it's open, with its own hints, as
  today.
- **The fact editor's buttons** are its footer: `Show/Hide` goes,
  `Keys (?)` joins them before `Quit (q)`. Narrow, the lowest buttons go
  first (Copy, Add, Delete, Edit), and `Save changes` becomes `Save`
  first, as today. Every dropped button still works by its key, in the menu (`m`)
  and in the overlay.
- **Widths** are display widths (ratatui's `Span::width`, as the picker
  measures its filter line today).

**Mock-ups** (each row exactly as wide as stated; `|` marks the edges):

```text
Log viewer
120 |Esc back · / search · f follow · w wrap · t time                                                         ? keys · q quit|
 80 |Esc back · / search · f follow · w wrap · t time                 ? keys · q quit|
 50 |Esc back · / search · f follow     ? keys · q quit|

Log viewer, a search in effect
120 |n/N match · Esc clear · f follow · w wrap · t time                                                       ? keys · q quit|
 80 |n/N match · Esc clear · f follow · w wrap · t time               ? keys · q quit|
 50 |n/N match · Esc clear · f follow   ? keys · q quit|

Log picker
120 |Enter open · / filter · Tab kind · ↑↓ move                                                               ? keys · q quit|
 80 |Enter open · / filter · Tab kind · ↑↓ move                       ? keys · q quit|
 50 |Enter open · / filter · Tab kind   ? keys · q quit|

Log picker, with trouble
120 |Docker events stopped; retrying   Enter open · / filter · Tab kind · ↑↓ move                             ? keys · q quit|
 80 |Docker events stopped; retrying   Enter open · / filter          ? keys · q quit|
 50 |Docker events stopped; retrying    ? keys · q quit|

Edit picker
120 |Enter edit · / filter · ↑↓ move                                                                          ? keys · q quit|
 80 |Enter edit · / filter · ↑↓ move                                  ? keys · q quit|
 50 |Enter edit · / filter · ↑↓ move    ? keys · q quit|

Fact editor (the buttons; the row is the screen less a column each side)
120 |  Edit (e)   Add (a)   Copy (c)   Delete (d)   Save changes (s)   Keys (?)   Quit (q)                                   |
 80 |  Edit (e)   Add (a)   Delete (d)   Save (s)   Keys (?)   Quit (q)              |
 50 |  Edit (e)   Save (s)   Keys (?)   Quit (q)       |

Announcements (`q close` for both callers: the viewer closes)
120 |↑↓ choose · PgUp/PgDn scroll · ● new                                                                    ? keys · q close|
 80 |↑↓ choose · PgUp/PgDn scroll · ● new                            ? keys · q close|
 50 |↑↓ choose · PgUp/PgDn scroll      ? keys · q close|

Restore, the review
120 |Enter open · Tab part · ← → file · ↑↓ move                                                               ? keys · q quit|
 80 |Enter open · Tab part · ← → file · ↑↓ move                       ? keys · q quit|
 50 |Enter open · Tab part · ← → file   ? keys · q quit|

Restore, the form
120 |Tab next field · Enter next (restores on the last)                                                  F1 keys · Esc cancel|
 80 |Tab next field · Enter next (restores on the last)          F1 keys · Esc cancel|
 50 |Tab next field                F1 keys · Esc cancel|
```

`● new` is a legend, not a key, its marker in the list's Ok colour, and
the first to go. The
restore screen ends sb (ops spec: "Restore has no step after it"), so
its tail says `q quit`.

### The `?` overlay

- **One overlay per screen,** drawn from the same key table as the
  footer, so the two can't disagree. It lists every key that works on
  that screen, aliases included (`k j`, `b Space`, `g G`, `F`), and
  nothing that doesn't.
- **Layout:** ratatui's popup pattern over the screen (`Clear`, then a
  rounded `Block` over `Rect::centered`; tui-popup's `Popup` has no
  bottom title), titled ` Keys: <screen> `, its body a `Paragraph` of
  groups (Move, Find, Show, Save, Change, Leave), each a heading and two
  columns: the keys, as wide as the column's widest, then the meaning. A
  last line names the mouse. When one column is taller than the screen
  and two fit, the groups go in two columns; otherwise one column,
  wrapped, scrolled with tui-scrollview when it is taller than the
  screen, with `↑↓ scroll` on its bottom border (`any key closes` when it
  fits). The fact editor's fits in two columns at 80×24. *Amended
  2026-11-19:* in one column a long description wraps at words to the
  box (display widths, as the footer measures them), each continuation
  under the description column, and the mouse's line under its text
  after `Mouse: `; a description column narrower than 8 is left to the
  paragraph's own wrap.
- **Closing:** any key that doesn't scroll it closes it and does nothing
  else (drawn whole, the scroll keys close it too); a paste under it does
  nothing, so `q` closes it (less's help habit) without quitting. Ctrl-C
  still stops sb. The screen behind keeps running (following, the live
  picker).
- **Mouse:** a click outside it closes it; the wheel scrolls it.

The log viewer's overlay at 80×24:

```text
app  service  following


 ╭ Keys: log viewer ──────────────────────────────────────────────────────────╮
 │                                                                            │
 │ Move                                   Show                                │
 │  ↑ ↓  k j      a line                   f F        follow on, off          │
 │  PgUp PgDn     a page                   w          wrap on, off            │
 │  b Space       a page up, down          t          time, origin on, off    │
 │  Home End g G  log's start; end follows                                    │
 │  ← →  h l      sideways (wrap off)     Leave                               │
 │                                         Esc        back to the list        │
 │ Find                                    q          quit                    │
 │  /             search the whole log     Ctrl+C     stop (exit 130)         │
 │  n N           next, previous match                                        │
 │  Esc           clear the search                                            │
 │                                                                            │
 │ Mouse: the wheel scrolls; Shift+wheel goes sideways                        │
 ╰ any key closes ────────────────────────────────────────────────────────────╯




Esc back · / search · f follow · w wrap · t time                 ? keys · q quit
```

The same at 50×19, one column, scrolled (the scrollbar on the right
border):

```text
app  service  following
 ╭ Keys: log viewer ────────────────────────────╮
 │ Move                                         █
 │  ↑ ↓  k j      a line                        █
 │  PgUp PgDn     a page                        █
 │  b Space       a page up, down               █
 │  Home End g G  log's start; end follows      █
 │  ← →  h l      sideways (wrap off)           █
 │                                              █
 │ Find                                         █
 │  /             search the whole log          │
 │  n N           next, previous match          │
 │  Esc           clear the search              │
 │                                              │
 │ Show                                         │
 │  f F           follow on, off                │
 │  w             wrap on, off                  │
 │  t             time, origin on, off          │
 ╰ ↑↓ scroll · other keys close ────────────────╯
Esc back · / search · f follow     ? keys · q quit
```

The other overlays, by group: the log picker (Move; Find: `/`, Esc;
Show: Tab, Shift-Tab kind; Leave: Enter open, Esc, `q`); the edit
picker (Move; Find; Leave: Enter edit, `q`); the fact editor (Move, with
← → `h` `l` to fold; Find; Change: `e` `a` `d` `u` `c`, `m` menu; Save:
`s` Ctrl+S; In a box: Enter keep, Alt+Enter new line, Esc cancel;
Leave); the announcement viewer (Move: ↑↓ ← → `h`
`l` the entry, the page keys and Home End the text; Leave); the restore
review (Move; Tab parts; Enter, Esc; Leave).

### Comparable tools and libraries

- **less** (the pager `journalctl` uses): `h` help, `q` quit, `j` `k`,
  Space and `b` a page, `d` `u` half a page, `g` and `G` (or Home and End)
  the ends, `F` follow, `/` and `n` `N` search, `s` saves piped input to
  a file, `-S` chops long lines
  ([less(1)](https://man7.org/linux/man-pages/man1/less.1.html),
  COMMANDS). sb takes its Space, `b`, `g`, `G`, `F`, `/`, `n`, `N`.
  Not `?`, which in less searches backwards: every other tool below
  uses `?` for help, and sb's search has `N`.
- **k9s:** `?` shows the keys, Esc backs out of a view or filter, `/`
  filters, `:q` or Ctrl-C quit
  ([commands](https://k9scli.io/topics/commands/)). Its log view: `w`
  wrap, `t` timestamps, Ctrl+S save, `c` copy, `s` autoscroll, `q` and
  Esc back
  ([internal/view/log.go](https://github.com/derailed/k9s/blob/master/internal/view/log.go),
  `bindKeys`). sb matches `w`, `t`, `c`, `?`, Esc. Its `s` is sb's
  `f`; sb keeps `s` for the fact editor's save (sb-go's) and `q` for
  quit (sb-go's); the log viewer has no save (2026-11-19).
- **lazygit:** `?` opens the keybindings menu, Esc cancels, `q` and
  Ctrl-C quit, `/` searches the view, `<` `>` or Home End go to the ends,
  PgUp PgDn scroll
  ([Keybindings_en.md](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md)).
- **htop:** F1, `h` or `?` the help screen; F3 or `/` search; F4 or `\`
  filter; F10 or `q` quit; Home, End, PgUp, PgDn
  ([htop(1)](https://man7.org/linux/man-pages/man1/htop.1.html), 3.6.0).
  sb takes F1 beside `?`, for text boxes.
- **ratatui 0.30.2 and its widgets:** no help or key-hint widget, in
  ratatui or in tui-widgets (its crates: tui-bar-graph, tui-big-text,
  tui-box-text, tui-cards, tui-equalizer, tui-popup, tui-prompts,
  tui-qrcode, tui-scrollbar, tui-scrollview;
  [tui-widgets](https://github.com/ratatui/tui-widgets)), nor among
  [the third-party widgets](https://ratatui.rs/showcase/third-party-widgets/).
  The popup pattern is ratatui's (`Clear`, then a bordered block,
  [popup example](https://ratatui.rs/examples/apps/popup/)), which
  tui-popup 0.7.7 wraps: centred, sized to its body, any `KnownSize`
  widget as the body
  ([docs](https://docs.rs/tui-popup/0.7.7/tui_popup/)). sb already uses
  tui-popup for the fact editor's dialogs and tui-scrollview 0.6.8 for
  scrolling them. The footer is a `Line` of `Span`s, as every screen
  draws it today. The rule that drops hints to fit is ours (domain
  logic), with no library to follow.
- **ratatui-textarea 0.9.3** maps Ctrl+R to redo, Ctrl+U to undo and
  Ctrl+V and Alt+V to the page keys, and leaves Ctrl+S unmapped
  ([key table](https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/index.html)).
  sb's boxes add no Ctrl key of their own once Ctrl+S stops revealing.

### sb-go's keys

sb-go 35a7acc had five of these screens (not the restore review, which
is sb's).

- **Logs viewer** (`cmd/logs.go`, `cmd/dockerLogs.go`; the keys sb-go
  doesn't take itself go to bubbles `viewport` v2.2.1, whose defaults
  add `b` and Space for a page, `h` `l` sideways and `u` `d` for half a
  page): ↑/`k`, ↓/`j`, ←/`h`, →/`l`, PgUp/`b`, PgDn/Space, `t`, `f`,
  Enter, Esc back, `q` quit, Ctrl-C. All kept; `b`, Space, `h` and `l`
  come back. `u` and `d`, never in sb-go's hint row, go (decision 1).
- **Logs picker** (bubbles `list` v2.2.1's defaults, filtering and help
  off): ↑↓ `k` `j`, PgUp PgDn `b`, Home End `g` `G` kept; `u`, `d`, `f`,
  `h`, `l`, ← and → as page keys not kept (`d` `u` are the fact
  editor's, `f` follows, `h` `l` ← → are directions).
- **Edit picker** (the same `list`, help and filter on): the same, plus
  `/` (kept, new in sb) and `?` (kept, as the overlay). The list's own
  `v` ("select") quit the list without a choice, bubbles' default; not
  kept.
- **Fact editor** (`factui/update.go`): `s` and Ctrl+S save (back),
  `q`, `/`, Esc clearing the search (back), ↑↓ `k` `j`, Home End PgUp
  PgDn, ← → fold, Enter, `a` `e` `d` (kept). Not kept: `x` and `c`
  (expand and collapse all; sb's page folds per app and `c` copies), `[`
  `]` (no inspector pane), the review dialog's `a` and `r` (sb's
  question is `y`, `d`, `n`).
- **Announcement viewer** (`saltbox/announcements.go`, bubbles
  `viewport`): ←/`h`, →/`l` previous and next (back), PgUp/`b`,
  PgDn/Space (kept), `q`, Esc, Ctrl-C (kept). ↑↓ scrolled the text in
  sb-go and choose the entry in sb (the reviewed design, decision 39);
  the viewport's `f`, `u` and `d` page aliases are not kept.
- **Restore form** (`cmd/restore.go`): Tab, Shift-Tab, ↑, ↓, Enter, Esc,
  Ctrl-C, all kept.

### Migration, per screen

What changes for a user's fingers. sb is unreleased, so "today" is main,
which only the maintainer's checks have used; sb-go is what users know.

| Screen | From sb-go | From today's main |
|---|---|---|
| Log picker | `?`, `/` filter (new in sb already), Shift-Tab; `u` `d` `f` `h` `l` no longer page | `g` `G` `b` Space, Shift-Tab, `?` added |
| Log viewer | `u` `d` no longer page (PgUp PgDn, `b` Space do); keys in a footer | `b` Space `h` `l`, `F`, `?` added; `u` `d` gone; save (`s`) and print (`p`) gone (2026-11-19); keys in a footer, one log row fewer |
| Edit picker | `/` and `?` as before; `v` no longer leaves | page keys, Home End `g` `G`, `/`, `?` added |
| Fact editor | `s` and Ctrl+S save again; every value shows, as in sb-go; Esc clears a search before it quits, as sb-go; `x`, `c` collapse, `[` `]` gone | `h` `l` fold, `g` `G` `b` Space, `?` added; **save is `s`, not `v`; nothing is masked, and Show/Hide and the boxes' Ctrl+S are gone**; Esc clears a search first; dialogs cancel with Esc, not `c` |
| Announcements | ← → `h` `l` move again; ↑↓ choose the entry | ← → `h` `l` added; `?` |
| Restore | the form as sb-go | the files show as they are; `s`, `a` and `c` gone (Continue by Enter on it, `q`, Esc); ← → previous, next file, not out of and into the pane; Space no longer presses Continue |

`v` does nothing anywhere afterwards, so a tester's old save habit costs
nothing.

### Testing

- **One key table per screen** in sb-tui, each row a key, its meaning
  and its group; the overlay draws it and the footers take its help key
  (`?`, or F1 where the table has no `?`). Each footer names a subset of
  the table in its own words.
  A test over all the tables checks rule 1: a key has the same meaning
  wherever it appears (with rule 7's arrows as the one declared class).
- **Each screen, on `TestBackend`:** every key in its table acts (not
  `Flow::Unchanged`), and a sample of keys not in it change nothing. In
  each text box, `?`, `q`, `s` and `r` type.
- **The footer:** snapshots at 120, 80 and 50 for every screen and
  state above; a test over every width from 10 to 240 that no hint is
  cut, the row is never wider than the screen, and the tail is there
  from 16 columns.
- **The overlay:** snapshots at 80×24 and 50×19 for each screen; any
  key closes it without acting (`q` doesn't quit); the scroll at 50.
- **Values:** the fact page, its boxes, the changes panel, the drift
  dialog and the restore pane show a secret-named value as it is; no
  fact value and no restored file's content reaches the run log.
- **PTY, through sb:** `sb logs` on a journal fixture (`?`, a key, `q`
  exits 0), `sb fact` on a fixture directory (`s` opens the save
  question), with `SB_LOG_DIR` in a temp directory.

### Scope

- **In:** the keys and hints of the screens above, the overlay, the
  labels.
- **Not doing:** keys set by the user; mouse changes; term-ui's inline
  prompts (`y/N`, not ratatui screens); the motd layout and `sb logs
  <name>` (declined 2026-11-18); expand-all and collapse-all in the fact
  editor.

### Success criteria

1. Every key in the map has one meaning on every screen (the table test).
2. No screen masks a value, and no key reveals one; the run log holds
   none.
3. At 50 columns every screen shows `? keys` and how to leave; at 80
   the viewer shows its search, follow, wrap and time keys.
4. `?` (F1 from a text box) lists every key of the screen, and only
   those.
5. sb-go's keys work where this section says they're kept.

### Secrets: where masking stays

Decision 2 (values are shown, not hidden): the screens show
every value as it is. Masking stays wherever text leaves the screen:

| Where | What masks | Kept because |
|---|---|---|
| validate-config's and `sb edit`'s issue lines, printed and logged | `facts::secret::is_secret_key` inside sb-config's `secret::is_secret` and `rules::secret_paths` (a secret path's value never quoted) | the terminal's scrollback and the run log are shared for support |
| sb-config's logged URLs | `is_secret_key` over a URL's query names (`http::loggable_url`) | the run log |
| The run log | no fact value and no restored file's content is logged (map decision 11) | the run log |
| term-ui's step output | `Line::secret` prints `[REDACTED]` unless `.shown()` | it stays in the scrollback and is pasted into support requests |
| exec's child records | `Cmd::secret` | the run log |
| The restore form's password fields | ratatui-textarea's `set_mask_char` | typed input, as any password prompt; not a value shown for review |

Removed, with no user left: the fact editor's dots, Show and Hide,
the boxes' mask and Ctrl+S, the drift dialog's dots; the restore pane's
mask, `s` and `a`, and the masked last frame it drew as it closed.
sb-config's `facts::secret::is_secret` and `is_secret_value` (the value
rule, a URL with credentials) stay, with no caller now: their only
caller was the restore pane, and they are ready for output meant for
sharing. Every screen erases its rows as the last step before it is
left (decision 6), so a terminal without an alternate screen (GNU
screen's default) keeps no frame with plain values, but for the two gaps
in Known limits (a panic on stdout, a resize).

### Decisions (the maintainer, 2026-11-18)

1. **`d` and `u`:** the viewer stops paging on them; the fact editor
   keeps `d` delete and `u` undo (option A).
2. **Reveal:** replaced. Neither `sb fact` nor restore's review hides a
   value, so there is no reveal key; masking stays where text leaves the
   screen ("Secrets: where masking stays"). The drift dialog keeps
   `Reload (r)`.
3. **Arrows:** ← → and `h` `l` keep a per-screen direction (option A).
4. **Esc** leaves a top-level screen when nothing is open (option A).
5. **The tail** `? keys · q quit` sits right-aligned on every screen
   (option A).
6. **Blank on exit** (the maintainer, 2026-11-19, on the security
   review's question of 2026-11-18): before leaving `sb fact`,
   restore's viewer and every other screen, the screen's area is cleared
   as the last step, so a terminal without an alternate screen (GNU
   screen's default) keeps no frame with plain values.

Built as (2026-11-19): every row erased in place (each row's
`MoveTo` and `Clear(CurrentLine)`, through ratatui's `Backend`), then
the cursor home, in `Tui::leave`, the screen's drop and the forced
exit's prepared sequence (on stdout and on the tty). Not ratatui's
`Terminal::clear`, which the decision named, nor `Backend::clear`: both
erase the display (ESC [2J), which GNU screen 4.09 and tmux 3.2a without
an alternate screen move into their scrollback, frame and values
included (docs/platform-notes/terminal-clear-scrollback.md); and
`Terminal::clear` first asks the terminal for the cursor's position and
fails without clearing when no answer comes.

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
  - `sb docker logs` lists the containers only until `Tab` comes (M70;
    the command removed 2026-11-18, decision 1).

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
    buffer's caps). The hints become `n/N match · Esc clear · ...` while a
    search is in effect (in the footer since the key map, 2026-11-19,
    fitted to the width).
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
    change there; a resize there makes the marks again) still grew
    with the column, since the message left
    of it is parsed for its styles: 5.7 to 5.8 ms at 4,000 on these
    records (17.8 before), more on CJK or colour-per-word text.
    Closed (the maintainer, 2026-11-15, the follow-up "The viewer's
    far-right rebuild frame meets 5 ms"): the colour parse resumes.
    term-ui's `DisplayText::resumable` reports where a parse can start
    again (before a printed character, about every 64 columns) and
    `DisplayText::from_bytes_at` parses from there; the viewer keeps
    those places for each line it built far right, with the screen
    column and ansi-to-tui's style there (`resume.rs`), in the frame
    cache and within its 4 MiB, across a change of theme, `t` or the
    search too. A text far right is built from the last place an eighth
    of a screen left of the window through a quarter right of it, and
    drawn with the rest as `Paragraph::scroll`'s offset. A message of
    several screen lines or one a carriage return takes back, a search
    for a query that can overlap itself, and a search on a line with a
    character that joins the one before it (a variation selector, a
    zero-width joiner, a keycap's mark, a skin tone) are built from
    their start. A `t` change or a search at 1,000 and 4,000 takes 2.3
    to 4.1 ms on plain, CJK and colour-per-word records (6.0 to 28.6 ms
    before at 4,000), a resize from 160 to 240 columns 3.8 to 4.9 ms
    (one frame of each a little over 5 ms in the noisiest run); a
    narrower resize keeps the texts. `viewer/tests.rs`'s
    `far_right_holds` times them and counts the bytes parsed (about the
    same at 4,000 as at 1,000), `resume/tests.rs`'s
    `a_resumed_line_draws_as_the_whole_text` and `viewer/tests.rs`'s
    `far_right_texts_draw_as_paragraph_scroll` check the drawing against
    the whole text cell by cell. A line seen for the first time far
    right (a page up or a jump there from far away) had no places yet
    and was parsed from its start once: 7.4 to 30.2 ms for a screen of
    them at 4,000. Closed (the maintainer, 2026-11-16, the follow-up
    "First sight far right meets 5 ms"): with wrap off, a worker thread
    (`ahead.rs`) makes ahead the places of the lines the next frames may
    show (two screens either way, the buffer's ends, the lines around
    the search's next and previous match) and, away from the first
    column, the texts of those a key brings in at once (a screen either
    way, the ends), for the window in view; such a frame draws them as
    they are, in 1.8 to 4.8 ms (one frame of 72 at 5.5 ms under very
    heavy load), and the lines' places stay kept. It is kept only for
    lines still wanted, within 4 MiB; a frame asks for at most 32 lines
    at once, and a panic on the worker ends the screen as before. `viewer/tests.rs`'s `first_sight_*`
    tests check that a page up, Home (with the log's start loaded) and
    End there build nothing; Home's own page, when it asks for one,
    starts at the first column, and the frame-cost test times its frame;
    `ahead/tests.rs` the worker, its bound and which texts serve. A
    jump to lines none of that covers, or one sooner than the worker
    finishes, still builds each line from its start once
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
  - RLIMIT_NOFILE: `sb logs` (and `sb docker logs` until its removal,
    2026-11-18) call
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

None. The key map's questions were answered on 2026-11-18 and 2026-11-19
("Keys: one map for every screen", Decisions). decided.md settled the
brief's nine; decisions 18 to 20 follow from recorded decisions. The editors' question (when `sb fact` takes a
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
  *Amended 2026-11-18:* the `docker logs` row reads "removed"
  (decision 1).
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
   reloads; `sb docker logs` (removed 2026-11-18, decision 1); the
   Docker-unreachable row.
3. **The unified picker (S):** Needs attention, the name filter
   (ratatui-textarea), the kind filter and `Tab`, mouse on both screens,
   the services' 5 s refresh.
4. **Search (S):** smart-case matches, highlights across spans, n/N,
   horizontal scrolling with wrap off.
5. **The inline fallback (M):** the picker on `/dev/tty` with the plain
   stream; the no-terminal error. (Its `s` and `p` were removed on
   2026-11-19: decision 24.)
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
