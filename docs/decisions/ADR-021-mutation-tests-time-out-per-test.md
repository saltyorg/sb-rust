# ADR-021: Mutation tests time out per test

## Status

Accepted (maintainer, 2026-11-13, hardening plan decision 34; Task 56)

## Date

2026-11-13

## Context

check.sh's mutation passes ran each mutant's tests with `cargo test`,
under cargo-mutants' 300 s timeout for the whole mutant. A timed-out
mutant counts as not caught (ADR-012, CONSTRAINTS.md's Mutation row).

Task 38 found where that misjudges a mutant. Replacing
`EntriesFwd::next`'s body with `Some(Ok(0))` or `Some(Ok(1))` makes the
iterator endless. Its bounded unit test fails at once, but about 30
other sb-journal tests consume the iterator and loop. libtest waits for
every test of a binary, so the binary never exits and the mutant times
out at 300 s: not caught, though a test failed.

## Decision

- **The mutation passes run their tests under cargo-nextest**
  (`--test-tool=nextest`, https://mutants.rs/nextest.html), with
  `.config/nextest.toml`'s `mutants` profile, passed to the test runs
  only (`--cargo-test-arg=--profile=mutants`,
  https://mutants.rs/cargo-args.html).
- **A test still running after 60 s is ended and counts as failed**
  (`slow-timeout = { period = "30s", terminate-after = 2 }`; on-timeout's
  default is `fail`: https://nexte.st/docs/features/slow-tests/). nextest
  ends the test's process group: SIGTERM, then SIGKILL after its 10 s
  grace period.
- **The first failure ends the tests still running**
  (`fail-fast = { max-fail = 1, terminate = "immediate" }`,
  https://nexte.st/docs/running/#termination-behavior). One failure
  already makes the mutant caught, so a hung test then costs nothing.
- **The 300 s mutant timeout stays** as the backstop, and still counts
  as not caught.
- **cargo-nextest 0.9.151 is pinned exactly** in CONSTRAINTS.md's Tools
  table. check.sh unsets every inherited `NEXTEST_*` variable, so a
  profile, retry count or config file from the environment can't
  override the profile (https://nexte.st/docs/configuration/env-vars/).
- **The floor guard watches `.config/nextest.toml`** as a tool config: an
  added or changed line of it is a finding (ADR-015).

## The timeout and its margin

Measured on the unmutated tree at 6ba7f93: `cargo nextest run` over all
eight crates (every crate the mutation passes test), as root with
`SB_TEST_ROOT=1`, under the host cap of a parallel run (16 CPUs, 32 GB),
three runs:

- 1,360 tests, all passed, about 13.5 s a run;
- the slowest, `sb::docker_control
  a_restart_whose_stop_polls_fail_says_why_no_start`, took 10.05 s,
  10.05 s and 10.09 s;
- the next, two term-ui runner tests, about 7.7 s.

60 s is about six times the slowest test. The margin covers a slower or
busier host; a narrow pass (sb-journal's: its own and sb-host's tests,
slowest about 3.6 s) runs fewer tests at once and has more.

cargo-mutants' baseline runs the mutated crates' own tests on the
unmutated tree, under the same profile (27.1.0, seen in its logs: the
baseline names the mutated packages, each mutant the `--test-package`
ones too). A test of those crates that can't finish in 60 s fails the
baseline, which exits 4, and check.sh refuses the pass as no verdict:
a timeout set too short there fails loudly. A tester crate's test
(sb-host's in the sb-journal pass, any other crate's in the wide pass)
isn't in the baseline. Too slow on the unmutated tree, it would fail on
every mutant and count each as caught; that is cargo-mutants' rule for
any test failing outside the baseline, under the old runner as well.
The margin is what keeps those tests clear: the slowest is the 10 s one
above.

## Shown on Task 38's case

Both mutants, run as check.sh runs the sb-journal pass (sb-journal's
and sb-host's tests) under this profile:
- With Task 38's change applied in a scratch copy (its bounded unit
  test): each caught in under a second. The bounded test failed, and
  nextest ended the 7 tests still running. The same copy under the old
  runner: both timed out at 300 s, not caught.
- On this ADR's base tree, with no bounded test: `Some(Ok(0))` caught in
  65 s. Two hung tests were ended at 60 s and counted as failed.

## Why this doesn't weaken the bar

- **A hung test fails, which is the old runner's verdict too, sooner.**
  Under libtest a test that hangs on a mutant stops its binary exiting
  until the mutant's timeout, which counts as not caught. Under nextest
  it is ended at 60 s and counts as failed, so the mutant is caught. A
  test that the unmutated tree passes in about 10 s and that runs past
  60 s on a mutant is the mutant's doing: an endless loop or a wait
  that never ends is behaviour a test should catch.
- **Slowness can now count as caught, per test and with a margin.** A
  test that a mutant only slows past 60 s, or that a busy host slows
  that far, fails, and the mutant counts as caught with no assertion
  failing. The margin (six times the slowest test) keeps that to a
  mutant that slows a test sixfold, and check.sh unsets the inherited
  `CARGO_MUTANTS_*` variables, so no job count runs several mutants at
  once beside each other's tests. The rejected
  alternative below has the same property at the whole mutant's 300 s,
  for any test, where a slow build or one slow test of hundreds can't be
  told from a hang.
- **The same tests run, except doc tests.** `cargo nextest list` and
  `cargo test -- --list` give the same 1,360 test names at 6ba7f93.
  nextest doesn't run doc tests (https://mutants.rs/nextest.html); there
  are two:
  - `crates/host/src/journal.rs`'s module example is `no_run`: libtest
    only compiles it. A mutant keeps every signature, so it compiles on
    every mutant and never failed one.
  - `crates/term-ui/src/text.rs`'s `DisplayText::from_bytes` example
    calls `from_bytes`, `lines` and `as_str`, all in text.rs. Its 41
    mutants were run with sb-term-ui's tests under both runners: 39
    caught and 2 unviable under each, the same 39. No mutant there needs
    the doc test.

  A later doc test that asserts something needs a unit test beside it,
  or its mutants show as missed: the score falls, it never rises.
  check.sh's Tests step still runs the doc tests (`cargo test --doc`).
- **Process state.** nextest runs each test in its own process; libtest
  runs a binary's tests as threads of one process. The tests that share
  process state were checked:
  - exec's `LIVE` and `ATTACHED` statics and their forced-exit hook: the
    tests assert on their own child's entry, by pid, so none reads
    another test's state. Under libtest the hook could also kill another
    test's children, a cross-test failure that only made catches flaky.
  - The `SPAWNS` locks (sb-docker's unit tests, sb's docker_control):
    they keep another test's fork from holding a stand-in program's
    write descriptor (ETXTBSY). Forks in another process don't inherit
    it, so the hazard they guard against can't happen across processes.
  - `subreaper` (`OnceLock`) in exec's tests and cli-core's
    `theme::CURRENT`: set once per process. The two tests that install a
    theme (`pty_colour`, term-ui's `run_log`) are alone in their binary.

  - Across binaries: `cargo test` runs the test binaries one after
    another, nextest runs tests from all of them at once. A grep of the
    crates' tests for fixed paths under `/tmp`, `/run` or `/var` and for
    fixed ports found only string fixtures and binds to port 0, and
    three runs of all 1,360 tests passed.

  A mutant caught only because one test's leftover state broke another
  test in the same process isn't caught by a test of the behaviour; no
  such catch is relied on.
- **Unviable and missed are unchanged.** A mutant that doesn't build is
  unviable under either runner, and a test that passes still passes.

## Alternatives Considered

### Bound every consuming test in code (`take(n)`, a deadline)
- Pros: no new tool.
- Cons: about 30 sb-journal tests consume `EntriesFwd` alone, and every
  later iterator or loop would need the same; a missed one silently
  costs a mutant. The caps protect against mutants only, not against
  anything sb does on Saltbox.
- Rejected.

### An exception for the timed-out mutants
- Pros: no change to the gate.
- Cons: an exception lowers the bar for a known case and has to be
  renewed; the next endless mutant needs another.
- Rejected.

### Count a timed-out mutant as caught
- Pros: one flag; no new tool.
- Cons: a mutant that only makes a test slow, or a test that is slow on
  a busy host, would count as caught without any test failing. The
  timeout is the whole mutant's, so it can't tell a hang from slowness,
  and its margin is over the whole run rather than over the slowest
  test.
- Rejected: the per-test timeout has the same property only past six
  times the slowest test, for the test that is slow.

### A shorter whole-mutant timeout
- Pros: no new tool.
- Cons: the mutant is still not caught, only sooner; and a slow test
  of the unmutated tree sets how short it can be.
- Rejected.

## Consequences

- A mutant that makes a test hang is caught, usually within seconds when
  another test fails first, else at 60 s.
- check.sh's task and full stages need cargo-nextest at 0.9.151.
- Doc tests don't judge mutants; one that alone would catch a mutant
  needs a unit test.
- ADR-012's timeout edge case (a test outside a narrow pass hanging)
  becomes rare: the hung test is ended and fails in the wide pass too.
- Each mutation pass leaves no process behind (2026-11-13, after 23
  orphaned `sb` processes from two parallel passes ran on for hours). Ending
  a test's process group, as nextest does at the timeout and cargo-mutants
  at the mutant's, misses a process that left the group or that the kill
  orphaned: util-linux `script`, which the terminal tests run sb under,
  puts its child in a new session. So each pass's temp copies go under a
  new directory of its own (`mktemp -d` under the system's temp
  directory, then made 0755, since sb's relaunch tests run from it as
  `nobody`; cargo-mutants copies under `TMPDIR`), and check.sh kills
  every process whose executable lies there after the pass, and on exit
  during one. The directory is outside any repository, since the tests
  inherit `TMPDIR` and sb-repo's expect git to find none above their temp
  directories. Another pass's or checkout's processes lie elsewhere and
  are never touched. A leaked process still runs until its pass ends, not
  past it; a check.sh killed with SIGKILL runs no cleanup. The floor
  guard's tests check it with a signal-ignoring process in its own
  session.
- A leaked process no longer runs until its pass ends either (2026-11-13,
  after 19 busy, signal-ignoring mutants of `sb` piled up during one long
  pass, each over 100% CPU). While a pass runs, a background sweeper in
  check.sh kills, every 3 s, each process whose executable lies under the
  pass's directory and whose parent is init (ppid 1), which is what a
  process orphaned by a timed-out test's kill becomes. A test's live `sb`
  still has its parent, so it is never touched. The sweeper stops when
  the pass ends and on exit during one, and ends by itself once the
  directory or check.sh is gone. A leak still runs for up to 3 s, and one
  adopted by a subreaper other than init waits for the sweep after the
  pass. The floor guard's tests check it with an orphan in the pass's
  copy, a live child of the pass and an orphan outside it.
- The sweeper kills only an orphan that has run for at least 120 s
  (2026-11-13, after it killed the orphan of sb's hangup test, which
  kills sb's parent shell on purpose and checks that sb still takes its
  grace and logs its ending; the unmutated baseline then failed). nextest
  ends any test at 60 s, so a test's own orphan never lives that long,
  where the leaked mutants ran for hours. A leak now runs for up to about
  two minutes. `SB_MUTATION_ORPHAN_AGE` sets the age in seconds; the
  floor guard's tests use 0, and check that a young orphan is left
  running at the default.
- Both sweeps match a process whose executable or working directory lies
  under the pass's directory (2026-11-13). exec's tests leave helper
  shells that run `/bin/sh` from a directory of the copy. Matched by
  executable only, hundreds outlived their passes, each holding its gate
  run's systemd scope open. The floor guard's tests add such a shell to
  both cases.
- A gate run killed during a pass no longer leaves the pass running
  (2026-11-15, after 185 of exec's helper shells, from five
  passes, were found running with their working directory in deleted
  copies). Reproduced on sb-exec: a whole pass left nothing, and a stop
  of the run as a supervising tool stops a command (SIGTERM to each
  descendant, SIGKILL about a second later) let the EXIT trap's sweep
  finish there. A check.sh killed with SIGKILL alone runs no trap, and
  cargo-mutants, adopted by init, went on through the rest of the pass
  with no sweeper; at its end it removed its copy and left the pass's
  directory, so what its mutants left ran on from a deleted copy. The
  sweeps had missed no process of a pass whose check.sh still ran. Two
  changes:
  - The sweeper, finding check.sh gone, does the cleanup on exit
    itself: it sweeps the pass's directory and removes it, after which
    cargo-mutants' builds and tests fail at once.
  - Each pass's directory names its check.sh (`check-sh.pid`: the pid
    and its start time, proc(5)'s stat field 22, so a reused pid isn't
    taken for it). Every check.sh stage starts by sweeping and removing
    each `sb-mutants-*` directory under the temp directory that names a
    check.sh that has ended, for a run whose sweeper was killed too, or
    whose trap was cut short. A directory whose check.sh still runs, one
    that names none (made before this, or by another kind of run), a
    symbolic link and another user's are left alone, and so is one kept
    for a look: it stops naming its check.sh. A pass's directory is
    removed with its `check-sh.pid` last, so that a removal cut short
    leaves one the next run still finds. The start-of-stage sweep
    reports a directory it can't sweep or remove and goes on.

  A leak of a run killed so lives until its sweeper sees it, within
  3 s, or until the next gate run. The floor guard's tests kill check.sh
  alone during a stubbed pass, kill it with its sweeper and run the next
  stage, check that live, unmarked, linked, reused-pid and ended
  directories are told apart, and that a directory kept for a look names
  no check.sh. Their stubbed passes make their directories in each run's
  own temp directory, never the host's.
- Each pass runs in a sandbox (the maintainer, 2026-11-15, after two
  mutants of the motd.yml migration wrote on the host as root: one wrote
  `/nonexistent` through a test's planted link, one left a root-owned
  `/opt/saltbox/backups` and two test backups). The passes run tests as
  root, so a mutant that bends a path or drops a safety flag (`O_EXCL`,
  `O_NOFOLLOW`) writes wherever the bent path points, and one that drops
  an override (a test's controller URL, `DOCKER_HOST`) reaches the
  host's real daemons. check.sh runs cargo-mutants in private mount and
  network namespaces (`unshare --mount --net`):
  - every mount is remounted read-only but `/tmp` (the pass's directory
    and copies, and the tests' temp roots), the pass's output
    (`target/mutants-run-N`) and the tests' log directory (`SB_LOG_DIR`),
    each bound to itself first, by its physical path, so that it keeps
    its read-write flag;
  - `/proc`, `/sys` and `/dev` stay as they are: kernel interfaces and
    devices. A mutant could still write a `/proc/sys` setting, the cgroup
    or BPF trees, `/dev/shm`, or replace a device node as root; and `/tmp`
    itself is the host's directory (a mutant could chmod it);
  - `/run` is an empty read-only tmpfs, so no daemon's socket (Docker,
    systemd, D-Bus) is reachable, and the network namespace has only its
    own loopback, up, so the tests' fake servers work and no daemon
    listening on the host's loopback (the Saltbox docker controller) is
    reachable.

  The working directory is entered again by its path inside, since it
  still names the mount it was entered through. Each step that fails, and
  a mount still writable after them (read back from mountinfo), stops the
  pass with no verdict (exit 2) rather than run it on a writable host.
  Cargo builds with its home read-only. The namespaces end with the pass;
  the sweeps still see its processes, which share the host's process
  namespace. Root only: util-linux's mount (2.37) refuses a user
  namespace's mapped user, so a pass not run as root runs as before,
  unsandboxed, and says so; such a mutant writes only what that user may.
  The floor guard's tests run a stubbed pass whose mutant writes to a
  directory on the host outside `/tmp` and to its own `TMPDIR` and
  output (the first fails, the others work), and read back its mount
  table (every other mount read-only), its `/run` (empty) and its network
  namespace (its own).
