# Spec: `exec` (approved 2026-11-04)

Module of the approved capability map (`exec`): child processes for every
later module (host's checks, apt and systemd, repo's git, python-env's uv,
ansible's playbooks, docker's `systemctl` check). The first vertical slice
(`cli-core` → `term-ui` → `docker` + `config`) is done; the build order
puts `exec` next. This spec covers the module's shape and its tests; which
command first uses it is decision 4.

## Assumptions

1. **Children are started by library calls only:** `tokio::process` for
   piped and attached children (the runtime is tokio, capability map
   decision 8; the workspace's tokio gains its `process` feature),
   portable-pty 0.9.0 for children under a pseudo-terminal (stack table;
   proto-runner and proto-musl used it), and rustix for process-group
   signals (already in the tree with its `process` feature). No shell is
   involved unless a caller runs one by name.
2. **proto-runner's findings are the design**
   (docs/prototypes/proto-runner.md): portable-pty starts the child in its own session
   (`setsid`, so its process group id is its pid) and Ctrl-C reaches only
   sb; cancelling sends SIGTERM to the child's process group and, after a
   grace period, SIGKILL to the group, not portable-pty's `Child::kill`
   (SIGHUP, then SIGKILL, to the one pid); a killed child is reported as
   killed, not by an exit code; output is split on bytes before decoding.
3. **term-ui shows the output.** `stream` and `pty` hand lines to a
   term-ui `Step` (`Step::output`), which splits on `\r`, keeps colour only
   on a coloured target, drops other control characters, masks marked
   secrets and logs every line (term-ui spec, decision 9); its failure
   block shows a failed step's last 10 lines. exec draws nothing itself.
   exec therefore depends on sb-term-ui: an amendment to the capability
   map's `exec` row (cli-core, term-ui), and to deny.toml's wrappers; the
   row's "ANSI stripping when not a terminal" moves to term-ui, which does
   it.
4. **sb-go's executor is the behaviour to keep where it matters**
   (executor/executor.go and its callers): `GIT_TERMINAL_PROMPT=0` for git
   not attached to the terminal; custom stdin refused under a
   pseudo-terminal; timeouts on most calls (the 5 s `systemctl` check in
   cmd/docker.go, 2 min per apt attempt in host/apt.go); a replaced
   environment (completion/detection.go); stderr matched for retries
   (host/apt.go). sb-go's five output modes map onto four ways to run.
5. **Carried items addressed to exec** (tasks/config/todo.md):
   `SB_RELAUNCHED` in children (moot: cli-core relaunches with `sudo -u
   root` and drops the variable, decision 6); `COLORTERM` for children
   (decision 3); the sudoers question on `--preserve-env` names beyond
   `env_keep`, with sudo's `use_pty` default and how it relays signals,
   recorded in docs/platform-notes/ (cli-core's relaunch owns the
   question; exec's plan answers it because attached children and signals
   depend on it).

## Objective

One place where sb starts child processes, so every module gets the same
cancellation, timeouts, logging, secret handling and errors, and none
re-implements them. Ctrl-C, SIGTERM or the command's own request stops
every child sb started, with the descendants that stay in its process
group, within the grace period plus one second.

### A command

`Cmd::new(program)` (named apart from std's and tokio's `Command`) with:

- **Arguments, shown unless a credential:** `arg(&'static str)` for
  literals written in sb's code; `shown_value(String)` for every value sb
  computes or passes on (paths, versions, file and unit names, branches,
  sb's own URLs, a user's ansible arguments), which the log and errors
  show as given;
  `secret(String)` for credentials (tokens, passwords, API keys),
  `[REDACTED]` in the log and also taken out of the child's output and
  of errors. There is no third kind: the method a caller picks says
  which it is (decision 14). The run log shows the program, the
  arguments as shown and the count of masked ones (the secrets).
  argv is readable in `/proc/<pid>/cmdline` by other users, so a caller
  passes a secret through the environment or stdin where the tool allows.
- **A joined option** (`--opt=<value>`, for a tool that refuses a value
  starting with `-` as a separate argument) is one `shown_value` the
  caller builds (`format!("--extra-vars={value}")`), or one `secret`
  when it carries a credential. `Cmd::shown()` is public: the rendering
  the log and errors use, secrets masked, for a caller that prints the
  command (ansible's `Executing...` line). *Amended 2026-11-13 by the
  ansible spec; `option_value` removed 2026-11-16 (decision 14).*
- **A shown value:** `shown_value(String)`: the rendering has the value
  in place, and it isn't counted as masked. ansible's playbook arguments
  came first (its decision 17: the maintainer, 2026-11-14: nothing is
  masked, and a user keeps a value out of the logs with an
  @file); since decision 14 it is the default for every value sb
  computes. *Amended 2026-11-14 by the ansible spec, and 2026-11-15.*
- **Environment:** inherited; `env(name, value)`,
  `secret_env(name, value)` (taken out of output too), `env_remove(name)`,
  or `env_clear()` (sb-go's `WithEnv`). The log names changed variables,
  never values.
- **Working directory:** `cwd(path)`; sb's own when not given. It must be
  an existing directory before spawning (portable-pty would silently fall
  back to `$HOME`), else a spawn error.
- **Stdin:** null unless `stdin(bytes)` gives bytes, written from their
  own task and then closed (a child that exits early is fine). Never the
  terminal for piped or PTY children: a child in its own group reading
  the terminal would stop on SIGTTIN.
- **Timeout:** `timeout(d)`, through the same SIGTERM, grace, SIGKILL path
  as cancellation, ending as timed out; `grace(d)` to change the grace
  period for one command.

### Ways to run

| Way | sb-go mode | Output | Typical use |
|---|---|---|---|
| `capture(&token)` | Capture, Combined, Discard | stdout and stderr collected separately, up to 1 MiB each (cli-core's cap for text sb reads), returned | `systemctl is-active`, `git rev-parse`, `uv --version`, `dpkg-query` |
| `stream(&mut step)` | Stream, managed output | each line to the step as it arrives (stdout and stderr); stderr's last 64 KiB kept for the caller (apt's retry match) | `apt-get update`, `git pull` under the runner |
| `pty(&mut step)` | `WithPseudoTerminal` | under a pseudo-terminal of sb's terminal size (24×120 when stderr isn't one, as sb-go), lines to the step; the child sees a TTY; stdin refused; echo off, and the end-of-file character written at start so a reading child's first read is end of file (a child that flushes its input before prompting, as a password prompt does, still waits: a PTY child must not prompt, so it gets a non-interactive flag or a timeout) | tools that colour or show progress only on a TTY |
| `attached(&shutdown)` | Interactive | sb's own stdin, stdout and stderr; nothing collected, output not logged; `stdin(bytes)` refused before spawning (decision 11); cli-core's hold on SIGINT and SIGQUIT while it runs (decision 2; the `Shutdown`, decision 13); not started in a run already interrupted, looked at before the hold and again once it has started, as a signal delivered just before the hold is decided then (exec-L6, 2026-11-18) | an editor, an `ansible-playbook` run shown as it is |

`stream` and `pty` take the step's token (term-ui's `Step::token`, a
child of cli-core's shared token); `capture` takes one; `attached` takes
cli-core's `Shutdown`, its token and its hold (decision 13).

- **Lines:** split at `\n` from bytes, each read bounded so memory is:
  tokio's `AsyncBufReadExt::read_until` through `AsyncReadExt::take(4 KiB
  + the longest secret + 1)` for pipes (the extra so a secret across the
  cut is found whole), std's `BufRead::read_until` through `Read::take` on
  the PTY's reader thread; a line longer than 4 KiB is cut there, at a
  character's start, and the rest up to its `\n` dropped; then each line
  is handed to the step, which keeps what follows the last `\r`. term-ui has no "replace the last line" call, so `\r` progress
  shows at the next `\n`; a final line without `\n` is still delivered.
  For `stream`, the final line without `\n` is delivered also when the
  2 s drain after exit ends the read (exec-L2). *Amended 2026-11-13 by
  the repo spec.*
- **Past a cap:** reading goes on (a full pipe would block the child);
  `capture` keeps the first 1 MiB of stdout and the last 1 MiB of stderr
  (its tail holds the error) and marks the result truncated.
- **After the child exits:** output is read to its end for at most 2 s
  (a background grandchild can hold the pipe or the PTY open, as sb-go's
  `WaitDelay` handles), then reading stops; on the PTY, EIO is the end.
- **Secrets in output:** every `secret` argument and `secret_env` value
  (empty ones ignored; one with newlines masked whole and line by line,
  lines under 4 bytes not on their own, so a credential read from a file
  is found without its newline and a JSON value's braces don't mask every
  brace) is taken out
  of captured text, streamed lines (with exec's mask before the step gets
  them, so the screen and the log read the same) and errors before
  anything is shown or logged. After a stop, output is still read for up
  to the 2 s, as after any exit.
  `attached` output goes straight to the terminal and can't be masked.

### Outcome and errors

Every way returns an `Outcome`: exited with a code, killed by a signal
(the number for piped children; for `pty`, portable-pty's own wait gives only the
signal's name and a code of 1, so sb waits on the PTY child itself with
rustix's `waitid` and keeps the number), interrupted (the token
was cancelled), or timed out; and the time it took. Once the token is
cancelled or the timeout passes, the outcome is interrupted or timed out
even if the child then exits 0 within the grace period.

- `check()` turns anything but exit 0 into an `exec::Failed` error naming
  the program (arguments masked as in the log) and the outcome. The
  results are `#[must_use]` (Task 9), so an outcome can't be dropped
  unread; a caller that checks and needs nothing more discards it
  explicitly: `let _ = cmd.stream(step).await?.check()?;`. For
  `capture` it carries stderr's last lines; for `stream` and `pty` it
  doesn't, since term-ui's failure block shows the step's last 10 lines.
- A spawn failure (program not found, not executable, no such directory)
  is its own error, naming the program, before any outcome.
- Exit codes for sb: an interrupted run ends as cli-core decides, by the
  signal itself (decision 7; shells see 130, 131 or 143), whatever a
  piped or PTY child did; for an attached child, as the attached rules
  below say. A step that fails with `exec::Failed`
  ends ✗ with its error and the step's last lines, and the times column
  shows `exit N` or the signal's name (term-ui spec, "Step endings");
  term-ui recognises the error for this, an amendment to its endings
  (decision 5). The command then exits 1, or with the child's own code
  where it asks to pass it through (decision 5). A quiet exit
  (`Exit`) isn't used for a failed child: term-ui shows no block for one.

### Cancellation, timeouts and process groups

- **Piped and PTY children:** each in its own process group, whose id is
  the child's pid (`process_group(0)`; portable-pty's `setsid`). On
  cancellation or timeout: SIGTERM and SIGCONT (a stopped process acts on
  SIGTERM only once continued) to that group, then SIGKILL to the group
  after the grace period while any member is left. A leader that exits on
  SIGTERM is reaped at once, and its group can still hold a grandchild
  that ignores SIGTERM (`sh` around a tool that traps it), so after the
  reap sb keeps checking the group (`test_kill_process_group`) and
  signals it while members remain. Linux keeps a process-group id from
  reuse while any member uses it (to be confirmed and recorded in
  docs/platform-notes/ with a test), so the id sb signals is always the
  one it created. sb waits for the child, so it leaves no zombie of its
  own.
