# Spec: `term-ui` (approved 2026-10-30; Checkpoint A 2026-11-01)

*Amended 2026-11-18 by the maintainer's decisions after a real `sb
update` run: one tree per command, paused while something else needs
the terminal, printed once, when its run ends, in one column; questions
asked during a run open on the alternate screen (decision 14 and
"Prompts"; decisions 3 and 11 amended). Amended 2026-11-21 by the
maintainer's choice of a layout for `sb bench`: the ruled table look
and the section heading shared with `sb list` ("Tables").*

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
appears only under CI, redirection or `TERM=dumb` (no `-v`: decision 9).

## Behaviour

### Colour on every inline path

- At startup, after cli-core settles the colour choice and theme, `sb`'s
  `main` calls one term-ui function (cli-core can't call term-ui: the
  dependency points the other way). It reads anstream's resolved choice
  for stdout and for stderr (`AutoStream::choice`, as proto-inline did) and
  sets both of console's switches from them every time, so console's own
  `auto` rules can't disagree; indicatif and dialoguer follow console.
  Under `ascii` both switches are off (Task 2, approved at Checkpoint A, below).
- **Adapters** from cli-core's resolved theme: a role's colour (resolved
  for the profile by anstyle-lossy, or ADR-006's pick under `ansi`)
  becomes styled text and a tabled colour, both rendered by anstyle. No
  colour maths in term-ui. Under `ascii` or with colour off the adapters
  give no styling.
- **`bar(theme, role, on, ratio, width)`:** a one-line usage bar,
  `█` for the filled part (`round(width × ratio)` cells) in `role` and
  `░` for the rest in Faint, through `paint` (the styled-text adapter,
  `src/style.rs`). Its first user is motd's Disk Usage (motd decision 8,
  with the "why not indicatif or ratatui" evidence). *Amended 2026-11-13
  by the motd spec.*
- **Chosen in Task 2, approved at Checkpoint A (maintainer,
  2026-11-01)** (with the anstyle rendering above, console's switches off
  under `ascii`, and the stack table's console row and `src/style.rs`
  line): not console's `Style`,
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
  (`├──`, the last `╰──`), times and exit codes ending in one column
  (at least column 66, further right as far as the tree's longest line
  needs, never past the screen: decisions 11, 13 and 14). The
  running step is a branch with its running label, spinner and label in
  the `Accent` role, and a live tail of its newest output lines under it:
  each running step's tail at most sb-go's 8 lines, all running tails
  together within half the terminal height (decision 7). Glyphs `Faint`;
  ✓ `Ok` with the done label, ✗ `Problem` with the failure label,
  interrupted `Warn`; elapsed `Muted`; output lines in `Muted`, a line's
  own colours kept where it has them, except under `SB_COLOR_PROFILE=ascii`,
  which strips them too (maintainer, 2026-11-05: `ascii` means no SGR code
  in anything term-ui draws; an `attached` child writes to the terminal
  itself, exec decision 3).
- **Labels:** each step has a running label and a done label
  ("Checking accounts.yml" → "accounts.yml checked"), and optionally a
  failure label, as proto-runner and sb-go's Running/Success/Failure.
- **Transient steps** (decision 12): a step marked transient is drawn
  like any step while it runs. A run with transient steps holds each
  finished step until it ends (plain lines too): if every step
  succeeded, the transient ones leave the tree, so a run of transient
  steps leaves nothing on the screen, title included; if any step
  failed, was interrupted or was stopped, the whole tree is printed as
  any run's.
- **One tree per command** (decision 14): a command draws one runner's
  tree for its whole run. While a question, a full-screen view, an
  attached child or other output needs the terminal, the command pauses
  the runner (`Runner::pause_over`, `Runner::pause_below`), and the same
  tree goes on at its next top-level step. Until the run ends, finished
  steps stay in the live area ("Taller than the terminal" collapses
  them); the tree is printed once, at the run's end, every line in one
  column.
- **Notices:** a step can add info and warning lines, which stay under it
  in the finished tree (sb-go's `Task.Info` and `Task.Warning`), drawn as
  bullets: `›` for info in the muted colour, an orange `!` for warnings
  (decision 7).
- **Long lines** (the maintainer, hardening plan decision 35; ui-L4):
  while live, a notice, an error line, a kept output line or the
  failure block's pointer wider than the screen is cut with `…`. In the
  finished tree it is shown whole, wrapped at words inside the tree:
  each continuation row starts with the guide lines and indent of the
  line it continues (a notice's and the pointer's under their text, past
  the mark or branch), for as many rows as it needs, so no row is left
  to the terminal's own wrap. A line's own indent and a blank line stay;
  a word wider than the row (a long path or URL) breaks at any
  character. Wrapping is tabled's `Wrap::wrap`, by display width, a
  child's colour closed and reopened on each row (textwrap isn't a
  dependency). The run log keeps every line whole. A kept line can be up
  to exec's 4 KiB, about 54 rows at 80 columns.
- **Nesting and concurrency:** a step can run sub-steps (a deeper branch),
  and sibling steps can run at the same time. How this fits the no-blink
  technique and the last-branch glyph: decisions 2 and 3; what a failing
  sibling and Ctrl-C do to the others: decision 7, and a group that lets
  every sibling finish: decision 10.
- **Failure:** the failed step prints ✗, the failure label, the error
  (every line of a multi-line error, indented under it) and its last 10
  output lines, then the log pointer (`╰── details: <file> (run <id>)`,
  cli-core's error-block label, from cli-core's `RunLog::pointer`, when
  the run is logged and the caller passed the log: validate-config and
  `sb edit` pass none and print the pointer once after the tree). The run then
  ends with a quiet `Exit(1)`, so cli-core doesn't repeat the error
  (cli-core spec: errors already shown exit quietly). The step's own log
  record carries the error and its causes, since the run record then only
  says `exit 1`.
- **No blinking:** one live bar holds the whole live area and is redrawn
  in place (indicatif's `set_move_cursor`, as proto-runner and the spike
  measured: 0 line erases while running), per amended decision 2.
- **Stderr not a terminal** (`TERM=dumb`, CI, redirection): no live
  parts; there is no verbose option, the run log has the detail
  (decision 9). The runner draws on stderr, so stderr
  decides (indicatif's target; sb-go checks stdout, so `sb validate-config
  > file` stays live here and goes plain in sb-go). Each step prints a
  start line (`- <running label>`), its output lines (colour codes
  stripped when colour is off or under `ascii`, other escapes always:
  "Escape handling"), notices, and an end line
  (`✓ <done label>   2.0s`, or ✗ with the reason,
  `✗ <label>   interrupted · 2.0s`, and for a failure the error and the
  pointer; three spaces before the time as in the live tree, decision
  11), flat as in sb-go's plain mode
  when not interactive. Lines from concurrent steps are prefixed
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
  term-ui changes the terminal sb draws on instead: stderr's, which is a
  terminal whenever the runner is live (no `/dev/tty` is needed). The
  cursor is hidden while live. Amended (maintainer, 2026-11-11,
  hardening plan decision 17): the echo is switched off too while live
  (`ECHO`, and `ECHONL` so Enter echoes nothing either), because keys
  typed or Enter pressed mid-run were echoed into the live area and left
  stale duplicate lines at its next redraw. Canonical mode and `ISIG`
  stay on: what is typed waits in the input queue, and Ctrl-C still
  signals. Every ending puts back the modes sb found, with `TCSAFLUSH`
  (rustix's `OptionalActions::Flush`): whatever is unread on the
  terminal when the run ends is discarded, the keys typed while live and
  any typed before the run quieted the terminal alike (those were
  echoed), so none reach the shell after sb ends, at an ordinary ending
  and at the forced exit's restore hook alike. *Amended
  2026-11-17 (maintainer, the hardening plan's Checkpoint C sitting,
  type-ahead), overruling the earlier call that they stay
  queued:* sb-go's live views are Bubble Tea programs
  (`charm.land/bubbletea/v2` v2.1.0), which read the keys themselves;
  apt restores the terminal after dpkg with `TCSAFLUSH` (apt 2.7.14,
  `pkgDPkgPM::StopPtyMagic` in `apt-pkg/deb/dpkgpm.cc`); and prompts
  drop stale input before they ask (the confirm, below). less, as of
  v712, restores with `TCSADRAIN`, keeping it; its development tree has
  switched to `TCSAFLUSH` on leaving. The tty is only
  touched in the foreground process group (sb-go's `tty.go` guard),
  checked again at every restore, so a background sb (or one moved there with Ctrl-Z and `bg`) never stops
  on SIGTTOU; a job-control shell puts its own modes back then. A second
  Ctrl-C restores the terminal through a hook cli-core runs before its
  immediate exit (decision 5), which also ends the live area's line.
  Known limit: Ctrl-Z itself (SIGTSTP) leaves the cursor hidden at the
  shell's prompt until sb resumes (bash restores the modes, not the
  cursor). Known limit: `kill -9` runs no restore, so it leaves the
  echo off (since decision 17, not only `ECHOCTL`) unless a job-control
  shell puts its own modes back, and the keys typed while live stay
  queued for the shell; `stty sane` turns the echo on again.
- **Taller than the terminal** (maintainer, hardening plan decision 17;
  ui-M1): when the live area has more lines than the terminal has rows,
  its oldest settled steps (each with its subtree, notices and failure
  block) collapse to one `… N finished` line in the `Muted` role, N
  counting steps, at the first collapsed step's branch, until the area
  fits; the newest finished steps fill what room is left, and the
  running steps and their output stay in view while they themselves fit
  (a terminal too short even for them shows their top lines). indicatif
  stops drawing at the terminal's height and ratatui's inline viewport
  is clamped to it, so neither picks what to keep; the rule is term-ui's. It is
  worked out every frame from the terminal's current height, so a
  terminal resized mid-run collapses more or less from the next frame.
  Nothing is lost: the finished tree, printed for good, holds every step
  in full, and the run log has every step whatever was drawn. A run
  holding failed steps reads `… N finished, M failed` in the `Warn`
  role instead, M counting the failed steps among the N (a parent that
  reads "sub-step failed" isn't counted again), so a failure doesn't
  vanish from view (the maintainer, hardening plan decision 35; Task 57).
- **Live block** (decision 15, 2026-11-22): a runner started with
  `Runner::start_block` draws a command's own text in its live part
  instead of the tree (`sb bench`'s report as its results arrive). The
  steps run, are logged, are cancelled and are interrupted exactly as
  any runner's; only the tree isn't drawn while the run goes on. On a
  terminal, the block's render is called at each frame with the
  screen's width and height, stderr's colour choice, the spinner's
  glyph and whether a signal interrupted the run; its text goes through
  `DisplayText` as every line term-ui draws from given text ("Escape
  handling": only SGR kept, under the profile's rule), and its lines are
  drawn in place through console_static_text 0.9.0 (D. Sherret's, the
  live text of Deno's CLI), each frame one synchronized update (DEC
  2026, crossterm's command), the cursor hidden and the echoes off as
  for the tree. `Runner::suspend` clears the block, runs a closure that
  writes on stdout (a finished section) and draws the block again, in
  one synchronized update, so nothing between shows a blank frame
  (indicatif's `MultiProgress::suspend` for the tree).
  - **Pace:** a block's frame moves every 120 ms, console_static_text's
    own example's and Deno's pace (the tree's spinner stays at 80 ms);
    console_static_text writes nothing when a frame's lines are the
    same as the last's, so the bytes sent follow what changes. A frame
    tick that finds the size changed draws nothing and waits for the
    next one, as the crate's example and Deno debounce a resize.
  - **Endings:** at a happy end the block is cleared and what the tree
    prints at a run's end is printed (nothing, for a run of transient
    steps that all succeeded; for a failed run, the whole tree, with
    the failure block and the pointer, as any failed run's). Interrupted
    by a signal, the block's last frame stays, drawn with the
    interruption, and the tree isn't printed (cli-core's block says
    "interrupted" and the pointer). Plain (stderr
    not a terminal), the render is never called and the runner prints
    as any runner does.
  - **Resize:** each frame reads the screen's size. console_static_text
    wraps the last frame at the new width to count the rows it now
    takes, moves up over them and clears down, so on a terminal that
    rewraps its lines (tmux, VTE, kitty, iTerm2, Windows Terminal,
    WezTerm, foot) the block is drawn again in place at the new width,
    with no copy. Its lines are never padded, so nothing is left to the
    terminal's own wrap. Known limit, measured on vt100 (which, as
    xterm and the Linux console, cuts lines rather than rewrapping
    them): narrowed below the block's widest line, the count overshoots,
    and as many rows above the block as its lines gained are cleared
    from the screen, never duplicated (bench's lines end at its rules,
    two columns past the report's widest line, about 62 columns). Taller than the screen, only its
    bottom rows are drawn (the crate cuts from the top), so the row
    being measured stays in view, and a height shrink leaves no copy
    while the block still fits the screen (measured in tmux).
- **Resize:** known limit from proto-runner: the live area's line count
  never shrinks while running (decision 2), so shrinking the terminal
  mid-run can leave stale lines (the tree drawn in the area still fits
  the new height, as above).
  *Amended 2026-11-22 (the maintainer):* each live row ends with a line
  end, not the terminal's wrap (`Rows`, under indicatif), so a terminal
  that rewraps its lines on a resize keeps them apart when it widens;
  after a narrowing past the tree's width, the next redraw also moves up
  over, and clears, the rows those lines were rewrapped onto. Measured
  with tmux 3.2a; VTE, kitty, Alacritty, `WezTerm`, Konsole, iTerm2 and
  Windows Terminal are said to rewrap the same way (UNVERIFIED). Known
  limits: rows a terminal moves into its history on the rewrap stay
  there (tmux keeps the cursor's row and pushes what the area grew by
  out of the screen); a wide character pushed to the next row at the
  wrap isn't counted, so a row can stay behind then. On a terminal that
  cuts lines on a resize instead of rewrapping them (xterm, `PuTTY`, the
  Linux console, by their documentation; UNVERIFIED here), a narrowing
  past the tree's width makes that redraw start as many rows higher and
  overwrite as many lines above the live area (for `sb bench`, the end
  of a section it printed); before 2026-11-22 those terminals redrew
  correctly and the rewrapping ones showed the title and steps twice.
  Which a terminal does can't be asked without reading its replies from
  the input. The maintainer chose this side: most terminals rewrap.
- **Run log:** every step's start, outcome and elapsed time, its notices,
  and its output lines go to the run log (capability map, decision 11),
  in the run's span (carried onto other threads for concurrent steps).
  Secret values in output, notices or error text (credentials, tokens)
  are never logged: the rest of the line is, with `[REDACTED]` in their
  place; prompt answers never are. Secret values are withheld on screen
  too, the same way, unless the command's job is to show them to its
  user, such as editing a config, and then only on a terminal (decision
  8). Each logged output or notice line is made by `DisplayText` with
  colour off (see "Escape handling"), so the log never holds an escape
  sequence or its payload, APC, PM and SOS included.

### Escape handling (amended 2026-11-11, journal spec decisions 2 and 5; tables 2026-11-12)

Every line term-ui draws from text it was given (a step's output and
notices, error lines, a child's last lines and reason, a table's headings
and cells, a live block's text: decision 15), and every output and
notice line it logs, goes through one
public sanitizer, `DisplayText`, shared with the log viewer (journal
spec, "Display (decision 2)"). A step's error record is logged as is,
through the run log's event formatter, which escapes every control
character, `\n` and `\r` included, so it stays one record (cli-core
spec, startup step 4):

```rust
/// Sanitized text, held as one `String` plus line ranges.
pub struct DisplayText { /* … */ }

