# ADR-023: Binary size with ratatui and crossterm

## Status

Proposed (module build M61, 2026-11-13); the maintainer accepts it with
the log viewer's first look (M67 and M68)

## Date

2026-11-13

## Context

The tui spec (decision 21, "Gate bookkeeping") asks for a size ADR when
ratatui and crossterm enter the binary, as ADR-004, ADR-007 and ADR-009
did for their stacks. M61 adds the `sb-tui` crate: terminal setup and
restore through `ratatui::try_init`/`try_restore`, mouse capture, the
restore hooks and the async event loop on crossterm's `EventStream`. No
command uses it yet (`sb logs` comes with M68), so the shipped binary is
unchanged by M61 itself. The growth was measured on a throwaway build in
which sb ran a probe screen through `sb-tui` (a bordered `Paragraph` of
`ansi-to-tui` text and a `List`, on `Tui::enter`, `run` and `leave`),
built the way `scripts/check.sh full` builds (cross, the production
profile, the x86_64 glibc target, ADR-020), then reverted:

| Build | Bytes | Change |
|---|---|---|
| sb at M61's base (a6b5ebc) | 11907672 | — |
| sb with the probe screen | 12193344 | +285672 (+2.40%) |

The growth is ratatui's core (buffer, layout with its cache, text and
the widgets the probe draws), crossterm (terminal modes, the event
reader with its `mio` poller and `signal-hook` 0.3, which crossterm
0.29.0 pins below the workspace's 0.4.5: a second copy, which cargo-deny
reports as a duplicate, not a failure), `ansi-to-tui`'s parser and
sb-tui's own code. tokio, futures-util and the I/O driver were already in
the binary for bollard (ADR-007). The full log viewer adds its own code,
`ratatui-textarea` (M70) and more widgets on top; the tui spec measures
again where the ratchet is re-measured ("When the binary is complete").

`sb --help` and `sb version` don't build the runtime or touch the
terminal, so their startup doesn't change.

The groundwork's other measurement, input latency on a PTY (tui spec,
Testing Strategy), from `crates/tui/tests/term.rs`'s
`input_reaches_the_screen_within_a_few_frames`, release build, three
runs: a key to the screen's change takes a median of 9.2 to 11.2 ms more
than the harness's own baseline (a raw-mode read loop that writes at
once, 0.06 to 0.09 ms), with the keys' timing spread over a whole render
interval. That is within one 60 fps frame (16.7 ms), as the recipe's
render interval allows, and below proto-logs' 14 ms on the multi-thread
runtime. The test's bound (100 ms added) catches the class of bug
proto-logs had: input seen only on a tick.

The frame time (tui spec, Testing Strategy, "Frame cost"; module build
M73, 2026-11-15) was measured by hand on the release build with
`crates/tui/src/viewer/tests.rs`'s `a_frames_cost_doesnt_grow_with_the_buffer`
(`cargo test --release -p sb-tui --lib frames_cost -- --nocapture`):
a buffer of 20,000 records of 5,000+ characters at 240×70, eleven keys,
each key's handling and its frame timed, three runs under load from
other work (a quiet host may be faster). Before M73 every frame measured
its window again, at about 0.3 ms a record, and the median frame took
9.1 to 9.6 ms. With each line's height kept for its layout, and the
paging check counting entries before it measures, the median is 2.8 to
2.9 ms. With wrap on (the default), every frame takes 1.9 to 3.7 ms, `t`
included. With wrap off, a frame takes 5.6 ms and the frame that turns
wrap off 9.9 ms: each builds the styled text of the 70 records in view.
Both are within one 60 fps frame (16.7 ms); they are above the spec's
5 ms budget, which the tui spec's verification notes leave open for the
maintainer.

## Decision

Accept ratatui `=0.30.2` (default features off: `crossterm_0_29`,
`layout-cache`, `underline-color`, `unstable-rendered-line-info`),
ratatui-core `=0.1.2` with `anstyle`, crossterm 0.29.0 with
`event-stream` and ansi-to-tui 8.0.1 in the binary, at about 280 KiB for
the groundwork. The production binary stays far below CONSTRAINTS.md's
ceiling of 45711520 bytes. The ratchet row in CONSTRAINTS.md doesn't move
now: its limit is the binary measured when it is complete (tui spec
decision 21; capability map, "When the binary is complete").

## Alternatives considered

- **ratatui's default features:** `all-widgets` brings the calendar
  widget and the `time` crate, which no screen uses; `macros` isn't used.
- **Termion or termwiz as the backend:** crossterm is ratatui's default
  and documented backend, with the async `EventStream` the recipe uses;
  termwiz is larger, and termion has no async reader.
- **Hand-written escape codes and a raw `read` loop:** proto-logs' series
  of input bugs came from exactly that (its FINDINGS.md, "Root cause of
  the whole series of input bugs"); the library-first rule.

## Consequences

- When the maintainer accepts this ADR, nothing in CONSTRAINTS.md moves;
  the ratchet row waits for the complete binary.
- crossterm's `signal-hook` 0.3 stays beside the workspace's 0.4.5 until
  a crossterm release moves to 0.4.
- Later screens measure their own additions on the production build
  where their specs ask for it.
