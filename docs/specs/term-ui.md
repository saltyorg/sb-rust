# Spec: `term-ui` (approved 2026-10-30; Checkpoint A 2026-11-01)

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
- **Stderr not a terminal** (`TERM=dumb`, CI, redirection): no live
  parts; there is no verbose option, the run log has the detail
  (decision 9). The runner draws on stderr, so stderr
  decides (indicatif's target; sb-go checks stdout, so `sb validate-config
  > file` stays live here and goes plain in sb-go). Each step prints a
  start line (`- <running label>`), its output lines (escape codes
  stripped when colour is off), notices, and an end line (`✓ <done label>
  2s`, or ✗ with the error and the pointer), flat as in sb-go's plain mode
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
  cursor is hidden while live. The tty is only touched in the foreground
  process group (sb-go's `tty.go` guard), checked again at every restore,
  so a background sb (or one moved there with Ctrl-Z and `bg`) never stops
  on SIGTTOU; a job-control shell puts its own modes back then. A second
  Ctrl-C restores the terminal through a hook cli-core runs before its
  immediate exit (decision 5), which also ends the live area's line.
  Known limit: Ctrl-Z itself (SIGTSTP) leaves the cursor hidden at the
  shell's prompt until sb resumes (bash restores the modes, not the
  cursor).
- **Resize:** known limit from proto-runner: the live area's line count
  never shrinks while running (decision 2), so shrinking the terminal
  mid-run can leave stale lines.
- **Run log:** every step's start, outcome and elapsed time, its notices,
  and its output lines go to the run log (capability map, decision 11),
  in the run's span (carried onto other threads for concurrent steps).
  Secret values in output, notices or error text (credentials, tokens)
  are never logged: the rest of the line is, with `[REDACTED]` in their
  place; prompt answers never are. Secret values are withheld on screen
  too, the same way, unless the command's job is to show them to its
  user, such as editing a config, and then only on a terminal (decision
  8).

### Tables

- **Look (proto-inline, reviewed):** tabled's rounded style with a rule
  between rows, bold centred headings, the border in the `Border` role,
  per-cell roles from the caller, right-aligned columns where asked.
- **Width:** fitted to the terminal (console's `Term::size_checked` on
  stdout); the widest column wraps first; a maximum column width where the
  caller asks (sb-go's `docker ps` caps at 100). Not a terminal: natural
  width.
- **Piped:** the same table without colour, at its natural width (sb-go's
  piped table wraps at 80 columns and keeps its bold and border codes;
  docker spec, differences). Plain or JSON output is decided per command
  (capability map, open items). Column spans (`sb list`) come with that
  command.

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
| Step errors (runner API, approved 2026-11-01) | `anyhow` (already the binary's) | cli-core's binary |
| Step records in the run's span (approved 2026-11-01) | `tracing` with `Instrument` (already cli-core's) | cli-core |
| In-memory screen for snapshots (approved 2026-11-01; dev only, term-ui's `testing` feature) | indicatif's `in_memory` feature (pulls `vt100`) | indicatif docs |

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
    /// if the terminal's modes can't be read, `ECHOCTL` is left alone.
    /// Registers a restore hook with `shutdown` for a second Ctrl-C (it
    /// first stops the frame task drawing, then writes through its own
    /// tty fd and puts `ECHOCTL` back), removed when the runner ends.
    /// `log` gives the failure block's pointer; step records go through
    /// `tracing` in the current span, which the runner captures here and
    /// carries onto concurrent steps' tasks (`Instrument`).
    pub async fn start(
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
    /// output); escape codes are kept on a coloured target and stripped
    /// otherwise; a line wider than the screen is cut with `…` on screen
    /// and logged whole.
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
    /// first. An aborted sibling's `spawn_blocking` work runs on, detached
    /// (an accepted limit).
    pub async fn join(self) -> Result<Vec<T>, Exit>;
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
| panics, or is dropped while panicking | ✗ failure label in the tree; the screen keeps its last frame (indicatif draws nothing while a panic unwinds) | nothing (the panic message) | (the panic goes on) |
| is dropped unfinished (`select!`, a timeout, an unjoined `Group`) | "stopped" (its subtree too) | nothing | (none) |
| returns `Err` after Ctrl-C (`Shutdown::interrupted`) | "interrupted", its last lines kept; parents too | nothing (cli-core shows "interrupted" once) | `Exit(1)` |
| returns `Err` after the command's own `Shutdown::request` | "stopped" | nothing (cli-core exits quietly with the requested code) | `Exit(1)` |
| returns `Err` after a sibling failed | "stopped" | nothing | `Exit(1)` |
| returns `Err` carrying a quiet exit (`quiet_exit` is `Some`: a sub-step's `Exit`, exec's child code later, a broken pipe) | "sub-step failed" (✗ for a broken pipe) | nothing more | the same `Exit(code)` |
| returns `Err` | ✗ failure label | the error's every line, the last 10 output lines, the log pointer; logged with its causes | `Exit(1)` |

In use, `validate-config` (illustration):

```rust
let options = Options::default();
let mut run = Runner::start("Validating Saltbox config", &shutdown, log.as_ref(), options).await;
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
- **Not here yet:** a child process's output and exit code in the times
  column (exec), prompts inside a step (decision 1), progress counts.
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
5. With stderr not a live terminal (piped, CI, `TERM=dumb`), the runner
   prints start lines, output lines, notices and end lines; not on a
   terminal with `auto`, no escape codes.
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
   bar); indicatif draws them. The runtime is tokio's current-thread one,
   with blocking work in steps through `spawn_blocking`.
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
