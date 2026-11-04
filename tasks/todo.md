# exec tasks

Plan: [tasks/plan.md](plan.md). Spec: [docs/specs/exec.md](../docs/specs/exec.md).
Every task: TDD where it changes behaviour, a code review (and the
library-reviewer: this is process and I/O code) before committing,
`scripts/check.sh full` before committing, Library-check and Review lines
in each commit (CONSTRAINTS.md). Never run sb as root without
`SB_LOG_DIR` in `target/` or a temporary directory. Never read or change the real
Saltbox checkout (`/srv/git/saltbox`); say so in every reviewer's brief.
Tests never send real signals to the test process; real signals only
through `exec-demo` on a PTY. Root-gated tests run only as root with
`SB_TEST_ROOT=1` (check.sh sets it).

Carried forward (open items owned by later work):
- Taken by this plan:
  - From cli-core: children don't inherit `SB_RELAUNCHED`, or an
    unprivileged sb started by one would refuse to relaunch (root.rs
    module docs). Moot: Task 1b relaunches with `sudo -u root` and drops
    the variable (decision 6).
  - From cli-core: `COLORTERM` for children, decided (exec decision 3;
    Tasks 7, 8b).
  - From cli-core: sudoers' `--preserve-env` names beyond `env_keep`,
    recorded in docs/platform-notes/ (Task 1).
- For the docker start/stop/restart plan (next): exec's criterion 9, the
  first command through the binary (`capture` for the `systemctl` check),
  with sb's binary size, startup and cargo-deny through sb.
