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
