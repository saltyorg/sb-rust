# Spec: `completion` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`completion`): Tab completion for
bash, zsh and fish through clap_complete's dynamic engine (`CompleteEnv`),
one tag completer for `sb install` reading the tag cache that `ansible`
writes, and the commands that install the shell registrations
(`completion`, `completion <shell>`, `completion install [shell]`, with
the hidden `--allow-unavailable`) or print one (`completion generate
<shell>`). The build order puts it in step 5, after `ansible` (the cache
reader, `TagCache::load`). `ops` (`update`, `setup`) calls its refresh.
The research is spec-research/B-ansible.md, sections 2 ("Completion"), 6
(items 15-19) and 7b-7c.

**Its first step is a pending action of the maintainer's:** the review
of proto-complete's look and feel (capability map, open items, "Shell
completion": "review it before the `completion` module spec"; recorded in
B-ansible.md 7b). This spec is written so the review can change it; its
slices stay paused until that review (slice 0). Unpaused by the
maintainer, 2026-11-17: they test sb's real completion once the CLI is
complete, instead of reviewing proto-complete first (tasks/modules/todo.md,
M57).

## Assumptions

1. **sb-go's behaviour is the reference** (capability map decision 1):
   cmd/completion.go, cmd/tag_completion.go and completion/ (manager.go,
   detection.go, files.go, aliases.go) at sb-go's current `main`. Every
   change is listed under "Differences from sb-go".
2. **The engine is clap_complete's `unstable-dynamic`**, pinned exactly
   (`clap_complete = { version = "=4.6.11", features =
   ["unstable-dynamic"] }`, ADR-001), each call site carrying a comment
   naming ADR-001. The shell side is clap's registration stub, which
   calls sb at Tab time; no per-shell completion script is ours
   (proto-complete, FINDINGS.md).
3. **Completion runs as the calling user, without sudo** (cli-core
   decision 3; cli-core spec, startup step 3), and reads only the
   world-readable tag cache and inventory. It never runs a playbook, never
   opens the run log and writes nothing.
4. **Installing writes under `/usr/local/share` and fish's system
   directory**, so the installing commands need root; `completion
   generate` and Tab completion don't.
5. **proto-complete tested bash and zsh** with `source <(COMPLETE=<shell>
   ...)` in a PTY; fish's stub was generated but not run, and sb-go's
   install method (a file in each shell's loading directory) was not
   tried. The verification items below cover that.

## Objective

A Saltbox user presses Tab after `sb` and gets sb's commands, flags and,
after `sb install`, the Ansible tags from the cache, in bash, zsh and
fish, with nothing to set up beyond what `sb update` or `sb setup` (or
`sb completion`) installs; and an upgrade from sb-go replaces sb-go's
completion files in place.

### Tab completion

- **Entry:** the first thing in `main`, before the colour pre-pass, the
  parse, the root check or any output:
  `CompleteEnv::with_factory(Cli::command).var("SB_COMPLETE").shells(Shells(&[&Bash, &Zsh, &Fish])).complete()`
  (https://docs.rs/clap_complete/4.6.11/clap_complete/env/struct.CompleteEnv.html:
  "stdout should not be written to before or after this has run").
  Without `SB_COMPLETE` set (or set empty or `0`), it returns and sb runs
  as normal.
- **Commands and flags** come from clap's tree; subcommand descriptions
  in zsh and fish from the `about` texts.
- **Every list is in alphabetical order** (the maintainer, 2026-11-20),
  as sb-go's lists are: commands, subcommands, flags, values and tags,
  in every shell, by the candidate's text (`--color` before `help`
  before `install`). clap's engine orders them by help heading and
  declaration, and clap's stubs keep that order (bash's `-o nosort`,
  zsh's `_describe -V`, fish's `--keep-order`), so sb's reply sorts
  them (decision 11).
