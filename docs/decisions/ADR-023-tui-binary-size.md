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
included. With wrap off, a frame took 5.6 ms and the frame that turns
wrap off 9.9 ms: each built the styled text of the 70 records in view,
and the switch built each twice (to measure, then to draw).

The maintainer asked for wrap off to meet 5 ms too (2026-11-15). With
wrap off a line's height is now its newlines, counted from the bytes
(`line::screen_lines`); its text is built only as far as the window
reaches (`line::within`); and each frame keeps the texts it drew for
the next (`Drawn`, bounded to the entries in view and 4 MiB). The same
timing, now with three more wrap-off frames (a row up, a step right
and back), three runs: the frame that turns wrap off takes 3.1 ms
(9.8 before, timed the same way), the other wrap-off frames 1.3 to
1.8 ms (5.3 to 5.8 before), and the wrap-on frames as before, 1.6 to
3.5 ms; the slowest frame of a run is 3.5 ms, 4.6 ms in a run under
heavier load. Every frame of the timing is within the 5 ms budget.

Far to the right with wrap off, a frame still costs more: ratatui's
`Paragraph::scroll` x offset walks every character to the left of the
column on each draw, about 0.2 ms a line at column 4,000. Measured by
hand on the same buffer (2,000 such records, wrap off, the column set
just short of 1,000, 2,000 and 4,000, then a step right and a row up
timed, release build), a step sideways or a row up at column 1,000
took 9.3 ms before and 4.9 ms now, at 4,000 18.7 ms before and 14.2 ms
now: within one 60 fps frame up to about column 2,000, and above it
past that.

The maintainer asked for those frames to be fast too (2026-11-15),
and the far-right cost is closed: with wrap off, each screen line in
view is now handed to ratatui cut at the column (`columns.rs`, styles
that began left of it kept), from marks kept with the text: each
span's start, and outside printable ASCII a grapheme every 64 columns
(or 64 graphemes of no width). A cut walks at most that far, wherever
the column is. A text
that is the whole message is also kept for every column, so a step
past a screen to the right no longer builds it again. The frame-cost
test now times wrap off at columns 0, 1,000 and 4,000 (2,000 records,
240x70, release, three runs): a step right, a row up and a step left
take 1.4 to 2.0 ms at each (4.7 to 4.8 ms at 1,000 and 13.6 to
14.1 ms at 4,000 before), and it checks that no screen line handed to
ratatui there is wider than the window and 64 columns. By hand, on
records of CJK text and of words each in its own colour, the same
steps take 1.4 to 1.9 ms at 4,000 (12.3 and 17.4 ms before).

A frame that builds every text in view far to the right (a jump there,
or a search, theme or `t` change while there; a resize there makes the
marks again too) still costs more, and
more the further right: the message to the left of the column has to
be parsed for its styles (`DisplayText` and ansi-to-tui, which have no
way to resume a parse). On the frame-cost test's records it takes 4.4
to 5.6 ms at 1,000 and 5.7 to 5.8 ms at 4,000 (7.4 and 17.8 ms
before); on CJK records 8.8 to 9.0 and 19.2 to 19.4 ms (9.2 and
22.7 before), on colour-per-word records 7.3 and 18.3 to 19.2 ms
(14.8 and 33.8 before). Such frames come once per jump or change, not
per step.

The maintainer asked for those frames to meet 5 ms too (2026-11-15,
the follow-up "The viewer's far-right rebuild frame meets 5 ms"), and
the rebuild is closed: the colour parse resumes. `DisplayText` (term-ui)
can report places where a parse of the same bytes can start again
(`DisplayText::resumable`: before a printed character, so its parser is
in its ground state, about every 64 columns) and parse from one
(`from_bytes_at`, from its column for the tab stops). The viewer keeps,
for each line it built far right, those places with the message's byte,
the screen column and the style ansi-to-tui had in effect there
(`resume.rs`): ansi-to-tui's state between spans is one `Style` that
each SGR sets, adds to, takes from or resets, so a parse begun with no
style, each span's style patched over the kept one, styles as the whole
parse does. A text far right is then built from the last place an
eighth of a screen left of the window through a quarter of a screen
right of it, and handed to ratatui with the rest of the way as
`Paragraph::scroll`'s offset, so no marks are made for it. The places
are kept with the line's text in the frame cache, counted in its 4 MiB,
and across a change of theme, `t` or the search, which drop the texts.
A message of several screen lines, one a carriage return takes back,
and a search for a query that can overlap itself (`aa`) are built from
their start as before, and so is a search on a line holding a
character that joins the one before it into a grapheme wider than the
two apart (a variation selector, a zero-width joiner, a keycap's mark, a
skin tone): a match that ends between them makes ratatui measure the two
apart. Release timing on the frame-cost test (now `far_right_holds`,
three runs under heavy load, 2,000 records at 240x70; before is the
same frames on the previous build): a `t` change at column 4,000 takes
2.5 to 2.7 ms on plain records, colour off or on (6.0 to 8.6 before),
3.8 to 4.0 ms on CJK records (19.8 to 22.9) and 3.9 to 4.1 ms on
colour-per-word records (18.7 to 21.6); at 1,000 2.3 to 3.0, 3.8 to 4.0
and 3.5 to 3.7 ms. The frame after a search there takes 3.2 to 4.1 ms
(13.1 to 28.6 before). A resize narrower keeps the texts and makes no
marks now (before, 4 columns narrower made the marks again: 3.4 to
10.3 ms); one from 160 columns to 240, which builds every text again
from its places, takes 3.8 to 4.9 ms. Steps sideways and a row up stay
at 1.5 to 2.4 ms. In the noisiest run, under heavier load, one `t`
frame took 5.6 ms and a resize 5.2 ms; every other far-right frame of
the three runs is within 5 ms.
`resume/tests.rs`'s `a_resumed_line_draws_as_the_whole_text` checks
that a line built from a place draws as the whole line scrolled, cell
by cell, every SGR form and reset included.

What still grows with the column is a line seen for the first time far
right, which has no places yet: a page up or a jump there from far
away parses each new line from its start once (on these records 5.5 to
9.2 ms for a whole screen of them at 1,000 and 4,000, 19.5 to 23.5 ms on
CJK and colour-per-word records at 4,000; 12.6 to 26.3 ms before, with
a search on).

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
