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

**Startup order** (each step is one function, unit-testable on its own):

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
   `sudo --preserve-env=VISUAL,EDITOR <exe> <args>` (exec, not a child:
   signals and the exit code then belong to the real run); if sudo asks
   for a password, the user answers its own prompt. Each command declares
   whether it needs root; the default is yes.
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
4. **Logging** (decision 11): a run record to `/var/log/sb`, rotated
   daily by tracing-appender, kept regardless of outcome, for every run
   as root. Help, version and completion, which run as the calling user,
   aren't logged (the directory is root-only). A log that can't be opened
   is a warning, not a failure.
5. **Colour** (decision 10): `--color=auto|always|never` (colorchoice-clap)
   decides once and is passed on to anstream, console and crossterm. clap's
   own help follows it (`Command::color`). Truecolor is the default; the
   fallback for other terminals (`SB_COLOR_PROFILE`, NO_COLOR) follows the
   colour ADR.
6. **Theme:** the palette roles (from proto-theme) take their values from
   `<server_appdata_path>/sb/config.toml`, `/opt/sb/config.toml` by
   default, so Saltbox backups cover it (maintainer, 2026-10-29).
   `server_appdata_path` comes from the Saltbox inventory, as sb-go's
   `layout` reads it. A theme names a built-in theme (`default` built in)
   and can override individual palette roles. A missing file means the
   built-in theme; an invalid one is a warning (shown and logged) and the
   built-in theme is used, so a typo can't block `sb update`.
7. **Run the command** with a cancellation token.

**Command tree:** sb-go's names, subcommands and flags (decision 1). The
`help` subcommand stays hidden, there is no generated `completion`
subcommand (completion has its own module) and no `--version` flag
(`version` is a command). `--color` is the one new global flag. A test
compares the tree with sb-go's command list.

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
own line, then the log pointer (`details: /var/log/sb/<file> (run <id>)`)
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
   sb-go's message.
3. Every other command run as a normal user execs `sudo` with `VISUAL`
   and `EDITOR` kept, and its exit code is the command's own. If sudo
   wants a password, its own prompt appears and the run continues once
   answered.
4. SIGINT exits 130 and SIGTERM 143, with the terminal restored.
5. An error shows one `✗` block with its causes and the log pointer, and
   exits 1.
6. Every root run writes its run record and outcome to `/var/log/sb`;
   files rotate daily.
7. `--color=never` gives no escape codes on any path, including help.
   `--color=always` gives truecolor when piped.
8. A theme in `/opt/sb/config.toml` (or under a moved `server_appdata_path`)
   selects a built-in theme and overrides roles; no file means the built-in
   theme; an invalid file warns and falls back.
9. The command-tree contract test passes against sb-go's command list.

## Decided in review (2026-10-29)

- Invalid theme config: warn, log and use the built-in theme.
- Theme shape: named built-in themes plus per-role overrides.
- Second signal: exit at once.
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
