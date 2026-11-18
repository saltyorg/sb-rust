# Spec: `ansible` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`ansible`): playbook runs by tag
(`sb install`), the tag listing and its cache (`sb list`, and the cache
that `completion`, `update`, `branch` and `branch-sandbox` share), and
`sb diag`, which is one playbook run. The build order puts it in step 3,
after `host` (its LXC and disk checks) and `repo` (the HEAD commit that
validates the cache); `completion` (step 5) reads the cache it writes, and
`ops` (step 7) calls its cache rebuild after each pull. The research is
spec-research/B-ansible.md; its one open question was decided in
decided.md (B-a).

## Assumptions

1. **sb-go's behaviour is the reference** (capability map decision 1):
   cmd/install.go, cmd/list.go, cmd/diag.go, ansible/ansible.go and
   ansible/cache.go at sb-go's current `main`. Every change is under
   "Differences from sb-go".
2. **Playbooks run through exec only.** `attached` for the runs a user
   watches (stdin is the terminal, for Ansible prompts: sb-go 225728b),
   `capture` for `--list-tags`. No shell, no argv joined into a string
   (exec spec, "A command").
3. **Paths are sb-go's** (layout/layout.go:9-48, layout/paths.go:81-122):
   `/usr/local/bin/ansible-playbook` by absolute path; Saltbox at the
   Saltbox root (`/srv/git/saltbox`, moved by `SB_SALTBOX_DIR`, cli-core
   spec), playbook `saltbox.yml`; Sandbox at `<server_appdata_path>/sandbox`,
   playbook `sandbox.yml`; saltbox_mod at `<server_appdata_path>/saltbox_mod`,
   playbook `saltbox_mod.yml`. `server_appdata_path` is cli-core's
   inventory read (crates/cli-core/src/config.rs:166), `/opt` by default.
   Each playbook runs in its repo directory, so the repo's `ansible.cfg`
   applies (Saltbox's sets `log_path = ./saltbox.log`, Sandbox's
   `./sandbox.log`: Ansible logs every run itself).
4. **Prerequisites from other plans:** host's `check_lxc`,
   `check_disk_space` and `disk_space_error` (host spec, "Setup checks"
   and "Disk space"; specified, no plan yet), and repo's HEAD commit read
   (`git rev-parse HEAD` through exec `capture`; the repo spec). The
   first slice (`sb diag`) needs neither.
5. **Ansible's exit codes** (ansible-core `lib/ansible/errors/__init__.py`,
   `ExitCode`): 0, 1, 2 (host failed), 4 (unreachable or parser error),
   5 (bad option), 99 (`KEYBOARD_INTERRUPT`), 250. On Ctrl-C
   `ansible-playbook` catches `KeyboardInterrupt`, prints "User
   interrupted execution" and exits 99; it does not die of SIGINT
   (`lib/ansible/cli/__init__.py`, `cli_executor`).
6. **Ansible's argparse refuses `--extra-vars -x=1` as two elements**
   ("expected one argument") and accepts `--extra-vars=-x=1` (checked by
   the research with Python 3.10's argparse, which ansible-core's CLI
   uses). So the one-element `=` form is the only form that passes a value
   starting with `-`.

## Objective

A Saltbox user runs roles by tag, finds the tag they want, and runs the
diagnostics role, with the same commands and results as sb-go, except
that every `-e` value reaches Ansible whole (a leading `-` included),
Ansible's own exit code comes back to the shell, no `-e` value is printed
or logged, and a broken cache rebuilds instead of failing.

### `sb install <tags>...`

Flags (sb-go's names, short letters and help texts: the contract test
checks them):

| Flag | clap | Passed to Ansible |
|---|---|---|
| `-e, --extra-vars <VARS>` | `ArgAction::Append`, no `value_delimiter`, `allow_hyphen_values(true)` | each value as one argv element `--extra-vars=<value>`, in the order given |
| `-s, --skip-tags <TAGS>` | `ArgAction::Append`, `value_delimiter(',')` (sb-go's `StringSlice`) | all values joined with `,` as one element `--skip-tags=<joined>`, to every playbook |
| `-v, --verbose` | `ArgAction::Count` | one element `-` followed by that many `v`s, to every playbook; none at 0 |
| `--no-cache` | `SetTrue` | nothing: skips the cache and tag validation |
| `--force-disk-full` (hidden) | `SetTrue` | nothing: fails with host's `disk_space_error` |

`-e`'s long help (`--help`, not the short help the contract compares)
adds how each value is passed and Ansible's forms by example (the
maintainer, 2026-11-15; difference 15):

```
Each value reaches Ansible whole, as one --extra-vars:
  -e my_var=value
  -e 'my_var=value other_var=value'
  -e '{"my_list": [1, 2]}'
  -e @vars.yml         a YAML/JSON file, relative to the repository the playbook runs in
  -e @/root/vars.yml   or an absolute path
A key=value value is passed as text; use the JSON form to pass a number or a list.
Repeat -e for more; later values win.
```

The forms are Ansible's "Defining variables at runtime"
(https://docs.ansible.com/ansible/latest/playbook_guide/playbooks_variables.html#defining-variables-at-runtime).
A relative `@` path is relative to ansible-playbook's working directory
(ansible-core's `DataLoader` starts there and the playbook CLI sets no
other base before extra vars load), which is the repository sb runs the
playbook in; later values win because each is combined over the ones
before it (`load_extra_vars`, `combine_vars`).

Steps, in sb-go's order (cmd/install.go:60-210):

1. **Arguments:** at least one (`num_args(1..)`, required; none is clap's
   usage error, exit 2). All joined with `,`, split on `,`, each trimmed,
   empties dropped, so `plex sonarr`, `plex,sonarr` and `"plex, sonarr"`
   are the same (sb-go 0058dbf). Nothing left: `no tags provided`, exit 1.
   Duplicates are kept, as sb-go.
2. **LXC:** host's `check_lxc` (sb-go runs it before parsing the tags,
   :64; so does sb).
