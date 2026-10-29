# Prototype: proto-logs

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
- **Real sources** on a real Saltbox install, read-only: the container with
  the longest log lines (over 5,000 characters), a container emitting ANSI
  colour, the busiest container, and a Saltbox-managed systemd service. Each
  was opened, followed, paused, searched and resumed in the PTY harness. Only
  structure was recorded (states, widths, escape leaks), never log content.

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
  thousands of characters (JSON, stack traces). After review, the design is:
  - **Wrap on by default**, toggled with `w` or the right-click menu.
  - **With wrap off, scroll sideways:** ←/→, Shift+wheel or a horizontal
    wheel, 8 columns per step, with the column shown in the header.
  - The earlier open-line popup (with JSON pretty-printing) was removed at
    review in favour of these two.
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
- **Bug found and fixed:** searching while following found nothing, because
  the search started after the newest line. It now jumps to the most recent
  match first.
- Wrapping needs exact row heights, which use ratatui's unstable
  `rendered-line-info` feature (`Paragraph::line_count`), the same pin as
  proto-markdown.
- **Performance (reported as laggy when opening a line):** measured with the
  opt-in `PROTO_LOGS_TIMING=<file>` frame log on a real source with very long
  lines.

  | Case | Before | After |
  |---|---|---|
  | Paused near the newest line (debug) | ~5 ms per redraw | unchanged |
  | Paused at the oldest line, popup open, mouse moving (debug) | ~70 ms | ~5 ms |
  | Same (release) | ~6 ms | ~0.5 ms |
  | Same with the buffer at its 5,000-line cap (debug) | not measured (scaled with buffer) | ~5 ms |

  Cause: keeping the paused view full measured every line from the top of the
  view to the end of the buffer on every redraw, so the cost grew with the
  buffer. Each mouse movement redraws, so moving over the popup queued up slow
  frames. Fix: stop measuring once the screen is full. Results were the same
  over two runs. Not changed: skipping redraws on mouse movement. At ~5 ms per
  frame it isn't needed, and an unmeasured "improvement" would only add code.
  The module needs a frame-time budget check with long-line fixtures so this
  can't regress unnoticed.
- **Lockups with a real mouse (reported: froze for seconds):** each input
  event caused a full redraw. A real mouse sends motion events faster than
  redraws finish in a large window, so redraws queued up. Measured with a
  burst of 300 back-to-back motion events at 240×70: 300 redraws at ~18 ms
  (debug), i.e. ~5 s of queued work. Fix: handle every waiting event, then
  redraw once. Result: 600 motion events caused 2 redraws (same over two
  runs). The earlier harness only sent one event per 150 ms, so it couldn't
  show this.
- **Terminal codes leaking into the shell after quitting:** mouse codes the
  terminal sends while capture is being switched off arrived after the app
  stopped reading, and the shell printed them. Fix: after disabling mouse
  capture, read and discard input for 150 ms before restoring the terminal.
  Measured by sending mouse codes 10, 50 and 100 ms after `q` and counting
  what the shell received: before, 600 bytes every time; after, 0.
- The launcher now builds release binaries, so trying a prototype feels like
  the real thing (debug redraws are ~10× slower).
- **"Still a tad slow" (after the lockup fixes):** measured again at 240×70
  (release) on the long-line source, with the timing log now also recording
  bytes written per frame. Scrolling redraws took ~1.3 ms and 7–20 KB, before
  and after the scrolling redesign, so neither CPU nor output volume
  explained it. The cause was the cursor-based scrolling described above.
  Render caching was considered and not added: at ~1.3 ms per frame it would
  add code without a measurable gain.
- **Production build (for comparing against sb-go):** a `production`
  profile (fat LTO, one codegen unit, stripped), built static for musl with
  `cross`, is 3.0 MB. Redraw medians while scrolling the long-line source at
  240×70, three runs each:

  | Build | Median per redraw |
  |---|---|
  | release, glibc | 1.55–1.65 ms |
  | production, musl, default allocator | 2.11–2.31 ms |
  | production, musl, mimalloc | 1.99–2.04 ms |

  musl's allocator costs ~40% per frame. mimalloc (musl builds only) recovers
  part of that and is kept. All of these are far below one 60 Hz frame
  (16 ms), so redraw time doesn't explain a feel difference against sb-go.