- For the ansible plan: exec decision 5's pass-through, an attached
  ansible run exiting with its own code after one line naming the
  program and its outcome (sb-go's "playbook … failed, scroll up …");
  exec builds the mechanism (Task 8b) and tests it with exec-demo.
- From config Task 12's review: an interrupted root run logs
  `run finished outcome="interrupted (exit 130)" error=exit code 1`; the
  error field repeats the runner's quiet `Exit(1)` beside the signal's
  code. Look at cli-core's run record when it next changes (Task 8a changes
  cli-core's signals: check it there).
- From cli-core:
  - Criterion 1: tab completion comes with the completion module.
  - The first command that takes argument values (`install -e`) adds an
    end-to-end test that a real secret never reaches the run log (exec's
    known-secret test covers a pass-through `value`, Task 2).
  - Help: fang's 120-column wrap of long abouts comes with the first ported
    command whose long text needs it (validate-config's long text is one
    short line, confirmed by config Task 11's help snapshot).
- From term-ui's spec: prompts (criterion 10) are built with their first
  user, self-update or git's confirm (decision 1).
- From docker Task 4's review: term-ui's `Cell` passes control characters
  through; escape them when a command shows less trusted text.
- From config Task 3's review: docker.rs's
  `ps_relaunches_through_sudo_when_not_root` gives `sb_as_nobody` a 0700
  temporary directory as `log_dir`, so the `nobody` run's theme path is
  unreadable and sb warns before relaunching. Use a 0777 parent with a
  missing child, as unprivileged.rs does, and assert no `warning:`; with
  the next docker change.
- From the docker spec, for the next docker plan: start/stop/restart (no
  `--verbose`, config decision 1; the unit-file check, timeouts and
  response cap); the picker's container list with the event stream.
- From the config spec, for the facts plan: the line-preserving writer,
  locking and drift detection; `sb update`'s call into validation.
- From the prototypes: the install script (tasks/prototypes/todo.md,
  "Later") depends on cli-core and the runner.

## Task 1: Platform facts and dependency trees (spike)

**Description:** Settle what the design leans on, each in
docs/platform-notes/ with versions and the method, no host details:
- sudoers(5) on `--preserve-env` names outside `env_keep`, `use_pty`'s
  default on the supported Ubuntu releases, how sudo relays SIGINT and
  SIGTERM to sb (carried item);
- whether Linux keeps a process-group id from reuse while a member lives,
  and whether `kill(-pgid, 0)` succeeds when the group holds only
  zombies, each confirmed by a test in a scratch project (the method in
  the note);
- signal-hook 0.4.5's `exfiltrator::WithOrigin`: its `extended-siginfo`
  feature compiles C (`extended-siginfo-raw`), so its build through cross
  for both musl targets and its deny tree;
- the API facts the design uses: portable-pty 0.9.0's cwd fallback and
  `Child::process_id()`, tokio's `Command::process_group(0)`, rustix's
  `test_kill_process_group`, `waitid` on a group and
  `set_child_subreaper` under the `process` feature;
- portable-pty's and tokio `process`'s added crates against deny.toml.
The scratch project and its `target/` are removed afterwards.

**Acceptance criteria:**
- [x] Each fact recorded with its source, version and method; the two
      kernel facts confirmed by a test.
- [x] `cargo deny` on the scratch tree with the new crates passes, or the
      exception is proposed; signal-hook's feature builds through cross,
      or the alternative goes to the maintainer before Task 8a.
- [x] Any finding that contradicts the spec raised with the maintainer
      before Task 2.

**Verification:**
- [x] code-reviewer on the notes.

Done (docs/platform-notes/exec-processes.md). Two findings went to the
maintainer and changed the spec: C sudo relays signals to sb's whole
process group, and a piped run's Ctrl-C arrives relayed (decision 2
revised: SIGINT never forwarded, so `WithOrigin` and its C feature
aren't needed); and `SB_RELAUNCHED` made every relaunch fail under a
rule naming commands without SETENV (decision 6: relaunch with `sudo -u
root`, no marker; Task 1b).

**Dependencies:** None
**Files:** `docs/platform-notes/`
**Scope:** S

## Task 1b: cli-core: relaunch with `sudo -u root`, no marker

**Description:** Decision 6 (maintainer, 2026-11-04, from Task 1): sb
relaunches with `sudo -u root --preserve-env=VISUAL,EDITOR,
SB_COLOR_PROFILE,NO_COLOR <sb> <args>`; sudo runs it as root or refuses,
so the relaunched run always passes the root check (effective uid 0).
`SB_RELAUNCHED`, its check and its "still not root" error are removed.
The cli-core spec's relaunch paragraph and root.rs's module docs amended;
sb's relaunch tests (the stand-in sudo in crates/sb/tests/nobody) check
that `-u root` is passed.

**Acceptance criteria:**
- [x] sudo's arguments tested (`-u root`, the four names, sb's path and
      arguments unchanged), failing first; the relaunch tests pass
      through the binary with the stand-in sudo; `SB_RELAUNCHED` appears
      nowhere in code or tests.

**Verification:**
- [x] `cargo test -p sb-cli-core`; `SB_TEST_ROOT=1 cargo test -p sb`;
      library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 1
**Files:** `crates/cli-core/src/root.rs`, `crates/sb/src/main.rs`,
`crates/sb/tests/nobody/mod.rs`, `crates/sb/tests/` (relaunch tests),
`docs/specs/cli-core.md`
**Scope:** S

## Task 1c: cli-core: end an interrupted run by the signal itself

**Description:** Decision 7 (maintainer, 2026-11-04): after cleanup and
the run record, cli-core ends an interrupted run by re-raising the signal
that ended it (signal-hook's `low_level::emulate_default_handler`), on the
forced exit after a second signal too (after term-ui's restore hooks),
instead of exiting with 130 or 143. A run that isn't interrupted exits
with its code as before. The cli-core spec's exit-code rules and the
term-ui spec's decision 5 amended. Tests that check 130 or 143 through
std's `ExitStatus` check the signal instead (`ExitStatusExt::signal`);
where a shell reads `$?` (the PTY tests), 130 and 143 stay. Assertions
that change form go through a test-only commit first (the floor guard's
precedent). SIGQUIT's re-raise, with core dumps off first (rustix's
`setrlimit`, `RLIMIT_CORE` 0), comes with Task 8a, where cli-core first
handles SIGQUIT, and is checked by Task 8b's "sb dies of SIGQUIT, no core
dump" cases.

**Acceptance criteria:**
- [x] The re-raise tested through the binary: SIGINT and SIGTERM to sb
      end it by that signal (`signal()` is 2 or 15, the run record still
      written and flushed, the terminal restored); two signals: sb dies of
      the signal, the terminal restored; `$?` in a shell reads 130 and
      143; a run that isn't interrupted still exits with its code.
- [x] Every existing signal test passes in its new form (cli-core's,
      term-ui's demo, sb's docker_signals.rs and
      validate_config_signals.rs).

**Verification:**
- [x] `cargo test -p sb-cli-core -p sb-term-ui`; `SB_TEST_ROOT=1 cargo
      test -p sb`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 1b
**Files:** `crates/cli-core/src/exit.rs`, `crates/cli-core/src/cancel.rs`,
`crates/cli-core/tests/`, `crates/term-ui/tests/signals.rs`,
`crates/sb/tests/` (signal tests), `docs/specs/cli-core.md`,
`docs/specs/term-ui.md`
**Scope:** M

## Task 2: The crate, `Cmd`, masking and the start record

**Description:** `crates/exec` (`sb-exec`) with `Cmd`: `arg(&'static
str)`, `value(String)`, `secret(String)`, `env`, `secret_env`,
`env_remove`, `env_clear`, `cwd`, `stdin(bytes)`; the environment rules
(`GIT_TERMINAL_PROMPT=0` for children not attached, applied to the
environment each way hands its spawner); the
masking of secrets out of text (empty values ignored); the start
record. The capability map's `exec` row (depends on cli-core and term-ui;
ANSI stripping moved to term-ui's row) and build-order line, and
deny.toml's wrappers. (`timeout` and `grace` come with Task 4, where they
act.)

**Acceptance criteria:**
- [x] The builder's rules and the masking tested as pure functions, each
      failing against a stub first.
- [x] The start record carries the program, the literals in place, the
      count of masked arguments, changed variable names, the working
      directory and the way it runs; tested with the known-secret test (a
      `secret`, a `secret_env` value, a pass-through `value`) in a test
      binary of its own.
- [x] `cargo deny --manifest-path crates/exec/Cargo.toml --all-features
      check` passes.

**Verification:**
- [x] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**As built (2026-11-04):** with Task 3 in one change: until `capture`
reads them, the builder's fields and the record have no reader, which the
dead-code lint rejects (and `#[expect]` needs an Exceptions row). The
start record's own test binary is crates/exec/tests/capture_log.rs,
through `capture`. The ways join `Way` with their tasks, so attached's
exemption from `GIT_TERMINAL_PROMPT=0` comes with Task 8b.

**Dependencies:** Task 1
**Files:** `Cargo.toml`, `crates/exec/`, `deny.toml`,
`docs/specs/capability-map.md`
**Scope:** M

## Task 3: `capture`, outcomes and the end record

**Description:** `capture(&token)` through `tokio::process` (the
workspace's tokio gains `process`): own process group, null stdin unless
bytes are given (written from their own task, EPIPE ignored), stdout and
stderr read concurrently with the caps (first 1 MiB of stdout, last 1 MiB
of stderr, truncation marked; a "why not `wait_with_output`" comment:
it has no cap), the 2 s drain after exit, spawn errors, the
working-directory check, `Outcome` (code or signal number, elapsed),
`check()` and `exec::Failed` with stderr's last lines, secrets masked in
the returned text and the error, the end record. The token is taken
here and acts from Task 4.

**Acceptance criteria:**
- [x] Real children: exit codes, a signal, spawn failures (not found, not
      executable, no such directory), stdin bytes and null stdin, both
      caps and truncation, a backgrounded short sleep holding stdout past
      the 2 s drain (done within it, its group killed afterwards), a
      child echoing a `secret` and a `secret_env` value (masked in stdout,
      stderr and the error).
- [x] The environment with real children (`env`):
      `GIT_TERMINAL_PROMPT=0`, changed variables present, an inherited
      `COLORTERM` left as it is (decision 3).
- [x] The end record (outcome, elapsed) logged with nothing masked
      leaking; `cargo deny` on crates/exec passes.

**Verification:**
- [x] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 2
**Files:** `Cargo.toml`, `crates/exec/src/run.rs`, `crates/exec/tests/`
**Scope:** M

## Task 4: Process groups: cancellation, timeouts, escalation

**Description:** The group path every piped and PTY child shares, with
`timeout(d)` and `grace(d)` (default 5 s, decision 1): on cancellation or
timeout, SIGTERM and SIGCONT to the group, SIGKILL after the grace while
members remain, checked with `test_kill_process_group` also after the
leader's reap, and bounded: one last check after SIGKILL, within the
grace plus one second, then the outcome is recorded; interrupted and
timed-out outcomes win over an exit 0 in the grace; the drop guard
(SIGKILL to the group); registration with cli-core's existing forced-exit
hook (`Shutdown::on_forced_exit`; any change to cli-core here is raised
first). `crates/exec/src/bin/exec-demo.rs` runs a piped child for real
signal tests; its child's group id reaches the test (as built: through
a file, see below) so a test can check the group. Tests run in a test binary that is a child subreaper with a
reaper running alongside: it peeks with `waitid(WaitId::Pgid(..),
EXITED | NOHANG | NOWAIT)` and reaps by pid every member but the leader
(left to tokio), recording each one's ending signal, so an exited
grandchild never sits as a zombie that makes the group look alive.
Tests assert pgid == pid before signalling and use a short grace.

**Before writing the guard:** check process-wrap's tokio `ProcessGroup`
wrapper (watchexec; the Task 2-3 library review's lead, unverified there:
does it kill the whole group on drop?) and use it or record why not.

**As built (2026-11-04):** the test reaper reaps each adopted grandchild
by the pid its script reports (`waitpid`), not from a `waitid` peek:
rustix's `WaitIdStatus` carries no pid, and `waitpgid` would also reap
the leader, which tokio reaps; `waitid(Pgid, NOWAIT)` still checks that
no zombie is left. exec-demo's signals go to its pid with exec-demo run
piped, not on a PTY: its child is piped and leaves the terminal alone,
and std's `ExitStatus` shows the death by signal (decision 7) that a
shell's `$?` reads as 130 or 143. The pids go to a file exec-demo is
given rather than to its output. The forced-exit hook is exec's own
(`sb_exec::on_forced_exit(&Shutdown)`, registered by the binary), so
cli-core is unchanged.

**Acceptance criteria:**
- [x] In-process, through the token: a child that exits on SIGTERM gets
      no SIGKILL; one that ignores it is SIGKILLed after the grace; a
      leader that exits on SIGTERM with a grandchild that ignores it: the
      grandchild SIGKILLed; a leader and a grandchild that both exit on
      SIGTERM: no SIGKILL, done well before the grace (the reaper's
      recorded signals); a stopped child ends; a timeout reads timed
      out; a child that traps SIGTERM and exits 0 reads interrupted; a
      dropped future leaves no member; no zombie of exec's own left (a
      `waitid` peek on the leader's group finds nothing); all within the
      grace plus one second.
- [x] The default grace is 5 s and `grace(d)` overrides it (the builder's
      value, so no test waits 5 s).
- [x] Through exec-demo (piped, see "As built"): SIGINT and SIGTERM to
      exec-demo with a grandchild-holding child end it by that signal and
      the group gone; a
      second signal makes the forced-exit hook SIGKILL the group.

**Verification:**
- [x] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 3
**Files:** `crates/exec/src/group.rs`, `crates/exec/src/bin/exec-demo.rs`,
`crates/exec/tests/`
**Scope:** M

## Task 5: term-ui: a failed child's ending and the "runner live" check

**Description:** term-ui owns a failed-child type (program, outcome text)
that exec's `Failed` carries: a step ending with it reads ✗ with the
error and the last 10 lines, the times column showing `exit N` or the
signal's name (from a library's names: rustix's or signal-hook's
constants, not a hand-written table); the command then exits 1
(decision 5); and a way to ask whether a `Runner` is live (for
`attached`, Task 8b). The term-ui spec's endings table and "not here yet"
note amended.

