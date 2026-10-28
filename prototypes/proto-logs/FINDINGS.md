# Prototype: proto-logs

**Question:** Can bollard log streams and `journalctl -f` output feed a ratatui
viewer with follow, scroll, and search, without blocking input?

**Crates tried:** ratatui@0.30.2 (`unstable-rendered-line-info`), crossterm@0.29 (`event-stream`), mimalloc@0.1.52 (musl builds),
ratatui-textarea@0.9.3 (search box), ansi-to-tui@8.0.1 (ratatui org),
bollard@0.21.1, tokio@1.54 (process, signal), futures@0.3.

**What was built:** about 480 lines, against roughly 2,070 non-blank lines
across sb-go's `logs.go` and `dockerLogs.go`. One viewer serves both source
types:
- A picker lists containers (bollard) and services (`systemctl list-units`
  filtered by the `saltbox_managed_` prefix).
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
- **Mouse reporting mode, a real difference from sb-go:** crossterm's
  `EnableMouseCapture` turns on all-motion tracking (1003), so the terminal
  sends an event for every pointer movement. That traffic competes with
  wheel events, especially over SSH. sb-go's bubbletea uses cell motion
  (clicks, wheel, drags only). The prototype now defaults to
  `--mouse buttons` (1000, 1002, 1006, verified from the emitted escape
  sequences), with `--mouse all` kept for comparison. Without motion events
  the right-click menu loses hover highlighting; clicking still works.
- **Design notes for the module:**
  - Search highlights whole lines. Per-match highlighting means splitting the
    spans ansi-to-tui produces.
  - The search query carries over when you switch source. It should probably
    reset.

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
