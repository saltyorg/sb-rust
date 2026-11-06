# sb tasks

No plan is in progress. Finished plans: [tasks/docker-control/](docker-control/plan.md)
(docker start, stop and restart), [tasks/exec/](exec/plan.md),
[tasks/config/](config/plan.md), [tasks/docker/](docker/plan.md) (`ps`),
[tasks/term-ui/](term-ui/plan.md), [tasks/cli-core/](cli-core/plan.md),
[tasks/prototypes/](prototypes/plan.md). The next plan starts its own
`tasks/plan.md` and task list here.

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
