# Erasing a screen under GNU screen and tmux without an alternate screen

Verified 2026-11-19 for the key map's "Blank on exit" (tui spec, "Keys:
one map for every screen", decision 6).

**Software:** GNU screen 4.09.00 with its defaults (`-c /dev/null`), and
tmux 3.2a with `alternate-screen off`, both on Ubuntu 22.04, amd64.

**Method:** a private session of each (a private `SCREENDIR`, a private
tmux socket, `-f /dev/null`), 80×24. A shell script in it enters the
alternate screen (ESC [?1049h), prints a marker line, erases it by one
of the methods below, leaves the alternate screen (ESC [?1049l) and
waits. Then the window's visible text and its scrollback are captured
(screen's `hardcopy` and `hardcopy -h`; tmux's `capture-pane -p` and
`capture-pane -p -S -`) and the marker counted in each.

## What they do

Neither honours the alternate screen here, so the script's frame stays
in the window unless it is erased:

| Erasing | Visible after | In the scrollback |
|---|---|---|
| none | yes | yes |
| ESC [2J (erase display: crossterm's `Clear(All)`, ratatui's `Backend::clear` and `Terminal::clear`) | no | **yes** |
| ESC [H ESC [J (home, erase below) | no | **yes** |
| each row: ESC [row;1H ESC [2K (erase line: crossterm's `MoveTo` and `Clear(CurrentLine)`) | no | no |

Both move a display erased with ESC [2J or ESC [J into their scrollback,
so the frame can still be scrolled back to (screen's copy mode, tmux's
`prefix [`). Erasing each row in place leaves nothing in either. ESC [3J
(erase the scrollback) isn't used: it would also erase the user's own
scrollback.

## What sb does with it

Every screen erases its rows one at a time as the last step before it
is left (`crates/tui/src/term.rs`, `erase_rows`, and the forced exit's
prepared sequence). ratatui itself still erases the display with ESC [2J
when the window is resized (ratatui-core 0.1.2 `terminal/resize.rs`), so
a frame shown before a resize can reach these multiplexers' scrollback
(tui spec, Known limits).
