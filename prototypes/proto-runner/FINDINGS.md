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
  lines work. Our own styled text is plain when piped, but **child output
  keeps its ANSI codes**, so the real module must strip them when the
  destination isn't a terminal (`console::strip_ansi_codes` or anstream).
  Library-owned either way.
- **Pitfalls found in indicatif (each fixed in the prototype):**
  1. Leaving finished bars inside the `MultiProgress` lost them: later
     redraws overwrote their lines. The robust pattern is
     `finish_and_clear()` plus `mp.println()` of the result line, so
     indicatif only manages live bars.
  2. Calling `mp.remove()` on cleared bars made the line accounting worse.
     Just `finish_and_clear()` them.
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
  - Exit code 130 on interrupt: the prototype returns 1.

## Verdict

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