**Acceptance criteria:**
- [x] The ending and the times column in term-ui's tree and plain tests
      (snapshots), failing first; exit 1 for a failed child; the live
      check tested.

**Verification:**
- [x] `cargo test -p sb-term-ui`; library-reviewer; `scripts/check.sh
      full`.

**Dependencies:** Task 4
**Files:** `crates/term-ui/src/`, `crates/exec/src/lib.rs`,
`docs/specs/term-ui.md`
**Scope:** S

## Checkpoint A: Capture is done
- [x] `scripts/check.sh full` passes (on 835ca3f's tree: changed-line
      coverage 100%, 23 of 23 viable mutants caught).
- [x] The maintainer reviews the builder's API and `Failed`'s shape
      before the other ways build on them, and decides a point the spec
      leaves open: `stdin(bytes)` with `attached`, refused (proposal: sb's
      own stdin is the child's) or allowed. (2026-11-04: the builder,
      `Outcome` and `Failed` approved as built; refused, decision 11.
      Task 1's facts were recorded in docs/platform-notes/exec-processes.md
      and led to decisions 2, 6 and 7 with the maintainer earlier.)

## Task 6: `stream`

**Description:** `stream(&mut step)`: lines from stdout and stderr read
with bounded `read_until` (`take(4 KiB + 1)`, the rest of a long line
dropped), secrets masked through `Line::secret`, handed to
`Step::output`; stderr's last 64 KiB kept for the caller; the step's
token through Task 4's group path; failures as Task 5's ending.

