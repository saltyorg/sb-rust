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
- **Cursor hidden while drawing** (review: a cursor block sat at the far
  right of the live lines). indicatif pads each line to the full width,
  which leaves the cursor parked at the right edge after every redraw.
  - The live area hides it with console's `Term::hide_cursor` (only on a
    terminal) and shows it again in `finish`, which every exit runs
    (success, failure, Ctrl-C), plus a `Drop` for panics.
  - Checked: hidden then shown on all three exits in a PTY; tmux reports
    the cursor invisible mid-run and visible after; no cursor codes when
    piped.
- **General run log** (capability map, decision 11). Review corrected a
  first version that kept a per-step file only on failure: logging is
  general for the CLI, and nothing is removed after a successful run.
  - Every run logs, through `tracing` and `tracing-subscriber`'s readable
    key=value format, to a `tracing-appender` file that starts daily at
    UTC midnight. With `max_log_files(14)` it prunes to one below that at
    every start, so 13 to 14 days remain:
    - a run record with its id, version and arguments;
    - each step's start, its output lines (plain text, the final state of
      in-place progress) and its end with outcome and elapsed.

    A `run` span (with the id) and a `step` span (with the step's name)
    tag every line, e.g. `run{run=…}:step{step="…"}: Get: …`; the
    library-reviewer pointed out that fields passed by hand on each event
    duplicated what spans do.
  - Not used: `latest_symlink`. It would let the pointer name a file, but
    the link moves at the next rotation, so a printed pointer would later
    lead elsewhere. Internal write errors (a full disk) are not printed, so
    they can't land on the live area.
  - A failed or interrupted step's last branch points at the log:
    `╰── full output: <dir> (run <id>)`.
  - The appender is the writer directly, so writes are synchronous and the
    early exits lose nothing. A log that can't be opened is a warning, not
    a failed run.
  - The prototype writes to `SB_LOG_DIR` or the system temp directory; sb
    will use /var/log/sb.
  - Checked: a successful and a failing run in one daily file, both kept,
    with every step's result and output.
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

_Reviewed by the maintainer (2026-10-29): done for now. The review chose
the rounded tree with aligned times from a gallery of formats, a live tail
of the step's output with no Ansible output inline, the command's own
colours, no blinking and a hidden cursor, `--color` for piped output, and a
rotated run log on the host for every run (decision 11). Still open: the
install script (tasks/prototypes/todo.md, Later) and the vt100/termwiz evaluation._

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

## term-ui spike: nested and concurrent async steps (term-ui Task 1)

**Question:** can the runner draw nested and concurrent async steps (tokio)
without blinking, keeping the newest finished line live until its place in
the tree is known (term-ui spec, decisions 2-4)?

**Built:** `--spike seq|nested|concurrent|tour [--fail]` (src/spike.rs):
async steps on a tokio runtime, a tree model in start order, frames drawn
by a tokio interval (80 ms, skipping missed ticks), notices under steps,
two ways to draw the live area (`--strategy single|pool`), sb's exit codes
(1 after a failure, 130 after Ctrl-C). Design options for review through
`SPIKE_STYLE` (`notice=label|bullet|branch|guide|child|inline`, bullet by
default since Checkpoint A; `tail=muted|faint|last`,
`spin=braille|dots|arc`, `conc=full|compact`); `SPIKE_CLEAR=off|keep|blank`
picks how the live area is cleared. The `ECHOCTL` guard is on only when
stderr is a terminal. Measured in `script` recordings at
100x40 (sized with `stty`), final screens replayed in pyte 0.8.2; erases
counted as `ESC[2K`; stale lines counted as spinner lines left on the final
screen.

**Measurements** (seq, nested, concurrent and tour shapes; endings: success,
`--fail`, `kill -INT`, a Ctrl-C keypress typed into the pty):

| Strategy | Clear | Erases per run | Stale lines | Exit codes |
|---|---|---|---|---|
| single | off | 4-9, all in the final clear | 0 (see the keypress note) | 0, 1, 130 |
| pool (14 bars) | off | 105: each `finish_and_clear` redraws the rest | 0 (see the keypress note) | 0, 1, 130 |
| pool (14 bars) | keep | 1 | 0 (see the keypress note) | 0, 1, 130 |

**Result:**
- **No blinking with either strategy:** the cursor-moving redraw
  (`MultiProgress::set_move_cursor`) erases nothing while running. Against
  indicatif's default redraw (349 erases in 3 s, measured earlier), every
  erase left is in the final clear.
- **A shrinking live area leaves old text behind** (2-6 stale lines per
  run at first). indicatif pads each line to the terminal's width in its
  cursor-moving redraw (draw_target.rs `draw_to_term`) but drops empty
  lines (style.rs), so a blank line shrinks the area, and that redraw never
  clears lines that fall off the bottom (`set_move_cursor`'s docs: the
  number of lines must not change). Fix: never shrink the area while
  running, a blank line being one space. After: 0 stale lines (the
  library-reviewer found the cause).
- **Single bar vs pool:** both are flicker-free and both clear cleanly (the
  pool in one erase when its bars are cleared with the cursor-moving redraw
  kept on). The single bar needs no count of bars sized in advance; a pool
  cuts off a live area taller than its bars. Recommended: single.
- **A Ctrl-C keypress showed one stale line in the replays: an artifact of
  pyte, not of the runner.** With the `ECHOCTL` guard on, the terminal
  still echoes the keypress as one raw `0x03` byte. indicatif leaves the
  cursor at the last column of a full-width line, and pyte 0.8.2 draws the
  unknown control byte, wrapping to the next row; real terminals ignore it.
  Removing that byte from the recordings gives 0 stale lines in every
  keypress run (found by the code reviewer). `NOFLSH` made no difference
  and was dropped. To confirm in a real terminal in Task 8, as
  proto-runner's tmux check did for its guard.
- **The tree is right:** glyphs, rounded last branches, times in one
  column, two levels of nesting, a failing step's output tail, a failing
  sibling stopping the other (errgroup, through a child cancellation token,
  marked "stopped"; a longer reason pushed the time out of its column),
  parents of a failure marked "sub-step failed", Ctrl-C marking running steps and
  their parents "interrupted" with their last lines kept.
- **Placement rule that works:** a finished step is printed for good when
  its subtree is settled and its place is known (a later sibling, its
  parent finished, or the run's end); until then it stays live. A step with
  sub-steps keeps its whole subtree live until it ends.
- **Frames from tokio, not indicatif's ticker:** indicatif's `{spinner}`
  is one per bar, and a single live area holds several spinners, so the
  frames (and spinner glyphs) come from a tokio interval task. indicatif's
  steady tick was not tested.

**What the runner's API must allow:** async steps that start sub-steps
(nested) and spawn concurrent siblings under one parent, each with
running/done labels, output lines and notices (a notice must arrive before
its step's outcome, or the runner holds the line until it does); a step's
outcome (done, failed with a reason, stopped); the run's end; a shared
cancellation token that tells "interrupted" (Ctrl-C) from "stopped because
a sibling failed".

**Questions for the maintainer (Checkpoint A):**
- One bar holding the whole live area instead of decision 2's fixed pool
  of bars (same no-blink result, nothing to size)?
- Frames driven by a tokio task instead of decision 4's "indicatif's own
  ticker" (needed for several spinners in one area)?
- When one concurrent sibling fails, stop the others (sb-go's errgroup,
  as the spike does) or let them finish?
- Ctrl-C with two steps running: both, and their parent, marked
  interrupted with their last lines (as the spike does)?
- On a failure, which steps show their output tail and where the log
  pointer goes: the spike shows the failed step's tail only, marks its
  parents "sub-step failed" and a stopped sibling "stopped", both in the
  warn colour, and has no pointer line yet.
- A step with sub-steps keeps its whole subtree live until it ends; for a
  long subtree, is that acceptable, or should finished sub-steps print as
  they complete (the parent's line printed early, without its result)?
- The design options (notices, output, spinner, compact parallel steps):
  see the runner lookbook recordings.

**Answered (maintainer, Checkpoint A, 2026-11-01; term-ui spec decision 7):**
one bar for the live area, frames from a tokio task, a failing sibling
stops the others ("stopped", parents "sub-step failed"), a subtree stays
live until its parent ends, Ctrl-C marks running steps and their parents
"interrupted" keeping their last lines, notices as bullets, the braille
spinner, concurrent steps in full. Output: up to 8 muted lines per running
step (the reviewed length; the spike had fallen back to 3), all running
tails together within half the terminal's height.
