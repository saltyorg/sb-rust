# Spec: `cli-core` (approved 2026-10-29)

Module `cli-core` in the approved capability map
([capability-map.md](capability-map.md)). It owns how `sb` starts, runs a
command and ends. Every other module builds on it.

## Objective

Give every sb-rust command the same frame:

- the command tree, global flags and help, compatible with sb-go;
- startup: host support check, root relaunch when the command needs it,
  logging, colour and theme;
- cancellation on Ctrl-C, SIGTERM and a hangup (SIGHUP), reaching every
  command;
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

`CompleteEnv` runs before startup step 1 when `SB_COMPLETE` is set
(completion spec); such a run prints only candidates and is never
logged. *Amended 2026-11-13 by the completion spec.*

1. **Host support check.** Refuse to run on anything but a supported
   Ubuntu release, for every command including `--help` and `version`, as
   sb-go does (maintainer, 2026-10-29). It reads `/etc/os-release` and
   needs no root. **One exception** (maintainer, 2026-11-18; a
   difference from sb-go): on an unsupported *Ubuntu release*, `version`
   and the standalone `self-update` run after a warning, `Ubuntu <found>
   isn't a supported release (supported versions: <list>); sb
   self-update may bring a version that supports it`. Self-update is how
   such a host reaches a binary that supports it. Another distribution,
   and every other command (`--help` and `sb update` included), still
   refuse. The supported releases list belongs to `host`;
   `cli-core` calls it through a startup hook so the dependency stays
   one-way (see Project structure).
2. **Parse arguments** with clap. Parse errors and help are printed here,
   as the calling user.