- **Slow picker (reported: moving up and down was "slow as hell"):** the
  batching change had polled crossterm's `EventStream` with a throwaway
  waker (`now_or_never`). The stream then woke that dead waker, so input was
  only noticed on the next 250 ms tick. Measured key-to-screen latency
  (harness baseline 51 ms subtracted): before, ~100 ms median and up to
  ~250 ms added; after, ~14 ms (within one frame).
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

Review asked for sb-go's service list and a friendlier picker. The
maintainer picked a grouped status list, with colour, stopped containers,
rich status, recent sources, and one picker for both kinds of source.

- **Same services as sb-go.** sb-go lists `saltbox_managed_*` plus
  `docker`, and hides units whose file is gone. The first prototype missed
  `docker` and kept stale units. The picker now uses the same filter, read
  from `systemctl list-units --output=json` rather than the human table
  sb-go parses. JSON table output is present in systemd v249 (Ubuntu 22.04),
  checked in the v249 source (`output_table()` in
  `src/systemctl/systemctl-util.c`). Timestamps come from `systemctl show`,
  whose output systemctl(1) calls computer-parsable.
- **Status words and colour.** Services show
  running/uptime, failed and how long ago, restarting, or idle with the next
  timer run. Containers show uptime, exit code, out-of-memory, restart count
  and failing health checks, from one bollard inspect per container, run in
  parallel. The words carry the meaning without colour. The rules are pure
  functions in `src/sources.rs`, test-first: the tests failed against stubs,
  then passed.
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
  state file) was built and then removed at the maintainer's request,
  together with its `dirs` dependency.
- **Measured:** a full load (three systemctl calls plus an inspect of every
  container, process start included) took about 20 ms across five runs on a
  real install with a few dozen containers. That is well under the 150 ms
  budget that would have called for showing the list before its status.
- **How it was checked:**
  - `TestBackend` render tests on synthetic sources check the sections,
    status text and cursor behaviour.
  - On the real install (names masked in the output), the picker listed the
    same services as sb-go's filter.
  - A throwaway container that exited with code 3 moved into Needs attention
    within a tick, without input. A restart-policy loop showed its restart
    count.
  - Filter-then-Enter opened the stopped container's log.
  - Clicks on headers were ignored, the wheel moved the cursor, and a click
    then a second click opened a source.
  - `logs-demo` now includes a crash-looping container.
  - `scripts/demo-log-sources.sh up` creates a demo source for every state
    the picker can show: 7 transient `sbdemo-*` units and 10 labelled
    containers (crash, out of memory, restart loop, unhealthy, recently
    restarted, running, health check pending, paused, stopped, never
    started). `view` opens the picker on them and `down` removes them. All
    17 showed with the intended section, symbol and words.
  - systemctl lists only loaded units. An idle unit file is unloaded, so
    the "stopped" demo is enabled at runtime, as an enabled Saltbox unit
    would be.
- **Colour (after review: "not a fan of the line selected having different
  colors", and too little colour overall):**
  - The selected row was `reversed()`, which swaps each span's own colours,
    so the bar came out in several colours. It is now one solid bar:
    ratatui paints `highlight_style` over the whole row after drawing it
    (ratatui-widgets 0.3.2 `list/rendering.rs`), so a style with both
    colours that also clears DIM replaces every span's styling. A
    `TestBackend` test checks every cell of the selected row, and failed on
    the old style.
  - **Truecolor palette (after review: design for truecolor, as sb-go
    forces it):** a 16-colour palette was tried first and replaced.
    - `src/theme.rs` holds sb-rust's own palette, for dark backgrounds,
      named by role: text tones, accent, health (ok, problem, warn, idle),
      kinds and sections (each with a darker rule tone), selection and
      search hit. It is not copied from sb-go or charmtone.
    - Explicit tones replace the DIM modifier, which terminals render
      inconsistently. The background stays the terminal's own.
    - crossterm writes `Color::Rgb` as 24-bit SGR without checking the
      terminal (crossterm 0.29 `style/types/colored.rs`), so truecolor is
      forced with no code. Checked on the real binary with
      `TERM=xterm-256color` and no `COLORTERM`: 116 24-bit SGR codes, no
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

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

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
