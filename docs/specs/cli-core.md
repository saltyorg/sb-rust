# Spec: `cli-core` (approved 2026-10-29)

Module `cli-core` in the approved capability map
([capability-map.md](capability-map.md)). It owns how `sb` starts, runs a
command and ends. Every other module builds on it.

## Objective

Give every sb-rust command the same frame:

- the command tree, global flags and help, compatible with sb-go;
- startup: host support check, root relaunch when the command needs it,
  logging, colour and theme;
- cancellation on Ctrl-C and SIGTERM, reaching every command;
- one error presentation and one exit-code policy;
- build information and the `version` command.

**Users:** Saltbox users running `sb` on their server, interactively or
from the install script, cron or other scripts. **Success:** the first
vertical slice (`sb version`, then `sb docker ps` and
`sb validate-config` in their own module specs) runs on this frame with no
per-command startup code.

### Behaviour

**Startup order** (each step is one function, unit-testable on its own).
The colour choice (step 5) and the theme (step 6: the theme file, then the
colour profile) are settled before step 1, so every message follows them;
both files they read are readable without root.

1. **Host support check.** Refuse to run on anything but a supported
   Ubuntu release, for every command including `--help` and `version`, as
   sb-go does (maintainer, 2026-10-29). It reads `/etc/os-release` and
   needs no root. The supported releases list belongs to `host`;
   `cli-core` calls it through a startup hook so the dependency stays
   one-way (see Project structure).
2. **Parse arguments** with clap. Parse errors and help are printed here,
   as the calling user.
3. **Root relaunch, only for commands that need it** (maintainer,
   2026-10-30; a factual improvement under decision 1). `--help`,
   `version` and Tab completion run as the calling user, with no sudo.
   Every other command, when not root, replaces itself with
   `sudo --preserve-env=VISUAL,EDITOR,SB_COLOR_PROFILE,NO_COLOR,SB_RELAUNCHED
   <exe> <args>` (exec,
   not a child as in sb-go: signals and the exit code then belong to the
   real run); if sudo asks for a password, the user answers its own
   prompt. Each command declares whether it needs root, in an exhaustive
   match, so a new command can't skip the decision.
   - **No relaunch loop:** the relaunched run carries `SB_RELAUNCHED=1`.
     If sudo ran sb as someone other than root, it stops with
     "relaunched through sudo but still not root" instead of relaunching
     again (sb-go would loop, piling up sudo processes).
   - **Completion without root:** commands, subcommands and flags come
     from the binary. Dynamic values (Ansible tags) come from the tag cache
     (`/srv/git/saltbox/cache.json`, as sb-go), which must stay readable
     by any user: the `ansible` module writes it with mode 0644 set
     explicitly, so a restrictive umask can't narrow it (sb-go writes
     0644 too).
   - **Theme without root:** the config file is 0664 and the inventory
     that gives `server_appdata_path` is world-readable, so help and
     version use the theme too. If either can't be read, the built-in
     theme is used.
4. **Logging** (decision 11): a run record to `/var/log/sb/sb.log` (the
   directory 0700), one file that Saltbox's logrotate rotates, kept
   regardless of outcome, for every run as root. The record holds sb's
   version and the command's name, never argument values (they can carry secrets), then the command's
   events and how the run ended; a failure's error and causes go in as
   tracing's error field. Help, version and completion, which run as the
   calling user, aren't logged (the directory is root-only). A log that
   can't be opened is a warning, not a failure. `SB_LOG_DIR` overrides the
   directory (tests and debugging; sudo doesn't pass it through).
5. **Colour** (decision 10): `--color=auto|always|never` (colorchoice-clap)
   decides once and becomes anstream's global choice (console and
   crossterm get it when term-ui brings them in). clap's help and errors
   follow it through that global choice, set by a pre-pass before the real
   parse (see Decided in review). Truecolor is the default; the fallback
   for other terminals (`SB_COLOR_PROFILE`, NO_COLOR) follows the colour
   ADR.
6. **Theme:** the palette roles (from proto-theme) take their values from
   `<server_appdata_path>/sb/config.toml`, `/opt/sb/config.toml` by
   default, so Saltbox backups cover it (maintainer, 2026-10-29).
   `server_appdata_path` comes from the Saltbox inventory, as sb-go's
   `layout` reads it. A theme names a built-in theme (`default` built in)
   and can override individual palette roles. A missing file means the
   built-in theme; an invalid one is a warning (shown and logged) and the
   built-in theme is used, so a typo can't block `sb update`.
7. **Run the command** with a cancellation token.

**Command tree:** sb-go's names, subcommands and flags (decision 1). There
is no generated `completion` subcommand (completion has its own module)
and no `--version` flag (`version` is a command). Two deliberate
extensions: `--color`, the one new global flag, and clap's `help`
subcommand, listed in help (sb-go removes its help command; maintainer,
2026-10-30, Checkpoint C). A test compares the tree with sb-go's command
list (walked before clap adds `help`).

