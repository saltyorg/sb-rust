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

**As built (2026-11-05):** the PTY child is waited on with rustix's
`waitpid` on its own thread (portable-pty's `wait` gives only
`strsignal`'s description), and exec-demo's PTY runs are signalled by
pid as in Task 4.

**Acceptance criteria:**
- [x] The child sees a TTY and its size (on a terminal through `script`,
      or an injected size); an exit code; a signal's name in the
      outcome; a spawn failure; `stdin(bytes)` refused; a reading child
      gets EOF; a missing directory is an error, not `$HOME`; `Failed`
      without stderr lines.
- [x] The PTY reader: a multi-byte character split across reads, `\r`
      progress at the next line, a last line without `\n`, a line past
      4 KiB, the 2 s drain with a backgrounded short sleep holding the
      PTY, a child echoing a `secret` and a `secret_env` value (masked).
- [x] The environment with real children: `GIT_TERMINAL_PROMPT=0`,
      `COLORTERM` truecolor or 256color, and removed for ansi and ascii;
      colours kept for a coloured target and stripped otherwise
      (term-ui's).
- [x] Cancellation and a timeout reach the session's group; through
      exec-demo's `pty` mode on a PTY: SIGINT and SIGTERM give 130 and 143
      with the session's group gone, a second signal makes the hook SIGKILL
      it; a dropped `pty` future leaves no member.
- [x] `cargo deny` on crates/exec passes with portable-pty.

**Verification:**
- [x] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Dependencies:** Task 6
**Files:** `Cargo.toml`, `crates/exec/src/pty.rs`, `crates/exec/tests/`
**Scope:** M

## Checkpoint B: Before the shared signal change
- [x] `scripts/check.sh full` passes; size and startup re-measured
      against CONSTRAINTS.md's values (2026-11-05, on e7dcecc's tree and
      again on the Checkpoint B commit's: full check green; sb 11645408
      bytes; `--help` median 1.06 ms).
- [ ] Project coverage re-measured against CONSTRAINTS.md's values: not
      done here (changed-line coverage 94%); carried to Checkpoint C.
- [x] The maintainer approves the signal behaviour (decision 2 as Task 8a
      will build it) before cli-core changes, and decides the cases the
      spec leaves open while an attached child runs: whether a second
      SIGTERM, or a SIGTERM then a SIGINT or SIGQUIT, forces sb's exit;
      if so, whether the forced-exit hook SIGKILLs the attached child's
      pid or sb exits and leaves it; what a terminal SIGINT does while a
      forwarded SIGTERM's grace runs. (SIGQUIT outside an attached child
      is decided: term-ui decision 5 keeps Ctrl-\ a hard escape, its
      default action; the hold changes that only while an attached child
      runs.) The decided table goes into the spec and Task 8a's acceptance
      criteria. (2026-11-05: decided, exec decision 12. A second SIGTERM
      forces the exit; a terminal SIGINT during a forwarded SIGTERM's
      grace doesn't; the forced exit SIGKILLs the attached child's pid.
      Extension by the author, not asked: a terminal SIGQUIT then is taken
      as the SIGINT is, the terminal delivering both to the child.)

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
off first (rustix's `set_dumpable_behavior`, not dumpable, as built:
`RLIMIT_CORE` 0 doesn't stop a `core_pattern` piping to apport; Task 1c's
re-raise).
The decision logic (hold state and signal) is a pure
function, tested in-process so the coverage and mutation gates reach it.
The cli-core spec amended. The carried run record (`error=exit code 1`
beside an interrupted run's code) checked and fixed or re-carried with a
reason.

**Acceptance criteria:**
- [x] The decision logic's every case, as decided at Checkpoint B (exec
      decision 12), tested in-process, failing first, and the same table
      through exec-demo in Task 8b:
      - SIGINT or SIGQUIT while an attached child runs: held, not counted
        toward the forced exit; sb ends by it only if the child died of it;
      - SIGTERM: passed to the child's pid, the grace and SIGKILL follow;
      - a second SIGTERM: the forced exit;
      - a terminal SIGINT (and, as the author's extension, SIGQUIT)
        during a forwarded SIGTERM's grace: not counted, sb waits;
      - the forced exit: SIGKILL to the attached child's pid as well as to
        exec's groups (exec's side: `attached` registers the pid with
        `sb_exec::on_forced_exit`'s kill, Task 8b; cli-core runs the hooks).
      Done in 8e54fa8 (in-process and through real signals); the same
      table through exec-demo is Task 8b's. Size 11649536 bytes (+4096),
      startup 1.1/1.6 ms; cargo-deny passes.

      As built: the hold is one `Mutex` + `Condvar` over the count and the
      notes, with signal-hook `flag::register` flags set at delivery, so
      the decision is about when a signal arrived, not when the signals
      thread read it (the Task 8a code review's race); a hold's start and
      end wait for undecided deliveries, and `child_ended_by` waits up to
      100 ms for sb's own copy. A child's interruption counts as the first
      signal. Ctrl-\ runs the restore hooks first (maintainer, 2026-11-05).
      Root runs are not dumpable from startup (security review).
- [x] cli-core's and sb's existing signal tests pass
      (`SB_TEST_ROOT=1`: validate_config_signals.rs, docker_signals.rs).
- [x] sb's binary size and startup measured, with an ADR past the
      tolerance; cargo-deny through sb passes.
- [x] The run-record finding recorded: fixed with a test, or carried.

**Verification:**
- [x] `cargo test -p sb-cli-core`; `SB_TEST_ROOT=1 cargo test -p sb`;
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
      exec-demo (the table in Task 8a's criteria: a second SIGTERM forcing
      the exit, the forced exit SIGKILLing the attached child's pid, a
      terminal SIGINT or SIGQUIT during the grace not counted).

**Verification:**
- [ ] `cargo test -p sb-exec`; library-reviewer; `scripts/check.sh full`.

**Carried from Task 8a:** the forced exit's SIGKILL of the attached
child's pid (exec registers it); the kernel makes an exec'd child
dumpable again, so a root attached child's core is the host's setting
(decide whether `RLIMIT_CORE` 0 is set for it, in its `pre_exec`-free
way, or recorded as a known limit). `Hold`'s calls block briefly (up to
100 ms in `child_ended_by`): call them where the stall is fine or through
`spawn_blocking`. A hold that starts while Ctrl-\'s restore hooks run
can spawn a child the forced-exit kill (already taken) won't reach:
refuse the spawn once the token is cancelled, or record the limit.

**Dependencies:** Task 8a
**Files:** `crates/exec/src/run.rs`, `crates/exec/src/bin/exec-demo.rs`,
`crates/exec/tests/`
**Scope:** M

## Task 9: the whole-codebase review's Required findings

**Description:** The rust-skills and Microsoft-guidelines review of every
crate (2026-11-05; the findings are in the body of the commit that added
this task: `git log -S'Review backlog' -- tasks/todo.md`) found these to
fix now. Each is its own commit, test first, after Task 8a (done,
8e54fa8).

**Acceptance criteria:**
- [ ] config: `Credentials::Ready` carries the parsed login (Cloudflare,
      or a named `DockerhubLogin { user, token }` with a redacting
      `Debug`), built once in the validator, so `run::checks` can't skip
      a check while the file reads validated. Test: a Docker Hub check
      runs with the login the rules accepted, the warn-and-continue arm
      is gone, and `format!("{:?}")` of the `Credentials::Ready` (derived
      `Debug`) shows no password or token.
- [ ] docker: `DOCKER_HOST` userinfo (`tcp://user:secret@host`) is kept
      out of the run log and every error (CONSTRAINTS.md: logs never
      contain secret values). Test: with that `DOCKER_HOST`, the captured
      log and the shown error contain neither the secret nor the user.
- [ ] exec: `Captured`, `Streamed` and `Ran` are `#[must_use]` (the
      compiler then refuses a dropped outcome under check.sh). Test:
      exec-demo with a failing PTY child exits non-zero (it exits 0 today,
      dropping `Ran`).
- [ ] term-ui: under `SB_COLOR_PROFILE=ascii` with colour on, a child's
      own SGR codes are stripped, live and plain, through the stripping
      the colour-off path already uses (no ANSI handling of our own).
      Test: the spec's guard test with `\x1b[31mred\x1b[0m` fed through
      `Step::output`, no `\x1b[` on the in-memory screen or in plain output.
- [ ] config: a Cloudflare `Retry-After` waits at most 60 s (maintainer,
      2026-11-05; config spec, differences). Test: a fake server
      answering 429 with `Retry-After: 3600` is retried after the cap,
      on tokio's paused clock.

**Verification:**
- [ ] Each test fails first; `scripts/check.sh full`; code-reviewer, and
      library-reviewer for the term-ui and exec items.

**Dependencies:** Task 8a
**Files:** `crates/config/src/{rules,run,validators,cloudflare}.rs`,
`crates/docker/src/connect.rs`, `crates/exec/src/{lib.rs,bin/exec-demo.rs}`,
`crates/term-ui/src/runner.rs`
**Scope:** M (five small commits)

## Task 10: `clippy::pedantic`

**Description:** The maintainer decided to turn on all of
`clippy::pedantic` (2026-11-05, from the review's M-STATIC-VERIFICATION
finding): `pedantic = { level = "warn", priority = -1 }` in
`[workspace.lints.clippy]` (errors under check.sh), so later per-lint
settings don't trip `lint_groups_priority`. Every hit is fixed or given
an `#[expect(…, reason)]` with an Exceptions row in CONSTRAINTS.md; a
workspace-level opt-out of one pedantic lint needs an ADR. CONSTRAINTS.md's
Lint row names the group. The review's rustc lints outside pedantic
(`missing_debug_implementations`, `trivial_numeric_casts`) come with it.
It absorbs the backlog's `# Errors`, `#[must_use]` and `Debug` items.

**Acceptance criteria:**
- [ ] The group on, every crate clean, the Exceptions table updated.

**Verification:**
- [ ] `scripts/check.sh full`; code-reviewer on each crate's commit.

**Dependencies:** Task 9
**Files:** `Cargo.toml`, `CONSTRAINTS.md`, every crate
**Scope:** L (one commit per crate where large)

## Review backlog (taken up when each crate is next touched)

From the same review; Optional findings and nits, by crate (the full
list, by reviewer finding, is in the commit body named in Task 9).
Headlines:
- cli-core: `listen_for_signals` once per process (guard it); restore
  hooks `FnOnce`; `Shutdown` as one `Arc<Inner>`; `snapshot()` returns a
  small ending value; `ConfigError` with its path (standalone messages);
  `Debug` on `RunLog`, `Shutdown`, `RestoreHook`; the theme passed to
  `finish_run`/`report`, `install` reporting a second call; `ensure_root`
  given its exe and args (its test is tautological); `config.rs` `Read`
  error's source shown twice; `build_info` `unwrap_or_default` →
  `expect`; `signal_exit_code` overflow on a non-signal; `RelaunchError`
  a struct; the re-exports' reasons; `Uid`/`Signal` types, `AsRef<Path>`,
  named log events (only if the run log wants them).
- host: the supported-releases test restates the constant.
- config: secret redaction typed, not by searching the error text; secret
  paths from the rule walk, not a re-parsed path string (a key with `.`
  or `[`); `Validator` split into sync and credential kinds;
  `max_length` `Option<NonZeroUsize>`; typed network-check errors; a
  `CLOUDFLARE_BASE_URL` path that can't be used is an error, not
  dropped; URLs parsed once; `Node` and `Value` with a redacting `Debug`
  (no production path formats them today: only tests); `Debug` on
  `Saltbox`, `Check`, `Settings`; internal modules `pub(crate)` (saphyr,
  regex and reqwest types leak); the `Validators` trait `pub(crate)` or
  concrete; the clock and `fs::metadata` injectable; silent fallbacks →
  `expect`.
- term-ui: the live area's total height capped for ended-but-live steps;
  one lock per output line (`colour`/`terminal` out of the shared
  state); `clean` per-character width; tree helpers' full scans;
  `#[must_use]` on builders; `Target` `#[non_exhaustive]`; the runner's
  clock through `tokio::time::Instant` (times testable); a plain
  in-memory target (an API change: ask); rustdoc without design
  history; `table_colour` `pub(crate)`; `Send + Sync` assertions;
  `write_all` retrying `EINTR`; `Debug` on the public types (`Line` by
  hand, masked).
- exec: a failed wait marks the leader reaped (the drop guard is off)
  and reads as a spawn failure; `pty_error` keeps `io::ErrorKind`; one
  line splitter for both ways; `value()`/`secret()` take `OsString`;
  `SpawnCause` `#[non_exhaustive]` or accessors; `Send` assertions on the
  futures; the forced-exit hook's registration checked; a missing signal
  read as signal 0 and the PTY wait thread's error → `expect`; tight
  wall-clock bounds in the group tests.
- docker: a non-UTF-8 Docker variable refused, not read as unset; `json()`
  and the fixtures fail loudly; bollard types `pub(crate)`; `Debug` on
  `Daemon`; typed errors instead of `anyhow` in the public API (decide
  before start/stop/restart).
- sb: `warn!("{warning}")` as a field; one runtime builder.

## Checkpoint C: Complete
- [ ] All tasks done; `scripts/check.sh full` passes; coverage, size and
      startup re-measured.
- [ ] Spec success criteria 1-8 met with evidence; criterion 9 carried by
      name to the docker start/stop/restart plan, decision 5's ansible
      use to the ansible plan.