- **Hidden commands and flags are never offered,** whatever the typed
  prefix: `sb self<Tab>`, `sb update --skip-g<Tab>` and `sb update
  --verb<Tab>` (the hidden `--verbose`) offer none of them, as sb-go's
  cobra scripts. clap_complete 4.6.11 drops a hidden candidate only when
  a visible one also matches (`engine/complete.rs:217-218`; its
  maintainers' choice in clap issue #3951, "keep a completion hidden
  unless there are no visible choices"), and has no option for it, so sb
  filters the engine's candidates itself (decision 11). Only candidates
  for a hidden command or a hidden argument of the command being
  completed are dropped:
  - the hidden aliases of a shown command still complete on a prefix,
    as clap does (`sb edit accounts.<Tab>` gives `accounts.yml`);
  - file names keep clap's rule for dot files (only `announcements
    check`, itself hidden, completes files).

  Once a hidden command's full name is typed, its own shown flags
  complete (`sb self-update --<Tab>` gives `--yes`, `--channel`, ...),
  as cobra does: the user has named the command already, and its help
  lists those flags. The name itself is not completed (`sb
  self-update<Tab>` adds nothing), nor is a hidden flag's.
- **A failing sb prints nothing at Tab** (decision 12). clap 4.6.11's
  bash and fish stubs don't redirect the completer's stderr, so a
  missing, unexecutable or broken `/usr/local/bin/sb` printed the
  shell's or sb's error above the prompt on every Tab; its zsh stub
  already discards it (`2>/dev/null`). sb adds the same `2>/dev/null` to
  the bash and fish stubs' call, and fish uses the reply only when the
  completer exits 0 (`set` passes on the command substitution's status,
  https://fishshell.com/docs/3.3/cmds/set.html, "Exit status"); bash's
  stub already drops a reply on a non-zero exit. A missing or broken sb
  then completes nothing (bash falls back to file names, through
  `-o bashdefault`), silently, as sb-go's scripts did. clap's zsh stub
  used the completer's stdout whatever its status (its `local` hides
  it), so a program at sb's path that prints to stdout and fails put a
  word into a zsh list (`Error` on an empty word); sb's zsh stub
  declares the array apart and returns when the assignment, whose
  status is the substitution's, fails (the maintainer, 2026-11-20;
  decision 14).
- **Descriptions in bash** (the maintainer, 2026-11-20; decision 13),
  as sb-go's bash lists show them: `name  (description)`, names padded
  to the longest, each line cut to the terminal's width less one column
  with `…`. Only when readline is about to list: bash's `COMP_TYPE` `?`
  (63, the second Tab or `M-?`), `!` (33, `show-all-if-ambiguous`) or
  `@` (64, `show-all-if-unmodified`)
  (https://www.gnu.org/software/bash/manual/html_node/Bash-Variables.html#index-COMP_005fTYPE);
  never on a call that inserts (Tab, 9; menu-complete, `%`, 37;
  insert-completions), so a description can't reach the line, whatever
  the editor (ble.sh calls completion with 9 and inserts COMPREPLY).
  Only when there is more than one candidate and every one has a help
  text, so `sb install <Tab><Tab>` (tags without help, and flags) stays
  a grid of names; and only when at least 20 columns of description
  fit, else plain names. The stub passes `$COLUMNS` (bash keeps it
  current with `checkwinsize`, on by default, and on SIGWINCH) as
  `_SB_COMPLETE_COLUMNS`; without it, plain names. No switch turns
  them off (sb-go's `SB_COMPLETION_DESCRIPTIONS` is ignored; difference
  9). Names, padding and cutting are measured in display columns
  (console's `measure_text_width` and `pad_str`, term-ui's `fit`).
- **Tags for `sb install`:** an `ArgValueCompleter` on install's tags
  positional (`num_args(1..)`), with sb-go's rules
  (cmd/tag_completion.go):
  - candidates are the cache's Saltbox tags plus `sandbox-` and each
    Sandbox tag; mod tags aren't offered;
  - the part after the last comma of the current word is completed; the
    typed prefix before it, and its whitespace, are kept in each
    candidate (`plex,so` → `plex,sonarr`);
  - tags already chosen, in earlier words or earlier in this word, are
    left out;
  - sorted, unique; cached tags containing a comma or a control character
    are dropped;
  - a current word with a control character, or starting with `'` or `"`
    (fish passes an unparsed opening quote), gets no candidates;
  - no file completion; `-e` and `--skip-tags` values get no candidates;
  - an unreadable or corrupt cache gives no tags (commands and flags
    still complete).
- **Earlier words:** clap_complete's `ValueCompleter` gets only the
  current word (https://docs.rs/clap_complete/4.6.11/clap_complete/engine/trait.ValueCompleter.html),
  so the completer reads the earlier words from the request itself: the
  arguments after `--` in sb's argv, up to the index clap's stub passes
  (`_CLAP_COMPLETE_INDEX`). `UNVERIFIED`: checked against
  clap_complete 4.6.11's env/shells.rs only; the slice-1 PTY tests
  confirm it in each shell, and if a shell passes the words differently
  the earlier-word rule is dropped for it, as a listed difference.
- **Cache location:** `<Saltbox root>/cache.json` (`SB_SALTBOX_DIR`
  moves it), keyed by Saltbox's root and `<server_appdata_path>/sandbox`
  (cli-core's inventory read), through ansible's `TagCache::load`.
- **Budget:** a Tab-time run, cache read included (about 300 tags),
  within the 10 ms startup budget (CONSTRAINTS.md).
- **Facts for `sb fact get` and `sb fact list`** (*added 2026-11-22*,
  decision 15): an `ArgValueCompleter` on each positional, through the
  derive's `add`
  (https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.ArgValueCompleter.html):
  - **apps** are the `*.ini` names in the facts directory
    (`<server_appdata_path>/saltbox`, the editor's), read through
    sb-config's read-only `facts::read::Store` (one `readdir`; the
    directory is 0755);
  - **instances, then keys,** come from parsing that app's file, the app
    (and the instance) read from the earlier words with clap's own parse,
    as the tag completer reads them;
  - it reads as the calling user, without sudo: the files are 0640
    `user:user`, so the Saltbox user gets instances and keys and anyone
    else none;
  - names only; a name with whitespace or a control character is
    dropped, and so is one that doesn't start with the current word; a
    current word with a control character gets none;
  - any error (no directory, a symlink, an unreadable or broken file)
    gives no candidates; nothing is logged or written;
  - within the 10 ms budget (one `readdir`, one file of at most 1 MiB).
    Why no names cache: one a root run wrote would go stale between
    Saltbox runs, because Ansible writes the store and sb doesn't.

### `sb completion`, `sb completion <bash|zsh|fish>`, `sb completion install [bash|zsh|fish]`

sb-go's three spellings, one code path (cmd/completion.go:25-90); `sb
completion` is hidden, its subcommands aren't; sb-go's help texts (the
contract test). Bare `completion` and `completion install` without a
shell install for every shell and skip the unavailable ones; naming a
shell makes it unavailable an error (exit 1), unless `--allow-unavailable`
(hidden, on `completion install` only: released sb-go 0.1.1's
self-update runs `<new binary> completion install <shell>
--allow-unavailable`, so a binary installed by an old updater must accept
it).

- **Names:** the real executable's base name plus every symlink to it in
  `/usr/local/bin`, `/usr/bin`, `/bin` and its own directory, each
  matching `^[A-Za-z0-9][A-Za-z0-9._-]*$` (completion.go:208-250).
- **Shells** are looked for in `/usr/local/bin`, `/usr/bin` and `/bin`
  only (not `PATH`), as sb-go.
- **Bash:** needs `/usr/share/bash-completion/bash_completion` (else
  `Skipped bash completion: bash-completion is not installed`); writes
  `/usr/local/share/bash-completion/completions/<name>.bash`, each file
  holding the registration of every name (bash-completion versions differ
  on whether they look up `<name>` or `<name>.bash`).
- **Zsh:** probes `zsh -d -f -c 'printf '%s\n' "${fpath[@]}"'`; if the
  clean `fpath` lacks `/usr/local/share/zsh/site-functions`, it is skipped
  with sb-go's reason; else writes `_<name>` there, `#compdef` kept as the
  first line.
- **Fish:** probes `fish --no-config -c 'printf '%s\n%s\n' "$version"
  "$__fish_sysconf_dir"'`; needs 3.3 or newer (else skipped with sb-go's
  reason) and a clean absolute directory; writes
  `<sysconf>/completions/<name>.fish`.
- **Probes** go through exec `capture` with `env_clear`, a fixed `PATH`
  (`/usr/local/bin:/usr/bin:/bin`) and a 5 s timeout (sb-go's
  `probeTimeout`; exec assumption 4 names this case).
- **Content:** clap's stub for that shell and name, from
  `EnvCompleter::write_registration(var, name, bin, completer, buf)` in
  process (https://docs.rs/clap_complete/4.6.11/clap_complete/env/trait.EnvCompleter.html),
  with `bin` the name and `completer` that name's absolute path (so a
  symlink's file calls that symlink), then a marker line `# sb completion
  <shell> <name>` (after `#compdef` for zsh, first otherwise).
- **Writes:** atomic (`tempfile` in the same directory, 0644 set, synced,
  `persist`), directories created 0755, as sb-go.
- **Migration and cleanup, as sb-go** (manager.go:284-420): after a
  successful write, the old outputs for that name are removed
  (`/etc/bash_completion.d/<name>`,
  `/usr/share/zsh/vendor-completions/_<name>`, a bare-name bash file that
  isn't another name's); in each directory, a file for a name no longer
  installed is removed when it carries sb's marker, sb-go's marker (`#
  sb-go completion <shell> <name>`, and `# sb-go managed completion v1
  ...`) or sb-go's older scripts' text, unless an executable of that name
  still exists in those directories (then `Preserved ...`).
- **Output lines,** sb-go's texts: `Installed <shell> completion:
  <path>`, `Skipped <shell> completion: <reason>`, `Preserved <shell>
  completion: <path>; <reason>`, `Removed obsolete <shell> completion:
  <path>`, `Removed old completion: <path>`, `Failed <shell> completion:
  <reason>`. Any failure: exit 1 after every shell is tried.

### `sb completion generate <bash|zsh|fish>`

Writes clap's registration stub for `sb` to stdout (`bin` and
`completer` both `sb`, looked up on `PATH` at Tab time; zsh's last line
guarded, verification item 1). No probe, no
cache read, no root. A usage error for another shell (exit 2).

### Refresh for `update` and `setup`

`refresh(step, paths, executable)` is the install for every shell, as a
term-ui step per shell (sb-go: "Refreshing <Shell> completion",
completion.go:180-206), its result lines as the step's output; a failure
is a warning, not the command's failure (sb-go's `runner.Warning`),
including an executable `std::env::current_exe` couldn't give. `ops`
calls it with `install::Paths::for_sb()` and sb's own executable, its
tests with a temporary root.

### Differences from sb-go (the complete list)

1. `completion generate <shell>` prints clap's registration stub, which
   calls sb at Tab time (sb-go: cobra's full script). The same follows for
   the installed files. Source: ADR-001, one completer for every shell.
2. The completion request variable is `SB_COMPLETE` (sb-go: cobra's
   hidden `__complete` command). Users don't type either.
3. New files carry `# sb completion <shell> <name>` (sb-go: `# sb-go
   completion ...`); sb recognises both. sb-go doesn't recognise sb's, so
   if a host goes back to sb-go, a file sb wrote for an alias since
   removed stays (it only registers a name with no executable).
4. Elvish and PowerShell, which clap_complete has, aren't offered (sb-go
   has the three shells).
5. Should a shell pass earlier words in a way the completer can't read
   (above, `UNVERIFIED`), the earlier-word rule is dropped for that shell.
   Listed here so it isn't a silent change; expected to be empty.
   *Empty (module build M57):* every shell passes them
   (docs/platform-notes/shell-completion.md); bash and zsh pass them as
   typed, so the completer unquotes each (shlex) before reading it; bash
   splits `--flag=value` at the `=`, so a word after a lone `=` gets no
   tags and the earlier words are joined again at each `=`.
6. On an empty word after `install` or a tag, the flags are offered as
   well as the tags (clap_complete's engine offers flags on an empty
   word; sb-go's only after a `-`). Found in module build M57.
   *But where only flags would be offered on an empty word, nothing is
   offered, as sb-go* (the maintainer, 2026-11-20): `sb update <Tab>`,
   `sb version <Tab>`, `sb branch <Tab>`, `sb docker restart <Tab>`, and
   `sb install <Tab>` with no, a corrupt or an empty tag cache. clap's
   list there is all flags, so every shell inserted their common prefix,
   `--`, for the user. The flags complete once a `-` is typed. Where
   other candidates exist (subcommands, tags, values), the list is as
   before, flags, `help`, `--color` and `--help` included. sb's reply
   does it (decision 11). With no reply, bash falls back to file names
   (clap's stub registers `-o bashdefault`), as it does for sb-go's
   commands with no completion: a directory with one visible entry gets
   that name inserted, not `--`.
7. In bash, an open quote completes: `'plex, so<Tab>` becomes `'plex,
   sonarr'` (readline takes the quote off the word and closes it after
   the candidate); sb-go's bash script offered nothing there. zsh and
   fish pass the quote to sb, which offers nothing, as sb-go. Found in
   module build M57.
8. sb-go's `bashAliasNames` isn't kept: when another executable of its
   own, not a symlink to this sb, is named `<name>.bash` (or `<name>`
   for a name ending `.bash`), sb-go rewrites that executable's bash
   file with this binary's, or fails on a name collision; sb leaves the
   other executable's file (a retired one is preserved as live). No
   Saltbox name ends in `.bash`. Module build M59.
9. bash's descriptions follow sb's rule, not cobra's (decision 13):
   only on a listing call, only when every candidate has help, dropped
   below 20 columns of description, and with no off switch (sb-go's
   cobra script describes on every Tab but menu-complete and
   insert-completions, describes the candidates that have help, keeps
   unpadded descriptions down to 8 columns, and honours
   `SB_COMPLETION_DESCRIPTIONS=false`). Under ble.sh, sb's lists are
   names (ble.sh reads cobra's scripts through an adapter of its own,
   and a description given on `COMP_TYPE` 9 went into the line). The
   maintainer, 2026-11-20.

### Scope

- **Here:** `CompleteEnv` in `main`, the tag completer, the four
  installing spellings and `generate`, the probes, paths, markers,
  migration, the result lines, `refresh` for `ops`, the PTY and container
  tests.
- **Elsewhere:** calling `refresh` from `update` and `setup` (`ops`,
  module build M87). The facts completers (decision 15) are here.
  **Later:** completion for other dynamic values (none in sb-go).
- **Not here:** writing the cache (`ansible`).

## Tech Stack

Rust 1.100.0 (edition 2024). New crate `sb-completion`
(crates/completion), depending on sb-cli-core, sb-exec, sb-term-ui (the
refresh step), sb-ansible (the cache reader) and sb-config (the facts
reader, *amended 2026-11-22*).

| Concern | Crate | Version | In Cargo.lock | Docs |
|---|---|---|---|---|
| Engine, stubs | `clap_complete` `unstable-dynamic`, exact pin | =4.6.11 (newest, 2026-09-15) | yes (proto-complete) | https://docs.rs/clap_complete/4.6.11/clap_complete/env/index.html, https://github.com/clap-rs/clap/blob/clap_complete-v4.6.11/clap_complete/src/env/shells.rs |
| Tag completer | `clap_complete` `ArgValueCompleter`, `CompletionCandidate` | =4.6.11 | yes | https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.ArgValueCompleter.html |
| Cache read | `sb-ansible` (`TagCache::load`, serde_json) | workspace | — | ansible spec |
| Probes | `sb-exec` (`capture`, `env_clear`, `timeout`) | workspace | yes | docs/specs/exec.md |
| Atomic writes | `tempfile` | 3.27.0 | yes | https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist |
| Name pattern, fish version | `regex` | 1.13.1 | yes | https://docs.rs/regex/1.13.1/regex/ |
| PTY tests | `portable-pty` (sb's tests'; exec's PTY was removed, exec decision 15), or `script` as validate_config_signals.rs; the stack table's `rexpect` isn't in Cargo.lock, and adding it is Ask first | 0.9.0 | yes | https://docs.rs/portable-pty/0.9.0/portable_pty/ |

## Commands

```
cargo test -p sb-completion            # unit tests (completer, paths, markers)
cargo test -p sb --test completion     # through the binary, temporary roots
crates/completion/tests/shells.sh      # by hand, capped: real shells in ubuntu:22.04/24.04/26.04
scripts/check.sh fast | task | full
```

## Project Structure

```
crates/completion/
  src/lib.rs        → the shells, names, errors; refresh
  src/tags.rs       → the install-tag ValueCompleter (sb-go's rules)
  src/facts.rs      → the fact get / list completers (apps, instances,
                      keys), the earlier words read through tags.rs's parse
  src/install.rs    → probes, paths, writes, markers, migration, results
  tests/            → sb-go-written files for migration; tag cases
  tests/shells.sh   → containers: install, then Tab in a PTY per shell
crates/sb/src/main.rs → CompleteEnv first; the Completion commands
```

Paths (the three directories, the bash-completion file, the shell
search directories, the name search directories) live in one `Paths`
value with a root prefix, so tests install under a temporary directory.

## Code Style

```rust
fn main() -> RunEnd {
    // First, before anything is printed (clap_complete's rule). The
    // engine is unstable-dynamic, pinned exactly: ADR-001.
    // https://docs.rs/clap_complete/4.6.11/clap_complete/env/struct.CompleteEnv.html
    CompleteEnv::with_factory(Cli::command)
        .var("SB_COMPLETE")
        .shells(Shells(&[&Bash, &Zsh, &Fish]))
        .complete();
    // ... cli-core's startup as today
}
```

## Testing Strategy

No test writes outside its temporary directory or runs as root; the
binary runs with `SB_SALTBOX_DIR`, `SB_LOG_DIR` and the `Paths` root in
the test's directory.

- **The tag completer, unit:** every case of sb-go's
  cmd/tag_completion_test.go (34 cases) as a table test, plus the
  research's list: commas, kept prefix and whitespace, earlier words,
  duplicates, `sandbox-`, mod tags absent, control characters, a leading
  quote, a corrupt cache, an empty cache.
- **bash's descriptions, unit:** the listing types and the others, one
  candidate, a candidate without help, the width (padding, the cut with
  `…`, the 20-column floor, no width given), wide characters; the bash
  stub's `COLUMNS` line and zsh's status check against clap's exact
  stub text.
- **sb's reply, unit:** the filter on a small tree (a hidden command,
  a hidden flag, a hidden global `-v`, a shown command's hidden alias,
  a hidden command's shown flag, an empty word with only flags and with
  other candidates, the order) and each shell's reply, which is clap's
  format byte for byte where nothing is hidden; each stub patch against
  clap's exact stub text (a stub it doesn't match fails the test).
- **Through the binary, no shell:** `SB_COMPLETE=bash sb -- sb install
  p` (and zsh, fish) with the stub's environment, output compared;
  run as a non-root user (crates/sb/tests/unprivileged.rs's way); no run
  log created; nothing printed before the candidates; `SB_COMPLETE`
  unset, empty or `0` runs sb as normal; the startup budget measured with
  hyperfine on the release build.
- **Install, under a root prefix:** fake shells (sh scripts in the
  prefix's `bin`) answering the probes, or failing, hanging (the 5 s
  timeout) or reporting fish 3.2; the files, their mode 0644 under a 0077
  umask, the marker lines, `#compdef` first; every result line; the
  explicit-shell error and `--allow-unavailable`; migration from
  sb-go-written fixtures (current marker, the v1 marker, the older script
  text, an unmarked file kept, a live executable preserved); a symlink
  name gets its own file calling its own path.
- **Real shells, in containers, by hand and capped** (`shells.sh`; one
  container at a time, `systemd-run --scope -p MemoryMax=... -p
  CPUQuota=...`): `ubuntu:22.04`, `24.04`, `26.04` with bash and
  bash-completion, zsh and fish from apt; `sb completion install` as root,
  then a login shell per user driven in a PTY with Tab presses:
  commands, `install s<Tab>`, `plex,so<Tab>`, `sandbox-`, the earlier-word
  rule; bash's descriptions (listing, a single match, menu-complete
  cycling, `show-all-if-ambiguous`, a narrow terminal, `sb install
  <Tab><Tab>`'s grid, ble.sh's plain names); zsh with a failing
  completer; hidden commands and flags on a prefix; no `--` inserted on an
  empty word with only flags; a missing, unexecutable and failing
  `/usr/local/bin/sb`. This is where the verification items below are settled.
- Coverage and mutation as CONSTRAINTS.md.

## Verification items (facts to settle in slice 1, not questions)

1. **zsh autoload.** clap's zsh stub (#compdef, a function, then
   `compdef _clap_dynamic_completer_sb sb`) has no `funcstack` check.
   Installed as an autoloaded `_sb` in `fpath` (sb-go's method;
   proto-complete only used `source <(...)`), the first Tab may run the
   file as the function body, which only defines and registers the
   completer and completes nothing, until the second Tab. `UNVERIFIED`;
   tested in a PTY with `compinit` and the file in `fpath`. If confirmed:
   a documented line after the stub (calling the completer when the file
   runs as the autoloaded function), recorded with a "why not" in the
   code and an upstream clap issue proposed to the maintainer.
   *Settled (module builds M57 and M58, and the follow-up after M60):*
   confirmed. The same stub's trailing `compdef` also remaps `sb` to
   `_clap_dynamic_completer_sb`, which no file in `fpath` defines, when
   the file is autoloaded. A `.zcompdump` rebuilt after a completion
   keeps that mapping, and every later shell then has no sb completion
   (clap issue #6566, where clap answers that `fpath` installs aren't
   supported yet). So sb replaces that last line with clap's static
   script's guard: as the autoloaded `_sb`, the file calls the completer
   and leaves the mapping; sourced, it runs the `compdef`. Checked by
   tests/shells.sh, including a dump rebuilt after a completion. The
   comment for #6566 is drafted for the maintainer, not posted.
2. **bash loader.** clap's stub is a plain `complete -F` with no
   bash-completion helpers; the file is found only through
   bash-completion's lazy loader in
   `/usr/local/share/bash-completion/completions/`, so sb-go's
   bash-completion requirement stays. Checked: `<name>.bash` against the
   bare name on 22.04 (bash-completion 2.11), 24.04 and 26.04; the
   one-file-holds-every-name layout loads for a symlink name too.
3. **fish:** the stub's quoting (`fish_quote_for_eval`) and sb-go's
   unparsed-opening-quote case (TestNativeFishQuotedEditorCompletion), on
   fish 3.3 (22.04) and the newer releases.
4. **Earlier words** reach the completer in each shell (above).
5. **The completer path** in each installed file is that name's own
   absolute path, not `args_os()[0]` of the installing run.
6. **Startup budget** with the cache read (about 300 tags) and the
   inventory read.

## Boundaries

- **Always:** clap_complete's engine and stubs, no shell script of ours
  beyond what verification item 1 may need and decision 12's
  redirections; nothing written to stdout
  before `CompleteEnv` runs; atomic writes with 0644; sb-go's paths,
  names and texts.
- **Ask first:** moving the `clap_complete` pin (ADR-001's procedure);
  falling back to static scripts (proto-complete's fallback); a shell
  beyond the three; writing outside the three directories.
- **Never:** run a playbook or write the cache at Tab time; need root or
  sudo for Tab; remove an unmarked file; write to a real `/usr` or `/etc`
  in tests.

## Success Criteria

0. The maintainer has reviewed proto-complete's look and feel, and this
   spec reflects the review.
1. In bash, zsh and fish on 22.04, 24.04 and 26.04, after `sb completion
   install`, Tab completes commands, flags and install tags with sb-go's
   tag rules, in a new shell with no other setup (the container check).
2. Every sb-go tag-completion case passes as a unit test.
3. A Tab-time run as a non-root user reads only the cache and the
   inventory, writes nothing, and fits the 10 ms startup budget.
4. `completion install` writes sb-go's paths with clap's stubs and the
   marker, replaces sb-go's files in place, removes the old outputs sb-go
   removes, keeps unmarked and live files, and prints sb-go's lines.
5. `completion install <shell> --allow-unavailable` exits 0 for an
   unavailable shell; without the flag it exits 1; the bare forms skip.
6. `completion generate <shell>` prints the stub without root.
7. Verification items 1-6 settled, each with a test or a platform note in
   docs/platform-notes/ (versions and method, no host details).
8. The contract test passes with the `completion` group added.

## Decisions

1. **`CompleteEnv` with one `ArgValueCompleter`**, `clap_complete =
   "=4.6.11"`, called first in `main`. Source: capability map stack table
   and decision 9; ADR-001; proto-complete.
2. **The completer keeps every sb-go rule.** Source: map decision 1.
3. **Install's positional takes many values**, as sb-go's `[tags]...`
   (proto-complete took one). Source: map decision 1; ansible spec.
4. **Tab completion runs unprivileged and reads only the cache**; the
   installing forms need root, `generate` doesn't. Source: cli-core
   decision 3; cli-core spec, startup step 3.
5. **`completion generate` writes clap's stub, not a full script.**
   Source: ADR-001 (one completer for every shell).
6. **sb-go's installed paths, names, probes and migration**, with
   sb-go's markers recognised. Source: map decision 1 (replace sb-go's
   files in place; sb-go and sb-rust may alternate on a host).
7. **Probes through exec `capture`** with `env_clear`, a fixed `PATH`
   and 5 s. Source: exec assumption 4.
8. **The hidden `--allow-unavailable` stays.** Source: map decision 1
   (released sb-go 0.1.1's self-update calls it).
9. **The request variable is `SB_COMPLETE`, not clap's default
   `COMPLETE`.** Reason: an exported `COMPLETE=bash` in a user's
   environment would turn every sb run into printing a registration and
   exiting 0; a name of sb's own costs nothing (`CompleteEnv::var`).
   Decided here from the stack decision (the engine) and the
   no-surprises rule of map decision 1; the maintainer can overrule it at
   approval.
10. **The proto-complete review comes first.** Source: capability map,
    open items ("review it before the `completion` module spec");
    B-ansible.md 7b.
11. **sb filters the engine's candidates** (Tab completion, "Hidden
    commands and flags are never offered"; difference 6's empty word
    with only flags): a shell adapter of sb's own
    (`EnvCompleter`, the extension point `CompleteEnv::shells` takes)
    calls clap's engine (`clap_complete::engine::complete`), drops the
    candidates for the hidden commands and arguments of the command
    being completed, then every candidate when the current word is
    empty and all that are left are flags, and writes the reply in the
    format clap's adapter
    for that shell writes, sorted (Tab completion, "Every list is in
    alphabetical order"); registration stays clap's, whose stubs keep
    the reply's order. Why not clap's
    adapters as they are: they offer hidden items on a prefix, and
    clap_complete 4.6.11 has no switch or hook for it (no option on
    `CompleteEnv`, `Shells` or the engine). Why not a tree without the
    hidden items: the engine still has to parse them in earlier words
    (`-v`, `--skip-git`, a hidden command's own flags). An option is
    proposed upstream (drafted for the maintainer, not posted). Source:
    this spec ("hidden ones left out"); the completion test report of
    2026-11-20 (B1, B2); the maintainer, 2026-11-20 (B2, the order).
12. **The bash and fish stubs are quiet on failure** (Tab completion, "A
    failing sb prints nothing at Tab"): sb patches clap's stub text, as
    it patches zsh's last line (verification item 1), each patch matched
    against clap 4.6.11's exact text, so a clap release that changes the
    stub fails a test instead of leaving the stub unpatched. Source: the
    completion test report of 2026-11-20 (B3, B4); sb-go's scripts are
    silent there.

13. **bash shows descriptions, formatted by sb** (Tab completion,
    "Descriptions in bash"; the maintainer's decisions of 2026-11-20 on
    the bash-descriptions research): sb's bash reply (decision 11)
    writes `name  (description)` entries on a listing `COMP_TYPE`, and
    the stub passes `$COLUMNS`. Why not clap's bash adapter: it drops
    the help it has (`env/shells.rs`, `Bash::write_complete` writes only
    the value) and ignores the `COMP_TYPE` it parses; clap_complete has
    no option for descriptions in bash. Why not cobra's rule (describe on
    every Tab but `%` and `*`, formatted in the shell): under ble.sh,
    which calls completion with `COMP_TYPE` 9 and inserts what it gets,
    it put the descriptions into the line, and the shell-side loops cost
    14-22 ms a Tab against 4-5 ms. The research's prototypes measured
    the listing rule clean in 11 readline profiles and ble.sh on 22.04,
    24.04 and 26.04.
14. **zsh's stub uses a reply only when the completer exits 0**
    (Tab completion, "A failing sb prints nothing at Tab"): the patch
    declares the array apart from its assignment and returns when the
    assignment fails, matched against clap's exact text as decision 12.
    Source: the maintainer, 2026-11-20 (the zsh case of B4).
15. **`sb fact get` and `sb fact list` complete apps, instances and keys
    from the store** (Tab completion, "Facts for `sb fact get` and `sb
    fact list`"; the maintainer, 2026-11-22, on the design "Showing one
    secret", tui decisions 43 to 46). Read as the calling user, names
    only, nothing on any error, no cache. The completers live here,
    beside the tag completer and its earlier-word parse, so sb-completion
    depends on sb-config (capability map amended; deny.toml's wrappers
    follow).

## Known limits

- The candidate menu shows the whole comma list (`plex,sonarr,
  sandbox-komga`), not only the last tag (proto-complete, cosmetic; it is
  what the shell inserts).
- Tags exist for completion only after a cache write (`sb install`,
  `list`, `update`); on a new host before that, only commands and flags
  complete (sb-go the same).
- `unstable-dynamic` may change in any clap_complete release; the pin
  and ADR-001's procedure hold it, and proto-complete's static-script
  fallback is the way out.

## Open questions

None. (Decision 9 is a choice made here; it is called out for the
maintainer's approval.)

## Amendments to other specs

1. **capability map**, `completion` row's "Depends on": "cli-core, exec,
   term-ui, ansible" (the probes, the refresh step); the Commands rows of
   `completion` move to "specified"; deny.toml's wrappers follow.
2. **cli-core** ("Command tree" and startup): "`CompleteEnv` runs before
   startup step 1 when `SB_COMPLETE` is set (completion spec); such a run
   prints only candidates and is never logged."

## Suggested slices for the plan

0. **The maintainer's review of proto-complete** (not a build task; it
   gates the rest). Slices 1-4 stay paused until the maintainer has
   reviewed proto-complete's look; no plan task starts before it
   (settled 2026-11-13). The prototype gains the fish run and the fpath-file
   method first, so the review sees all three shells as installed.
1. **First vertical slice (M):** `CompleteEnv` in `main`, the tag
   completer with sb-go's cases, `completion generate`; the container
   check for the three shells settling verification items 1-6.
2. **`completion install` (M):** names, probes, paths, stubs per name,
   markers, atomic writes, result lines, `--allow-unavailable`, the three
   spellings.
3. **Migration and cleanup (S):** sb-go's files and markers, the old
   outputs, live-executable preservation, from sb-go-written fixtures.
4. **`refresh` for `ops` (S):** the per-shell steps and the
   warning on failure.