**Cancellation:** the first SIGINT or SIGTERM cancels a token shared by
every command; work stops at its next await or check, child processes are
killed through `exec`. A second signal exits at once with the same code,
skipping remaining cleanup. A command can also
request shutdown with its own exit code (sb-go's `Shutdown(code)`).

**Exit codes:** 0 success; 1 error; 130 after SIGINT, 143 after SIGTERM; a
code a command requested; a wrapped child's own code when sb only passes it
through (`sudo` relaunch, plain pass-through commands); 141, quietly, when
the reader of sb's output goes away (`sb ... | head`), as sb-go gets when
SIGPIPE kills it.

**Errors:** one block on stderr: `✗` and the message, each cause on its
own line, then the log pointer (`details: /var/log/sb/sb.log (run <id>)`)
when the run is logged. A cancellation shows "interrupted" rather than an
error chain. Errors already shown (a failed step's tree) exit quietly, as
the runner does.

**Build info:** version, git commit and the pinned uv version are set at
build time; release builds can switch self-update off. `sb version` prints
`Saltbox CLI version: <v> (commit: <c>)`, and `--json` prints
`{"version", "git_commit", "uv_version"}`, as sb-go.

## Tech stack

Rust 1.99.1, edition 2024 (`rust-toolchain.toml`). From the capability
map's stack: clap 4 (derive), colorchoice-clap, anstream/anstyle, console,
tokio (signals, cancellation via `tokio-util`'s `CancellationToken`),
tracing + tracing-subscriber + tracing-appender, rustix (euid), serde and
`toml` for the config. Exact versions are pinned when the crate is added;
each library pattern is checked against its docs and cited in the code
(CLAUDE.md, library first).

## Commands

```
cargo build -p sb                                   # debug build
cargo build -p sb --profile production --target x86_64-unknown-linux-musl
cargo test --workspace                              # all crates, prototypes included
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check -p sb -p sb-cli-core
scripts/check-commit-messages.sh <base> HEAD
```

Heavy builds and live runs are capped with `systemd-run --scope -p
MemoryMax=... -p CPUQuota=...` (CLAUDE.md).

## Project structure

```
crates/
  cli-core/        sb-cli-core: startup, clap root, flags, colour, theme,
                   logging setup, cancellation, errors, exit codes, build info
  sb/              the `sb` binary: wires modules into the command tree
prototypes/        unchanged; evidence only, never a dependency of crates/
docs/specs/        capability-map.md, cli-core.md, later module specs
docs/decisions/    ADRs
```

One crate per map module, named `sb-<module-id>`, so the compiler enforces
the map's one-way dependencies. `cli-core` depends on no other module:
startup steps that need other modules (the host check) are hooks the `sb`
crate passes in. The workspace `members` gains `crates/*`.

## Code style

Matches the prototypes: rustfmt defaults, workspace lints (`unsafe_code`
forbidden, `#[allow]` denied; justified exceptions use
`#[expect(lint, reason = "...")]` plus a `CONSTRAINTS.md` row), doc
comments that say why, library patterns cited with full URLs.

```rust
/// Exit code for a run that ended because of a signal: 128 + the signal
/// number, the shell convention (130 for SIGINT, 143 for SIGTERM).
pub fn signal_exit_code(signal: i32) -> i32 {
    128 + signal
}
```

Errors use `anyhow` in the binary and typed errors (`thiserror`) at module
boundaries that callers match on.

## Testing strategy

- **Unit tests** beside the code: startup steps, root decision per command,
  exit-code mapping, error formatting, config parsing, colour decision.
- **CLI tests** (`assert_cmd`): help, version and `--json`, parse errors,
  exit codes, `--color` with and without a terminal.
- **Contract test:** the command tree against sb-go's command and flag list.
- **PTY smoke test** (`rexpect`): SIGINT and SIGTERM give 130 and 143 and
  restore the terminal.
- Tests that need root or a real `/var/log/sb` run only when asked (an
  environment variable), never by default.
- Each new test is confirmed by breaking the code it covers. Coverage and
  other thresholds come from `CONSTRAINTS.md` once it exists.

## Boundaries

- **Always:** run tests, clippy and fmt before each commit; keep the
  `Library-check:` line; read `CONSTRAINTS.md` before writing code; keep
  sb-go compatibility unless the spec records a change.
- **Ask first:** new dependencies beyond the map's stack; any change to
  the command tree, flags or exit codes; a new unstable-feature dependency
  (needs an ADR); anything touching `/opt/sb` or `/var/log/sb` on a live
  host.
- **Never:** edit sb-go, Saltbox or other consumer repositories; edit live
  Saltbox facts or config; push, tag or publish; log secrets; weaken lints
  or thresholds without an ADR.

## Success criteria

1. `sb --help`, `sb version` and `sb version --json` run as a normal user,
   with no sudo, on a supported host, and give sb-go's output shape. Tab
   completion runs without sudo and offers commands, flags and Ansible
   tags.
2. On an unsupported host every command, including `--help`, exits 1 with
   sb-go's message, as sb-rust's error block on stderr (`✗ <message>`;
   sb-go prints it plainly to stdout; maintainer, 2026-10-30).
3. Every other command run as a normal user execs `sudo` with `VISUAL`
   and `EDITOR` kept, and its exit code is the command's own. If sudo
   wants a password, its own prompt appears and the run continues once
   answered.
4. SIGINT exits 130 and SIGTERM 143, with the terminal restored.
5. An error shows one `✗` block with its causes and the log pointer, and
   exits 1.
6. Every root run writes its run record and outcome to
   `/var/log/sb/sb.log`; logrotate's rotated copies count as sb's logs.
7. `--color=never` gives no escape codes on any path, including help.
   `--color=always` gives truecolor when piped.
8. A theme in `/opt/sb/config.toml` (or under a moved `server_appdata_path`)
   selects a built-in theme and overrides roles; no file means the built-in
   theme; an invalid file warns and falls back.
9. The command-tree contract test passes against sb-go's command list.

## Decided in review (2026-10-29)

- Invalid theme config: warn, log and use the built-in theme.
- Theme shape: named built-in themes plus per-role overrides.
- Second signal: exit at once. Amended 2026-10-30 (term-ui spec, decision
  5): the second signal is handled on the signals thread, which runs
  registered terminal-restore hooks (term-ui's cursor and `ECHOCTL`) and
  then exits with the same code, instead of exiting inside the handler.
- The root relaunch keeps `SB_COLOR_PROFILE` and NO_COLOR through sudo
  (sb-go loses them), so a user's colour settings reach root commands; an
  unknown profile warns only in the first run. An empty value is unset,
  as in sb-go; an unknown one warns where sb-go is silent.
- Under `ascii`, clap's help and errors get clap's plain styles, so no
  output is coloured; the other profiles keep clap's 16-colour defaults.
- Colour profiles (ADR-002) resolve the palette once at startup with
  anstyle-lossy (16 colours matched against its VGA palette) and are
  installed as the run's theme; sb's own messages go through anstream, so
  `--color` and NO_COLOR win. Each output library's adapter (ratatui,
  console, tabled) comes with the module that brings that library.
- Unsupported host: the message is sb-go's, shown as sb-rust's error
  block on stderr rather than plain on stdout.
- Tests that need root run only with `SB_TEST_ROOT=1` and fail, never
  skip, when asked without root; `scripts/check.sh` asks when it runs as
  root.
- `--color` reaches clap's help and errors through a pre-pass of the
  same command (`--help` and `help` made plain) that sets anstream's
  global choice before the real parse. A pre-pass that parses is final.
  When it fails, a partial parse and then clap's lexer look for the flag;
  the lexer may then mistake another option's value for it, which only
  colours clap's exit-2 error. The real parse's choice is re-applied.
- `SB_CONFIG` names the config file instead of
  `<server_appdata_path>/sb/config.toml` (tests and debugging; sudo
  doesn't pass it through, so a relaunched root run reads the real file,
  and its log records that file's warnings). Startup warnings (theme
  file, colour profile) show once, in the first run, and go to the run
  log in the run that has one. Config and inventory are read up to 1 MiB;
  config errors are one line with control characters escaped (the file is
  group-writable). The real config is opened beneath
  `<server_appdata_path>` with `openat2` (no symlinks, no mount crossing),
  non-blocking and without taking a controlling terminal, and only a
  regular file is read: a planted link, FIFO or device can't hang or
  redirect a root run.
- `SB_COLOR_PROFILE` beats the theme file's `color_profile` (ADR-002);
  empty means unset, and an unknown value warns and falls back to the
  file's profile, else truecolor. An empty `SB_CONFIG` is unset too.
- `SB_LOG_DIR` stays in release builds. The log directory must belong to
  the user running sb (another user's could hold a planted
  `sb.log` link to a root file); an existing one that isn't already 0700
  and holds anything but sb's logs (`sb.log` and logrotate's copies) is
  refused, not changed.
- One log file, `sb.log`, rotated by Saltbox's logrotate instead of daily
  files that sb prunes (maintainer, 2026-10-30): one path to follow,
  compressed history, handled like Saltbox's other logs. No signal from
  logrotate is needed or wanted: each run opens the file afresh, a run
  overlapping a rotation finishes in the renamed copy, and sb doesn't
  handle SIGHUP. The rule (docs/platform-notes/logrotate.md) uses
  `delaycompress` so that copy isn't compressed under a running sb, and
  `nocreate`: sb creates the file itself.
- Root: only commands that need root relaunch through `sudo`; help,
  version and Tab completion run as the calling user. (Revised twice: an
  always-root draft, as sb-go, was dropped once completion could read a
  world-readable tag cache instead of going through sudo.)
- Config file ownership: like Saltbox's own configuration, the file and
  its `sb` directory belong to the Saltbox user (from `accounts.yml`, as
  sb-go's `GetSaltboxUser` reads it) and their group, with fairly open
  modes (file 0664, directory 0775): it holds no secrets. sb keeps that
  owner and mode when it writes the file, as proto-facts' saves do.

## Open questions

None.
