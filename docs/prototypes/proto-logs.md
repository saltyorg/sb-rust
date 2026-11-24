# Prototype: proto-logs

> The prototype's code was removed once its module, `sb logs` (crates/tui,
> crates/host, crates/docker), was built (the maintainer, 2026-11-06); it
> is in git history at f5f05afc (`git show
> f5f05afc:prototypes/proto-logs/`). These findings stay as the record.
> Figures measured in the prototype's own runs are left out: each result
> says what was checked and what it showed.

**Question:** Can bollard log streams and `journalctl -f` output feed a ratatui
viewer with follow, scroll, and search, without blocking input?

**Crates tried:** ratatui@0.30.2 (`unstable-rendered-line-info`), crossterm@0.29 (`event-stream`), mimalloc@0.1.52 (musl builds),
ratatui-textarea@0.9.3 (search box), ansi-to-tui@8.0.1 (ratatui org),
bollard@0.21.1, tokio@1.54 (process, signal), futures@0.3, serde_json@1
(systemctl JSON), jiff@0.2 (Docker timestamps), dirs@7 (state directory).

**What was built:** about 480 lines, against roughly 2,070 non-blank lines
across sb-go's `logs.go` and `dockerLogs.go`. One viewer serves both source
types:
- A picker lists services and containers with their status, grouped into
  Needs attention, Services and Containers (see "Picker redesign"
  below and `docs/ideas/log-picker.md`).
- The viewer streams the chosen source. Containers use bollard's
  `logs(follow, tail=500)`; services use a `journalctl -u … -n 500 -f` child.
- Both feed one `tokio::select!` loop alongside crossterm's async
  `EventStream` and a redraw tick.
- Viewer features: a 5,000-line ring buffer, FOLLOW/PAUSED states, arrow and
  page scrolling, `/` search with hit count and n/N jumps, and Esc back to the
  picker.
- `--inline <source>` streams plain output to stdout instead, for comparison.

**How it was checked:**
- **Synthetic sources:** a throwaway container and a transient systemd unit
  with predictable coloured output, removed afterwards. Used for search and
  switching.
- **Real sources** on a Saltbox install, read-only: a container with very
  long log lines, a container emitting ANSI colour, a busy container, and a
  Saltbox-managed systemd service. Each was opened, followed, paused,
  searched and resumed in the PTY harness. Only structure was recorded
  (states, widths, escape leaks), never log content.

## Result

- **Input never blocks.** Keys stay responsive while lines stream in. Queued
  lines are drained in one batch per wake-up, so a busy log costs one redraw,
  not one per line.
- **Follow and pause work.** Scrolling up pauses at the current position.
  End or `f` resumes following.
- **Search:** finds every match, shows the hit count, and n/N jumps between
  matches while pausing follow.
- **ANSI colours** in container output are parsed by ansi-to-tui, with no raw
  escape sequences on screen. That code is library-owned.
- **Switching sources is clean.**
  - Each stream is tagged with a generation number, so lines from the source
    you left are dropped.
  - Aborting the task drops the `journalctl` child, which is killed via
    `kill_on_drop`.
  - No `journalctl` was left behind after Esc, quit, or Ctrl-C in inline mode.
- **Streaming beats sb-go's approach.** sb-go polls journal cursors on a
  timer. Here `journalctl -f` and bollard's follow stream push lines as they
  arrive, which removes the cursor bookkeeping entirely.
- **Inline mode vs TUI:**
  - Inline (picker, then plain stream) gets scrollback, search and copy from
    the terminal for free, and suits piping into `grep` or a file. It can't
    switch source without restarting.
  - The TUI adds switching between sources, an in-app search count, and
    follow/pause.
  - Proposal: keep both. The TUI is the default on a terminal, and
    `--follow`/`--no-tui` or piped output use inline mode.
- **Real logs:** all four real sources rendered without any raw escape
  sequences, and follow, pause, search and resume behaved as with the
  synthetic sources. The journal source streamed like the container sources.
- **Long lines (shown only by real logs):** application lines can run to
  thousands of characters (JSON, stack traces). The design:
  - **Wrap on by default**, toggled with `w` or the right-click menu.
  - **With wrap off, scroll sideways:** ←/→, Shift+wheel or a horizontal
    wheel, 8 columns per step, with the column shown in the header.
  - These two replace an open-line popup (with JSON pretty-printing).
