# sb tasks

No plan is in progress. Finished plans:
- [tasks/journal/](journal/plan.md) (the sb-journal crate);
- [tasks/docker-control/](docker-control/plan.md) (docker start, stop
  and restart);
- [tasks/exec/](exec/plan.md);
- [tasks/config/](config/plan.md);
- [tasks/docker/](docker/plan.md) (`ps`);
- [tasks/term-ui/](term-ui/plan.md);
- [tasks/cli-core/](cli-core/plan.md);
- [tasks/prototypes/](prototypes/plan.md).

The next plan starts its own `tasks/plan.md` and task list here.

## Follow-ups from docker start, stop and restart

- docker: a failed job's containers aren't named (the controller's
  `failed`, `outcomes` and `error` would name them; a listed difference
  from sb-go to decide); sb's wait can give up before the controller's own
  job timeout (docker spec, Known limits).
- sb tests: the terminal helpers (`KillOnDrop`, `last_row`,
  `exit_code`, the restore checks) exist in validate_config_signals.rs
  and docker_control_signals.rs; a third copy moves them to
  `tests/common`. Both kill only `timeout` when a test fails before sb's
  pid is known (a process group would reach `script`).
- Contract test: the command tree's dropped flags (`-v` on docker start,
  stop and restart, as on validate-config) aren't recorded; a
  full-equality contract test records them (docker spec, Known limits).
- The controller's richer feedback (live per-container progress, queue
  position, cancel, a version and features check) is for the
  controller's Rust rewrite, the maintainer's (docker decision 9).

## Follow-ups from the journal reader (proto-journal, sb-journal spec)

- cli-core's `error_block` prints error text raw, and cli-core can't use
  term-ui's `DisplayText` (term-ui depends on cli-core). Check whether
  any error text can carry untrusted bytes (a child's stderr, a file
  name), and if so move the sanitizer's core below cli-core or sanitize
  at the source. Also fix the stale comment in cli-core/src/log.rs
  (about lines 184-186) that says child output is stripped with
  `console::strip_ansi_codes`; the runner uses `DisplayText` now.

- cargo-deny reports advisory-not-detected for exception E1's two
  ignores (RUSTSEC-2025-0141, RUSTSEC-2024-0320), although bincode,
  yaml-rust and syntect are still in Cargo.lock. Found in Task 11:
  syntect is in the lock only through `prototypes/proto-markdown`
  (tui-markdown), not in crates/sb's graph, which the gate checks
  (`cargo tree -p sb -i syntect` matches nothing). The ignores stay
  until the announcements renderer brings tui-markdown into sb's graph,
  or E1 is decided again at its expiry (2027-01-28).

- Done: the consumer's wake wait. tokio 1.54.1 deprecates `AsyncFd::new`
  as unsound (tokio PR #8587), and its replacement `AsyncFd::register` is
  `unsafe`, which the workspace forbids. Maintainer's decision
  (2026-11-11): sb-host's `journal::FollowWait` waits with rustix poll on
  the wake fd and a stop eventfd, inside spawn_blocking; its
  `FollowStop` handle, or dropping it, writes the stop fd. The journal
  spec's wake protocol is amended (decision 12), and the follow test in
  crates/host/tests/journal.rs waits through it.

- Closed in Task 11: crates/cli-core/src/exit.rs's anstream `strip_str`
  (which passes C1 code points through) is only in its test module,
  over the test's own fixed error, so no child or file text reaches it.

- Done in Task 7: the local CI workflow (journal-matrix.yml) runs the
  crate's follow.sh and stress.sh after the page checks; still unpushed.

- Done in Task 6: the two mutation survivors at the `e.is_io()` guards
  in file.rs's `test_object` and forward.rs's `all_matches_from` are
  killed by `the_systems_error_reading_a_probed_entry_stops_the_bisect`
  and `the_systems_error_in_the_resumed_list_is_the_walks`.

- The tui spec's viewer (the first consumer): the release's entry order
  can form a cycle when a boot is in no file's tail (Difference 20,
  `forward_paging_cycles_where_the_order_does`), so paging forward one
  entry at a time can loop. A page knows only its cursor, so the
  viewer (or sb-host) must stop at a cursor it has already shown.
  Decide where the guard lives with the tui spec; raise at Checkpoint A.
  The security audit (Task 11) adds a requirement the tui spec must
  record: the viewer passes a much smaller `Limits` than the 256 MiB
  default (a few MiB), and caps the size of one record it displays
  (`DisplayText` expands tabs up to 8 times). The audit accepted a page's
  per-file memory on an imported directory of crafted files (about a
  third of the limit per file) on that condition.