3. **Root relaunch, only for commands that need it** (maintainer,
   2026-10-30; a factual improvement under decision 1). `--help`,
   `version` and Tab completion run as the calling user, with no sudo.
   Every other command, when not root, replaces itself with
   `sudo -u root --preserve-env=VISUAL,EDITOR,SB_COLOR_PROFILE,NO_COLOR
   <exe> <args>` (exec,
   not a child as in sb-go: signals and the exit code then belong to the
   real run); if sudo asks for a password, the user answers its own
   prompt. Each command declares whether it needs root, in an exhaustive
   match, so a new command can't skip the decision.
   - **No relaunch loop:** `-u root` asks sudo for root explicitly, so
     sudo runs sb as root or refuses; without it sudo runs the command as
     `runas_default`, which a sudoers rule can set to another user, and
     the relaunched sb would relaunch again (sb-go has no `-u` and no
     guard). No marker is passed: an earlier `SB_RELAUNCHED` variable was
     refused by sudo under rules naming commands without SETENV (exec
     spec, decision 6; docs/platform-notes/exec-processes.md).
   - **No core dumps:** after the relaunch check, sb makes itself not
     dumpable (every run; a root run holds secrets) (`PR_SET_DUMPABLE` 0, falling back to
     `RLIMIT_CORE` 0), so a crash can't write its memory, secrets
     included, wherever `core_pattern` points; the kernel resets this at
     `execve`, so children are unaffected (Task 8a, security review).
   - **Startup warnings once:** the real parse runs before anything is
     printed (silently; its help, version and errors print in their place
     after the host check), so a run about to relaunch prints no startup
     warnings: the root run it hands over to reads its own settings and
     warns about those. A run that doesn't relaunch (an unsupported host,
     a failed relaunch, a command that needs no root) prints them.
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
   tracing's error field. Every record carries the run id: work sb
   moves off the run's thread (tokio's blocking pool, a thread of sb's own)
   takes the caller's span along, with tracing's `Span::current()` and
   `in_scope`, through cli-core's `spawn` (`thread`, `scoped_thread`,
   `blocking`): clippy.toml disallows the bare std and tokio spawns
   outside it (*amended 2026-11-16 by the maintainer*; CONSTRAINTS.md,
   Floor). Tasks on sb's runtime need nothing: it is
   current-thread, so they run on the run's thread, in its span; a
   multi-thread runtime would need `Instrument` on each task
   (*amended 2026-11-15 by the maintainer*: ownership records from the
   blocking pool had no run id). A panic, on any thread, leaves an ERROR record
   `panicked: <message>` with its location in the run (a panic hook
   installed once the log is open, which then calls std's own, so the
   message still reaches stderr). When the panic ends the run, that
   record takes the place of "run finished" and sb exits 101; a panic
   that is caught (a restore hook's) leaves the record and the run ends
   as it would have. Each record is one line in tracing-subscriber's
   fmt format: no logged value can break it, because the run log's event
   formatter (fmt's own `Format`, wrapped through `map_event_format`)
   escapes every control character, `\n` and `\r` included, and U+2028
   and U+2029, in the escape_debug form (`\n`, `\u{1b}`), in every record:
   its message, an error and its causes, Debug and Display (`%`) fields,
   span fields, and records bridged from dependencies' `log`, their
   target included. Messages and errors keep fmt's own `\x1b` form for
   what fmt escapes itself (ESC, BEL, BS, FF, DEL, C1). Bidi and
   zero-width marks pass, as in table cells. The guarantee is one line
   per record, not per field: a value can append text that looks like a
   field inside its own record (the first occurrence of a key is the real
   one). Help, version and completion, which run as the calling user,
   aren't logged (the directory is root-only). A log that can't be opened
   is a warning, not a failure. `SB_LOG_DIR` overrides the
   directory (tests and debugging; sudo doesn't pass it through).
5. **Colour** (decision 10): `--color=auto|always|never` (colorchoice-clap)
   decides once and becomes anstream's global choice (console and
   crossterm get it when term-ui brings them in). Help and parse errors
   follow it through that global choice, set by a pre-pass before the real
   parse (see Decided in review). Truecolor is the default; the fallback
   for other terminals (`SB_COLOR_PROFILE`, NO_COLOR) follows the colour
   ADR.
6. **Theme:** the palette roles (from proto-theme) take their values from
   `<server_appdata_path>/sb/config.toml`, `/opt/sb/config.toml` by
   default, so Saltbox backups cover it (maintainer, 2026-10-29).
   `server_appdata_path` comes from the Saltbox inventory, as sb-go's
   `layout` reads it, except that the YAML is read as Ansible's loader
   reads it: a file it refuses (a second document or a tag it has no
   constructor for among them) means `/opt`, as a parse error does, and
   nesting is limited by depth, never by length, in the YAML or the read
   (config spec, "Parsing"). *Amended by hardening Tasks 59 and 61.* A theme names a built-in theme (`default` built in)
   and can override individual palette roles. A missing file means the
   built-in theme; an invalid one is a warning (shown and logged) and the
   built-in theme is used, so a typo can't block `sb update`.
7. **Run the command** with a cancellation token.

**Command tree:** sb-go's names, subcommands and flags (decision 1). There
is no generated `completion` subcommand (completion has its own module)
and no `--version` flag (`version` is a command). Deliberate extensions:
`--color`, the one new global flag; clap's `help` subcommand, listed in
help (sb-go removes its help command; maintainer, 2026-10-30, Checkpoint
C); and `sb docker ps --json` (maintainer, 2026-11-03, docker spec
decision 2). A test compares the tree with sb-go's command
list (walked before clap adds `help`).

Further deliberate extensions (ops spec): `sb restore --user` and
`--password-file`, `sb setup`'s hidden `--verbose`, and `sb bench`'s help
text, with the contract test's `EXTENSIONS` entries `flag\tsb
restore\tlocal\tuser`, `flag\tsb restore\tlocal\tpassword-file`,
`flag\tsb setup\tglobal\tverbose` and `command\tsb bench`; `sb setup`'s
`--keep-branch` and `--reset-branch` (ops decision 29; entries
`flag\tsb setup\tglobal\tkeep-branch` and `flag\tsb
setup\tglobal\treset-branch`); and the new `--migrations` on `sb update`
(`flag\tsb update\tlocal\tmigrations`). `sb announcements` is shown where
sb-go hid it (ops decision 27): a `REWORDED` entry for its command line.
sb-go's `-v/--verbose` on the 14 commands where sb has no verbose mode
(all but `install`, whose `-v` is Ansible's, and `setup`, whose is
hidden and silent) is accepted, hidden, and changes nothing but a
notice on stderr naming the run log: a `REWORDED` entry each, the flag
hidden on sb's side (the maintainer, 2026-11-18; crates/sb/src/verbose.rs).
The notice names this run in the log as the error block does (`<file>
(run <id>)`); a run whose log couldn't open, already warned about,
says `-v isn't needed: sb has no verbose output`.
The completion run (`CompleteEnv` with `SB_COMPLETE`) comes before
startup, above. *Amended 2026-11-13 by the ops and completion specs,
2026-11-15 by the ops spec.*

**Cancellation:** the first SIGINT, SIGTERM or SIGHUP cancels a token
shared by every command; work stops at its next await or check, child
processes are killed through `exec`. A hangup (a dropped SSH session) is
handled as SIGTERM is (hardening plan decisions 2 and 8, 2026-11-12,
replacing "sb doesn't handle SIGHUP"): the children are stopped with
their usual grace, the run log gets its ending (writes to the gone
terminal fail and are ignored), then sb ends by SIGHUP. When SIGHUP was
ignored at startup (`nohup`), sb leaves it ignored and a hangup doesn't
cancel; sb reads that from `SigIgn` in `/proc/self/status`
(`proc_pid_status(5)`) before it registers any handler, and an unreadable
status counts as not ignored. A second signal ends sb at once, skipping
remaining cleanup, except a copy of the first: a SIGTERM or SIGHUP
within the repeat window (250 ms from when sb read a first SIGTERM or
SIGHUP, whichever came first; `REPEAT_WINDOW`; hardening plan decisions
2 and 9) isn't a second signal, and sb ends by the one it read first.
Such copies come from a session scope's stop (a reboot or shutdown
during an SSH session, `loginctl terminate-session`: SIGTERM, then
SIGHUP, as logind's scopes have `SendSIGHUP=yes`), from sudo, which
relays its own copy of a signal sent to the whole scope, and from an
interactive bash, which re-sends a hangup to its jobs before the kernel
hangs up the foreground group; all came within half a millisecond
(docs/platform-notes/exec-processes.md, "Doubled signals"). Two SIGINTs
still force the exit, however close (decided 2026-11-12, also for a
scope-wide SIGINT under sudo). Either way an interrupted run ends by the
signal itself, re-raised with its default action after cleanup (or after the
restore hooks), not by an exit code: a calling shell then sees the death
by signal and stops its script, as "wait and cooperative exit" requires
(exec spec, decision 7). A command can also
request shutdown with its own exit code (sb-go's `Shutdown(code)`).
Ctrl-\\ (SIGQUIT) is the hard escape (term-ui decision 5, amended by the
maintainer, 2026-11-05): the restore hooks run within their deadline, then
sb ends by it, made not dumpable first, so no core is written. While an attached
child runs, exec holds the signals (`Shutdown::hold`; exec decisions 2
and 12): SIGINT and SIGQUIT are noted, not acted on and not counted, as
the terminal delivers them to the child; a SIGTERM or SIGHUP still
cancels (exec passes a SIGTERM to the child's pid) and a second one
still forces the exit; when the child dies of a signal sb itself received, the run is
interrupted by it. The decision is one pure function (`decide`),
tested case by case. A SIGINT sb raises at itself (console does for
Ctrl-C at a prompt) is decided by `Shutdown::interrupt_raised` once the
raise returns: signal-hook's handler sets the SIGINT delivery flag
itself (`flag::register`), so a first SIGINT is recorded then, with no
wait for the signals thread, whose read of that delivery then does
nothing; a Ctrl-C delivered after it is a second signal, and one that
wouldn't be a first, or that a hold notes, is left to the thread.
The thread records a first signal under the holds' lock, so a delivery
is seen either undecided or recorded, and the first signal counted is
kept there for both (after a command's own request too). *Amended 2026-11-15
(maintainer).*

**Exit codes:** 0 success; 1 error; 2 for a parse error; death by
SIGINT, SIGTERM or SIGHUP when interrupted (`$?` reads 130, 143 or
129); a
code a command requested; a wrapped child's own code when sb only passes it
through (`sudo` relaunch, plain pass-through commands); 141, quietly, when
the reader of sb's output goes away (`sb ... | head`), as sb-go gets when
SIGPIPE kills it.

**Errors:** one block on stderr: `✗` and the message, each cause on its
own line, then the log pointer (`details: /var/log/sb/sb.log (run <id>)`)
when the run is logged. Error text can carry untrusted bytes (a file
name, a daemon's message), so every line of the block (the message, each
cause, the pointer) and every `warning:` line goes through
`exit::escaped`: its control characters, invisible and direction marks
are escaped (`\u{1b}`, `\r`) in `char::escape_debug`'s forms, while
quotes, apostrophes and backslashes stay as they are, so text escaped
already isn't escaped twice. A combining mark stays with its base
character (an NFD file name, Thai), escaped only at the start of a line,
right after one of those three or after an escaped character. sb's own
escaper does this on every toolchain: Rust 1.100's `str::escape_debug`
escapes every grapheme extender (plan decision 40). A newline in a
cause shows as a literal `\n` on that cause's one line: each cause stays
one line of the block, and none can be forged. A cancellation shows
"interrupted" rather than an error chain. Errors already shown (a failed
step's tree) exit quietly, as the runner does.

**Help and parse errors** (maintainer, 2026-10-30, after proto-help): help
follows sb-go's (fang's) format in sb's palette: the about (long text when
there is one), bold section headings, the usage line in a shaded block,
commands with fang's usage hints (`[command]`, the arguments, `[--flags]`),
flags as `-s --long` with defaults as ` (value)`, all in one column. sb
draws it from clap's own command data at startup and sets each command's
text as its `Command::before_help` under a `{before-help}` help template,
so `-h`, `--help`, `sb help <cmd>`, the colour choice and the exit code
stay clap's. A command with a shown flag that has a long help
(`Arg::long_help`) also gets a long text, its `Command::before_long_help`,
with that flag's long help in the flag's row (later lines under the help
column): clap prints it for `--help` and `sb help <cmd>`, and the short
text for `-h` (maintainer, 2026-11-14; `override_help`, used before, gives
one text for both). Help starts with one blank line above the about, as
fang's does, and ends with its last row, where fang adds one more
(maintainer, 2026-11-14): clap drops a text's first line when it is
blank, so sb's text starts with two. Bare
`sb` prints the help on stdout and exits 0, as sb-go (clap would treat it
as an error). Every help line wraps at words to the help's width: the
width of stdout's terminal, else `COLUMNS` (clap's order; fang reads
only the terminal), else 120, and never more than 120, fang's cap
(maintainer, 2026-11-14). The about wraps under its two-space indent as
fang's does; a command's or a flag's text wraps under the help column,
its later lines too (fang leaves those rows to the terminal, which wraps
them to column 0), and a blank line in a row's text (a long help's
paragraph break) is left out, so the command and flag lists have none
(maintainer, 2026-11-14). A help column that leaves fewer than 20
columns wraps the text at 20; the usage line isn't wrapped. Wrapping is
tabled's `Wrap::wrap`, as term-ui's tree wraps (textwrap isn't a
dependency). It differs from fang's in two rare cases: the line after
one that fills the width exactly breaks a column early, and fang breaks
after a hyphen and starts an overlong word on a fresh line. clap's own
wrap (`wrap_help`, `max_term_width`) stays off, since it would wrap sb's
text again from column 0. `sb help help` stays clap's own (clap adds
that subcommand after sb sets the others). Short helps fit the 120
columns in a command or flag row: a sb-go text that would wrap there is
reworded, as a difference in the spec that owns the command, and the
contract test's `REWORDED` list pairs sb-go's line with sb's
(maintainer, 2026-11-14); a test checks every row of every `-h` at 120.
A command with a long help ends its `-h --help` row with ` (--help shows
more details)`, in `-h` and `--help` alike; one without keeps sb-go's
`Help for <name>` (maintainer, 2026-11-15; fang has one help text, so
sb-go has no hint; the contract test doesn't list the help flag). A
row's indented line that wraps goes on under its own indent, or, when
it has a note after a gap of two or more spaces, under the note, so an
example's command stays whole (`-e`'s examples, ansible spec). The
note's hang is tried first, then the indent's, each only while it
leaves 20 columns; else the line goes on under the help column, as an
unindented line always does.
Parse errors (unknown command or flag, bad value) follow sb-go's layout
in sb's palette
(maintainer, 2026-11-01, look H1 from proto-help): an `ERROR` badge, clap's message with
the value you typed in the problem colour, clap's tips as `hint:` lines
(label orange, the fix green; "a similar … exists: 'x'" reads "did you
mean 'x'?") and its context lines (possible values in green), with no
pointer to help, as sb-go. They go to stderr and exit 2, the usage-error
convention (sb-go exits 1; a deliberate difference). Run errors keep sb's
`✗` block. `-v` on a command sb-go gave it is no parse error (the
maintainer, 2026-11-18; above); on one sb-go didn't, it still is.

**Build info:** version, git commit, the release channel and the pinned
uv version are set at build time (`SB_VERSION`, `SB_GIT_COMMIT`,
`SB_CHANNEL`: `release`, `beta` or `none`, unset `none`, anything else a
build error; release spec, "Release channels"). `sb version` prints
`Saltbox CLI version: <v> (commit: <c>)`, as sb-go, then `Release
channel: <channel>` (` (self-update disabled)` for `none`), and `--json`
prints `{"version", "git_commit", "uv_version", "channel"}`, sb-go's
three first. *Amended 2026-11-18: the channel replaces
`SB_DISABLE_SELF_UPDATE`.*
`BuildInfo::released`: true when `SB_VERSION` was set at build time.
Self-update treats a build that isn't released as disabled (release spec,
decision 11); `sb version` is unchanged. *Amended 2026-11-13 by the
release spec.*

## Tech stack

Rust 1.100.0, edition 2024 (`rust-toolchain.toml`). From the capability
map's stack: clap 4 (derive), colorchoice-clap, anstream/anstyle, console,
tokio (signals, cancellation via `tokio-util`'s `CancellationToken`),
tracing + tracing-subscriber + tracing-appender, rustix (euid), serde and
`toml` for the config; icu_properties for the escaper's combining marks
(plan decision 40); tabled's `Wrap::wrap` for help's line wrap (already
term-ui's; 2026-11-14). Exact versions are pinned when the crate is added;
each library pattern is checked against its docs and cited in the code
(CLAUDE.md, library first).

## Commands

```
cargo build -p sb                                   # debug build
CARGO_TARGET_DIR=target/cross-gnu cross build -p sb --profile production --target x86_64-unknown-linux-gnu  # in build/Dockerfile's ubuntu:22.04 image, ADR-020
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
- **PTY smoke test:** SIGINT and SIGTERM give `$?` 130 and 143 and restore the
  terminal. Done through util-linux `script` on the built binary with
  `sb validate-config` (config plan Task 12), as term-ui's own PTY tests
  are; rexpect isn't added (one PTY tool for every test).
- **Hangup tests** (crates/sb/tests/hangup.rs): one closes the
  terminal's master side (`script` killed), so sb's writes and terminal
  restore fail with EIO; sb still logs its ending, leaves no child and
  dies by SIGHUP (`$?` 129). A `kill -HUP` covers the plain case, and a
  run under `nohup` shows a hangup then doesn't cancel. The one that stops
  a session scope (`a_session_scope_stop_takes_the_grace`) needs systemd,
  which the gate's sandbox hides: it skips in the gate's test runs and
  runs by hand on a systemd of its own, in a container
  (`crates/sb/tests/hangup-systemd.sh`, with the container matrices).
  *Amended 2026-11-17.*
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
   sb-go's message (but `version` and `self-update` on an unsupported
   Ubuntu release: startup step 1), as sb-rust's error block on stderr (`✗ <message>`;
   sb-go prints it plainly to stdout; maintainer, 2026-10-30).
3. Every other command run as a normal user execs `sudo` with `VISUAL`
   and `EDITOR` kept, and its exit code is the command's own. If sudo
   wants a password, its own prompt appears and the run continues once
   answered.
4. SIGINT and SIGTERM end sb by that signal (`$?` 130 and 143), with the
   terminal restored; a hangup ends it by SIGHUP (`$?` 129) with its
   children stopped and the run log's ending written, except under
   `nohup`.
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
  registered terminal-restore hooks (term-ui's cursor and echoes) and
  then exits with the same code, instead of exiting inside the handler. A
  hook still running after 500 ms is abandoned and sb exits (term-ui
  Task 3). Amended 2026-11-12 (hardening plan decisions 2 and 9): a
  SIGTERM or SIGHUP within 250 ms of a first SIGTERM or SIGHUP isn't a
  second signal (a session scope's stop sends both, sudo relays copies,
  an interactive bash re-sends a hangup); SIGINT has no window.
- The root relaunch keeps `SB_COLOR_PROFILE` and NO_COLOR through sudo
  (sb-go loses them), so a user's colour settings reach root commands; an
  unknown profile warns only in the first run. An empty value is unset,
  as in sb-go; an unknown one warns where sb-go is silent.
- Under `ascii`, help and parse errors are plain, so no output is
  coloured. (Before the sb-go format, clap's 16-colour defaults applied to
  the other profiles; help now takes the theme's colours.)
- Colour profiles (ADR-002) resolve the palette once at startup with
  anstyle-lossy (16 colours matched against its VGA palette, except the
  built-in palette's reviewed picks per role, ADR-006) and are
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
  and its log records that file's warnings). `SB_SALTBOX_DIR` moves the
  Saltbox root (`/srv/git/saltbox`) the inventory is read under (tests
  and debugging, as `SB_CONFIG`; unset or empty means the default; sudo
  drops it too). The completion tag cache (`cache.json`, above) moves
  with it when the ansible and completion modules are built. *Amended
  2026-11-03 (config spec, one Saltbox root; config plan Task 3).*
  `SB_ANSIBLE_PLAYBOOK` replaces `/usr/local/bin/ansible-playbook` (tests
  and debugging; unset or empty means the default; sudo drops it).
  *Amended 2026-11-13 by the ansible spec.*
  Startup warnings (theme file, colour profile) show once, in the first
  run, and go to the run log in the run that has one. Config is read
  up to 1 MiB, the inventory whole (`read_inventory`: it is as long as
  its owner makes it; hardening Task 61); config errors are one line with
  control characters escaped (the file is group-writable). The real
  config is opened beneath
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
  overlapping a rotation finishes in the renamed copy. A SIGHUP would end
  a running command (sb handles it as a hangup, like SIGTERM), so none
  must be sent. The rule (docs/platform-notes/logrotate.md) uses
  `delaycompress` so that copy isn't compressed under a running sb, and
  `nocreate`: sb creates the file itself.
- Root: only commands that need root relaunch through `sudo`; help,
  version and Tab completion run as the calling user. (Revised twice: an
  always-root draft, as sb-go, was dropped once completion could read a
  world-readable tag cache instead of going through sudo.) A third
  revision, running most commands as the caller and relaunching on a
  permission error, was specified and built on 2026-11-17, then
  dropped by the maintainer the same day for the
  workarounds it needed: checks before any output, a restart mid-command,
  a log directory with a permissive window and a handover, and a log
  writer of sb's own. sudo up front stays, as sb-go.
- Config file ownership: like Saltbox's own configuration, the file and
  its `sb` directory belong to the Saltbox user (from `accounts.yml`, as
  sb-go's `GetSaltboxUser` reads it) and their group, with fairly open
  modes (file 0664, directory 0775): it holds no secrets. sb keeps that
  owner and mode when it writes the file, as proto-facts' saves do.

## Open questions

None.