- **Scrolling moves the view directly, by screen rows** (no selection
  cursor). The earlier design moved a highlight and only scrolled once it
  hit the edge, so the first wheel notches changed nothing visible, which
  felt slow. Now:
  - one notch moves the view immediately;
  - wrapped long lines scroll smoothly instead of jumping a line at a time;
  - scrolling up pauses, and reaching the bottom resumes following;
  - a paused view that would leave rows empty (e.g. after turning wrap off)
    follows instead, so the screen stays full.
- **Container restarts:** the follow stream ends when a container stops. The
  viewer now writes "container stopped, waiting for it to come back…", polls
  until the container (or a re-created one with the same name) runs again,
  says so, and resumes from the stop time. Checked by restarting a demo
  container mid-stream. Real containers were not restarted.
- **Search while following** jumps to the most recent match first: a
  search that starts after the newest line finds nothing.
- Wrapping needs exact row heights, which use ratatui's unstable
  `rendered-line-info` feature (`Paragraph::line_count`), the same pin as
  proto-markdown.
- **Redraw cost with long lines:** measured with an opt-in frame-time log
  on a source with very long lines. Keeping the paused view full measured
  every line from the top of the view to the end of the buffer on every
  redraw, so the cost grew with the buffer, and each mouse movement redraws,
  so moving over the popup queued up slow frames. Fix: stop measuring once
  the screen is full; the redraw cost then no longer grew with the buffer.
  Not changed: skipping redraws on mouse movement. The fixed redraws didn't
  need it, and an unmeasured "improvement" would only add code. The module
  needs a frame-time budget check with long-line fixtures so this can't
  regress unnoticed.
- **Lockups with a real mouse:** each input event caused a full redraw. A
  real mouse sends motion events faster than redraws finish in a large
  window, so redraws queued up. A burst of back-to-back motion events
  showed it: one redraw for each event. Fix: handle every waiting event,
  then redraw once, so a burst costs a redraw or two. A harness that sends
  one event at a time can't show this.
- **Terminal codes leaking into the shell after quitting:** mouse codes the
  terminal sends while capture is being switched off arrived after the app
  stopped reading, and the shell printed them. Fix: after disabling mouse
  capture, read and discard input for 150 ms before restoring the terminal.
  Checked by sending mouse codes shortly after `q`: before the fix the shell
  received them, after it nothing.
- The prototypes were tried as release builds, since debug redraws are
  much slower.
- **Scrolling that still felt slow (after the lockup fixes):** redraw time
  and the bytes written per frame were small and the same before and after
  the scrolling redesign, so neither CPU nor output volume explained it. The
  cause was the cursor-based scrolling described above. Render caching was
  considered and not added: it would add code without a measurable gain.
- **Production build (for comparing against sb-go):** a `production`
  profile (fat LTO, one codegen unit, stripped), built static for musl with
  `cross`. musl's default allocator made each redraw measurably slower than
  the glibc release build; mimalloc (musl builds only) recovered part of
  that and was kept. Every build redrew well within one 60 Hz frame
  (16 ms), so redraw time doesn't explain a feel difference against sb-go.
- **Slow picker:** the batching change had polled crossterm's `EventStream`
  with a throwaway waker (`now_or_never`). The stream then woke that dead
  waker, so input was only noticed on the next 250 ms tick. After the fix,
  key-to-screen latency fell to within one frame.
