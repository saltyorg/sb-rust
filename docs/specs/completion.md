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
- **Commands and flags** come from clap's tree, hidden ones left out, as
  clap_complete does; subcommand descriptions in zsh and fish from the
  `about` texts.
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
`completer` both `sb`, looked up on `PATH` at Tab time). No probe, no
cache read, no root. A usage error for another shell (exit 2).

### Refresh for `update` and `setup`

`refresh(step)` is the install for every shell, as a term-ui step per
shell (sb-go: "Refreshing <Shell> completion", completion.go:180-206),
its result lines as the step's output; a failure is a warning, not the
command's failure (sb-go's `runner.Warning`). `ops` calls it.

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

### Scope

- **Here:** `CompleteEnv` in `main`, the tag completer, the four
  installing spellings and `generate`, the probes, paths, markers,
  migration, the result lines, `refresh` for `ops`, the PTY and container
  tests.
- **Later, elsewhere:** calling `refresh` from `update` and `setup`
  (`ops`); completion for other dynamic values (none in sb-go).
- **Not here:** writing the cache (`ansible`).

## Tech Stack

Rust 1.100.0 (edition 2024). New crate `sb-completion`
(crates/completion), depending on sb-cli-core, sb-exec, sb-term-ui (the
refresh step) and sb-ansible (the cache reader).

| Concern | Crate | Version | In Cargo.lock | Docs |
|---|---|---|---|---|
| Engine, stubs | `clap_complete` `unstable-dynamic`, exact pin | =4.6.11 (newest, 2026-09-15) | yes (proto-complete) | https://docs.rs/clap_complete/4.6.11/clap_complete/env/index.html, https://github.com/clap-rs/clap/blob/clap_complete-v4.6.11/clap_complete/src/env/shells.rs |
| Tag completer | `clap_complete` `ArgValueCompleter`, `CompletionCandidate` | =4.6.11 | yes | https://docs.rs/clap_complete/4.6.11/clap_complete/engine/struct.ArgValueCompleter.html |
| Cache read | `sb-ansible` (`TagCache::load`, serde_json) | workspace | — | ansible spec |
| Probes | `sb-exec` (`capture`, `env_clear`, `timeout`) | workspace | yes | docs/specs/exec.md |
| Atomic writes | `tempfile` | 3.27.0 | yes | https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist |
| Name pattern, fish version | `regex` | 1.13.1 | yes | https://docs.rs/regex/1.13.1/regex/ |
| PTY tests | `portable-pty` (exec's), or `script` as validate_config_signals.rs; the stack table's `rexpect` isn't in Cargo.lock, and adding it is Ask first | 0.9.0 | yes | https://docs.rs/portable-pty/0.9.0/portable_pty/ |

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
  src/lib.rs        → the shells, names, errors; refresh(step)
  src/tags.rs       → the install-tag ValueCompleter (sb-go's rules)
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
  rule. This is where the verification items below are settled.
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
  beyond what verification item 1 may need; nothing written to stdout
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
4. **`refresh(step)` for `ops` (S):** the per-shell steps and the
   warning on failure.