**As built (2026-11-04):** secrets are taken out with exec's `Mask`
(over a window reaching the longest secret past the 4 KiB cut, as
capture's caps) before the step gets the line, not through
`Line::secret`: the mask already finds where they sit, and the screen and
the log get the same `[REDACTED]` text. Lines reach the step through a
tokio channel from one reader per pipe.

**Acceptance criteria:**
- [x] Real children through term-ui's in-memory target: exit codes, a
      signal, a spawn failure, stdin bytes and null stdin; lines from
      both streams, a multi-byte character split across reads, `\r`
      progress shown at the next line, a last line without `\n`, a line
      past 4 KiB, the 2 s drain with a backgrounded short sleep, the
      stderr tail, a child echoing a `secret` and a `secret_env` value
      (masked in the step's lines and the log); `check()` gives `Failed`
      without stderr lines, shown with Task 5's ending; cancellation
      through the step's token.
- [x] The environment with real children as Task 3's.

**Verification:**
- [x] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Checkpoint A
**Files:** `crates/exec/src/run.rs`, `crates/exec/tests/`
**Scope:** M

## Task 7: `pty`

**Description:** `pty(&mut step)` through portable-pty: sb's terminal size
(24×120 when stderr isn't one), the working directory always set and
checked, `stdin(bytes)` refused, the writer dropped at start, the reader
on its own thread with bounded reads and EIO as the end, the 2 s drain,
the signal's name in the outcome, `COLORTERM` from the colour profile
(decision 3), the environment rules applied to portable-pty's own
`CommandBuilder` environment, Task 4's group path with the PTY's own
reaping (its wait thread), the drop guard and the forced-exit hook's
registration. exec-demo gains a `pty` mode.

**Acceptance criteria:**
- [ ] The child sees a TTY and its size (on a terminal through `script`,
      or an injected size); an exit code; a signal's name in the
      outcome; a spawn failure; `stdin(bytes)` refused; a reading child
      gets EOF; a missing directory is an error, not `$HOME`; `Failed`
      without stderr lines.
- [ ] The PTY reader: a multi-byte character split across reads, `\r`
      progress at the next line, a last line without `\n`, a line past
      4 KiB, the 2 s drain with a backgrounded short sleep holding the
      PTY, a child echoing a `secret` and a `secret_env` value (masked).
- [ ] The environment with real children: `GIT_TERMINAL_PROMPT=0`,
      `COLORTERM` truecolor or 256color, and removed for ansi and ascii;
      colours kept for a coloured target and stripped otherwise
      (term-ui's).
- [ ] Cancellation and a timeout reach the session's group; through
      exec-demo's `pty` mode on a PTY: SIGINT and SIGTERM give 130 and 143
      with the session's group gone, a second signal makes the hook SIGKILL
      it; a dropped `pty` future leaves no member.
- [ ] `cargo deny` on crates/exec passes with portable-pty.

**Verification:**
- [ ] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 6
**Files:** `Cargo.toml`, `crates/exec/src/pty.rs`, `crates/exec/tests/`
**Scope:** M

## Checkpoint B: Before the shared signal change
- [ ] `scripts/check.sh full` passes; project coverage, size and startup
      re-measured against CONSTRAINTS.md's values.
- [ ] The maintainer approves the signal behaviour (decision 2 as Task 8a
      will build it) before cli-core changes, and decides the cases the
      spec leaves open while an attached child runs: whether a second
      SIGTERM, or a SIGTERM then a SIGINT or SIGQUIT, forces sb's exit;
      if so, whether the forced-exit hook SIGKILLs the attached child's
      pid or sb exits and leaves it; what a terminal SIGINT does while a
      forwarded SIGTERM's grace runs. (SIGQUIT outside an attached child
      is decided: term-ui decision 5 keeps Ctrl-\ a hard escape, its
      default action; the hold changes that only while an attached child
      runs.) The decided table goes into the spec and Task 8a's acceptance
      criteria.

## Task 8a: cli-core's signals while an attached child runs

**Description:** cli-core's amendment (decision 2): a hold that exec turns
on while an attached child runs; while it's on, a terminal SIGINT doesn't
cancel the token and held SIGINTs and SIGQUITs don't count toward the
forced exit; SIGINT and SIGQUIT are noted and ignored, never forwarded
("wait and cooperative exit", decision 2 revised after Task 1; SIGQUIT's
default action would otherwise end sb, and may dump core); a SIGTERM is
handed to the holder to forward; the holder reports a child that died of
SIGINT or SIGQUIT, which records the interruption on `Shutdown` (sb then
ends by that signal, Task 1c) and cancels the token, only if sb itself
received the signal; a run ended by SIGQUIT re-raises it with core dumps
off first (rustix's `setrlimit`, `RLIMIT_CORE` 0; Task 1c's re-raise).
The decision logic (hold state and signal) is a pure
function, tested in-process so the coverage and mutation gates reach it.
The cli-core spec amended. The carried run record (`error=exit code 1`
beside an interrupted run's code) checked and fixed or re-carried with a
reason.

**Acceptance criteria:**
- [ ] The decision logic's every case, as decided at Checkpoint B,
      tested in-process, failing first; the same table through exec-demo
      in Task 8b.
- [ ] cli-core's and sb's existing signal tests pass
      (`SB_TEST_ROOT=1`: validate_config_signals.rs, docker_signals.rs).
- [ ] sb's binary size and startup measured, with an ADR past the
      tolerance; cargo-deny through sb passes.
- [ ] The run-record finding recorded: fixed with a test, or carried.

**Verification:**
- [ ] `cargo test -p sb-cli-core`; `SB_TEST_ROOT=1 cargo test -p sb`;
      library-reviewer; a security-auditor pass on the signal change;
      `scripts/check.sh full`.

**Dependencies:** Checkpoint B
**Files:** `crates/cli-core/Cargo.toml`, `crates/cli-core/src/cancel.rs`,
`docs/specs/cli-core.md`, `docs/decisions/` (if an ADR is due)
**Scope:** M

## Task 8b: `attached`

**Description:** `attached(&token)`: sb's stdin, stdout and stderr, sb's
group, never a group signal; `stdin(bytes)` refused (sb's own stdin is
the child's); refused while a term-ui runner is live; `COLORTERM`
(decision 3); no `GIT_TERMINAL_PROMPT`; Task 8a's hold while it runs;
`timeout` and `grace` by pid; the start and end records written, its
output not logged; decision 5's pass-through mechanism (an `attached`
child's own code, with one line naming the program and its outcome).
exec-demo runs an attached child for the signal tests. (Ordered after
Task 7; it doesn't use portable-pty.)

**Noted at Checkpoint A:** the `stdin(bytes)` refusal is checked to come
before any spawn (no start record written).

**Acceptance criteria:**
- [ ] Through exec-demo on a PTY: one and two Ctrl-Cs with a child that
      survives SIGINT (sb waits), a child that dies of it (sb dies of
      SIGINT), Ctrl-\ with a child that dies of SIGQUIT (sb dies of
      SIGQUIT, no core dump) and with one that ignores SIGINT (it ends on
      Ctrl-\; sb dies of SIGQUIT, no core dump), a child signalled on its
      own (not an interruption: the step fails as the child's failure,
      exit 1), a SIGINT sent to sb alone not forwarded, a SIGTERM
      forwarded (sb dies of SIGTERM); a timeout reads timed out; a child
      ignoring the forwarded SIGTERM is SIGKILLed after the grace; signals
      go to the child's pid, never a group.
- [ ] An exit code, a spawn failure, `stdin(bytes)` refused, refusal while
      a runner is live; the environment with real children (`COLORTERM`
      per the profile, no `GIT_TERMINAL_PROMPT`); the records written and
      no output logged; the pass-through: exec-demo exits with the child's
      code after its one line; Checkpoint B's decided cases through
      exec-demo.

**Verification:**
- [ ] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 8a
**Files:** `crates/exec/src/run.rs`, `crates/exec/src/bin/exec-demo.rs`,
`crates/exec/tests/`
**Scope:** M

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes; coverage, size and
      startup re-measured.
- [ ] Spec success criteria 1-8 met with evidence; criterion 9 carried by
      name to the docker start/stop/restart plan, decision 5's ansible
      use to the ansible plan.