- **Attached children** stay in sb's process group, so they can use the
  terminal (decision 2): the terminal's Ctrl-C reaches them directly; sb
  never signals sb's own group. While one runs, sb follows the shell
  convention "wait and cooperative exit" (M. Cracauer, "Proper handling of
  SIGINT/SIGQUIT"; bash; POSIX `system()`; git's editor launch): it takes
  no action on SIGINT or SIGQUIT, noting that one came, and waits for the
  child. If sb received SIGINT or SIGQUIT and the child died of that
  signal, sb records the interruption on cli-core's `Shutdown` and cancels
  the token, so later steps don't run and sb ends by the same signal
  (decision 7); if the child exited normally (it caught the signal for its
  own use, as ansible's `pause` prompt does), or sb received none (the
  child was signalled on its own), the run isn't interrupted: sb goes on,
  and a child that failed fails its step. Held signals don't count toward
  cli-core's forced exit. As under any shell, a child that ignores Ctrl-C
  keeps the terminal: Ctrl-\ (SIGQUIT, which the terminal sends to the
  child) or a SIGTERM to sb ends it. While an attached child runs, Ctrl-\
  is therefore no hard escape for sb itself (term-ui decision 5 keeps it
  one otherwise): sb ignores it until the child dies of it. (Editors take
  the terminal in raw mode, where Ctrl-C is a key, not a signal.) sb never
  forwards a SIGINT or SIGQUIT: the terminal delivers it to the child, as
  POSIX `system()` and git's editor launch rely on. Attached children
  assume sb's stdin and stdout are the user's terminal: piped runs of such
  commands aren't supported (the install script runs `sb setup` attached,
  capability map; users have no need to pipe them), so sudo's relays in a
  piped run aren't designed for (docs/platform-notes/exec-processes.md). A
  SIGTERM to sb, or a hangup (SIGHUP, handled as SIGTERM is: cli-core
  spec, "Cancellation"; not held, as the terminal's hangup reaches the
  child itself), is sent on to the child's pid as a SIGTERM (sudo-rs
  relays to sb's pid only; under C sudo the child may get it twice); then the grace period
  applies, and `timeout` works the same way, by pid. No attached child
  runs while a term-ui `Runner` is live: term-ui exposes whether one is,
  and `attached` refuses to start then. This changes how cli-core handles
  signals while an attached child runs (an amendment to the cli-core spec
  with this one).