impl DisplayText {
    /// `colour`: the caller passes `keeps_codes(colour_on, profile)`.
    pub fn from_bytes(bytes: &[u8], colour: bool) -> DisplayText;
    /// `from_bytes`, with a `Resume` about every `every` columns of each
    /// line, the first line starting at display column `column` (the
    /// log viewer far right with wrap off, tui spec "Hardening as built").
    pub fn resumable(bytes: &[u8], colour: bool, column: usize, every: usize)
        -> (DisplayText, Vec<Resume>);
    /// `from_bytes` from display column `column`: the parse a `Resume`
    /// starts, of the bytes from its `byte` on, which makes the rest of
    /// the text from there.
    pub fn from_bytes_at(bytes: &[u8], colour: bool, column: usize) -> DisplayText;
    /// The lines, in order; at least one.
    pub fn lines(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator;
    /// The lines joined by `\n`.
    pub fn as_str(&self) -> &str;
}
// `Display` writes `as_str()`.

/// Where a parse can start again: just before a printed character (the
/// parser in its ground state, no carriage return pending), next to a
/// printable ASCII character; none from the first byte changed before
/// parsing (invalid UTF-8, C1) on, none left on a line a carriage return
/// took back.
pub struct Resume { pub byte: usize, pub line: usize, pub column: usize, pub visible: usize }

/// Whether a line's own colour codes are kept: colour on, and the profile
/// isn't `ascii`.
pub fn keeps_codes(colour: bool, profile: Profile) -> bool;
```

- Invalid UTF-8 is decoded lossily (U+FFFD).
- Newline splits lines, as `str::lines` does: a final newline adds none,
  but a line after it exists when anything follows, even if all of it is
  dropped (`a\n\x07` is two lines); empty text is one empty line. Tab
  expands to the next 8-column stop, by display width.
- Carriage return is the one other C0 control applied: trailing ones are
  ignored, then what follows the last one on its line is kept (progress
  output). A dropped control or sequence after it counts as what follows,
  so `abc\r\x1b[K` is an empty line.
- An SGR is kept only with `colour`, and is written from the input's own
  bytes, ESC to the final byte, never rebuilt from parsed parameters, so
  colon forms such as `38:2::r:g:b` stay byte-exact. A CSI counts as SGR
  only when it matches `ESC [ [0-9;:]* m`: `ESC[?1m` or an intermediate
  byte makes it something else.
- Every other escape sequence is dropped, with its payload: OSC (titles,
  OSC 8 links, OSC 52 clipboard), DCS, APC, PM, SOS, cursor and mode
  CSIs, terminal queries, plain `ESC` sequences, and a CSI past the
  parser's parameter limit (32; its `ignore` flag). An unterminated string
  sequence takes the rest of the text, newlines included.
- Other C0 controls, DEL and C1 are dropped; CAN and SUB end a sequence,
  as in the DEC model. Bidi and zero-width characters pass unmarked.
- sb requires a UTF-8 terminal, in every profile, `ascii` included: its
  own tree and table glyphs are UTF-8 already (`├` is E2 94 9C, `─` is
  E2 94 80). A terminal not in UTF-8 mode can read the bytes 0x80 to
  0x9F inside a UTF-8 character as C1 controls (`‛`, E2 80 9B, holds a
  CSI), so untrusted text could drive it; filtering that text alone
  wouldn't make such a terminal usable, so it isn't filtered for one.
  *Amended 2026-11-17 (maintainer, the performance pass's security
  review).*

**Tables** sanitize in `Table::render`, the one place every title,
heading and cell passes, with colour off: a cell's own SGR codes are dropped (a
container label is untrusted text, as `sb docker ps` shows it), and the
colour the table adds itself (border, bold headings, a cell's role) is
applied afterwards, so it stays. A cell's `\n` still separates its
lines; a tab and a carriage return are applied as above. `sb docker
ps --json` doesn't pass through a table, and its output escapes every
control character, C1 and DEL included (docker spec, `--json`).

**The parser** is anstyle-parse 1.0.0 (already in the tree through
anstream; https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/), which runs
the DEC state machine; console's `AnsiCodeIterator`, used before,
recognises only CSI, OSC and DCS, so APC, PM and SOS payloads reached the
screen and the log. **The 8-bit C1 forms are ours:** anstyle-parse, like
vte, only supports 7-bit codes, so a pre-pass maps U+0090, U+0098,
U+009B, U+009D, U+009E and U+009F to their `ESC` forms and drops the rest
of U+0080 to U+009F (U+009C, the 8-bit ST, among them). A kept 8-bit SGR
is written in its 7-bit form. **Non-ASCII characters reach the parser as
a stand-in** (U+0080, absent once the pre-pass has run), swapped back
when printed: outside its ground state the parser doesn't decode UTF-8,
and its DCS, SOS, PM and APC states end at a 0x9C byte, which a
character's own UTF-8 can hold (`“` is E2 80 9C), so a payload holding
one would otherwise end early and show. The stand-in's bytes are no-ops
in every other state of anstyle-parse 1.0.0's table (payload in OSC);
UNVERIFIED for later versions, pinned by a test of every two-byte
character and three- and four-byte samples inside each string
sequence.

### Tables

- **Look (proto-inline, reviewed):** tabled's rounded style with a rule
  between rows, bold centred headings, the border in the `Border` role,
  per-cell roles from the caller, right-aligned columns where asked.
- **Width:** fitted to the terminal (console's `Term::size_checked` on
  stdout); the widest column wraps first; a maximum column width where the
  caller asks (sb-go's `docker ps` caps at 100). Not a terminal: natural
  width.
- **Wrapping (ui-L3, 2026-11-13):** tabled chooses the column widths;
  term-ui wraps each line of a cell on its own (tabled's `Wrap::wrap`), at
  words, or at any character in a column the caller marks (`docker ps`'s
  bindings, whose spaces are padding). A wrapped cell has no blank line
  where a line ended exactly at the width. A terminal narrower than the
  narrowest table (a character a cell; a wide character in a column one
  character wide shows as `�`) gets a line saying so and the
  width needed, instead of a table of empty cells. Why not tabled's
  `keep_words` on the whole table: it counts a cell's line break as a
  character, leaves those blank lines, splits a binding at its padding,
  and below that width empties cells, a column at a time.
- **Piped:** the same table without colour, at its natural width (sb-go's
  piped table wraps at 80 columns and keeps its bold and border codes;
  docker spec, differences). Plain or JSON output is decided per command
  (capability map, open items). `Table::spanning_row(text)` adds a bold
  row across every column (`sb list`'s later repos), through tabled's
  `Span`. *Amended 2026-11-13 by the ansible spec.* `sb bench` lays its
  piped tables out at 80 columns instead (ops spec, "`sb bench`";
  the maintainer, 2026-11-18).
- **A title row:** `Table::titled(title, headings)` draws a row across
  every column above the column headings; both are bold and centred, as
  headings are, through the same `Span` (bench's Speedtest table). The
  title wraps across the whole table when fitted. Every other table
  draws as before. *Amended 2026-11-17 (maintainer).*
- **The ruled look** (`Table::ruled(headings)`, `sb bench`'s: ops spec,
  decision 43; *amended 2026-11-21, the maintainer*): tabled's
  `Style::empty()`, so no border and no rule between rows; no padding
  but a two-space gap, after each left-aligned column but the last,
  before each right-aligned column but the first, and before a
  left-aligned column after a right-aligned one; headings in Muted,
  aligned as their columns, none
  drawn when the list is empty; a column's minimum width where the
  caller asks (tabled's `Width::increase`, `MinWidth`), which counts
  the gap; a cell bold where the caller asks (`Cell::bold`), with its
  role's colour or alone; an empty cell drawn without codes. Fitted and wrapped as every table, the
  narrowest a character a cell plus the gaps; each line's trailing
  spaces dropped. Escape handling as every table's. The boxed look is
  unchanged.
- **Section headings** (`Heading`; *amended 2026-11-21, the maintainer,
  for `sb bench`*): `sb list`'s heading, moved here so `sb list` and
  `sb bench` share it: a name, bold, in the role its caller names; any
  hints after it, each in its own role (list's `  31 tags` and `  ·
  install as sandbox-<tag>`, bench's `  10 nodes`); then a rule as long
  as its caller says (`sb list`'s across the width, bench's past its
  widest line; amended 2026-11-22) in its role, Muted unless the caller
  names another. With
  colour off or under `ascii`, no codes at all, bold included, and the
  rule in `-` instead of `─`. Its text is sb's own words, never
  untrusted input. `sb list`'s output is byte for byte what it was.

### Prompts (specified now, built with their first user: decision 1)

- confirm (default no), text with validation, password with a fixed-length
  mask, themed from the palette through dialoguer's `Theme` trait (its
  default `[y/N]` layout with `ColorfulTheme`'s `?` prefix, painted with
  term-ui's anstyle adapters: `ColorfulTheme`'s own fields are console
  styles, which write bright colours as 256-colour codes). *Amended
  2026-11-13 with M26.*
- The confirm reads the whole line, on dialoguer's text input: `y` or
  `yes`, in any case, with spaces around trimmed, accepts; anything else
  (`nay`, `no thank you`, Enter alone) declines. dialoguer's `Confirm`
  keeps the last `y` or `n` typed, so `nay` would accept, and the confirm
  guards replacing a root-run binary. The answer shows as `yes` or `no`.
  *Amended 2026-11-13 with M26.*
- Every prompt takes its answer from the caller when one was given (the
  command's flag), without asking.
- Before the confirm asks, what was typed already is dropped (rustix's
  `tcflush` with `TCIFLUSH` on the terminal it reads), so a stray `y`
  and Enter, or a paste, can't answer a question never seen. *Amended
  2026-11-17 (maintainer): written down here from the code, as built
  with `release` (4a6b474).*
- Ctrl-C at the confirm ends sb by SIGINT, every time. console raises
  SIGINT for it and returns at once, before cli-core's signals thread
  has read the signal, so sb could end with exit 1 and an "interrupted"
  block (about 3 runs in 8 under the coverage build). The confirm takes
  the run's `Shutdown` and calls `Shutdown::interrupt_raised` when
  console returns: the delivery flag signal-hook's handler set
  (`flag::register`) is decided there, as the signals thread would, so
  the run is interrupted before the confirm returns, without a wait.
  M50's bounded wait in `sb edit` is gone; self-update's confirm gets
  the same. *Amended 2026-11-15 (maintainer).*
- **A question during a run opens on the alternate screen**
  (`confirm_over(question, context, answer, flag, shutdown)`; the
  maintainer, 2026-11-18, decision 14): the command pauses its runner
  over it (`Runner::pause_over`), and the confirm switches the terminal
  it asks on to the alternate screen (crossterm's `EnterAlternateScreen`,
  `\e[?1049h`, written to that terminal,
  <https://docs.rs/crossterm/0.29.0/crossterm/terminal/struct.EnterAlternateScreen.html>;
  dialoguer and console have no alternate-screen call), writes
  `context` there (what the question is about: the current branch, the
  migration table), asks exactly as `confirm` does (its keys, texts and
  answer rules), and leaves the alternate screen
  (`LeaveAlternateScreen`) on every ending: an answer, Ctrl-C (sb then
  ends by SIGINT, the terminal restored), a failed read, and a second
  Ctrl-C (a restore hook on the run's `Shutdown`). The normal screen,
  the live tree on it included, is never touched. The command records
  the answer as a `›` notice under its step (`Runner::info`). With no
  terminal, or an answer from the flag, it is `confirm`: no screen.
  The question starts at the alternate screen's top left (crossterm's
  `MoveTo(0, 0)`: `?1049h` keeps the cursor where the live area left
  it). It switches screens only where a runner would draw live
  (indicatif's rule for stderr, `ProgressDrawTarget::is_hidden`: a
  terminal, `TERM` set and not `dumb`); elsewhere (stderr redirected, a
  dumb terminal) there is no live tree to keep, and `context` is
  written above the question on the screen as it is.
- Prompts read the terminal, not stdin (`curl … | sudo bash` leaves stdin
  as the script; the install-script plan says prompts use `/dev/tty`). No
  terminal, or a background process group, and no answer: a clear error
  naming the flag, exit 1; never a hang.
- Built with `release` (self-update's confirm); the label is the caller's
  text, `?` and the `[y/N]` hint are dialoguer's. *Amended 2026-11-13 by
  the release spec.*
- ~~The text and password prompts' first user is `sb restore` (ops spec;
  the confirm comes earlier with `release`). dialoguer's text and
  password prompts have no cancel key; Ctrl-C is the way out (cli-core's
  interrupted ending). *Amended 2026-11-13 by the ops spec.*~~
  Superseded 2026-11-17: restore asks in tui's form (next bullet).
- Not built: `sb restore` became a full-screen form on a terminal
  (sb-tui; ops decision 32, the maintainer, 2026-11-16), so it asks no
  line prompt, and no other command needs a text or password prompt yet.
  Found while building them (M76), for whoever does: dialoguer's
  `Password` can't be left with Ctrl-C under sb's signal handling.
  console reads it in canonical mode, where Ctrl-C is the kernel's
  SIGINT; cli-core's signal-hook handler (`SA_RESTART`) restarts the
  read and the kernel drops the typed line, so Enter returns an empty
  password, which `Password` asks again (a scratch program on console
  0.16.6 and signal-hook 0.4.5, in tmux). A raw-mode reader for the
  whole prompt (crossterm's raw mode and `event::read`) worked. Why not
  console's `read_key` per key: raw for one key at a time, so a key
  typed between two reads would be echoed. *Amended 2026-11-17.*

## Out of scope

Full-screen screens (`tui`), Markdown output (announcements, `tui`),
starting processes and PTYs (`exec`), per-command output formats
(`--plain`, JSON). *Amended 2026-11-13 by the ops spec:* "restore
prompts (deferred with the restore backend)" dropped. *Amended
2026-11-17:* restore's prompts are tui's form (ops decision 32), so no
text or password prompt is built yet (Prompts).

## Tech stack

Pinned in the workspace when added, each confirmed at source in its task:

| Need | Crate | Backed by |
|---|---|---|
| Spinners, live lines | `indicatif` 0.18.6 (printed-lines pattern) | proto-runner |
| Widths, ANSI stripping, terminal size, colour switches | `console` 0.16 (indicatif's own); styles rendered by anstyle (Adapters) | proto-runner |
| Escape sequences in text drawn or logged (`DisplayText`) | `anstyle-parse` 1.0.0 (anstream's own) | journal spec, decision 5 |
| Tree glyphs | `termtree` `GlyphPalette` (the rounded `╰` checked in the plan) | proto-runner |
| Tables | `tabled` 0.22 (`ansi` feature) | proto-inline |
| Prompts | `dialoguer` 0.12 (console 0.16) | proto-inline |
| `ECHOCTL`, `ECHO`, `ECHONL` | `rustix` `termios` feature (already a dependency) | proto-runner |
| Async steps (decision 4) | `tokio`, minimal features (approved with the plan, 2026-11-01) | proto-logs, proto-runner |
| Snapshot tests (dev) | `insta` (approved with the plan, 2026-11-01) | capability map |
| Colour, theme, `Shutdown`, exit, run log | `sb-cli-core` | cli-core |
| Step errors (runner API, approved 2026-11-01) | `anyhow` (already the binary's) | cli-core's binary |
| Step records in the run's span (approved 2026-11-01) | `tracing` with `Instrument` (already cli-core's) | cli-core |
| In-memory screen for snapshots (approved 2026-11-01; dev only, term-ui's `testing` feature) | indicatif's `in_memory` feature (pulls `vt100`) | indicatif docs |
| A live block's in-place redraw (decision 15; the maintainer, 2026-11-22) | `console_static_text` 0.9.0, without its `sized` feature (the size is passed in) | the bench-live prototype A' |
| A live block's test screen (dev only, term-ui's `testing` feature) | `vt100` 0.16 (already under indicatif's `in_memory`) | indicatif docs |

The binary is expected to grow past the 5% ratchet; the ADR comes with
the commit that crosses it (Task 11 records it), as ADR-004 did.

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
  src/block.rs             → the runner's live block (decision 15)
  src/text.rs              → `DisplayText`, the shared sanitizer
  src/table.rs             → tables
  src/prompt.rs            → prompts (with their first user)
  tests/                   → PTY tests of the runner (script)
```

deny.toml's architecture bans get a `sb-term-ui` entry (only `sb` and later
modules may depend on it).

## Code style

As cli-core: library-first, every pattern cited with its documentation URL,
no `#[allow]`, output through `Write`, never `print!`.

### Runner API (Task 4; approved at Checkpoint A, maintainer, 2026-11-01)

Signatures as they would be written; bodies come with Tasks 5-9.

```rust
/// One run of steps, drawn as one tree (the root line is the title).
/// Live on a terminal; plain when the target isn't one (piped, CI,
/// `TERM=dumb`). No verbose option: the run log has the detail
/// (decision 9).
pub struct Runner { /* … */ }

impl Runner {
    /// Start drawing. Called inside the command's tokio runtime (sb builds
    /// it after argument parsing); the frame task runs on it. Never fails:
    /// if the terminal's modes can't be read, the echoes (`ECHOCTL`,
    /// `ECHO`, `ECHONL`) are left alone.
    /// Registers a restore hook with `shutdown` for a second Ctrl-C (it
    /// first stops the frame task drawing, then writes through its own
    /// tty fd and puts the echoes back), removed when the runner ends.
    /// `log` gives the failure block's pointer; step records go through
    /// `tracing` in the current span, which the runner captures here and
    /// carries onto concurrent steps' tasks (`Instrument`).
    pub fn start(
        title: &str,
        shutdown: &Shutdown,
        log: Option<&RunLog>,
        options: Options,
    ) -> Runner;

    /// Run one top-level step to its end; see [`Step::step`]. `&mut self`
    /// keeps top-level steps one after another (concurrency is
    /// [`Step::group`]'s).
    pub async fn step<T>(
        &mut self,
        labels: Labels,
        body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
    ) -> Result<T, Exit>;

    /// End the run at its happy end: print what is still live, show the
    /// cursor, put the terminal back, remove the restore hook. Dropping
    /// the runner is the safety net (a command leaving early with `?`, or
    /// a panic): it does the same without panicking and stops the frame
    /// task first, so no frame lands after the last lines.
    pub fn finish(self);

    /// A runner whose live part is `block`'s text instead of the tree
    /// (decision 15): the same steps, log, cancellation and endings,
    /// the tree printed only for a failed run.
    pub fn start_block(
        title: &str,
        shutdown: &Shutdown,
        log: Option<&RunLog>,
        options: Options,
        block: Block,
    ) -> Runner;

    /// Hide the live part, run `f` (which writes on stdout), and draw
    /// the live part again, in one synchronized update; plain, `f` alone.
    pub fn suspend<R>(&mut self, f: impl FnOnce() -> R) -> R;
}

/// A live block's text, rendered at each frame (decision 15).
pub struct Block { /* the render; with `testing`, a test screen */ }

impl Block {
    pub fn new(render: impl FnMut(&BlockFrame) -> String + Send + 'static) -> Self;
    /// With term-ui's `testing` feature: drawn on `screen` (a vt100
    /// screen whose size the test sets), always live, its spinner still.
    #[cfg(any(test, feature = "testing"))]
    pub fn on(self, screen: Screen) -> Self;
}

/// What a block's render gets: the screen's width and rows, stderr's
/// colour choice, the spinner's glyph, and whether a signal interrupted
/// the run.
#[derive(Clone, Copy, Debug)]
pub struct BlockFrame {
    pub width: usize,
    pub rows: usize,
    pub colour: bool,
    pub spinner: &'static str,
    pub interrupted: bool,
}

/// Whether a `Runner` is live in this process (started, not yet finished
/// or dropped), for exec's `attached`.
pub fn runner_live() -> bool;

/// A failed child (exec's `Failed`): a step failing with it anywhere in
/// its error chain shows `reason` before its time and `last_lines` after
/// the error.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ChildFailure { /* message, reason, last_lines */ }

impl ChildFailure {
    pub fn new(message: impl Into<String>, reason: impl Into<String>, last_lines: Vec<String>) -> Self;
    pub fn reason(&self) -> &str;
    pub fn last_lines(&self) -> &[String];
}

/// How and where the runner draws.
#[derive(Default)]
pub struct Options {
    /// Where to draw: stderr by default.
    pub target: Target,
}

#[derive(Default)]
pub enum Target {
    #[default]
    Stderr,
    /// An in-memory screen of a fixed size, colour on or off, for the
    /// snapshot tests: indicatif's `InMemoryTerm` (its `in_memory`
    /// feature, which pulls vt100), only with term-ui's `testing` feature,
    /// so it never ships in sb.
    #[cfg(any(test, feature = "testing"))]
    InMemory { term: indicatif::InMemoryTerm, colour: bool },
}

/// A step's words: running, done, and (optional) failure, as sb-go's
/// Running/Success/Failure. "stopped", "interrupted" and "sub-step
/// failed" are shown after the running label; ✗ shows the failure label,
/// else the running label.
#[derive(Clone, Debug)]
pub struct Labels { /* … */ }

impl Labels {
    pub fn new(running: impl Into<String>, done: impl Into<String>) -> Self;
    pub fn failure(self, failure: impl Into<String>) -> Self;
    /// Shown only while it runs (decision 12).
    pub fn transient(self) -> Self;
}

/// A running step. Sequential bodies borrow it (`&mut`), so it can't
/// outlive their step or start siblings behind the runner's back. A
/// concurrent sibling owns its `Step` (its task is `'static`); if the body
/// keeps it past its end (a detached task), later lines are logged only
/// and `step`/`group` return `Exit(1)` without drawing. `Send + Sync`.
pub struct Step { /* … */ }

impl Step {
    /// The token this step watches: a child of `shutdown.token()`,
    /// cancelled by Ctrl-C, by the command's own `Shutdown::request`, or
    /// by a failing concurrent sibling (decision 7). A body stops early
    /// when it is cancelled and returns its error.
    pub fn token(&self) -> CancellationToken;

    /// An info notice (`›`, muted) under this step, kept and logged
    /// (sensitive text: see `Line`).
    pub fn info(&mut self, text: impl Into<Line>);
    /// A warning notice (orange `!`) under this step, kept and logged
    /// (sensitive text: see `Line`).
    pub fn warn(&mut self, text: impl Into<Line>);
    /// Output: shown in the live tail (or plainly) and logged (sensitive
    /// text: see `Line`). Text is
    /// split into lines at `\n`; a `\r` keeps what follows it (progress
    /// output); colour codes are kept on a coloured target, except under
    /// `ascii`, and stripped otherwise; every other escape sequence goes
    /// with its payload ("Escape handling"); a line wider than the screen
    /// is cut with `…` while live, wrapped inside the finished tree ("Long
    /// lines"), and logged whole, with colour off.
    pub fn output(&mut self, text: impl Into<Line>);

    /// Run a sub-step (a deeper branch) to its end; this step's subtree
    /// stays live until this step ends (decision 7). See "Step endings".
    pub async fn step<T>(
        &mut self,
        labels: Labels,
        body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
    ) -> Result<T, Exit>;

    /// Concurrent sub-steps: [`Group::spawn`] each, then [`Group::join`].
    pub fn group<T: Send + 'static>(&mut self) -> Group<'_, T>;

    /// Concurrent sub-steps that all run to their end (decision 10):
    /// [`GroupAll::spawn`] each, then [`GroupAll::join`].
    pub fn group_all<T: Send + 'static>(&mut self) -> GroupAll<'_, T>;
}

/// Concurrent sibling steps under one parent, drawn in full in start
/// order (decision 7). Each runs as a tokio task (`JoinSet`,
/// https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinSet.html), so bodies
/// own what they use and get their own `Step`. Borrows the parent, so the
/// parent can't add lines or steps meanwhile.
#[must_use = "join the group; dropping it stops its steps"]
pub struct Group<'a, T> { /* … */ }

impl<T: Send + 'static> Group<'_, T> {
    pub fn spawn<F, Fut>(&mut self, labels: Labels, body: F)
    where
        F: FnOnce(Step) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static;

    /// Wait for every sibling; results in spawn order (`join_next_with_id`
    /// mapped back, not `join_all`'s completion order). The first failure
    /// seen cancels the others' tokens and its `Exit` comes back once all
    /// have returned (errgroup also waits for them). A panicking sibling
    /// is marked failed, the others are cancelled and waited for, then the
    /// panic resumes (`std::panic::resume_unwind`, as tokio's docs
    /// advise); the default panic hook's message lands in the live area
    /// first, and all drawing stops for the rest of the run, so nothing
    /// draws over it (ui-M3). An aborted sibling's `spawn_blocking` work runs on, detached
    /// (an accepted limit).
    pub async fn join(self) -> Result<Vec<T>, Exit>;
}

/// A `Group` whose failures and panics cancel no sibling (decision 10).
/// Ctrl-C, the command's own request and dropping it unjoined act as on
/// a `Group`.
#[must_use = "join the group; dropping it stops its steps"]
pub struct GroupAll<'a, T>(/* … */);

impl<T: Send + 'static> GroupAll<'_, T> {
    pub fn spawn<F, Fut>(&mut self, labels: Labels, body: F)
    where
        F: FnOnce(Step) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<T>> + Send + 'static;

    /// Wait for every sibling; each one's result in spawn order (`Exit(1)`
    /// for one the runtime cancelled). A panicking sibling is marked
    /// failed, the others are waited for (not cancelled), then the panic
    /// resumes. The parent's body returning the first failure with `?`
    /// reads "sub-step failed", as after `Group::join`.
    pub async fn join(self) -> Vec<Result<T, Exit>>;
}

/// A notice or output line: plain text, with secret values marked where
/// they sit (decision 8, amended). A secret value shows as `[REDACTED]`
/// in its place, on screen and in the log; the rest of the line stays.
/// From `&str`, `String` and `&String`: all plain.
pub struct Line { /* … */ }

impl Line {
    pub fn new(text: impl Into<String>) -> Self;
    /// Add a secret value: `Line::new("cloudflare.token: ").secret(token)`.
    pub fn secret(self, value: impl Into<String>) -> Self;
    /// Add plain text after a secret value.
    pub fn text(self, text: impl Into<String>) -> Self;
    /// A line that is a secret value as a whole.
    pub fn sensitive(value: impl Into<String>) -> Self;
    /// Show the secret values to the command's user (creating a token,
    /// say): on a terminal only, since stderr anywhere else is a log.
    /// Still never logged.
    pub fn shown(self) -> Self;
}
```

And in cli-core, so the run record can honour it too (cli-core can't
depend on term-ui):

```rust
/// sb_cli_core::log: a secret value inside a message sb writes. Its
/// Display and Debug are `[REDACTED]`, so `anyhow!("got {}",
/// Secret::new(&token))` never holds the token, only the placeholder in
/// its place; reachable only through `expose()`.
pub struct Secret<T>(/* private */);

/// sb_cli_core::log: an error whose text must not be shown or logged at
/// all, for errors sb doesn't write (a library's); for its own messages
/// sb marks just the value with `Secret`.
/// It says nothing of what it holds: its Display and Debug are the
/// placeholder and it has no cause, so wherever it sits in a chain (even
/// behind a transparent wrapper, or in anyhow's `{:?}`) it and everything
/// below it show as the placeholder on screen (decision 8; errors have no
/// `shown`) and are logged as withheld, by a runner step's record and by
/// `RunLog::finished` alike. Not `#[error(transparent)]`, which forwards
/// Display and causes to the secret (found in Task 7's review). A boxed
/// error, as cli-core takes any `std::error::Error`; reachable only
/// through `inner()`. It carries text, not control flow.
pub struct Sensitive(/* private: Box<dyn std::error::Error + Send + Sync> */);

impl Sensitive {
    pub fn new(err: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self;
    pub fn inner(&self) -> &(dyn std::error::Error + Send + Sync + 'static);
}
```

**Step endings** (for [`Runner::step`], [`Step::step`] and each sibling).
The first matching row decides, so a parent of an interrupted step reads
"interrupted", not "sub-step failed". Every row logs the step's start,
outcome and elapsed time (criterion 8); a cancelled body's error is
logged with the outcome, never shown.

| The body | The step reads | Shown | Returns |
|---|---|---|---|
| would start after the token was cancelled | not drawn; the body doesn't run | nothing | `Exit(1)` |
| returns `Ok` (even after a cancel) | ✓ done label | | `Ok` |
| panics, or is dropped while panicking | ✗ failure label in the tree; the screen keeps its last frame (indicatif draws nothing while a panic unwinds). A group sibling's panic stops all drawing for the rest of the run: in a `group_all` the screen stays as it was under the panic message until the other siblings finish, and the finished tree isn't printed | nothing (the panic message) | (the panic goes on) |
| is dropped unfinished (`select!`, a timeout, an unjoined `Group`) | "stopped" (its subtree too) | nothing | (none) |
| returns `Err` after Ctrl-C (`Shutdown::interrupted`) | "interrupted", its last lines kept; parents too | nothing (cli-core shows "interrupted" once) | `Exit(1)` |
| returns `Err` after the command's own `Shutdown::request` | "stopped" | nothing (cli-core exits quietly with the requested code) | `Exit(1)` |
| returns `Err` after a sibling failed | "stopped" | nothing | `Exit(1)` |
| returns `Err` carrying a quiet exit (`quiet_exit` is `Some`: a sub-step's `Exit`, exec's child code later, a broken pipe) | "sub-step failed" (✗ for a broken pipe) | nothing more | the same `Exit(code)` |
| returns `Err` carrying a failed child (`ChildFailure`, exec's `Failed`, anywhere in the chain) | ✗ failure label, how the child ended before the time (`exit 3 · 2.0s`, `SIGTERM · 2.0s`) | as the next row, the child's last lines (capture's stderr) after the error | `Exit(1)` |
| returns `Err` | ✗ failure label | the error's every line, the last 10 output lines, the log pointer; logged with its causes | `Exit(1)` |

In use, `validate-config` (illustration):

```rust
let options = Options::default();
let mut run = Runner::start("Validating Saltbox config", &shutdown, log.as_ref(), options);
let accounts_step = Labels::new("Checking accounts.yml", "accounts.yml checked");
run.step(accounts_step, async |step| {
    for key in unknown_keys(&accounts) {
        step.warn(format!("unknown key: {key}"));
    }
    // Short and in memory; longer blocking work goes through
    // `spawn_blocking` with cloned data.
    check_accounts(&accounts)
})
.await?;
run.step(Labels::new("Checking credentials", "credentials valid"), async |step| {
    let mut group = step.group();
    // Each check takes its `Step` and its (cloned) settings by value.
    let cloudflare = Labels::new("Checking Cloudflare token", "Cloudflare token valid");
    group.spawn(cloudflare, |s| check_cloudflare(s, cf));
    let docker_hub = Labels::new("Checking Docker Hub login", "Docker Hub login valid");
    group.spawn(docker_hub, |s| check_docker_hub(s, hub));
    group.join().await?;
    Ok(())
})
.await?;
run.finish();
```

Choices behind it:

- **The body's result decides the step's ending** (the table), not calls
  on `Step`: there is no way to end a step twice or forget to end it, and
  a `&mut Step` can't outlive it.
- **`Exit` as the error** is cli-core's quiet exit, so a shown failure is
  never repeated, a sub-step's or a child's code passes up unchanged, and
  cli-core needs no new error type.
- **Async closures** (`AsyncFnOnce`, stable since Rust 1.85) for
  sequential steps borrow freely; concurrent siblings are tokio tasks and
  own their data.
- **Runtime: tokio's current-thread runtime** (the stack table's minimal
  features; approved at Checkpoint A). Bodies do their blocking work
  through `spawn_blocking`, or the spinner stops while they run; config
  and docker reads are async or short. The runner starts no runtime and no processes.
- **Sensitive text is marked where it is made** (`Line::secret`, `Secret`,
  `Sensitive` in an error chain), so logging can't forget a case.
- **A failed child** is term-ui's `ChildFailure` (exec re-exports it as
  `Failed`; the maintainer, exec Checkpoint A, 2026-11-04), so term-ui
  shows how it ended without depending on exec (a `#[error(transparent)]`
  wrapper hides it, as it hides a quiet exit). `runner_live()` says
  whether a `Runner` is live, for exec's `attached`.
- **Not here yet:** prompts inside a step (decision 1), progress counts.
  Each can be added without changing these signatures.

## Testing strategy

- Rendering tests at a fixed width with colour on and off: tree glyphs,
  the one time column, notices, the failure block, table layout at 120, 70
  and 50 columns, the rule between rows. References are insta snapshots
  taken from the reviewed prototypes' output, drawn on indicatif's
  in-memory terminal (`Target::InMemory`, term-ui's `testing` feature);
  PTY tests measure erases and endings.
- Guard tests per profile: every SGR colour code term-ui writes is 24-bit
  under `truecolor`, 256-colour under `ansi256`, 16-colour under `ansi`,
  none under `ascii` or with colour off; under `ascii` also with a
  child's own SGR output fed through `Step::output`, live and plain.
- PTY tests (`script`, as cli-core's colour tests): live frames, no line
  erases, clean endings for success, failure and SIGINT, `ECHOCTL` and
  `ECHO` restored, and keys typed mid-run not echoed and not left for
  the shell after an ordinary ending or the forced exit.
- `DisplayText`: a test per class of "Escape handling", 7-bit and 8-bit
  (SGR kept byte-exact or dropped by `colour`; OSC, DCS, APC, PM and SOS
  with their payloads; cursor and mode CSIs; the parameter limit;
  `ESC[?1m`; C0, DEL and C1; CR; lossy UTF-8), and an exhaustive check of
  every short input over an alphabet of sequence bytes: no control
  character but a plain SGR's ESC gets through. A parse resumed at any
  `Resume` makes the rest of the text, on crafted lines and every short
  input over such an alphabet. The run log drops APC, PM
  and SOS payloads.
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
   plain styles), a child's own included; under the other profiles a
   child's own codes pass through unchanged.
3. The runner matches the reviewed tree (snapshot), with running and done
   labels, notices, and times in one column; each running step's tail at
   most 8 lines, all running tails together within half the terminal
   height; nesting and concurrent steps as decisions 2, 3 and 7 decide (a
   failing sibling stops the others, marked "stopped", and its parents
   read "sub-step failed"); 0 line erases while running (PTY
   measurement).
4. A failed step shows ✗, its error in full, its last 10 lines and the log
   pointer, once; sb exits 1 without cli-core repeating the error.
5. With stderr not a live terminal (piped, CI, `TERM=dumb`), the runner
   prints start lines, output lines, notices and end lines; not on a
   terminal with `auto`, no escape codes.
6. Ctrl-C marks every running step and its parents interrupted in the
   tree, cli-core shows "interrupted" and the pointer once, the cursor and
   the echoes (`ECHOCTL`, `ECHO`) are restored, and sb ends by the signal
   (`$?` 130, SIGTERM 143; exec decision 7).
7. A table matches the reviewed `docker ps` look (snapshot) and fits 120,
   70 and 50 columns; not on a terminal it keeps its natural width.
8. Every runner step's start, outcome, elapsed time, notices and output
   lines are in the run log, and a known secret marked sensitive is not.
9. Help and version stay within the startup budget (term-ui does nothing
   for them); the binary growth has its ADR.
10. (With their first user) a prompt with no terminal and no answer fails
    with a message naming the flag; a given answer is used without asking.

## Decided (maintainer, 2026-10-30; amendments and decisions 7-9 on 2026-11-01)

1. **Prompts** are specified here and built with their first user
   (self-update or git's confirm).
2. **Nesting and concurrency against no-blink:** proto-runner reached 0
   line erases by keeping the number of live bars constant, and indicatif
   warns against its cursor-moving redraw when bars come and go. The
   plan's first task is a spike measuring erases with a fixed pool of bars
   sized for the deepest nesting and widest concurrency used, shown to the
   maintainer with the look of notices, before the runner's API is fixed.
   **Amended at Checkpoint A (maintainer, 2026-11-01), from the spike
   (docs/prototypes/proto-runner.md):** one indicatif bar holds the
   whole live area (no pool to size; equally flicker-free), and the area's
   line count never shrinks while running (indicatif drops empty lines and
   its cursor-moving redraw can't clear lines that fall off the bottom).
3. **The last branch:** the newest finished step stays a live line until
   the next step starts or the run ends, then is printed with the right
   glyph (a line in the live area, per amended decision 2). With
   concurrent siblings the tree keeps start order, so "newest" means the
   last started. **Amended at the config plan's Checkpoint A
   (maintainer, 2026-11-03):** the tree is drawn in tree order at any
   depth (each parent's children in start order, each subtree whole), so
   a step started late inside a nested sibling is drawn in its own
   subtree, not after a later outer sibling. *Amended 2026-11-18
   (decision 14):* every finished step stays live until the run ends,
   so each step's place is known when the tree is printed.
4. **Steps are async** and get cli-core's cancellation token; the runner
   draws on indicatif's own ticker (tokio, capability map decision 8).
   **Amended at Checkpoint A:** a tokio task drives the frames (several
   spinners share one live area, and indicatif's `{spinner}` is one per
   bar); indicatif draws them. The runtime is tokio's current-thread one,
   with blocking work in steps through `spawn_blocking`.
5. **Double Ctrl-C restores the terminal, then exits.** A second signal
   still exits without waiting for cleanup, but the terminal is put back
   first, as well-behaved CLIs do: cli-core handles the second signal on
   its signals thread (code in the handler itself would need signal-hook's
   unsafe registration, which the workspace forbids), runs restore hooks
   that term-ui registers (show the cursor, restore the echoes), then exits
   130 or 143. Ctrl-\ and `kill -9` stay as hard escapes. Recorded in the
   cli-core spec too. Amended by exec decision 7 (maintainer, 2026-11-04):
   the exit is a re-raise of the first signal, so sb dies of it (`$?`
   still 130 or 143). Amended (maintainer, 2026-11-05): Ctrl-\ runs the
   same restore hooks, within the same deadline, before sb ends by SIGQUIT
   (no core), as cli-core now catches SIGQUIT anyway; only `kill -9` ends
   sb without them.
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
8. **Sensitive text (maintainer, 2026-11-01, Task 6):** never shown,
   unless the command's job is to show it to its user (editing a config,
   say), and never logged either way. Wherever stderr goes (a terminal, a
   file, the journal, CI), sensitive text shows as a placeholder unless the
   command opts in (`Line::new(..).secret(..).shown()`), and even then only
   on a terminal: stderr anywhere else is a log. **Amended at Checkpoint B
   (maintainer, 2026-11-01):** only the secret value is replaced, in place,
   by `[REDACTED]` (Rust's secrecy crate's form; compared with Ansible's
   `********`, GitHub Actions' `***` and Terraform's `(sensitive value)`),
   on screen and in the log, so the rest of the line or error stays
   readable: `Line::secret` for lines, `Secret` inside error messages sb
   writes; `Sensitive` still hides a whole error sb doesn't write.
9. **No verbose option (maintainer, 2026-11-01, Checkpoint B):** the run
   log records every step's start, notices, output and outcome, so plain
   output on a terminal would only repeat it; `Options::verbose` is gone.
   Plain output stays for stderr that isn't a live terminal. This is sb's
   own output only: Ansible's verbosity is still passed through for
   installs (the exec and install modules' concern, not the runner's).
   With no flag, the log is verbose by default (capability map, decision
   11): the runner logs every step's start, notices, output lines,
   outcome, time and error.
10. **A group that lets every sibling finish (config spec, decision 3;
    approved by the maintainer at the config plan's Checkpoint A,
    2026-11-03):**
    `Step::group_all` gives a `GroupAll`, beside decision 7's stopping
    group, which is unchanged. A failing or panicking sibling cancels no
    other; every sibling's ending is drawn and its result returned in
    spawn order; a panic resumes once all have returned. Ctrl-C and the
    command's own request still end every running sibling (the endings
    table). `sb validate-config` checks every file this way.
11. **The time column fits the tree (maintainer, 2026-11-04, the config
    plan's Checkpoint B):** a label is never cut while the screen has
    room. The time column starts at 64 (proto-runner's reviewed layout;
    66 since decision 13)
    and moves right as far as the longest line not yet printed needs, an
    ending's reason (`interrupted · 12s`) included, with at least three
    spaces before the time so a reason never reads as part of the label
    (the maintainer, 2026-11-04, trying the first version; fewer on a
    screen too narrow for three, and a screen too narrow even for a step's
    branch and time cuts the whole line, so no line wraps). It never moves
    left, so the live area never sits left of printed lines and a shorter
    done label doesn't make it jump (lines printed before a longer one
    keep their column); a screen narrower than the tree still cuts labels
    with `…`. Custom layout, not a library's: tabled lays out a whole
    table at once and indicatif's template widths are fixed, so neither
    keeps one column that only moves right across printed and live lines.
    A fixed column cut long labels, such as an interrupted credential
    check's ("Validating Cloudflare API credent…"). Plain end lines, which
    have no column, put the same three spaces before the time and reason
    (`✗ Pulling   interrupted · 2s`; the maintainer, 2026-11-04).
    *Amended 2026-11-18 by decision 14, which supersedes "lines printed
    before a longer one keep their column":* the tree is printed for
    good only when its run ends, so the column is the whole tree's and
    no line of it ends left of another's. The rest stands: no label is
    cut while the screen has room, never left of 66, never past the
    screen, never moving left in the live area.
12. **Transient steps (the maintainer, 2026-11-14):** `Labels::transient()`
    marks a step shown only while it runs, for work that takes a while
    but whose result is the command's output (`sb list`'s steps, its
    cache rebuilds and `-m`'s `saltbox_mod` listing: "show listing
    saltbox_mod but it should not be in the output once we print").
    **A run with transient steps holds each finished step until it
    ends:** once a transient step has started, nothing more is printed
    for good; the live area keeps drawing every step meanwhile. At the
    end, if every step succeeded, the transient steps and everything
    under them leave the tree, and a run of transient steps leaves
    nothing on the screen, its title included. If any step failed, was
    interrupted or was stopped, the whole tree is printed as a run
    without transient steps prints it, the steps that succeeded
    included (the maintainer's choice, 2026-11-14, over keeping only the
    failed step). A failure the command caught counts too: any step
    that ended other than done keeps the tree. The run's title is held back until the first line
    printed: live, it heads the live area meanwhile (so the bar has
    drawn before the title is printed above it); a run with no step
    still prints its title. Plain (stderr not a terminal) buffers
    likewise: from the first transient step on, every plain line waits
    for the run's end; a run whose steps all succeeded then prints only
    the lines of the steps that aren't transient (nor under one), a
    failed one prints every line, in order (a panic counts as a
    failure). The run log is unchanged: every
    step, transient or not, is logged as it runs. Why not indicatif's
    `finish_and_clear` on a bar of its own: the live area is one bar
    already (decision 2), and a failure must show as the runner's
    failure block does.
13. **Step times show tenths under a minute (the maintainer,
    2026-11-15):** a step's time, live and finished, in the tree and on
    plain end lines, reads in tenths of a second under a minute
    (`0.3s`, `4.7s`, `59.9s`), cut rather than rounded so it never reads
    `60.0s`; from a minute on, whole units as sb-go's `FormatDuration`
    (`1m 2s`, `1h 3m`, `1d 2h`, a zero minor unit left out). The running
    spinner's time follows the same rule. The time column's least end
    moves from 64 to 66, so `59.9s` leaves a label the room `59s` did.
    **Difference from sb-go:** sb-go's runner shows no step times at
    all (`terminal/runner.go` draws the mark and the message); the time
    column and its tenths are sb-rust's own, and only the form from a
    minute on is sb-go's `FormatDuration` (which it uses for systemd's
    and the motd's times, and which cuts rather than rounds). Before
    this, sb-rust's tree showed whole seconds, past a minute too
    (`125s`). The snapshot tests'
    in-memory screen has a clock that stands still (`Clock::Frozen`), so
    their times read `0.0s` on any machine.
14. **One tree per command, printed once, at its run's end (the
    maintainer, 2026-11-18: the tree is never printed
    early):** that run drew three trees,
    each titled `sb update` (the self-update check with its two notices
    after it, the main steps, validation and completions), and its time
    column jumped from 66 to 71 mid-tree: a late long label ("Ansible
    virtual environment was already up to date") came after the apt
    step had been printed for good (`take_printable` and `column`).
    - **One runner per command run, its tree printed once, at the end,**
      every line ending in one column: the widest line's need over the
      whole run, at least 66, never past the screen (decision 11, as
      amended). Until then every finished step stays in the live area,
      collapsing to `… N finished` when the tree is taller than the
      screen, as before. Ctrl-C, a signal and a failure print it at the
      end as before (decision 7's endings, the failure block, the
      pointer).
    - **Pausing.** While something else needs the terminal, the command
      pauses the runner; nothing is printed then. Its next
      `Runner::step` starts drawing again, and the title is printed
      once, with the tree.
      - `Runner::pause_over`, for what opens on the alternate screen (the
        announcement viewer, a question asked with `confirm_over`): the
        live area stays as drawn under it, the frames stop, the
        terminal's modes are put back, and the runner no longer counts
        as live (`runner_live`: the prompts and exec's `attached` refuse
        while one is); the next step redraws the same area in place.
      - `Runner::pause_below`, for what writes on the normal screen (an
        attached child, such as `update`'s migration playbooks, and the
        lines around it): the live area is cleared as well, and the next
        step (or the run's end) draws it again below what was written.
      - Custom, not indicatif's `MultiProgress::suspend`: that takes a
        synchronous closure and holds the area's lock throughout ("avoid
        long-running operations"), and a pause spans awaited work; nor
        a hidden draw target swapped in, which would lose the in-place
        redraw's line count.
    - **The end's print is one `MultiProgress::println`** of the whole
      tree (it splits the lines itself), so the area is redrawn once,
      not once per line. What it prints, the plain lines and tables are
      recorded in cli-core's `tail` for the blank line at the end of the
      run (cli-core spec, "The blank line at the end"), and a question
      answered on the normal screen counts as a line there.
    - **`Runner::info` and `Runner::warn`:** a notice under the run's
      newest top-level step, finished or not, for what a command decides
      between steps (a question's answer, the migrations run or skipped),
      kept in the tree like a step's own notices and logged the same way.
    - **Plain** (stderr not a terminal) **is already line by line and
      stays so** (checked 2026-11-18: `State::plain` prints each start,
      output, notice and end line as it comes, except while a transient
      step holds them); a pause prints nothing there, and the title is
      printed once for the run.
    - **The run log is unchanged:** each step is recorded as it runs.
    - **The commands** (every `Runner::start`, swept 2026-11-18):
      `update` had three runners and has one (paused over the branch
      questions, the announcement viewer and the migrations question,
      below for the migrations' playbooks); `self-update` had two (the
      check, then the install after its question) and has one, paused
      over its question. Already one tree at most per run, unchanged:
      `bench` (a transient runner per section, between its tables: a
      successful run draws no tree, and a failed section ends the run
      with its own), `install` and `list` (one runner started at its
      first step), `setup` (its branch question comes before its
      runner), `gha`, `branch`, `branch-sandbox`, `validate-config`,
      `announcements` and `announcements preview`, the python-env
      commands, and `docker start`, `stop` and `restart`. `edit` keeps a
      runner per check of its edit-again loop: each check's tree is what
      its question asks about, so it is printed before the question, and
      the editor takes the whole screen in between.
15. **A live block for a command's own report (the maintainer,
    2026-11-22, from the bench-live research, after "research
    alternatives to our tree task, since it doesn't really fit well
    with bench specifically"):** `sb bench` shows its ruled report as
    results arrive, drawn by the runner (prototype A', the "option 4"
    policy: each section printed when it ends, only the one in progress
    live; ops decision 47). The runner gains a live-block mode beside
    the tree's ("Live block" above); the tree's own drawing, on
    indicatif, is unchanged. Why console_static_text and not the tree's
    indicatif area: indicatif pads each live line to the width and
    moves up by its line count, so on a terminal that rewraps, a
    narrowing doubles its rows and leaves copies (measured in tmux
    3.2a), and it stops drawing at the screen's height, cutting the
    bottom, where the row being measured is. Why not ratatui's inline
    viewport (0.30.2): it clears the whole screen on a narrowing (tmux
    kept four copies of the report), its height is fixed, and crossterm
    writes its cursor query on stdout, into a pipe. Why not a
    hand-written area (prototype A): console_static_text does the same
    (no padding, the last frame re-wrapped at a new width to count its
    rows, cut from the top to the height, silent when nothing changed)
    and is maintained and shipped by Deno. Decided: the
    120 ms pace and the resize debounce (the crate's example and Deno);
    `Runner::suspend` for the hand-over to stdout; the endings above (an
    interrupted run keeps the frame and prints no tree, as the
    prototype; a failed one clears it, since its spinner would say the
    measurement goes on, and prints the tree, so its error and pointer
    show); the measured limit on terminals that cut lines.
