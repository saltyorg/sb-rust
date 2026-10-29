# Prototype: proto-runner

**Question:** Can indicatif and portable-pty reproduce sb-go's task runner:
spinner per step, live child output under a PTY, an output tail on failure,
clean Ctrl-C, and plain output when piped?

**Crates tried:** indicatif@0.18.6, console (indicatif's styling dependency),
portable-pty@0.9.0, signal-hook@0.4.5, rustix@1.1.6 (process, termios).

**What was built:** a sequential runner of three or four steps. One runs a
real `ansible-playbook --version`, one simulates package output with colours
and `\r` progress, one is a 30-second step to interrupt, and one fails. Each
step runs under a PTY sized to the terminal width. Checked with a real
terminal emulator (pyte) replaying a `script` recording, piped to `cat`, and
with SIGINT mid-step in both modes.

## Result

- **Works.** The live frames show `⠋ Installing packages 2s` with the last
  three child lines beneath it. Finished steps show as `✓ Packages installed 4s`.
  A failure prints `✗ <step>: failed (exit N)` followed by the last 10 output
  lines. About 270 lines, against sb-go's 537-line runner, though the
  prototype has no nested subtasks yet.
- **PTY:** colours survive (`ANSIBLE_FORCE_COLOR`, child sees a TTY).
  portable-pty starts the child in its own session, so Ctrl-C reaches only us,
  and `kill_process_group(SIGTERM)` cleanly stops the child and its
  grandchildren. No processes were left behind in either mode.
- **Piped:** indicatif's hidden draw target plus plain `- step` / `✓ step`
  lines work. Child output kept its ANSI codes at first. It is now stripped
  with `console::strip_ansi_codes` (already in the tree through indicatif)
  when the destination isn't a terminal: 0 escape sequences in piped
  output.
- **Library audit:** the `\r`/`\n` line splitting stays our own: no
  dependency in the tree provides it. A terminal emulator crate (`vt100`)
  was not considered at first. It is now an open item (see Gaps).
  Process-group kill (rustix), signals (signal-hook) and the PTY
  (portable-pty) are library calls.
- **Pitfalls found in indicatif (each fixed in the prototype):**
  1. Leaving finished bars inside the `MultiProgress` lost them: later
     redraws overwrote their lines. This is documented indicatif behaviour
     (MultiProgress docs: a dropped bar is wiped on the next `println` and
     not redrawn), not a bug. The pattern is `finish_and_clear()` plus
     `mp.println()` of the result line, so indicatif only manages live bars.
  2. UNVERIFIED (no measurement kept): calling `mp.remove()` on cleared bars
     seemed to make the line accounting worse. Just `finish_and_clear()`
     them.
  3. `\r` progress output needs "overwrite the last line" handling, and
     `\r\n` split across a read boundary needs care. This is our code, but
     small.
- **No async needed.** A reader thread plus `recv_timeout` is enough. The
  runner doesn't need tokio, though it can live inside a tokio app via
  `spawn_blocking`.
- **Gaps for the real module:**
  - Nested subtasks: sb-go renders a tree. indicatif's `insert_after` should
    cover this, but it wasn't tried.
  - Forwarding terminal resize (SIGWINCH) to `MasterPty::resize`: not tried.
  - Evaluate a terminal-emulator crate for the output tail: `vt100`, or
    termwiz's escape parser (already in the lockfile as an optional ratatui
    dependency; it keeps state across reads, but is heavy). Either would
    handle cursor and erase sequences (`ESC[K`, cursor up) that the line
    splitter passes through, but changes how the tail is built. vt100's
    docs don't say whether escapes or UTF-8 split across `process` calls are
    handled.

## Truecolor and library review

