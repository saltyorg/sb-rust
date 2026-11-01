# Spec: `term-ui` (approved 2026-10-30)

Module of the approved capability map (`term-ui`: inline UI only; depends
on `cli-core`). The first vertical slice is `sb version` (done) →
`sb docker ps` → `sb validate-config`, built as `cli-core` → `term-ui` →
`docker` + `config`. The map's build order puts `exec` before `term-ui`;
the slice order is chosen on purpose: term-ui itself never starts
processes. (`config` will: sb-go's `validate-config` runs `rclone
listremotes` for its rclone-remote check, so that check needs a captured
`exec` run or waits for `exec`; the `config` spec decides.)

## Assumptions

1. **Rendering only.** term-ui draws; it never starts processes. Steps are
   the caller's code; output lines are handed to the runner. `exec`, later,
   runs children under a PTY and feeds their lines in.
2. **The first slice needs the runner and tables, not prompts.**
   `validate-config` runs through sb-go's `terminal.Runner`, with notices
   (schema warnings) and concurrent sibling checks (Cloudflare and Docker
   Hub credentials, errgroup), and one check runs a child process (rclone,
   above). `docker ps` is a table. sb-go's
   `docker start/stop/restart` (not in the first slice) check the
   controller unit with `systemctl` and call the Saltbox Docker
   controller's HTTP API; the capability map's `docker` row says "via
   bollard" for them, which is wrong (decision 6).
3. **The reviewed prototypes are the design.** The runner is proto-runner's
   rounded tree; tables are proto-inline's sb-go `docker ps` layout on
   tabled (both reviewed 2026-10-29). Prompts follow proto-inline's
   dialoguer evidence (restore prompts deferred).
4. **cli-core owns the colour decision, the theme, `Shutdown`, exit codes
   and the run log.** term-ui applies them; it decides nothing of its own.

## Objective

One inline output layer that every non-full-screen command draws through,
so `sb docker ps`, `sb validate-config` and later every runner command look
the same, follow `--color`, `NO_COLOR` and `SB_COLOR_PROFILE`, take their
colours from the theme, and fall back to plain lines when not on a
terminal.

Users: Saltbox users at a terminal. The install script runs `sb setup`
attached to the terminal (capability map, decision 10), so plain output
appears only under CI, redirection or `-v`.

## Behaviour

### Colour on every inline path

- At startup, after cli-core settles the colour choice and theme, `sb`'s
  `main` calls one term-ui function (cli-core can't call term-ui: the
  dependency points the other way). It reads anstream's resolved choice
  for stdout and for stderr (`AutoStream::choice`, as proto-inline did) and
  sets both of console's switches from them every time, so console's own
  `auto` rules can't disagree; indicatif and dialoguer follow console.
  Under `ascii` both switches are off (proposed in Task 2, below).
- **Adapters** from cli-core's resolved theme: a role's colour (resolved
  for the profile by anstyle-lossy) becomes styled text and a tabled
  colour, both rendered by anstyle. No colour maths in term-ui. Under
  `ascii` or with colour off the adapters give no styling.
- **Proposed in Task 2, awaiting the maintainer** (with the anstyle
  rendering above, console's switches off under `ascii`, and the stack
  table's console row and `src/style.rs` line): not console's `Style`,
  because console writes a bright 16-colour as `38;5;8`-`15`, a
  256-colour code, which criterion 2 forbids under `ansi`; indicatif and
  console draw styled text as is, measuring it with escape codes
  stripped. dialoguer's themes take console styles; prompts
  settle that with their first user (decision 1). The runner colours
  its lines through the adapters, never through indicatif template
  styles (`{spinner:.cyan}`), which console renders.
- The runner and prompts write to stderr; tables and other results write
  to stdout. Each follows its own stream's choice.

### Task runner

- **Look (proto-runner, reviewed):** a root line, then one branch per step
  (`├──`, the last `╰──`), times and exit codes ending in one column. The
  running step is a branch with its running label, spinner and label in
  the `Accent` role, and a live tail of its newest output lines under it:
  each running step's tail at most sb-go's 8 lines, all running tails
  together within half the terminal height (decision 7). Glyphs `Faint`;
  ✓ `Ok` with the done label, ✗ `Problem` with the failure label,
  interrupted `Warn`; elapsed `Muted`; output lines in `Muted`, a line's
  own colours kept where it has them.
- **Labels:** each step has a running label and a done label
  ("Checking accounts.yml" → "accounts.yml checked"), and optionally a
  failure label, as proto-runner and sb-go's Running/Success/Failure.
- **Notices:** a step can add info and warning lines, which stay under it
  in the finished tree (sb-go's `Task.Info` and `Task.Warning`), drawn as
  bullets: `›` for info in the muted colour, an orange `!` for warnings
  (decision 7).
- **Nesting and concurrency:** a step can run sub-steps (a deeper branch),
  and sibling steps can run at the same time. How this fits the no-blink
  technique and the last-branch glyph: decisions 2 and 3; what a failing
  sibling and Ctrl-C do to the others: decision 7.
- **Failure:** the failed step prints ✗, the failure label, the error
  (every line of a multi-line error, indented under it) and its last 10
  output lines, then the log pointer (`╰── full output: <file> (run <id>)`,
  from cli-core's `RunLog::pointer`, when the run is logged). The run then
  ends with a quiet `Exit(1)`, so cli-core doesn't repeat the error
  (cli-core spec: errors already shown exit quietly). The step's own log
  record carries the error and its causes, since the run record then only
  says `exit 1`.
- **No blinking:** one live bar holds the whole live area and is redrawn
  in place (indicatif's `set_move_cursor`, as proto-runner and the spike
  measured: 0 line erases while running), per amended decision 2.
- **Verbose (`-v`) or stderr not a terminal** (`TERM=dumb`, CI,
  redirection): no live parts. The runner draws on stderr, so stderr
  decides (indicatif's target; sb-go checks stdout, so `sb validate-config
  > file` stays live here and goes plain in sb-go). Each step prints a
  start line (`- <running label>`), its output lines (escape codes
  stripped when colour is off), notices, and an end line (`✓ <done label>
  2s`, or ✗ with the error and the pointer), flat as in sb-go's plain mode
  (`Verbose || !IsInteractive()`). Lines from concurrent steps are prefixed
  with their step's running label, so interleaved lines stay readable.
- **Cancellation:** steps see cli-core's `Shutdown` (decision 4).
  When a signal interrupted the run, every running step and its parents
  are marked interrupted in the tree (`Warn`), each keeping its last
  output lines, with no pointer of its own (decision 7); the cursor is
  shown again. cli-core's existing block then prints "✗ interrupted" and
  the `details:` pointer once, unchanged.
  A command's own `Shutdown::request` is not an interruption.
- **Terminal restore:** proto-runner switched off `ECHOCTL` while live so
  Ctrl-C doesn't echo `^C` into the tree, and restored it (rustix
  `termios`, a feature the workspace doesn't enable yet; UNVERIFIED in the
  prototype). It changed stdin, which is a pipe under `curl … | sudo bash`;
  term-ui changes the terminal sb draws on (stderr's, or `/dev/tty`)
  instead. The tty is only touched in the foreground process group
  (sb-go's `tty.go` guard), so a background sb doesn't stop on SIGTTOU.
  A second Ctrl-C restores the terminal through a hook cli-core runs
  before its immediate exit (decision 5).
- **Resize:** known limit from proto-runner: the live area's line count
  never shrinks while running (decision 2), so shrinking the terminal
  mid-run can leave stale lines.
- **Run log:** every step's start, outcome and elapsed time, its notices,
  and its output lines go to the run log (capability map, decision 11), in the run's span
  (carried onto other threads for concurrent steps). Output, notices or
  error text a step marks as sensitive (credentials, tokens) are never
  logged, and prompt answers never are. Sensitive output still shows on
  screen, to the user who ran the command.

### Tables

- **Look (proto-inline, reviewed):** tabled's rounded style with a rule
  between rows, bold centred headings, the border in the `Border` role,
  per-cell roles from the caller, right-aligned columns where asked.
- **Width:** fitted to the terminal (console's `Term::size_checked` on
  stdout); the widest column wraps first; a maximum column width where the
  caller asks (sb-go's `docker ps` caps at 100). Not a terminal: natural
  width.
- **Piped:** the same table without colour, as sb-go. Plain or JSON output
  is decided per command (capability map, open items). Column spans
  (`sb list`) come with that command.

### Prompts (specified now, built with their first user: decision 1)

- confirm (default no), text with validation, password with a fixed-length
  mask, themed from the palette (dialoguer's `ColorfulTheme` fields).
- Every prompt takes its answer from the caller when one was given (the
  command's flag), without asking.
- Prompts read the terminal, not stdin (`curl … | sudo bash` leaves stdin
  as the script; the install-script plan says prompts use `/dev/tty`). No
  terminal, or a background process group, and no answer: a clear error
  naming the flag, exit 1; never a hang.

## Out of scope

Full-screen screens (`tui`), Markdown output (announcements, `tui`),
starting processes and PTYs (`exec`), per-command output formats
(`--plain`, JSON), restore prompts (deferred with the restore backend).

## Tech stack

Pinned in the workspace when added, each confirmed at source in its task:

| Need | Crate | Backed by |
|---|---|---|
| Spinners, live lines | `indicatif` 0.18.6 (printed-lines pattern) | proto-runner |
| Widths, ANSI stripping, terminal size, colour switches | `console` 0.16 (indicatif's own); styles rendered by anstyle (Adapters) | proto-runner |
| Tree glyphs | `termtree` `GlyphPalette` (the rounded `╰` checked in the plan) | proto-runner |
| Tables | `tabled` 0.22 (`ansi` feature) | proto-inline |
| Prompts | `dialoguer` 0.12 (console 0.16) | proto-inline |
| `ECHOCTL` | `rustix` `termios` feature (already a dependency) | proto-runner |
| Async steps (decision 4) | `tokio`, minimal features (approved with the plan, 2026-11-01) | proto-logs, proto-runner |
| Snapshot tests (dev) | `insta` (approved with the plan, 2026-11-01) | capability map |
| Colour, theme, `Shutdown`, exit, run log | `sb-cli-core` | cli-core |

The binary will grow past the 5% ratchet; the plan writes that ADR, as
ADR-004 did.

## Commands

```
cargo test -p sb-term-ui
scripts/check.sh fast|task|full <base>
```

## Project structure

```
crates/term-ui/            → sb-term-ui (library)
  src/lib.rs               → the colour hook-up called from sb's main
  src/style.rs             → theme adapters (anstyle text, tabled)
  src/runner.rs            → the task runner
  src/table.rs             → tables
  src/prompt.rs            → prompts (with their first user)
  tests/                   → PTY tests of the runner (script)
```

deny.toml's architecture bans get a `sb-term-ui` entry (only `sb` and later
modules may depend on it).

## Code style

As cli-core: library-first, every pattern cited with its documentation URL,
no `#[allow]`, output through `Write`, never `print!`. Illustration only;
the API is designed in the plan (`agent-skills:api-and-interface-design`)
after decisions 2 to 4:

```rust
let run = Runner::start("Validating Saltbox config", &shutdown, log.as_ref())?;
run.step(Labels::new("Checking accounts.yml", "accounts.yml checked"), |step| {
    step.warn("unknown key: foo");
    check_accounts(&config)
})?;
run.finish()
```

## Testing strategy

- Rendering tests at a fixed width with colour on and off: tree glyphs,
  the one time column, notices, the failure block, table layout at 120, 70
  and 50 columns, the rule between rows. References are insta snapshots
  taken from the reviewed prototypes' output. indicatif's in-memory
  terminal is checked at source as a way to test live frames without a
  PTY.
- Guard tests per profile: every SGR colour code term-ui writes is 24-bit
  under `truecolor`, 256-colour under `ansi256`, 16-colour under `ansi`,
  none under `ascii` or with colour off.
- PTY tests (`script`, as cli-core's colour tests): live frames, no line
  erases, clean endings for success, failure and SIGINT, `ECHOCTL`
  restored.
- A known secret marked sensitive never reaches the run log
  (CONSTRAINTS.md logging rule).
- Prompts, when built: no-terminal failure and the given-answer path end to
  end.
- CONSTRAINTS.md applies: changed-line coverage, mutation score, startup
  budget, binary size ratchet.

## Boundaries

- **Always:** take colours from cli-core's theme; follow the one colour
  decision; give every prompt a non-interactive answer; log runner steps;
  keep sensitive output and prompt answers out of the log.
- **Ask first:** a new dependency beyond the table above; any change to
  the reviewed looks; a per-command output format.
- **Never:** colour maths or ANSI of our own; reading `NO_COLOR` or the
  terminal ourselves where a library does; starting processes; logging
  secrets.

## Success criteria

1. `--color=never` or `NO_COLOR`: no SGR codes at all (no colour, no
   bold) from tables, runner or prompts; on a terminal, the runner's
   cursor movement remains. Not a terminal with `auto`: no escape codes at
   all. `--color=always` when not a terminal: the theme's colours.
2. Under each `SB_COLOR_PROFILE`, every SGR colour code term-ui writes is
   of that profile's kind; `ascii` writes no SGR codes (as cli-core's
   plain styles).
3. The runner matches the reviewed tree (snapshot), with running and done
   labels, notices, and times in one column; each running step's tail at
   most 8 lines, all running tails together within half the terminal
   height; nesting and concurrent steps as decisions 2, 3 and 7 decide (a
   failing sibling stops the others, marked "stopped", and its parents
   read "sub-step failed"); 0 line erases while running (PTY
   measurement).
4. A failed step shows ✗, its error in full, its last 10 lines and the log
   pointer, once; sb exits 1 without cli-core repeating the error.
5. With `-v` or stderr not on a terminal, the runner prints start lines,
   output lines, notices and end lines; not on a terminal with `auto`, no
   escape codes.
6. Ctrl-C marks every running step and its parents interrupted in the
   tree, cli-core shows "interrupted" and the pointer once, the cursor and
   `ECHOCTL` are restored, and sb exits 130 (SIGTERM 143).
7. A table matches the reviewed `docker ps` look (snapshot) and fits 120,
   70 and 50 columns; not on a terminal it keeps its natural width.
8. Every runner step's start, outcome, elapsed time, notices and output
   lines are in the run log, and a known secret marked sensitive is not.
9. Help and version stay within the startup budget (term-ui does nothing
   for them); the binary growth has its ADR.
10. (With their first user) a prompt with no terminal and no answer fails
    with a message naming the flag; a given answer is used without asking.

## Decided (maintainer, 2026-10-30; decision 7 on 2026-11-01)

1. **Prompts** are specified here and built with their first user
   (self-update or git's confirm).
2. **Nesting and concurrency against no-blink:** proto-runner reached 0
   line erases by keeping the number of live bars constant, and indicatif
   warns against its cursor-moving redraw when bars come and go. The
   plan's first task is a spike measuring erases with a fixed pool of bars
   sized for the deepest nesting and widest concurrency used, shown to the
   maintainer with the look of notices, before the runner's API is fixed.
   **Amended at Checkpoint A (maintainer, 2026-11-01), from the spike
   (prototypes/proto-runner/FINDINGS.md):** one indicatif bar holds the
   whole live area (no pool to size; equally flicker-free), and the area's
   line count never shrinks while running (indicatif drops empty lines and
   its cursor-moving redraw can't clear lines that fall off the bottom).
3. **The last branch:** the newest finished step stays a live line until
   the next step starts or the run ends, then is printed with the right
   glyph (a line in the live area, per amended decision 2). With
   concurrent siblings the tree keeps start order, so "newest" means the
   last started.
4. **Steps are async** and get cli-core's cancellation token; the runner
   draws on indicatif's own ticker (tokio, capability map decision 8).
   **Amended at Checkpoint A:** a tokio task drives the frames (several
   spinners share one live area, and indicatif's `{spinner}` is one per
   bar); indicatif draws them.
5. **Double Ctrl-C restores the terminal, then exits.** A second signal
   still exits without waiting for cleanup, but the terminal is put back
   first, as well-behaved CLIs do: cli-core handles the second signal on
   its signals thread (code in the handler itself would need signal-hook's
   unsafe registration, which the workspace forbids), runs restore hooks
   that term-ui registers (show the cursor, restore `ECHOCTL`), then exits
   130 or 143. Ctrl-\ and `kill -9` stay as hard escapes. Recorded in the
   cli-core spec too.
6. **The capability map's `docker` row** (start/stop/restart "via
   bollard"; sb-go uses `systemctl` and the Saltbox Docker controller's HTTP
   API) is corrected in the `docker` spec, with its dependencies.
7. **Checkpoint A (maintainer, 2026-11-01), from the spike and the runner
   lookbook:**
   - When one of several concurrent steps fails, the others are stopped
     (sb-go's errgroup) and shown ✗ "stopped"; parents of a failure read
     "sub-step failed".
   - A step with sub-steps keeps its whole subtree live until it ends.
   - Ctrl-C marks every running step and its parents "interrupted", each
     keeping its last output lines; on a failure only the failed step keeps
     its output.
   - Notices are bullets: `›` for info in the muted colour, an orange `!`
     for warnings, indented under their step.
   - Output under a running step: up to 8 lines in the muted colour, all
     running steps' tails together within half the terminal's height.
   - The braille spinner; concurrent steps shown in full while running.