- A mutant can make a walk allocate without bound (`advance ->
  Some(Ok(0))`). Under a `systemd-run` MemoryMax scope, systemd's default
  OOMPolicy then stops the whole scope, cargo-mutants included. Run
  `check.sh task` with `-p OOMPolicy=continue` added, so only the test
  process dies and the mutant counts as caught. Consider a per-test
  memory cap inside check.sh's mutation pass, so an uncapped run can't
  exhaust the host.

- Decided in Task 6: the follow reads every header first on each
  drain (`refresh()`), which caps the guessed `object_head` reads at the
  file's length then, and an `Io` from a cut inside one drain is
  waited for once (`Wait::Entry`), like any read, before the next
  drain reads the header again
  (`a_file_cut_while_followed_reads_up_to_the_cut`).

- A page holds a descriptor for every listed file at once (Task 3's
  design, kept in Task 4), and a follow holds one per followed file for
  as long as it runs (Task 6), plus its inotify descriptor, and each
  waiter (`journal::FollowWait`) two more: the wake fd's duplicate and
  its stop eventfd, so one eventfd more per follow than the `AsyncFd`
  design. Past the soft `RLIMIT_NOFILE` (1024 by default)
  the extra opens fail with EMFILE, reported one per file. journald's
  defaults keep a journal far below that; decide whether sb raises the
  soft limit to the hard one at startup (rustix `setrlimit`, beside
  cli-core's `RLIMIT_CORE`) or caps the files per page (security audit of
  Task 4, Low; Task 11's audit kept it Low and open for the maintainer:
  the opens past the limit are reported, never dropped).

- CI: a workflow running `scripts/check.sh` (CONSTRAINTS.md asks for
  `full` in CI once CI exists; the runner needs cargo-deny, cargo-mutants,
  cargo-llvm-cov and cross). `.github/workflows/journal-matrix.yml` runs
  only the journal matrix (sb-journal spec, decision 7).
- CI: pushing and running the journal matrix, then afl.rs fuzzing, each
  on the maintainer's go-ahead.

## Review backlog (taken up when each crate is next touched)

From the whole-codebase review (2026-11-05), moved here from
[tasks/exec/todo.md](exec/todo.md); Optional findings and nits, by
crate (the full list, by reviewer finding, is in the commit body named
in the exec plan's Task 9).
Headlines:
- cli-core: `listen_for_signals` once per process (guard it); restore
  hooks `FnOnce`; `Shutdown` as one `Arc<Inner>`; `snapshot()` returns a
  small ending value; `ConfigError` with its path (standalone messages);
  the theme passed to
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
  (no production path formats them today: only tests); internal modules
  `pub(crate)` (saphyr,
  regex and reqwest types leak); the `Validators` trait `pub(crate)` or
  concrete; the clock and `fs::metadata` injectable; silent fallbacks →
  `expect`.
- term-ui: the live area's total height capped for ended-but-live steps;
  one lock per output line (`colour`/`terminal` out of the shared
  state); `clean` per-character width; tree helpers' full scans;
  `Target` `#[non_exhaustive]`; the runner's
  clock through `tokio::time::Instant` (times testable); a plain
  in-memory target (an API change: ask); rustdoc without design
  history; `table_colour` `pub(crate)`; `Send + Sync` assertions;
  `write_all` retrying `EINTR`.
- exec: a failed wait marks the leader reaped (the drop guard is off)
  and reads as a spawn failure; `pty_error` keeps `io::ErrorKind`; one
  line splitter for both ways; `value()`/`secret()` take `OsString`;
  `SpawnCause` `#[non_exhaustive]` or accessors; `Send` assertions on the
  futures; the forced-exit hook's registration checked; a missing signal
  read as signal 0 and the PTY wait thread's error → `expect`; tight
  wall-clock bounds in the group tests.
- docker: `json()` and the fixtures fail loudly; bollard types
  `pub(crate)`; typed errors instead of `anyhow` in
  `ps`'s public API (start/stop/restart's are typed: the docker-control plan).
- sb: `warn!("{warning}")` as a field; one runtime builder.
