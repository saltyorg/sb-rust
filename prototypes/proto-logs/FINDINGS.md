# Prototype: proto-logs

**Question:** Can bollard log streams and `journalctl -f` output feed a ratatui
viewer with follow, scroll, and search, without blocking input?

**Crates tried:** ratatui@0.30.2, crossterm@0.29 (`event-stream`),
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

**How it was checked:** the PTY harness drove the viewer against a throwaway
container and a transient systemd unit. Both emitted synthetic lines
(coloured, with a search word every seventh line) and both were removed
afterwards.

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
- **Design notes for the module:**
  - Search highlights whole lines. Per-match highlighting means splitting the
    spans ansi-to-tui produces.
  - The search query carries over when you switch source. It should probably
    reset.
  - Long lines are truncated, not wrapped. Decide between wrap and
    horizontal scroll.
  - Mouse support (wheel scroll, click in the picker) wasn't added. The
    proto-facts pattern applies directly.

## Verdict

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