3. **Disk:** `--force-disk-full` fails at once with
   `disk_space_error(<appdata root>)`; otherwise `check_disk_space` on `/`
   and the appdata root (`server_appdata_path`), host spec's texts.
4. **Routing:** a `mod-` prefix goes to saltbox_mod, else `sandbox-` to
   Sandbox, else Saltbox; the prefix is stripped; `mod-` is checked first
   (:136-144).
5. **Cache and validation** (skipped with `--no-cache`): the Saltbox and
   Sandbox tag lists come through the cache ("The tag cache", below), both
   loaded even when only one repo has requested tags; a repo whose entry is
   missing, empty or for another commit is rebuilt as a term-ui step. Mod
   tags are never cached or validated. A Saltbox list that fails or is
   empty is fatal: `saltbox install appears broken: tags cache missing or
   empty`; a Sandbox failure counts as no tags (logged). Each requested
   Saltbox and Sandbox tag, in order:
   - in its repo's tags: fine;
   - an exact match in the other repo: `Tag: X not present in Saltbox`
     with `Try: sandbox-X (from Sandbox)` (or the reverse);
   - a Levenshtein distance of at most 2 to a tag of its own repo
     (`strsim::levenshtein`, per Unicode scalar as sb-go's
     agnivade/levenshtein), the first lowest in cache order (strict `<`):
     `Did you mean: Y`;
   - at most 2 to a tag of the other repo: `Did you mean: sandbox-Y (from
     Sandbox)` (or the reverse);
   - else `not present in Saltbox or Sandbox` with `Add: --no-cache if
     developing your own role`.

   The suggestions, sorted by the typed tag, print as one error under
   `Tag validation found some issues:` in sb-go's layout, coloured from
   sb's palette roles (sb-go 514ecd2, 5a8135e). Any suggestion stops the
   run: exit 1, nothing runs. The runner has ended before step 6
   (exec's `attached` refuses while one is live).
6. **Runs:** Saltbox, then saltbox_mod, then Sandbox, each only if it has
   tags (:192-210). Each is
   `/usr/local/bin/ansible-playbook <playbook> --become --tags <a,b>
   [--extra-vars=<v>]... [--skip-tags=<x,y>] [-v...]`, `attached`, in the
   repo directory, environment inherited (exec adds `COLORTERM` by the
   colour profile, exec decision 3). `<playbook>` is the file name
   (`saltbox.yml`), a literal, since the run is in its directory.
   - Before each: `Executing Ansible playbook with command: <command>`,
     where `<command>` is exec's rendering with every argument as given,
     as sb-go prints it (decision 17): the program, the literals
     (`saltbox.yml`, `--become`, `--tags`, `-vvv`), the tags,
     `--extra-vars=<value>` and `--skip-tags=<x,y>`, each user value
     through exec's `shown_value` (amendment 5). The run log has the same
     rendering (exec's start record), and so has exec's failure block. A
     value the user wants kept off the screen and out of the log goes in
     an `@file` (`-e @vars.yml`): only the file's name is shown.
   - After a success: `Playbook <absolute playbook path> executed
     successfully.` (sb-go's line), then the next playbook.
   - A failure stops the rest: exec's `Ran::pass_through` (exec decision
     5) prints one error block naming the command (as shown) and its ending,
     then sb prints `Scroll up to the failed task to review.` (sb-go's
     hint) and exits with Ansible's own code: 2 for a failed host, 4, 5,
     250 and so on; 128 plus the signal for a playbook killed by a signal
     sb didn't receive.
7. **Ctrl-C during a playbook:** the terminal delivers SIGINT to Ansible
   and sb, sb holds and waits (exec decision 2). Ansible prints "User
   interrupted execution" and exits 99, not by SIGINT, so by exec's rule
   the run is not interrupted: sb passes 99 through as a failure of that
   playbook (the block names `exit 99`, the hint is replaced by `Ansible
   was interrupted.`), and later playbooks don't run. A playbook that does
   die of SIGINT (Ansible killed before its handler is set) ends sb by
   SIGINT (exec decision 7). A SIGTERM or hangup to sb reaches Ansible as
   a SIGTERM (exec, "Attached children"); sb then ends by its signal.
8. **Ctrl-C during the cache rebuild** (a `capture` child in its own
   process group) is a normal interruption: SIGTERM to the group, the step
   ends interrupted, sb ends by SIGINT, nothing runs.

Exit codes: 0; Ansible's code for a failed playbook (99 after Ctrl-C);
1 for no tags, LXC, disk, tag suggestions, a Saltbox cache failure, a
spawn failure (`ansible-playbook` missing); 2 for clap's usage errors;
death by signal for an interrupted run (cli-core).

### `sb list [query]`

- Flags: `-m, --include-mod`. sb-go's counted `-v` is dropped (exit 2), as
  in `validate-config` and `docker` (config decision 1, docker decision 6,
  term-ui decision 9): the run log holds the detail.
- Repos: Saltbox and Sandbox through the cache (rebuilt as a step when
  stale); with `-m`, saltbox_mod if its directory exists, listed fresh
  every time and never cached (list.go:119-128), in a step `Listing
  Saltbox_mod tags`; a missing directory prints `Saltbox-mod directory
  not found, skipping.  Ensure Saltbox-mod is installed.` and goes on;
  another stat error is that repo's error.
- **Steps:** every step of `sb list` (the rebuilds and the listing) is a
  transient term-ui step (decision 18): shown while it runs; when every
  step succeeded, only the tags are left, without the `sb list` title;
  when any step failed, the whole tree stays under the title, the steps
  that succeeded included, with the failure's cause. `sb install`'s
  rebuild steps stay.
- **No query** (the maintainer's "refined columns" design, 2026-11-16;
  decision 19, difference 12): sb-go's notice for a missing
  `saltbox_mod`, if any; a blank line; then for each repo a heading,
  a rule and its tags, with a blank line between repos; then a blank
  line and the footer. Nothing but the notice when no repo printed.
  - **Heading:** the repo's name (`Saltbox`, `Sandbox`, `Saltbox-mod`)
    in the heading slot, bold, then `  <N> tags` (`  1 tag`) in the count
    slot. Sandbox adds `  ·  install as ` in the hint slot and
    `sandbox-<tag>` in the command slot; Saltbox-mod the same with
    `mod-<tag>`. No cache state: whether the
    cache was used or rebuilt is in the run log only (a `tags listed`
    record for Saltbox and for Sandbox, with `cached`; a rebuild is
    also its step's records).
  - **Rule:** `─` across the whole width, in the rule slot; `-` when the
    output is plain (see **Plain**).
  - **Tags:** filled top to bottom, every column the same width in every
    repo: the widest tag of any repo shown (display width, after
    escaping, each character's width summed as the grid measures), plus 2. Each repo fits as many columns as the width holds
    (the last column needs no gap); lines carry no trailing spaces.
  - **Footer:** `sb list <query>` in the command slot and ` search` in
    the hint slot, with or without `-m` (help explains `-m`). Never after
    a search.
  - **Colours:** each slot is drawn in one theme role, set in one place,
    `COLOURS` in `crates/ansible/src/list.rs` (decision 19): heading
    `service` (and bold), count `muted`, rule `muted`, command `accent`,
    hint `muted`.
  - **Width:** stdout's terminal width; 80 when it isn't a terminal.
  - **Plain:** with colour off (piped, `--color=never`, `NO_COLOR`) or
    under `ascii`, the same lines with no styling, the rule in `-`; the
    `·` stays, as the step tree's glyphs do under `ascii`.
- **Query:** the lowercased query matched as a substring (distance 0) or
  within Levenshtein 2 of the whole tag; sorted by distance, then repo
  (Saltbox, Sandbox, mod), then tag; `Found N matching tag(s) for 'q':`
  and one term-ui table, the first repo as its heading, each later repo as
  a bold row spanning both columns (amendment 2), columns the tag and
  `sb install <prefix><tag>` (list.go:230-426). No match: `No tags found
  matching 'q'`, exit 0.
- Per-repo errors are collected; the other repos still print; then one
  error block, exit 1.

### `sb diag`

`ansible-playbook saltbox.yml --become --tags diag`, attached, in the
Saltbox repo (sb-go cmd/diag.go; attached since 93702bf), with the same
`Executing...` and success lines and the same failure, Ctrl-C and exit
rules as an `install` run. No LXC or disk check, no cache, no flags
(`cobra.NoArgs`). The capability map's row says "ops, ansible"; it needs
nothing from ops, so it lands here.

### The tag cache

- **File:** `<Saltbox root>/cache.json` (`/srv/git/saltbox/cache.json`;
  Saltbox's `.gitignore` lists it), sb-go's shape: one JSON object keyed by
  each repo's absolute path, `{"<path>": {"commit": "<sha>", "tags":
  ["…"]}}` (ansible/cache.go). Read with `serde_json` into a
  `BTreeMap<String, RepoTags>`; entries for other paths (an old appdata
  root) are kept as they are on write. Mod is never stored.
- **Valid** when the entry exists, its `tags` aren't empty and its
  `commit` equals the repo's HEAD (repo's read). Invalidated by commit
  only: uncommitted role edits need `--no-cache` (sb-go e8e7341).
- **Rebuild:** `ansible-playbook <playbook> --become --list-tags
  --skip-tags=always`, `capture`, in the repo directory, inside a term-ui
  step labelled `Updating <Saltbox|Sandbox> tags cache`, sb-go's label
(cmd/update.go:300, cmd/branch.go:95), the same in every command. Its stdout,
  after term-ui's escape stripping (defensive: Saltbox's `ansible.cfg`
  sets `force_color = True`, but ansible-core 2.21.3 prints the listing
  plain; Verification notes), is matched with `TASK TAGS:\s*\[(.*?)]`; the first
  match, split on `,` and trimmed, in Ansible's order, is the list. No
  match: an error naming the playbook. A failure ends the step ✗ with
  stderr's last lines (exec's `Failed`). Every writer uses this one
  listing: sb-go's `list` added a no-op `sanity_check` skip (the real tag
  is `sanity-check`, already `always`-tagged), which changed nothing.
- **Write:** only when an entry changed: the whole file to a
  `tempfile::NamedTempFile` in the same directory, mode set to 0644
  explicitly (completion runs unprivileged and reads it: cli-core spec,
  startup step 3), synced, then `persist` over the old file.
- **An unreadable or corrupt file** is treated as empty: logged, rebuilt
  and written over (sb-go fails `install` and `list` with "error creating
  cache").
- **API for other modules:** `TagCache::load(root)` (read only, no child:
  `completion`'s reader, which must fit the 10 ms startup budget),
  `tags(repo, step)` (load, rebuild if stale, write), and
  `refresh(repo, step)` (always rebuild and write; `update`, `branch`,
  `branch-sandbox` in `ops`), and `run(repo, tags, &RunArgs)`, one
  attached playbook run by tag with the `Executing...` and success
  lines, returning exec's `Ran` (`sb install`, `sb diag`, and ops'
  announcement migrations).

### Differences from sb-go (the complete list)

1. Each `-e` value goes as one argv element `--extra-vars=<value>` (sb-go:
   two elements, `--extra-vars <value>`, which Ansible's argparse refuses
   for a value starting with `-`). `-e -x=1` parses (`allow_hyphen_values`).
2. `--skip-tags` goes as one element `--skip-tags=<joined>` (sb-go: two
   elements), for the same reason.
3. The playbook is named by its file name in the repo directory sb runs
   in (sb-go: its absolute path). The same file runs.
4. `Executing Ansible playbook with command:` prints the full argv as
   sb-go does, the tags, skip-tags and `-e` values included, as typed; the
   line differs from sb-go's only as 1-3 change the argv. The run log and
   exec's failure block show the same. `@file` keeps a value off the
   screen and out of the log. (Decision 17, amended 2026-11-14.)
5. A failed playbook exits with Ansible's own code (sb-go: 1), with exec's
   error block and sb-go's hint line (sb-go: `playbook <path> run failed,
   scroll up to the failed task to review.\nExit code: N`).
6. **Ctrl-C during a playbook passes Ansible's exit 99 through**, after
   Ansible's own handling (sb-go: SIGKILLs `ansible-playbook` right after
   the terminal's SIGINT, so Ansible's interrupt handling and its log line
   may not finish, and exits 130). This is exec's rule for attached
   children (exec decisions 2 and 5).
7. `-v` turns on no sb-side debug lines (sb-go: `terminal.Debug` lines);
   the run log always has them (capability map decision 11).
8. `sb list` has no `-v` (exit 2). *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*
9. A cache rebuild in `install` and `list` shows as a step, labelled as
   in `update` and `branch` (`Updating <Repo> tags cache`), also when
   only the commit changed (sb-go's `install`: `INFO: Cache missing or
   incomplete, updating the cache` only for a missing or empty entry,
   nothing for a commit change; its `list`: nothing). In `list` the step
   is transient (difference 14).
10. A corrupt or unreadable cache is rebuilt (sb-go: an error).
11. The cache is written atomically, with 0644 set explicitly, and only
    when it changed (sb-go: `os.WriteFile` in place on every use).
12. `sb list` without a query has the maintainer's "refined columns"
    layout (2026-11-16, decision 19): a blank line first, a heading per
    repo with its tag count and install prefix, a rule under it, the
    columns one width across every repo (the widest tag of any repo plus
    2), and a footer hint; no `(cached)` (sb-go: `Saltbox tags:`,
    `\nSandbox tags (prepend sandbox-):`, `\nSaltbox_mod tags (prepend
    mod-):`, ` (cached)`, a blank line, and columns as wide as that repo's
    widest tag plus 2, no colour).
13. The rebuild's listing never adds `sanity_check` to the skipped tags
    (sb-go's `list` did; it changed nothing).
14. `sb list -m` shows `Listing Saltbox_mod tags` while `saltbox_mod`'s
    tags are listed (sb-go: nothing while listing); that step and `sb
    list`'s rebuild steps are gone from the output once the tags print
    when all succeeded, and all kept, with the failure's cause, when one
    failed (sb-go: nothing while listing or rebuilding; a failure shows
    only in its closing error).
15. **`sb install --help` explains `-e` with examples** (the maintainer,
    2026-11-15; the text above), and its `-h --help` row reads `Help for
    install (--help shows more details)` in `-h` and `--help` (cli-core
    spec, Help). sb-go has one help text and neither.

### Scope

- **Here:** the clap definitions of `install`, `list` and `diag`; the
  playbook paths; the argv builder; the attached runs and their lines;
  the tag cache (read, rebuild, atomic write); validation and
  suggestions; `list`'s two outputs; the API `completion` and `ops` use;
  the two amendments.
- **Later, elsewhere:** the dynamic tag completer (`completion` spec);
  `update`, `branch`, `branch-sandbox` calling `refresh` (`ops`);
  announcement migrations that run playbooks by tag (`ops`, through this
  module's run function).
- **Not changed:** mod tags stay unvalidated and uncompleted (sb-go); no
  parallel rebuild of the two repos (sb-go rebuilds them in turn).

## Tech Stack

Rust 1.100.0 (edition 2024, the workspace pin). New crate `sb-ansible`
(crates/ansible), depending on sb-cli-core, sb-term-ui, sb-exec, sb-host
and sb-repo.

| Concern | Crate | Version | In Cargo.lock | Docs |
|---|---|---|---|---|
| Flags (`Count`, `Append`, `allow_hyphen_values`, `value_delimiter`) | `clap` | 4.6.7 | yes | https://docs.rs/clap/4.6.7/clap/enum.ArgAction.html, https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.allow_hyphen_values |
| Child processes | `sb-exec` (`Cmd`, `attached`, `capture`, `Ran::pass_through`) | workspace | yes | docs/specs/exec.md |
| Suggestions | `strsim` (`levenshtein`) | 0.11.1 | yes (through clap) | https://docs.rs/strsim/0.11.1/strsim/fn.levenshtein.html |
| Cache JSON | `serde` + `serde_json` | 1.0.229 / 1.0.154 | yes | https://docs.rs/serde_json/1.0.154/serde_json/ |
| Atomic write | `tempfile` (`NamedTempFile::persist`) | 3.27.0 | yes | https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist |
| `TASK TAGS:` match | `regex` | 1.13.1 | yes | https://docs.rs/regex/1.13.1/regex/ |
| Tag columns | `uutils_term_grid` | 0.8.1 (2026-10-23) | **no: new** | https://docs.rs/uutils_term_grid/0.8.1/term_grid/ |
| Tag width (padding to the shared column width) | `unicode-width` (`UnicodeWidthStr::width`) | 0.2.3 | yes (cli-core's) | https://docs.rs/unicode-width/0.2.3/unicode_width/trait.UnicodeWidthStr.html |
| Search table | term-ui `Table` on `tabled` (`Span`) | 0.22.0 | yes | https://docs.rs/tabled/0.22.0/tabled/settings/span/struct.Span.html |

`uutils_term_grid` is the one new dependency (Ask first): uutils' `ls`
grid, about 1.2M recent downloads, from the uutils organisation. Why not
tabled without borders: tabled fills rows, not columns, and has no
"as many columns as fit" layout; sb-go's own column maths would be ours
to keep. Its binary-size cost is measured when it lands.

## Commands

```
cargo test -p sb-ansible               # unit and integration tests
cargo test -p sb --test ansible        # through the binary, fake ansible-playbook
scripts/check.sh fast | task | full    # the quality bar (CONSTRAINTS.md)
```

## Project Structure

```
crates/ansible/
  src/lib.rs        → Repo (Saltbox, Sandbox, mod), playbook paths, errors
  src/args.rs       → clap arguments for install and list; tag parsing
  src/run.rs        → the argv builder, the attached run, its lines
  src/cache.rs      → TagCache: load, rebuild, atomic write, --list-tags parse
  src/suggest.rs    → validation and the four suggestion kinds
  src/list.rs       → sb list: columns and the search table
  tests/            → fixtures: sb-go-written cache.json, --list-tags output
  tests/fixtures/fake-ansible-playbook     → sh: records argv NUL-separated, exits as told
  tests/fixtures/argparse-ansible-playbook → python3: ansible-core's -e/-v argparse definitions
crates/sb/src/main.rs → the Install, List and Diag commands; needs_root
crates/sb/tests/ansible.rs, ansible_signals.rs → through the binary
```

The playbook program is `/usr/local/bin/ansible-playbook`, replaced in
tests by `SB_ANSIBLE_PLAYBOOK` (tests and debugging; sudo drops it, as
`SB_SALTBOX_DIR`; amendment 3).

## Code Style

As the other crates: library calls cited with full URLs, `thiserror` for
the crate's errors, no `#[allow]`, no `unsafe`.

```rust
/// `-` and 255 `v`s: clap's count is a `u8`, so every `-v...` is a slice of
/// this and stays a literal the log shows (capability map, open items).
const VERBOSE: &str = match str::from_utf8(&{
    let mut bytes = [b'v'; 256];
    bytes[0] = b'-';
    bytes
}) {
    Ok(text) => text,
    Err(_) => panic!("ASCII"),
};

let mut cmd = Cmd::new(self.program.clone())
    .cwd(&repo.dir)
    .arg(repo.playbook) // "saltbox.yml": the run is in its directory
    .arg("--become")
    .arg("--tags")
    .shown_value(tags.join(","));
for value in &args.extra_vars {
    // One argv element: argparse refuses `--extra-vars -x=1` (assumption 6).
    cmd = cmd.shown_value(format!("--extra-vars={value}"));
}
if !args.skip_tags.is_empty() {
    cmd = cmd.shown_value(format!("--skip-tags={}", args.skip_tags.join(",")));
}
if args.verbose > 0 {
    cmd = cmd.arg(&VERBOSE[..=usize::from(args.verbose)]);
}
```

## Testing Strategy

Nothing touches a real host: no Ansible, no `/srv`, no root. The binary
runs with `SB_SALTBOX_DIR` and an inventory in a temporary directory,
`SB_ANSIBLE_PLAYBOOK` pointing at a fixture, and `SB_LOG_DIR` in the
test's directory; the existing harness in crates/sb/tests does the same
for validate-config.

- **The pass-through requirements (capability map open item; maintainer,
  2026-11-13).** Through the binary with `fake-ansible-playbook`, which
  writes its argv NUL-separated to a file, so an element with spaces,
  newlines or quotes is compared whole:
  - `-v` → `-v`, `-vv`, `-vvv`, `-vvvv`; `-v -v` → `-vv`; `--verbose
    --verbose --verbose` → `-vvv`; `-vvvvvvv` → `-vvvvvvv` (past Ansible's
    6, still passed); none → no `-v` element; the same element reaches
    every playbook of a run (Saltbox, mod and Sandbox tags together).
  - `-e`: each test asserts the exact element `--extra-vars=<value>`:
    multi-key JSON `{"a": 1, "b": [2, 3]}`; nested quotes
    `{"msg": "say \"hi\", ok", "q": "it's"}`; unicode
    `{"name": "Zoë 名前 🚀"}`; a value starting with `-` (`-x=1`, and
    `--no-cache` given as `-e --no-cache`, which is a value, not the flag);
    `@vars.yml` (unchanged, no file read by sb); `greeting=hello world`;
    an empty value; several `-e` in order (`-e a=1 -e '{"b": 2}' -e
    @f.yml` → three elements in that order); never split on commas,
    joined, or passed through a shell.
  - The same cases through `argparse-ansible-playbook`, a python3 script
    with ansible-core's own `-e/--extra-vars` (`action="append"`) and
    `-v` (`action="count"`) definitions, which prints what it parsed as
    JSON: each value arrives whole, a leading `-` included. (Python 3 is
    in every supported Ubuntu. The fixture follows the user's Python
    validation: ruff check and format, mypy, Bandit.)
  - A shown-value test: a token inside a `-e` value is in the
    `Executing...` line, the run log and the failure block, as given; and
    `sb install x -e '{"a": 1, "b": [2, 3]}'` shows the JSON whole in the
    line and the run log (decision 17).
- **Unit tests:** tag parsing (spaces, commas, empties, `no tags
  provided`); routing (`mod-sandbox-x` goes to mod); suggestions, each
  kind, ties (first lowest in cache order), sorting, Saltbox-empty fatal,
  Sandbox failure as no tags; `TASK TAGS:` parsing from fixtures (with and
  without colour codes, no match); cache read of an sb-go-written file,
  unknown entries kept, a corrupt file, the commit check; the rendering
  of each argv.
- **Cache write:** 0644 under a `0077` umask (a child process with its
  umask set, as host's ownership tests do), the old file replaced whole,
  no write when nothing changed (mtime kept).
- **Runs and exit codes:** the fake exits 0, 2, 4, 250: sb exits the same,
  later playbooks don't run (the argv file shows which ran); a missing
  program is a spawn error, exit 1; `--force-disk-full`; `--no-cache`
  runs without any listing.
- **Signals, on a PTY** (as validate_config_signals.rs, through `script`):
  the fake traps SIGINT, prints "User interrupted execution" and exits 99:
  sb exits 99 with the block and `Ansible was interrupted.`, and the next
  playbook doesn't run; a fake that dies of SIGINT: sb dies of SIGINT; a
  SIGTERM to sb reaches the fake; Ctrl-C during a slow `--list-tags`
  (fake sleeping): the step ends interrupted, sb dies of SIGINT, no
  playbook runs.
- **`sb list`:** snapshots (insta) of the listing at 80 and 180 columns,
  coloured and plain, piped (80, plain), and of the search at widths 40,
  80 and 120 and piped; the shared width (a long Saltbox tag widens
  Sandbox's columns), `-m` with and without the directory, the footer
  the same with and without `-m`, a failing repo between two good ones
  (exit 1, the others printed).
- **By hand, capped, in a container** (`ubuntu:24.04` with ansible-core
  from pip, a one-play playbook that prints its extra vars): the `-e`
  cases reach a real Ansible whole; Ctrl-C gives 99; a fixture of real
  `--list-tags` output under `force_color = True`, kept without host
  details. The check is `crates/ansible/tests/container.sh` (M35), with
  stand-in Saltbox and Sandbox checkouts in
  `crates/ansible/tests/fixtures/container/`. The play records the
  variables each run's `-e` values added, and the script compares them
  with what ansible-core's `load_extra_vars` makes of the whole value
  (`greeting=hello big world` gives `greeting: hello` and `_raw_params:
  big world`, and `--no-cache` gives `_raw_params: --no-cache`). A value
  split or joined on the way would give something else. An empty value
  is the exception: Ansible skips it, so only the `Executing...` line
  (`--extra-vars=` with nothing after) shows that it was passed. The
  script also checks that the line shows every value as given and that
  the run log has each one. `sb list` rebuilds both caches from the
  real listing. Ctrl-C is a `^C` written to the PTY that `script(1)`
  gives sb, which is the terminal's SIGINT to the foreground group. `sb install slow,sandbox-after` must
  then exit 99, with the play stopped and Sandbox's playbook not run (a
  run of `sandbox-after` alone shows that it would run otherwise). The
  listing goes to `fixtures/list-tags-real.txt`, and the script checks
  that it names none of the container's paths and not its host name.
  Docker caps the container at 2 CPUs and 4 GiB, and apt and pip are its
  only network use.
- Coverage and mutation as CONSTRAINTS.md.

## Boundaries

- **Always:** argv built element by element through exec; every
  user-given value through `shown_value`, never `arg`; the
  runner ended before an attached run; the cache written atomically with
  0644; sb-go's texts kept unless a difference above changes them.
- **Ask first:** a dependency beyond those named; validating or caching
  mod tags; parallel rebuilds; a cache format change (sb-go and sb-rust
  may alternate on a host).
- **Never:** a shell, or a string split into argv; split, join or rewrite
  an `-e` value; read an `@file` ourselves;
  run Ansible, or write under `/srv`, in tests.

## Success Criteria

1. Every `-v` case above reaches each playbook as one `-v...` element of
   the same count; no `-v` gives no element.
2. Every `-e` case above reaches the fake as one `--extra-vars=<value>`
   element, byte for byte and in order, and reaches the argparse fixture
   whole; `-e -x=1` parses.
3. `sb install` matches sb-go's argument handling, routing, run order,
   validation messages and success lines on the same inputs (fixtures
   from sb-go's tests and the research's cases).
4. A failed playbook's code is sb's exit code; Ctrl-C during a playbook
   gives 99 with later playbooks not run; Ctrl-C during a rebuild ends sb
   by SIGINT.
5. The `Executing...` line, the run log and exec's failure block show
   every argument as given, `-e` values included (sb-go's line); the
   shown-value tests pass.
6. `cache.json` written by sb-go is read by sb, and sb's file by sb-go's
   reader (the same JSON shape; a fixture round trip); it is 0644 after a
   write under any umask; a corrupt file is rebuilt.
7. `sb list` output matches its snapshots; the search table spans its
   repo rows; per-repo errors don't hide the other repos.
8. `sb diag` runs the `diag` tag attached and passes Ansible's code.
9. The contract test passes with `install`, `list` (minus `-v`) and
   `diag` added; `needs_root` is true for all three.
10. Binary size and startup are measured with `uutils_term_grid` linked,
    within CONSTRAINTS.md's limits.

## Decisions

1. **`-v` is counted and passed as one `-v...` argument of the same
   count**, to every playbook, no cap (clap `ArgAction::Count`). sb's own
   detail stays in the run log, unchanged by `-v`. Source: the
   maintainer's requirement of 2026-11-13 (capability map, open items);
   map decision 11; term-ui decision 9.
2. **Each `-e` value goes whole as one argv element
   `--extra-vars=<value>`**: `ArgAction::Append`, no delimiter,
   `allow_hyphen_values`; tests as listed. Source: the maintainer's
   requirement of 2026-11-13 (capability map, open items).
3. **`@file` stays as given**, resolved by Ansible in the repo directory;
   the long help says so. Source: the same requirement ("exactly as
   given").
4. **`Executing Ansible playbook with command:` prints exec's
   rendering**, which decision 17 makes every argument as given. Source:
   decided.md B-a, amended by the maintainer on 2026-11-14 (decision 17).
5. **Playbooks run `attached` in their repo directory** (stdin for
   Ansible prompts; Ansible's own `log_path` has the output, which sb's
   run log doesn't). Source: exec spec ("Ways to run"; decision 4's first
   `attached` user); sb-go 225728b.
6. **A failed playbook passes Ansible's code through**; later playbooks
   don't run. Source: exec decision 5 (`Ran::pass_through`).
7. **Ctrl-C during a playbook passes Ansible's 99 through**; during a
   cache rebuild it ends sb by SIGINT. Source: exec decisions 2, 5 and 7.
8. **The cache keeps sb-go's path and JSON shape**, moves with
   `SB_SALTBOX_DIR`, is written atomically with 0644 set, and a corrupt
   one is rebuilt. Source: map decision 1 (on-disk compatibility);
   cli-core spec, startup step 3 and `SB_SALTBOX_DIR`.
9. **Invalidation by HEAD commit only**, `--no-cache` for local role work.
   Source: map decision 1 (sb-go e8e7341); build order (repo before
   ansible).
10. **One listing for every writer** (`--list-tags --skip-tags=always`).
    Source: map decision 1 (the same tags; sb-go's extra skip is a no-op).
11. **Routing, validation, suggestions and run order as sb-go**
    (`strsim` for Levenshtein). Source: map decision 1.
12. **LXC, then disk, before anything runs** (`--force-disk-full`
    kept). Source: host spec ("Setup checks", "Disk space").
13. **The cache rebuild is a term-ui step**, ended before an attached run.
    Source: term-ui's runner; exec ("`attached` refuses while a runner is
    live").
14. **`sb list` drops `-v`.** Source: config decision 1, docker decision
    6, term-ui decision 9.
15. **`sb diag` lands here**, the same attached run with `--tags diag`.
    Source: map decision 1 (the command kept); it needs nothing from ops.
16. **`list`'s columns through `uutils_term_grid`.** Source: capability
    map library policy 2 (don't own terminal layout) and CLAUDE.md's
    Library first. Since decision 19 each cell is padded to the shared
    width first: the grid sizes each column by its own widest cell and
    has no fixed width (`GridOptions` holds only `direction`, `filling`
    and `width`), so equal cells give equal columns.
17. **Every argument sb passes to `ansible-playbook` is shown as given**,
    in the `Executing...` line, in exec's failure block and in the run
    log: the tags, `--skip-tags`, every `--extra-vars=<value>` and
    `-v...`, for every playbook run sb-ansible starts (`install`, `diag`,
    the tag listing). The line matches sb-go's, `-e` values included. A
    value to keep off the screen and out of the log goes in an `@file`:
    only `--extra-vars=@<file>` is shown, and Ansible reads the file. The
    user values go through exec's `shown_value` (amendment 5); exec's
    masking stays for every other crate's commands. *Amended 2026-11-15
    by exec's decision 14: every value sb computes is shown too; masking
    stays only for credentials.* `-e` values are the
    user's own input, not secrets sb holds, so CONSTRAINTS.md's Logging
    row ("logs never contain secret values") is kept: a secret goes in an
    `@file`, and the run log stays root's only (0700). Source: the
    maintainer, 2026-11-14 (`sb install portainer` printed `--tags [ARG]`
    where sb-go printed `--tags portainer`): nothing is masked,
    and a user keeps a value out of the logs with
    an @file. *Amended 2026-11-14; it replaces the
    decision of 2026-11-13, which masked the tags as
    `[ARG]` under exec's fail-closed rule.*
18. **Every step of `sb list` is transient** (term-ui's
    `Labels::transient`, its decision 12). `saltbox_mod`'s listing in
    `sb list -m` shows as `Listing Saltbox_mod tags`: `--list-tags` takes
    seconds with real Ansible and nothing showed meanwhile. It and the
    stale cache's rebuild steps (`Updating Saltbox tags cache`,
    `Updating Sandbox tags cache`) show while they run. When every step
    succeeded they are all gone, the runner's title with them, so only
    the tags are left. When any step failed, was interrupted or was
    stopped, the whole tree stays, the steps that succeeded included, the failed one
    with its cause; a failure is also one line of the closing error
    (`listing the tags failed`, `error running and caching ansible tags
    for <path>`). The rebuild runs through `TagCache::tags_on`, shared
    with `install`: `LazyRunner::transient` makes every step of that
    runner transient (the listing's too), and only `sb list` asks for
    it, so `install`'s rebuild steps stay. Source: the maintainer,
    2026-11-14 ("show listing saltbox_mod but it should not be in the
    output once we print"; then: the whole step tree goes when
    everything succeeds, the rebuild steps included; then, on any
    failure the whole tree stays, the steps that succeeded included).

19. **`sb list`'s refined columns** (the maintainer, 2026-11-16, from
    the design canvas the capability map's output-design phase names,
    brought forward for this command). The canvas's headings, rules,
    column-major columns and footer hint are taken, with the
    maintainer's changes: one column width across Saltbox, Sandbox and
    Saltbox-mod, so the columns line up between repos (the canvas sized
    each repo on its own); no cache note (the canvas's right-hand
    `cached` and `rebuilt, repo changed`, and sb-go's `(cached)`, are
    dropped: the run log has it); a blank line before the first heading
    (the maintainer's note of 2026-11-14: the canvas lacked one); the
    footer hints at the query only, never at `-m`, which help explains
    (the maintainer, 2026-11-16, after the colour look), and there is no
    footer after a search. **Colours:** the canvas's, through theme roles
    (the maintainer, 2026-11-16, after the first look): the heading in
    `service` (the palette's blue), bold; the commands (`sandbox-<tag>`,
    `mod-<tag>`, `sb list <query>`) in `accent` (cyan); the counts,
    the rule and the hint text in `muted` (the canvas's dim grey). The
    one place to change them is the slot table, `COLOURS` in
    `crates/ansible/src/list.rs`: one line per slot (heading, count,
    rule, command, hint), each naming a role, as motd maps its slots to
    roles; nothing else in the listing names a role. What a role looks
    like needs no code change: sb's config file
    (`<server_appdata_path>/sb/config.toml`, `/opt/sb/config.toml` by
    default) sets a role's colour under `[theme.colors]` as `#RRGGBB`,
    for example `service = "#79C0FF"` (cli-core spec, startup step 6),
    but for every screen that uses the role, not only `sb list`.
    Decided here (the brief left them open): piped
    output is the same layout without styling at 80 columns, footer
    included, so the lines a pipe gets match the screen's; plain output
    (colour off for any reason, or `ascii`) draws the rule in `-`; the
    missing-`saltbox_mod` notice keeps its place, first, before the
    blank line. Unchanged: the notice's text, the search output, the
    transient step tree.

## Known limits

- `allow_hyphen_values` makes `-e` take the next element whatever it is:
  `sb install plex -e --no-cache` passes `--no-cache` to Ansible as extra
  vars (sb-go's pflag does the same).
- `-e` values must be UTF-8 (clap's `String`; exec's `shown_value`); a
  non-UTF-8 value is a usage error, exit 2.
- Ansible's output isn't in sb's run log (exec, "Logging"); Ansible's own
  `log_path` has it.
- The `Executing...` line isn't shell-quoted (sb-go's isn't either): a
  value with spaces or quotes can't be pasted back as it is.
- A secret given in an `-e` value is on the screen and in the run log
  (decision 17); `@file` keeps it out.
- Mod tags are runnable but not validated or completed (sb-go).

## Verification notes

- **The container check (M35, 2026-11-15).**
  `crates/ansible/tests/container.sh`, under the cap, in a throwaway
  ubuntu:24.04 (24.04.5, Python 3.12.3) with ansible-core 2.21.3 from
  pip and sb at f4ff61c (a debug build). Every check passed:
  - the eleven `-e` cases (multi-key JSON, nested quotes, unicode,
    `-x=1`, `--no-cache` as a value, `@vars.yml`, `greeting=hello big
    world`, `list=a,b`, an empty value, three different values passed in
    order, and two values for one key, where the later wins) reached the play as
    ansible-core's `load_extra_vars` makes them of the whole value; the
    `Executing...` line showed each as given, and the run log had each;
  - `sb list` rebuilt both caches from the real listing, and `sb install
    sandbox-after` ran Sandbox's playbook;
  - Ctrl-C on the PTY during `sb install slow,sandbox-after`: Ansible's
    "User interrupted execution", exec's block naming exit 99, `Ansible
    was interrupted.` and sb's exit 99, with the play stopped and
    Sandbox's playbook not run;
  - the listing names none of the container's paths or its host name. It
    is `tests/fixtures/list-tags-real.txt`, and cache.rs's
    `a_real_ansible_listing_parses` reads `[baseline, check, plex, slow,
    sonarr, sonarr-config]` from it.
- **The real listing is plain.** The first run failed the script's own
  check that the listing had escape codes. Under `force_color`,
  ansible-core 2.21.3 colours its warnings but prints the `--list-tags`
  listing with a plain `display.display(msg)`
  (`lib/ansible/cli/playbook.py`). The check now asks only for the
  `TASK TAGS:` line. sb still drops escape codes before the match
  ("Rebuild"), which changes nothing on plain text, and
  `list-tags-colour.txt` keeps that path tested.
- **By hand (2026-11-15).** Real ansible-core 2.21.2 on a Saltbox host
  (Ubuntu 22.04.5), sb 24675b8, through a debug-only saltbox_mod role: `sb install
  mod-<role> -e ...`. Each value reached the play exactly as given, and
  every run ended `changed=0`:
  - `key=value` and `'a=1 b=2'`;
  - JSON with a list, typed as a list;
  - `@relative.yml` and `@/absolute.yml`;
  - two `-e` flags, where the later one wins;
  - nested JSON: dict to list to dict, a list of dicts, six levels deep,
    empty `[]` and `{}`, and lists of lists;
  - strings with commas, quotes, a backslash and unicode; `""`, `null`,
    `true`, `1.5`, `-7` and a 20-digit integer;
  - YAML flow syntax, parsed as YAML;
  - multi-line JSON;
  - a value starting with `-x=1 --not-a-flag`, received as a string;
  - a nested YAML `@file`.

  The `Executing...` line showed every value as typed; multi-line JSON
  shows its newlines escaped as `\n` on screen. Ctrl-C, confirmed by the
  maintainer earlier: exit 99 and `Ansible was interrupted.`.

## Open questions

None. The one raised in research (tags on the `Executing...` line) was decided on
2026-11-13 from recorded rules, then by the maintainer on 2026-11-14:
decision 17.

## Amendments to other specs

1. **exec** ("A command"), add after `secret(String)`: "`option_value(&'static
   str, String)` for a literal option joined to a user value in one
   element (`--extra-vars=<value>`): the child gets the two joined, the
   log and errors show the literal and `[ARG]` (`--extra-vars=[ARG]`)."
   And: "`Cmd::shown()` is public: the masked rendering the log and
   errors use, for a caller that prints the command (ansible's
   `Executing...` line)." *`option_value` was removed 2026-11-16
   (exec's decision 14); the joined option is one `shown_value`.*
2. **term-ui** ("Tables"), replace "Column spans (`sb list`) come with
   that command." with: "`Table::spanning_row(text)` adds a bold row
   across every column (`sb list`'s later repos), through tabled's
   `Span`."
3. **cli-core** (after `SB_SALTBOX_DIR`): "`SB_ANSIBLE_PLAYBOOK` replaces
   `/usr/local/bin/ansible-playbook` (tests and debugging; unset or empty
   means the default; sudo drops it)."
4. **capability map**, `ansible` row's "Depends on": "exec, host, repo,
   term-ui, cli-core" (host's LXC and disk checks); `diag`'s Commands row:
   "ansible". deny.toml's wrappers follow.
5. **exec** ("A command"), after the joined option: "`shown_value(String)`
   for a value the caller decided the log and errors show as given."
   (2026-11-14, decision 17.)

## Suggested slices for the plan

1. **First vertical slice, `sb diag` (S):** the playbook paths, the argv
   builder with exec's rendering (amendments 1 and 3), the attached run,
   the success line and pass-through, Ctrl-C and 99, through the binary
   with the fake. No host or repo prerequisite.
2. **`install` without the cache (M):** clap flags, tag parsing, routing,
   run order, the `-v` and `-e` tests with both fixtures, `--skip-tags`,
   `--no-cache` as the only path for now. Needs host's LXC and disk
   (`--force-disk-full`).
3. **The tag cache (M):** load, the listing step, the `TASK TAGS:` parse,
   commit validation through repo, the atomic 0644 write, corrupt files,
   sb-go fixtures. Needs repo's HEAD read.
4. **Validation and suggestions (S):** the four kinds, sorting, the
   fatal and lenient repo failures, the error layout.
5. **`sb list` (M):** columns with `uutils_term_grid`, `(cached)`, `-m`,
   the search table with spans (amendment 2), error collection.
6. **The container check by hand (S):** real Ansible with the `-e` cases
   and Ctrl-C; the `--list-tags` fixture.