- **Root cause of the whole series of input bugs:** the event loop, batching,
  mouse modes and terminal setup were hand-written instead of following the
  libraries' documented patterns. The prototype now uses the ratatui async
  event handler recipe
  (https://ratatui.rs/recipes/apps/terminal-and-event-handler/):
  - a task reads `EventStream` via `next().fuse()` in `tokio::select!` and
    sends events over an unbounded mpsc channel;
  - rendering happens on a 60 fps interval, only when something changed;
  - a 4/s tick runs alongside.

  Removed: the hand-rolled batching, the byte-counting stdout wrapper
  (standard `ratatui::init()` again), and the hand-written mouse-mode escape
  codes (back to crossterm's `EnableMouseCapture`). With frame-rate
  rendering, pointer-motion events no longer cause redraws, which was the
  only reason to hand-write button-only mode. Still own code: windowed
  scrolling, because `Paragraph::scroll` applies after wrapping, so
  following would need the unstable `line_count` over the whole buffer every
  frame; and the input drain on exit, which no doc covers but measurably
  stopped the leak (marked UNVERIFIED in the code).
- **Lesson for the module:** every framework-level pattern (event loop,
  terminal setup, input modes) follows the official recipe or docs, cited in
  the code. Measure input latency end to end, not just redraw time.
- **Design notes for the module:**
  - Search highlights whole lines. Per-match highlighting means splitting the
    spans ansi-to-tui produces.
  - The search query carries over when you switch source. It should probably
    reset.

## Picker redesign

The maintainer's decisions (2026-10-29): sb-go's service list, in a
grouped status list with colour, stopped containers, rich status, recent
sources, and one picker for both kinds of source.

- **Same services as sb-go.** sb-go lists `saltbox_managed_*` plus
  `docker`, and hides units whose file is gone. The picker uses the same
  filter, read from `systemctl list-units --output=json` rather than the
  human table sb-go parses. JSON table output is present in systemd v249 (Ubuntu 22.04),
  checked in the v249 source (`output_table()` in
  `src/systemctl/systemctl-util.c`). Timestamps come from `systemctl show`,
  whose output systemctl(1) calls computer-parsable.
- **Status words and colour.** Services show
  running/uptime, failed and how long ago, restarting, or scheduled with the next
  timer run. Containers show uptime, exit code, out-of-memory, restart count
  and failing health checks, from one bollard inspect per container, run in
  parallel. The words carry the meaning without colour. The rules are pure,
  tested functions in `src/sources.rs`.
- **Timer-driven services** count as healthy between runs (`✓ scheduled ·
  next run in …`): a refresh-style unit working as intended shouldn't look
  idle. A failed run leaves the unit `failed`, which is already under Needs
  attention, so inactive with a next run scheduled means healthy.
- **Disabled units are shown, not hidden.** `list-units` returns only
  loaded units, and systemd unloads a disabled unit that nothing uses. sb-go
  and the first version of this picker therefore hid disabled
  `saltbox_managed_*` units completely. A disabled unit is a lack of
  cleanup, so the picker also reads
  `systemctl list-unit-files --output=json`.
  - A disabled unit that is neither running nor scheduled by a timer shows
    as `! disabled · not cleaned up`, in amber, under Needs attention.
  - Running or timer-scheduled disabled units keep their normal status.
  - Checked against v249 source: JSON table output, the `unit_file` key
    (spaces in headers become underscores) and bare unit names
    (`basename(u->path)`).
  - A demo disabled leftover unit showed as intended on real systemd.
- **Needs attention:** failed or auto-restarting units, containers that
  exited non-zero, ran out of memory, are restarting, are unhealthy, or were
  restarted by Docker less than 10 minutes ago. A container that exited with
  code 0 is listed as stopped, not as a problem.
- **Sections are complete per kind;** Needs attention is a shortcut on
  top, so a source is always under its own kind as well.
  ratatui's `List` has no unselectable rows, so the cursor skips headers in
  our own `step()`. Selection survives reloads and filtering.
- **Live updates.** Docker's event stream (bollard `events`, type
  `container`) marks the list stale. Services re-read every 5 s while the
  picker shows. Reloads start on the 4/s tick, which also batches event
  bursts. The first load runs in the background, so the screen appears at
  once.
- **Recent section: removed.** A Recent section (last 5 opened, kept in a
  state file) was built and then removed by the maintainer's decision,
  together with its `dirs` dependency.
- **Load time:** a full load (three systemctl calls plus an inspect of
  every container, process start included) stayed well under the 150 ms
  budget that would have called for showing the list before its status.
- **How it was checked:**
  - `TestBackend` render tests on synthetic sources check the sections,
    status text and cursor behaviour.
  - On a Saltbox install, the picker listed the same services as sb-go's
    filter.
  - A throwaway container that exited with code 3 moved into Needs attention
    within a tick, without input. A restart-policy loop showed its restart
    count.
  - Filter-then-Enter opened the stopped container's log.
  - Clicks on headers were ignored, the wheel moved the cursor, and a click
    then a second click opened a source.
  - A demo script (`scripts/demo-log-sources.sh`, also in git history at
    f5f05afc) created a demo source for every state the picker can show:
    8 `sbdemo-*` units (one a disabled leftover) and 10 labelled containers
    (crash, out of memory, restart loop, unhealthy, recently restarted,
    running, health check pending, paused, stopped, never started). All
    showed with the intended section, symbol and words.
  - systemctl lists only loaded units. An idle unit file is unloaded, so
    the "stopped" demo is enabled at runtime, as an enabled Saltbox unit
    would be.
- **Colour:** one solid selection bar, and more colour overall.
  - The selected row was `reversed()`, which swaps each span's own colours,
    so the bar came out in several colours. It is now one solid bar:
    ratatui paints `highlight_style` over the whole row after drawing it
    (ratatui-widgets 0.3.2 `list/rendering.rs`), so a style with both
    colours that also clears DIM replaces every span's styling. A
    `TestBackend` test checks every cell of the selected row.
  - **Truecolor palette** (designed for truecolor, as sb-go forces it): a
    16-colour palette was tried first and replaced.
    - sb-rust's own palette, for dark backgrounds, in a shared proto-theme
      crate (also used by proto-markdown), now cli-core's theme
      (`crates/cli-core/src/theme.rs`). Colours are named by role: text
      tones, accent, health (ok, problem, warn, idle), kinds and sections
      (each with a darker rule tone), selection and search hit. It is not copied from sb-go or charmtone.
    - Explicit tones replace the DIM modifier, which terminals render
      inconsistently. The background stays the terminal's own.
    - crossterm writes `Color::Rgb` as 24-bit SGR without checking the
      terminal (crossterm 0.29 `style/types/colored.rs`), so truecolor is
      forced with no code. Checked on the binary with
      `TERM=xterm-256color` and no `COLORTERM`: only 24-bit SGR codes, no
      16-colour ones.
    - A `TestBackend` test fails if any cell on the picker or viewer
      (including the menu and a search hit) uses a named ANSI colour.
      Mutation-checked: putting back one `Color::Green` fails it.
    - The fallback for terminals without truecolor is planned, not built
      (capability map, decision 10).
- **Open:** whether "+N more" is needed when many sources fail, and whether a
  source that recovers should stay marked for a while. Services have no live
  signal yet (systemd's D-Bus signals would give one, at the cost of a D-Bus
  dependency).

## Verdict

_Approved by the maintainer (2026-10-29), open to later improvements. The
review shaped the picker (grouped status list, sb-go's service filter,
disabled leftovers flagged, timer services healthy, no Recent section), the viewer (scroll instead of a popup, faster input) and
the truecolor palette._

**Adopt.** One async loop with crossterm `EventStream`, bollard streams and a
`journalctl -f` child covers both log viewers, at a fraction of sb-go's size.

## Map impact

- Stack table: add `ansi-to-tui` for ANSI log output, and the crossterm
  `event-stream` feature with tokio for async screens.
- `tui` module: one shared log viewer for docker and systemd. Streaming, not
  polling. Generation-tagged streams. Inline fallback when not a TTY or when
  asked.
- `docker` and `host` modules expose log streams (bollard and a
  `journalctl -f` child with `kill_on_drop`) that the viewer consumes.
- `host` module: the service list is sb-go's filter, read from systemctl's
  JSON output (`list-units`, `list-unit-files`, `list-timers`, `show`), so
  disabled leftovers are visible. The status rules (`src/sources.rs`) are
  pure, tested functions and carry over as-is.
- `docker` module: container status from list plus a parallel inspect, kept
  live by bollard's event stream.
- `tui` module: the picker design is in `docs/ideas/log-picker.md`. The
  role-named truecolor palette (proto-theme, now cli-core's theme) is the
  starting point for a shared theme across screens (capability map,
  decision 10).
- Stack table: add `serde_json` (systemctl JSON) and `jiff` (Docker
  timestamps).