- **sb's forced exit** (a second signal, outside an attached child; while
  one runs, a second SIGTERM, decision 12): before exiting, cli-core's
  hook sends SIGKILL to every group exec created that still has members,
  and to a running attached child (decision 12), through a pidfd opened
  at its spawn, so a reaped child's pid, if reused, is never reached.
- **Dropped futures:** dropping a running piped or PTY child sends SIGKILL
  to its group at once (tokio's `kill_on_drop` reaches only the pid, so
  exec keeps its own guard) and leaves the reaping to tokio's orphan
  handling or the PTY's wait thread. Dropping a running attached child
  sends SIGKILL to its pid (tokio's `kill_on_drop`; it has no group of
  its own), which can leave an editor's terminal in raw mode.
- **Reading after a stop:** the grace plus one second bounds the group;
  the output still coming after it is read for up to the 2 s drain, as
  after any exit (a process that left the group but holds the pipe adds
  those 2 s).
- **Known limits:** descendants that leave the group (`setsid`,
  daemonizing helpers, ssh's ControlPersist master) aren't reached; if sb
  itself is killed with SIGKILL (OOM, `kill -9`), its piped children keep
  running (stopping them needs `PR_SET_PDEATHSIG` from an unsafe
  `pre_exec`, which the workspace forbids). Why not make sb a child
  subreaper (rustix's safe `set_child_subreaper`) to adopt the helpers
  that leave the group: it would also adopt daemons a tool starts on
  purpose (ssh's ControlPersist master, a service), and killing those is
  wrong; tests use it, sb doesn't. An attached child is stopped by its
  pid alone (it stays in sb's group, which sb never signals): its own
  children can outlive a SIGKILL after the grace or the forced exit
  (under sudo with `use_pty`, sudo's closing PTY usually hangs them up).
  A Ctrl-C the child dies of while a timeout's stop is under way reads
  as timed out, not interrupted. A child that catches a held signal and
  exits normally before sb's own copy of it is delivered (extreme load:
  the child's handling and exit and sb's reap all run first) leaves that
  copy to be decided outside the hold: a Ctrl-C then cancels the run, a
  Ctrl-\ is the hard escape. A Ctrl-C between the hold's start and
  the child's `execve` (one spawn, well under a millisecond) is noted as
  held, and the child never sees it: it starts, and the user presses
  Ctrl-C again. A SIGTERM or SIGHUP isn't settled by the hold (only the
  held signals are), so one delivered just before it can let the child
  start and be stopped at once: accepted, as SIGTERM means stop.
  `attached` checks once, before the spawn, that no term-ui runner is
  live; nothing stops a runner starting
  while an attached child runs (commands don't). A root attached child is
  dumpable again after `execve` (the kernel resets sb's setting), so its
  core follows the host's own (as from a shell). One started in the 500 ms
  of Ctrl-\'s restore hooks, just before sb ends by it, isn't killed with
  sb: the hard escape doesn't cancel the token, so a command whose step
  ends in those 500 ms can still start one. Accepted: Ctrl-\ outside an
  attached child is the user abandoning sb. A tool that opens `/dev/tty`
  itself can still prompt there: the askpass rule below covers ssh, not
  every such tool. *Amended 2026-11-13 by the repo spec.*

### Environment rules

- `GIT_TERMINAL_PROMPT=0` for every child that isn't attached, git or not
  (sb-go sets it only for git; uv, ansible-galaxy or a script can run git
  too, and the variable means nothing to other programs).
- `COLORTERM` from the colour profile for `pty` and `attached` children
  only (truecolor, 256color; an inherited one removed for ansi and
  ascii), decision 3.
- `SSH_ASKPASS_REQUIRE=force` and `SSH_ASKPASS=/bin/false` for every
  child that isn't attached, so an ssh run by git (or by any child) asks
  no terminal and fails at once (exec-M1; ssh(1), ENVIRONMENT: with
  `force`, the askpass program is used for all passphrase input).
  `UNVERIFIED` for the host-key confirmation until the platform note's
  container check. *Amended 2026-11-13 by the repo spec.*

### Logging

A run-log record when a child starts (program, arguments as shown, the
count of masked ones, changed variable names, working directory, the way
it runs) and when it ends (outcome, elapsed). `stream` and `pty` lines are
logged by term-ui as the step's output, secrets taken out; `capture`
output is logged only if the caller shows it; `attached` output never
reaches the log. A caller may run a read with tracing silenced, and its
children's records then go unwritten: the log picker's 5 s service
refresh does (tui spec, "Verification notes": the refresh's run log;
maintainer, 2026-11-14).

### Differences from sb-go (the complete list)

- Cancellation and timeouts reach a piped or PTY child's process group,
  with SIGTERM first and SIGKILL after a grace period (sb-go: Go kills the pid at
  once, so grandchildren can outlive it).
- Ctrl-C during an attached child leaves it to the child (sb-go cancels
  its context, which kills the child).
- A PTY child gets sb's terminal size (sb-go: always 24×120).
- Credentials are masked in logs and errors, and taken out of output
  too; every other argument is shown as given, as sb-go shows it
  (sb-go: callers mask by hand, where they do).
- Combined and Discard aren't separate ways: `capture` returns both
  streams.
- Output is capped in memory (sb-go: unbounded); `stream` keeps stderr's
  last 64 KiB for the caller, everything streamed is in the run log.
- `GIT_TERMINAL_PROMPT=0` for every child not attached (sb-go: git only).
- `COLORTERM` for `pty` and `attached` children only (sb-go sets it
  process-wide from the colour profile, so every child gets it).
- After a child exits, output is read for at most 2 s (sb-go: until the
  pipe closes, except where a caller sets `WaitDelay`).
- ssh's prompts are sent to a failing askpass (sb-go: none). *Amended
  2026-11-13 by the repo spec.*

### Scope

- **Here:** the `Cmd` builder, the four ways, timeouts, cancellation with
  process groups, the environment rules, logging, errors, the cli-core
  amendment for attached children, the capability map's row, the sudoers
  platform note, tests with real children.
- **Not here:** forwarding a terminal resize to a running PTY child and a
  terminal-emulator crate for cursor-moving child output (capability map
  open items, for when a command needs them); each module's own use.

## Tech Stack

Rust 1.100.0 (edition 2024, the workspace pin); tokio with `process`
(added), portable-pty 0.9.0 (new; it brings nix 0.28 among others, so the
binary size and cargo-deny are measured when sb first links it, per the
ratchet), rustix (process groups and signals), tokio-util's
`CancellationToken` (cli-core's), sb-term-ui for `Step`. New crate
`sb-exec` (crates/exec), depending on sb-cli-core and sb-term-ui.

## Commands

```
cargo test -p sb-exec                 # unit and integration tests
scripts/check.sh fast | task | full   # the quality bar (CONSTRAINTS.md)
```

## Project Structure

```
crates/exec/
  src/lib.rs        → Cmd, Outcome, errors
  src/run.rs        → capture, stream, attached (tokio::process)
  src/pty.rs        → pty (portable-pty): size, reader thread, writer
  src/group.rs      → process groups: SIGTERM, SIGCONT, escalation, guard
  src/mask.rs       → secrets out of output and errors
  tests/            → real children (sh scripts, sleep, a grandchild);
                      signal tests through a test binary on a PTY
```

## Code Style

As the other crates: library calls cited with full URLs, `thiserror` for
the crate's errors, no `#[allow]`, no `unsafe`.

```rust
let outcome = Cmd::new("systemctl")
    .arg("is-active")
    .arg("saltbox_managed_docker_controller.service")
    .timeout(Duration::from_secs(5))
    .capture(&token)
    .await?;
```

## Testing Strategy

- **Real children, no mocks:** `sh -c` scripts and `sleep` stand in for
  tools. Exit codes, signals, spawn failures; stdin bytes, null stdin, a
  child that reads stdin under `pty` (end of file at its first read);
  output split
  across reads (a multi-byte character at a read boundary, `\r` progress,
  a last line without `\n`, a line past 4 KiB), the caps and truncation;
  a backgrounded `sleep 30` holding stdout (done within the 2 s drain).
- **Cancellation and timeouts:** a child that starts a grandchild and
  ignores SIGTERM: cancel, both gone within the grace plus a margin,
  SIGKILL sent; a child that exits on SIGTERM: no SIGKILL; a timeout:
  timed out, same path; a stopped child (SIGSTOP) still ends.
- **Groups after the leader:** a leader that exits on SIGTERM with a
  grandchild that ignores it: the grandchild gets SIGKILL after the grace.
- **Attached:** through a test binary (`crates/exec/src/bin/exec-demo.rs`,
  as term-ui's demo) on a PTY (`script`), as validate_config_signals.rs:
  Ctrl-C once and twice with a child that survives SIGINT (sb waits) and
  one that dies of it (sb dies of SIGINT); Ctrl-\ with a child that dies
  of SIGQUIT (no core dump) and with one that ignores SIGINT; a child
  signalled on its own (not an interruption: the step fails as the
  child's failure, exit 1); a SIGINT sent to sb alone (not passed on);
  SIGTERM forwarding.
- **PTY:** the child sees a TTY (`test -t 1`), its size (`stty size`,
  run on a terminal through `script`, or with an injected size), colours
  kept for a coloured target and stripped otherwise; a missing working
  directory is an error, not `$HOME`.
- **Environment and logging:** `GIT_TERMINAL_PROMPT=0`, changed variables
  present, no value logged; the known-secret test on a `secret` argument,
  a `secret_env` value, a child echoing them, and a `shown_value` that
  stays in the output and the error.
- **Rules that keep the harness safe:** in-process tests cancel through
  the token and never send real signals to the test process; real
  SIGINT and SIGTERM only through a test binary; before any group signal,
  a test asserts the group id is the child's pid; nothing calls
  `waitpid(-1)`; "gone" is checked with `test_kill_process_group`, with
  the test binary a child subreaper (`set_child_subreaper`) so it reaps
  orphaned grandchildren itself (a container without a reaping init would
  keep them as zombies); the
  grace period is short in tests, so a mutation run doesn't wait.
- Coverage and mutation as CONSTRAINTS.md.

## Boundaries

- **Always:** library calls for processes, groups and the PTY; every
  piped and PTY child killed with its group on cancel or timeout; secrets
  marked where they enter; every other argument shown (decision 14).
- **Ask first:** a dependency beyond those named; a fifth way to run;
  further changes to cli-core's cancellation or term-ui's `Step` API.
- **Never:** build a command line from strings for a shell; log a secret
  argument or an environment value; signal sb's own process group; leave
  a piped or PTY child running after sb exits on a signal it handles.

## Success Criteria

1. Each way to run works with real children: exit codes, signals, spawn
   failures, stdin, stdout and stderr as the table says, the caps and
   truncation, a failure's error as specified.
2. Ctrl-C, SIGTERM, the command's own request and a timeout stop a piped
   or PTY child and the descendants in its group within the grace period
   plus one second, SIGKILL only when SIGTERM didn't end them, also after
   the leader exited; a stopped child too; no zombie of sb's own left.
3. Attached: Ctrl-C and Ctrl-\ reach the child, and sb ends by the same
   signal only if it received one and the child died of it, repeated
   signals included; a child that ignores Ctrl-C ends on Ctrl-\ or a
   SIGTERM to sb; a SIGINT to sb alone isn't passed on; a SIGTERM or
   SIGHUP to sb reaches the child as a SIGTERM; `attached` refuses while
   a runner is live.
4. A PTY child sees a TTY of sb's size; its colours follow sb's colour
   decision; a byte-split character arrives whole; `\r` progress shows at
   the next line; a reading child gets EOF; a missing directory is an
   error.
5. Output after exit is read for at most 2 s.
6. No secret argument, `secret_env` value or other environment value in
   any log record or error, nor in output exec shows (`capture`, `stream`, `pty`; `attached`
   output is the child's own on the terminal), the known-secret test with
   a shown value beside the secrets.
7. `GIT_TERMINAL_PROMPT=0` for every child not attached.
8. The sudoers and `use_pty` question answered in docs/platform-notes/;
   the capability map's row, deny.toml and the cli-core spec amended.
9. The first command that uses exec (decision 4) works through the
   binary; binary size and startup measured, with an ADR past the
   tolerance.

## Decisions (maintainer, 2026-11-04)

1. **Grace period:** 5 s by default before SIGKILL, with a per-command
   `grace()` for longer critical sections (proto-runner used 3 s; sb-go
   has none).
2. **Attached children keep sb's terminal and process group:** the
   terminal's Ctrl-C goes to the child; sb holds its own reaction, waits,
   and ends interrupted (130) only if the child died of SIGINT; held
   SIGINTs don't count toward the forced exit. Revised after plan Task 1
   (maintainer, 2026-11-04): sb follows "wait and cooperative exit"
   (Cracauer; bash, POSIX `system()`, git): SIGINT and SIGQUIT are noted
   and ignored while the child runs, never forwarded; sb ends by the
   signal (decision 7) only if it received one and the child died of it.
   Only a SIGTERM to sb is forwarded to the child's pid. No escape key
   beyond the convention's: a child that ignores Ctrl-C is ended by Ctrl-\
   or a SIGTERM. Attached children assume sb is on the user's terminal:
   piped runs of such commands aren't supported. The cli-core spec is
   amended with this one.
3. **`COLORTERM`:** set from the colour profile only for `pty` and
   `attached` children (truecolor, 256color; an inherited one removed for
   ansi and ascii); `capture` and `stream` children's environment is left
   as it is.
4. **First user:** all four ways are built in this module with real-child
   tests; `capture` is proven through the binary by docker
   start/stop/restart (its `systemctl` check), next after this module;
   `stream` by host's apt, `pty` and `attached` by their first commands
   (apt progress, ansible) when those land. *Amended 2026-11-08 (maintainer;
   host spec, decision 7):* apt runs without a terminal, so `pty`'s first user
   is the first module whose child needs one.
5. **A failed child:** term-ui learns `exec::Failed` (an amendment to its
   endings): ✗ with the error and the step's last lines, `exit N` or the
   signal's name in the times column. sb exits 1; only an `attached`
   ansible run passes its own code through, with one line naming the
   program and its outcome (as sb-go's "playbook … failed, scroll up …"):
   `Ran::pass_through` (Task 8b) writes cli-core's error block for it (the
   arguments masked) and returns cli-core's quiet `Exit` with the child's
   code, 128 plus a signal's number, or 1 for a timeout; an interrupted
   run returns a quiet `Exit(1)` with no line, so the command stops and
   cli-core ends sb by the signal.
6. **sb relaunches with `sudo -u root` and needs no loop marker**
   (maintainer, 2026-11-04, from plan Task 1): sudo then runs sb as root
   or refuses, so the relaunched run always passes sb's root check
   (effective uid 0) and can't loop, given a real sudo or sudo-rs on
   `PATH` (a program there that ignores `-u` would make sb relaunch
   again and again, piling up sudo processes, since sudo forks the
   command and waits for it). `SB_RELAUNCHED`, which a sudo rule
   naming commands without SETENV refused on every relaunch, and its
   check are removed (a cli-core change, exec plan Task 1b;
   docs/platform-notes/exec-processes.md).
7. **sb ends an interrupted run by the signal itself** (maintainer,
   2026-11-04): after cleanup, cli-core resets the handler and re-raises
   the signal that ended the run (signal-hook's safe
   `low_level::emulate_default_handler`), with core dumps off for SIGQUIT
   (rustix's `set_dumpable_behavior`, not dumpable; as built in Task 8a:
   `RLIMIT_CORE` 0 doesn't stop a `core_pattern` that pipes to a helper
   such as apport), instead of exiting with 130, 131 or 143; so does the
   forced exit on a second signal, after term-ui's restore hooks (term-ui
   decision 5). A calling shell then sees a death
   by signal and stops its script, as the convention requires ("You cannot
   'fake' the proper exit status by an exit(3) with a special numeric
   value", Cracauer); `$?` still reads 128 plus the signal. Both sudos
   pass it on (sudo(8), "EXIT VALUE": "sudo will send itself the same
   signal"; sudo-rs `exit_with_signal`). A child whose code sb passes
   through (decision 5) and that died of a signal sb didn't receive gives
   exit 128 plus the signal, as its failure, not sb's interruption. A
   cli-core change, exec plan Task 1c; the cli-core and term-ui specs'
   forced-exit text amended with it.
8. **The failed-child error is term-ui's** (maintainer, 2026-11-04, ahead
   of Checkpoint A): `exec::Failed` is term-ui's `ChildFailure`
   re-exported, carrying the command (masked) and how it ended, the short
   reason for the times column and stderr's last lines, so term-ui shows
   the reason without depending on exec. Its block reads as any error's,
   with no cause line repeating the ending.
9. **A failure keeps stderr's last lines only** (maintainer, 2026-11-04):
   a caller that needs a failed child's stdout, or its exit code to branch
   on (systemctl's 3 for inactive), reads `Captured` before `check()`; the
   failure itself carries the ending as text. A child cancelled through a
   caller's own token (not sb's shutdown) fails its step with the reason
   `interrupted`, a failure like any other, not the run's interruption.
10. **Output past the 2 s read after exit isn't marked**: none of
    capture's planned users (systemctl, git rev-parse, uv --version,
    dpkg-query, docker start/stop/restart) leaves a process holding its
    pipe, and sb-go's one `WaitDelay` (completion's shell probes) is a
    bound, not an observed case; the maintainer asked whether it was more
    than theoretical (2026-11-04). Added if a real command needs it.
11. **`stdin(bytes)` with `attached` is refused** (maintainer, 2026-11-04,
    Checkpoint A, which also approved the builder, `Outcome` and `Failed`
    as built): refused before spawning, as a missing working directory
    is, since an attached child's stdin is sb's own, for the user's
    keyboard; sb-go's callers never feed one (its executor would accept
    custom stdin for an interactive child, but only the MOTD banner sets
    any, under capture). Exec plan Task 8b.
12. **Checkpoint B** (maintainer, 2026-11-05), for attached children and
    the PTY:
    - A second SIGTERM while an attached child runs (after the first was
      passed to its pid) is the forced exit: SIGKILL to the child's pid,
      the terminal put back, sb dies of SIGTERM at once.
    - A Ctrl-C during that grace doesn't count toward the forced exit:
      the terminal gave the child the SIGINT, and sb keeps waiting ("wait
      and cooperative exit"), bounded by the grace and SIGKILL under way.
      (Extension by the author, not asked of the maintainer: a Ctrl-\
      (SIGQUIT) then is taken the same way, as the terminal delivers it
      to the child too.)
    - sb's forced exit SIGKILLs a running attached child's pid too, as the
      forced-exit hook does piped and PTY groups.
    - A PTY child's `TERM` is inherited as is, as sb-go does: without one
      sb is almost always off a terminal, and its output plain anyway.
    - portable-pty's `SHELL` (set even after `env_clear`) is accepted, as
      its behaviour; `env_clear` stays exact for `capture` and `stream`.
    - `pty` returns its own result (`Ran`: the outcome and `check()`), not
      `Streamed`, whose stderr tail a PTY can't fill.
13. **`attached` takes the `Shutdown`** (maintainer, 2026-11-06, Task
    8b): `attached(&shutdown)`, not `attached(&token)`, as the hold on
    SIGINT and SIGQUIT (decision 2, cli-core's `Shutdown::hold`) needs
    the `Shutdown`, not only its token; explicit, with no process-wide
    copy in exec.
14. **Values sb computes are shown; only credentials are masked**
    (maintainer, 2026-11-15, after reading a `sb reinstall-venv` run whose
    `uv pip sync --python [ARG] ... [ARG]` hid sb's own paths). Shown is
    the default for every value sb computes or passes on: paths,
    versions, file, unit and package names, branches, sb's own URLs,
    session ids, an editor's arguments. `secret` and `secret_env` are for
    credentials: tokens, passwords and API keys. A URL that can carry
    userinfo (one a user configures) goes through `secret`, or has its
    userinfo taken out first, as sb-config's `loggable_url` does.
    *Amended 2026-11-16 (maintainer):* exec's masking `value` and
    `option_value` are removed, so only `shown_value` and
    `secret`/`secret_env` remain and the method name makes the choice at
    every call; a joined option is one `shown_value` (or `secret`).