- **Palette:**
  - The runner's own lines use the shared palette: the running step in the
    accent, ✓ done green, ✗ failed coral, interrupted amber, elapsed and
    the output tail muted.
  - ratatui's `Color` prints as `#RRGGBB`, which console's
    `Style::from_dotted_str` (and so indicatif's templates) reads as 24-bit
    colour, so no colour code of ours is involved.
  - console writes RGB as 24-bit codes whatever the terminal reports.
  - Tests check for 24-bit codes only; they failed on the old named colours.
  - In a PTY every ✓, ✗ and spinner frame used the palette RGB.
  - Child output keeps its own colours.
- **Library review** (the `library-reviewer` agent, whole prototype). Each
  fix was test-first where a unit test could reach it:
  - **Status lines could vanish:** with stderr on a terminal but `TERM` unset
    or `dumb`, indicatif hid its draw target while our own TTY check said
    "draw", and `MultiProgress::println` does nothing on a hidden target.
    The draw target is now always indicatif's stderr target, and our "live"
    flag is `!mp.is_hidden()`. Checked end to end: 2 ✓ lines with `TERM`
    unset.
  - **Colour followed the wrong stream:** styles were stdout-gated while
    the lines go to stderr. They are now `for_stderr()`, as indicatif does
    for its bars. Stripping follows `console::colors_enabled_stderr()`
    (honouring `CLICOLOR`/`CLICOLOR_FORCE`), and stdout output follows
    `colors_enabled()`.
  - **Elapsed** on the finished line now uses indicatif's `HumanDuration`,
    the same format as the live spinner.
  - **A signalled child** is reported as "killed by signal (…)" from
    portable-pty's `ExitStatus::signal()`, not "exit 1".
  - **Characters split across reads** came out as U+FFFD. The splitter now
    splits bytes before decoding.
  - **Stop signals:** signal-hook's shutdown signals (`TERM_SIGNALS`:
    SIGTERM, SIGQUIT, SIGINT) now all stop the run, so being killed also
    stops the child's process group (checked: none left running). One flag
    records which signal arrived (signal-hook's documented `register_usize`
    pattern, found by a second review), and the exit code is 128 + that
    signal: 130 for Ctrl-C, 143 for SIGTERM. The exit is explicit, which
    signal-hook recommends over re-raising the signal in a multi-threaded
    program.
  - **Escalation** after 3 s now sends KILL to the whole process group
    (rustix), not portable-pty's `Child::kill`, which signals only the
    child's pid.
  - **Terminal size** comes from `console::Term::size_checked` (rows too).
    rustix's termios feature is dropped.
  - **The test's named-colour check** now walks the parameters properly, so
    a named colour mixed into a 24-bit sequence is caught, using console's
    `AnsiCodeIterator`.

## Rounded tree (maintainer review)

From the `--gallery` comparison the maintainer picked the tree, with a
rounded last branch and the times in one column. The examples are no longer
Ansible output, since the runner will not inline Ansible's own output.
- **The live runner is now that tree:**
  - a root line, then a branch per finished step (`├──`, the last `╰──`);
  - the running step as the newest branch, with its live tail under it
    (`├──`, the newest line `╰──`);
  - a failed step keeps its last lines under it.

  Times and exit codes end in one column, both live and once finished.
- **The run is simulated:** checking the system, updating package lists,
  installing packages (apt-style `Get:` lines and an in-place
  `Progress: [ 40%]` line updated with `\r`) and enabling services. The
  `--fail` and `--slow` steps are apt-style too. The Ansible step and the
  `--ansible` option are gone.
- **Libraries:**
  - Glyphs come from termtree's `GlyphPalette`, with ratatui's rounded
    corner for the last branch. termtree only draws a finished tree in one
    go, so the live runner borrows its glyph set and draws branches line by
    line through indicatif (`set_prefix`).
  - The running line's name and elapsed columns use indicatif's own
    template width and alignment (`{msg:<N}`, `{elapsed:>N}`), which pad by
    display width.
  - Finished lines pad with `console::pad_str` to the same column.
  - Tests check the branch glyphs and that every finished line ends in the
    same column.
