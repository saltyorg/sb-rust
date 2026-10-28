# Prototype: proto-logs

**Question:** Can bollard log streams and `journalctl -f` output feed a ratatui
viewer with follow, scroll, and search, without blocking input?

**Crates tried:** ratatui@0.30.2 (`unstable-rendered-line-info`), crossterm@0.29 (`event-stream`), serde_json (`preserve_order`),
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
- **Gap shown only by real logs, now addressed:** application lines can run
  to thousands of characters (JSON, stack traces). The first version cut them
  off at the window edge. The viewer now has:
  - **Wrap on by default**, toggled with `w` or the right-click menu. A
    following view sits against the bottom edge, and a paused view stays full
    after wrap changes.
  - **Open a line:** ↑/↓ or the wheel highlight a line (pausing follow);
    Enter or a click opens it in a scrollable popup. JSON in the line is
    pretty-printed with its key order kept (serde_json `preserve_order`).
  - A **right-click menu** on a line (Open line, Wrap, Follow), also on the
    keyboard via `m` and each item's key.
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