- **Command output is no longer greyed out** (review): the live tail shows
  a command's own colours and leaves uncoloured text in the terminal's
  default colour; only the tree glyphs are faint. The simulated "Enabling
  services" step prints a `systemctl status`-style green line to show this.
- **`--color=auto|always|never`** (review: the Saltbox installer script
  pipes sb's output and wants colour). Plain when piped stays the default.
  The value type is clap's `ColorChoice`, applied to console's colour
  switches, which every style and indicatif's bars follow. Measured piped:
  0 lines with escape codes for `auto` and `never`, 5 for `always` (root
  plus four steps).
- **Live tail length** (review: three lines was too few): the tail now
  shows sb-go's 8 lines (`liveTaskOutputLines`), capped so the live area
  stays within half the terminal's height, since indicatif skips bars that
  don't fit. Checked: 7 lines once 7 existed at 24 rows, 5 at 10 rows.
- **Blinking lines** (review): diagnosed by measuring what reaches the
  terminal.
  - indicatif's default redraw erases every live line and writes them again,
    up to 20 times a second. Measured over 3 s of the install step: 349
    line erases. The terminal can show the gap, so the lines blink.
    indicatif 0.18.6 has no synchronized output (sb-go wraps frames in mode
    2026 for the same reason).
  - indicatif's documented alternative is `MultiProgress::set_move_cursor`:
    overwrite in place, no erase. It "can reduce flickering, but do not
    enable it if you intend to change the number of progress bars".
    - Tried as is, with bars added and removed per step: stale lines in tmux
      in all three endings.
    - So the spinner and tail bars are now created once and reused by every
      step (a constant bar count). The cursor-moving redraw is switched off
      only for the final clear.
  - After: 0 line erases in the same measurement. Clean endings in tmux
    for success, failure and Ctrl-C.
  - Side effect fixed: the root line, printed through indicatif before any
    bar existed, was drawn over. The cursor-moving redraw counts only bar
    lines, so with none drawn it moves up zero lines and writes `\r` at the
    start of the printed line (indicatif 0.18.6 `draw_target.rs`,
    `draw_to_term`). It now goes straight to stderr before any bar exists.
  - Between steps the bars are blanked, not removed, so the old spinner and
    output don't show for a frame under the finished step's line.
  - Known limit: indicatif skips bars that don't fit the terminal height. If
    the terminal is made shorter mid-run, fewer lines get drawn while the
    cursor-moving redraw is on, which can leave stale lines. The tail length
    is worked out once, at the start.
- **A failed step now exits 1 without repeating itself.** anyhow had printed
  "Error: step failed" under the tree; the tree already says it.
- **Ctrl-C left a stale spinner line**, in tmux as in any real terminal:
  - The terminal echoes `^C` among indicatif's lines and its line count
    goes wrong.
  - The runner now turns off `ECHOCTL` while drawing (rustix termios) and
    restores it on exit, including on interrupt.
  - Measured in tmux: one stale line without it, none with it. `NOFLSH`
    made no difference and was left out. `echoctl` is back on after the run.
  - The PTY harness's emulator (pyte) doesn't show the echo, so this was
    checked in tmux. A first diagnosis that blamed line overflow turned out
    wrong: indicatif's padding parks the cursor at the right edge on
    purpose.
  - No library or doc covers this, so it is marked UNVERIFIED in the code.

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt with changes.** indicatif and portable-pty replace sb-go's
bubbletea-based runner, provided the "finished steps become printed lines"
pattern is followed.

## Map impact

- Stack table: add `console` (already in the tree through indicatif) for
  styled one-off lines and ANSI stripping. Add `signal-hook` for Ctrl-C.
- `term-ui` module: build on the printed-lines pattern, strip child ANSI when
  piped, and use exit code 130 on interrupt. Prototype nested tasks and resize
  forwarding when the module is specified.
- `exec` module: PTY spawn with a process-group kill. The sync reader thread
  is sufficient.
