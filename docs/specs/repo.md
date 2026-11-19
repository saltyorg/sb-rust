# Spec: `repo` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`repo`), build order step 2
(`host` → `repo`, `release`). It is the one place sb runs git on
Saltbox's checkouts: Saltbox (`/srv/git/saltbox`) and Sandbox
(`<server_appdata_path>/sandbox`). Its first users are `sb branch` and
`sb branch-sandbox` (ops), then `sb update` (ops) and `sb setup`'s clone
(ops). It builds host's ownership part, which names `repo` its first user
(host decision 1).

## Assumptions

1. **git is the git CLI, run through sb-exec** (the maintainer's answer
   D-Q1, 2026-11-13), as sb-go does (git/git.go, git/remote.go). Saltbox's
   setup installs git, so every supported host has it.
2. **sb runs git as root in repositories the Saltbox user owns.** That
   works because Saltbox adds `/srv/git/saltbox`, sb's own checkout and
   `<appdata>/sandbox` to root's global `safe.directory`
   (https://raw.githubusercontent.com/saltyorg/Saltbox/master/roles/pre_tasks/tasks/subtasks/git.yml).
   sb doesn't add its own.
3. **The remotes are Saltbox's public https GitHub repositories** and the
   default branch is `master` for both. A user can change a remote; sb
   uses whatever `origin` is.
4. **The Saltbox user comes from `config`** (`user.name` in
   `accounts.yml`; host spec, Scope: `GetSaltboxUser` is config's). It is
   a parameter here; `repo` doesn't read Saltbox's config.
5. **Prompts aren't `repo`'s.** The branch prompt is the command's (ops):
   `repo` reports whether a choice is needed and takes the answer
   (term-ui "Prompts": every prompt has a flag; a given answer is used
   without asking).

## Objective

Saltbox's checkouts are updated and switched the same way by every
command, with sb-go's git steps, without ever waiting on a terminal, with
git's output in the step and the run log, and with the checkout owned by
the Saltbox user afterwards.

### Operations

`Repo::open(name, path)` names the repository for labels and errors
("Saltbox", "Sandbox") and checks the path: sb-go's `requireDirectory`
texts, `inspect required directory <p>: <cause>` and `<p> exists but is
not a directory` (cmd/update.go:391-400).

- **`head()`:** `git rev-parse HEAD`, captured (exec `capture`, 10 s).
  Callers compare the hash before and after an update (the tag cache).
- **`current_branch()`:** `git rev-parse --abbrev-ref HEAD`, captured;
  `HEAD` means detached (git/git.go:89-97). Error:
  `failed to get current branch: <cause>`.
- **`branch_choice(current, wanted, answer)`:** a pure function.
  - `current == wanted`: `wanted`, nothing to ask.
  - otherwise with `answer` `Reset`: `wanted`; `Keep`: `current`, with
    sb-go's info `<Repo>: Updating the current branch '<b>'`;
    no answer: `Ask`, and the caller prompts (term-ui confirm,
    `<Repo>: Do you want to reset to the '<wanted>' branch?`) or fails
    naming the flag.
  - sb-go's info `<Repo>: Currently on branch '<b>'` comes first whenever
    they differ (git/git.go:101).
  - *Amended 2026-11-18 (ops decision 39):* when the caller asked, its
    step's notice is the answer instead, `<Repo>: Kept branch '<b>' (your
    answer)` or `<Repo>: Reset from branch '<b>' to '<wanted>' (your
    answer)` (ops' `branch::resolve`, `update`'s and `setup`'s); the
    current branch was shown with the question.
- **`check_branch_name(name)`:** refuses a name that starts with `-`, and
  a name `git check-ref-format --branch <name>` rejects or rewrites (it
  expands `@{-1}`; the printed name must equal the given one). Error:
  `'<name>' is not a valid branch name`. Runs before anything changes.
- **`ensure_fetch_all_branches()`:** sb-go's (git/git.go:46-76, 2f2ac78):
  if no `remote.origin.fetch` value contains
  `refs/heads/*:refs/remotes/origin/*`, add
  `+refs/heads/*:refs/remotes/origin/*`. `git config --get-all` exits 1
  when the key is unset; that counts as "no values", not an error.
  Called by `fetch_and_reset`, so `update` gets it too (decision 6).
- **`fetch_and_reset(branch, owner, step)`:** sub-steps with sb-go's
  labels (git/git.go:196-213):
  1. "Fetching repository changes": `git fetch --progress` (a remote op,
     below).
  2. If `branch` isn't the current branch: `git rev-parse --verify
     --quiet refs/remotes/origin/<branch>` must succeed, else
     `branch '<branch>' doesn't exist on <Repo>'s remote` before anything
     in the worktree changes (decision 8).
  3. *Added 2026-11-19 (decision 12):* if `branch` is a local branch
     (`git rev-parse --verify --quiet refs/heads/<branch>`), or is the
     current branch, `git rev-parse --verify --quiet <branch>@{u}` must
     succeed, else `<Repo>: branch '<b>' has no upstream to update from`,
     before anything in the worktree changes. A branch only on the remote
     needs no check: the reset's `checkout <branch> --` creates it
     tracking `origin/<branch>` (git's checkout guess; Known limits for
     what can defeat it).
  4. "Resetting repository to <branch>": `git clean --quiet -df`,
     `git reset --quiet --hard` (HEAD, decision 7), `git checkout --quiet
     <branch> --`, `git clean --quiet -df`, `git reset --quiet --hard
     @{u}`. The upstream is checked again before that last reset, in case
     the checkout changed since step 3; it fails with the same text.
  5. "Updating git submodules": `git submodule update --progress --init
     --recursive` (a remote op).
  6. "Setting repository ownership": host's
     `ensure_owner_recursive(owner, [path])` (host spec, "Ownership").
     `owner` is the Saltbox user, or root for setup's clone, as sb-go.

  **Order** (decision 12): every refusal that can be decided before the
  worktree changes is: an invalid branch name (the callers'
  `check_branch_name`), the branch missing on the remote (step 2), no
  upstream (step 3). A refused run leaves the working tree, the index,
  HEAD and the local branches as they were, byte for byte; only the
  fetch's remote-tracking refs and the widened refspec (decision 6) have
  changed. What can only fail later (the reset itself, the submodules,
  the ownership, and git's checkout guess for a branch only on the
  remote, Known limits) fails after the reset, as before. `--keep-branch`,
  `--reset-branch` and the default path all reach `fetch_and_reset`, so
  the order holds for each: `--keep-branch` on a branch without an
  upstream, or a reset to a local branch whose upstream was removed, is
  refused with the checkout untouched.
- **`discards(current, branch)`** *(added 2026-11-19, decision 13)*:
  what `fetch_and_reset(branch)` would throw away, read before anything
  changes, each a local read (10 s), the counts as of the last fetch:
  - **local commits:** when `refs/heads/<branch>` resolves, `git
    rev-list --count <branch>@{u}..refs/heads/<branch>`, the commits the
    final reset to `@{u}` drops; a branch only on the remote has none. A
    local branch, or the current one, without an upstream is step 3's
    refusal, `<Repo>: branch '<b>' has no upstream to update from`,
    returned here, so no question is asked that would only lead to it
    (found in review, 2026-11-19);
  - **detached commits:** when `current` is `HEAD` and `branch` isn't,
    `git rev-list --count HEAD --not --branches --remotes --tags`, the
    commits only the detached HEAD reaches, which the checkout leaves
    behind;
  - **tracked changes:** `git diff --name-only -z --no-renames
    --ignore-submodules=dirty HEAD`, staged or not, what `reset --hard`
    puts back (a file touched but unchanged isn't one: git's index
    refresh);
  - **untracked files:** `git -c core.quotePath=false clean -dn`'s
    `Would remove <path>` lines (names outside ASCII as they are, as
    `diff -z` gives the tracked ones),
    exactly what the reset's first `clean -df` removes; ignored files
    aren't among them (no `-x`), so Saltbox's config stays out of it.
  `local_changes()` is the last two alone, for a checkout left as it is.
  Nothing changes but git's index stat cache, which `git diff`
  refreshes.
  The texts are `Discards::summary`: `2 local commits on 'master', 1
  commit only on the detached HEAD, 1 tracked change and 3 untracked
  files`, each part only when not zero, singular or plural as its
  number needs. Whether to ask, refuse or go on is the caller's
  (assumption 5).
- **`clone(url, branch, step)`** and **`init_from_remote(url, branch,
  step)`** for `sb setup` (G-ops research §2: `clone --progress --depth 1
  -b <b>`, then submodules; or `init -b <b>` (difference 12), `remote add origin`, `fetch --all
  --prune`, `branch <b> origin/<b>`, `reset --hard origin/<b>`,
  submodules). Specified here so the boundary is right; built with
  setup's plan. `clone` is a constructor, `Repo::clone(name, path, url,
  branch, step) -> Result<Repo, _>`, since `Repo::open` needs an existing
  directory; both check the branch name first (decision 3). On failure
  they leave what sb-go leaves: git removes a failed clone's directory
  itself, a failed submodule update after a clone leaves the checkout,
  and a failed `init_from_remote` leaves what its earlier steps made (the
  decision for M8, 2026-11-13; see Known limits).

### Remote operations

Every fetch, clone and submodule update goes through one wrapper
(git/remote.go:17-56, 85440d7):

- The child is `git` with exec's environment rules
  (`GIT_TERMINAL_PROMPT=0`, built) plus `LC_ALL=C` (decision 4), streamed
  into the step (exec `stream`), no timeout: Ctrl-C and SIGTERM stop it
  through exec's process group.
- If it fails and its stderr tail (exec keeps the last 64 KiB) contains
  `could not read `, `for 'https://github.com` and `terminal prompts
  disabled`, it runs once more as `git -c http.version=HTTP/1.1 <args>`
  under a child step with sb-go's labels: `Retrying <Repo> Git <op> over
  HTTP/1.1` / `Retried <Repo> Git <op> over HTTP/1.1` / `<Repo> Git <op>
  retry over HTTP/1.1`, where `<op>` is the first argument, or the first
  two for `submodule update` (git/remote.go:82-90).
- If the retry fails the same way: sb-go's error, word for word:
  `GitHub unexpectedly requested authentication for the public <Repo>
  repository.` and its two paragraphs (git/remote.go:131-142), then
  `Git details:` with git's last lines (sanitized, term-ui `DisplayText`).
- Any other failure is exec's `Failed` (the command and its exit), with
  the step showing git's last lines (exec, "Outcome and errors").

### Never waiting on a terminal

- git's own credential prompt: `GIT_TERMINAL_PROMPT=0` (exec, built).
- ssh's prompts (host key, passphrase) for a user-configured ssh remote:
  exec's amendment below (exec-M1), so ssh asks no terminal and fails at
  once.
- No child gets sb's stdin (exec's rule for piped children).

### Differences from sb-go (the complete list)

1. Branch names are checked before use (`check_branch_name`); sb-go
   passed any name to `git checkout`, so `-x` reached it as an option
   (decision 3).
2. `fetch_and_reset` widens the fetch refspec on every call, so `update
   --reset-branch` works on a single-branch clone of another branch; sb-go
   did it in `branch` only (decision 6).
3. The reset before `checkout` is to `HEAD`, not `@{u}`: it discards the
   same local changes, and doesn't fail on a detached HEAD or a branch
   without an upstream, where sb-go stopped (decision 7).
4. A branch missing on the remote fails before the worktree changes,
   naming it; sb-go cleaned and reset first, then failed with git's
   `pathspec ... did not match` (decision 8).
5. `checkout` gets `--` after the branch, so a branch can't be read as a
   path.
6. git's children run with `LC_ALL=C`, so the HTTP/1.1 retry's text
   match holds in any locale; sb-go matched English text under the user's
   locale (decision 4).
7. ssh never prompts on the terminal (exec amendment); sb-go's git could
   hang on an ssh host-key or passphrase prompt.
8. git's output streams into the step and the run log; sb-go captured it
   and printed it only in errors (decision 11).
9. A failure's error names the command and exit, with git's lines in the
   step's output; sb-go appended the whole combined output to the error
   text.
10. No custom hook commands: no caller passes any today (b5d4a4d removed
    the last), so the parameter is gone.
11. The prompt itself is the caller's (assumption 5); `branch_choice`
    gives `Ask`. sb-go's prompt read stdin and kept the current branch on
    anything but `y`, on a read error and with no TTY (ops section lists
    the command-level differences).
12. `init_from_remote` runs `git init -b <branch>`; sb-go's plain `git
    init` left HEAD on git's default branch (`master`, or root's
    `init.defaultBranch`), so `-b develop`, or a `main` default, ended on
    that branch at the wanted commit with no upstream, and the next
    `update` asked to switch (found in M8's review, 2026-11-13).
13. A branch without an upstream is refused before the worktree changes
    (decision 12); sb-go ran `clean -df` first, so untracked files were
    lost before its `reset --hard @{u}` failed.

14. *Added 2026-11-19 (decision 13):* `discards` says what an update
    would throw away before anything changes, so the commands can ask;
    sb-go discarded local commits, tracked changes and untracked files
    without a word.

### Scope

- **Here:** the operations above, the remote wrapper, the branch rules,
  host's `ensure_owner` and `ensure_owner_recursive` (as host.md
  specifies them), the exec amendments, tests against local repositories.
- **Later (with their commands):** `clone` and `init_from_remote` (setup's
  plan); the commands themselves (ops). Not here: the Saltbox user
  lookup (config), the runtime steps and the tag cache (python-env,
  ansible).

## Tech Stack

- **git**, the host's, through sb-exec's `Cmd`: `capture` for the local
  reads (`rev-parse`, `config --get-all`, `check-ref-format`), `stream`
  into term-ui steps for everything that changes the checkout.
- **sb-host** `ensure_owner_recursive`, on `walkdir` 2.5.0 (in
  Cargo.lock) and `nix` 0.28 (`user`, already linked; host decision 2).
- **sb-term-ui** `Step` for sub-steps and notices.
- No new crate. `thiserror` for the error type (workspace).

**Why not gix** (decision 1; https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md,
gix 0.90.0, 2026-10-22):
- crate-status leaves `checkout, switch, restore and reset orchestration
  over refs, index and worktree mutation` unchecked; `clean` only lists
  untracked files; submodules are read-only. sb would hand-write reset,
  checkout, clean and submodule update, which Library first forbids.
- Its HTTP transports are blocking only (`blocking-http-transport-reqwest`
  and `-curl`; https://docs.rs/crate/gix/latest/features), against
  capability map decision 8 (no blocking-HTTP crate).
- **Revisit when** crate-status checks the checkout/reset orchestration
  item and gix has an async HTTP transport.

**Why not git2** (0.21.0): libgit2 is C, its https on Linux goes through
openssl-sys (against the rustls/aws-lc-rs stack and the pure-Rust
preference), `clean` would still be hand-written, and its ownership rules
aren't git's `safe.directory`.

## Commands

```
Check: scripts/check.sh fast | task | full
Test:  systemd-run --scope -p MemoryMax=4G -p CPUQuota=200% cargo test -p sb-repo -p sb-host
Root:  SB_TEST_ROOT=1 SB_LOG_DIR=<scratch> cargo test -p sb-repo --test owner   (as root)
```

## Project Structure

```
crates/repo/              sb-repo
  src/lib.rs              Repo, errors, branch_choice, check_branch_name
  src/git.rs              the git Cmd (environment, LC_ALL=C)
  src/remote.rs           the remote wrapper, HTTP/1.1 retry, the public-repo error
  src/update.rs           ensure_fetch_all_branches, fetch_and_reset
  tests/                  local bare repositories (file:// remotes)
crates/host/src/owner.rs  ensure_owner, ensure_owner_recursive (host spec)
crates/exec/src/run.rs    the two amendments
docs/specs/repo.md        this spec
```

deny.toml: `sb-repo` added to the wrappers of `sb-exec`, `sb-host` and
`sb-term-ui`; its own entry allows only `sb` until ops exists (then the
ops crate).

## Code Style

Pure rules, tested first; the git calls thin around them.

```rust
/// Whether the update needs the user's answer (git/git.go:80-134).
#[must_use]
pub fn branch_choice(current: &str, wanted: &str, answer: Option<Answer>) -> Choice {
    if current == wanted {
        return Choice::Use(wanted.to_owned());
    }
    match answer {
        Some(Answer::Reset) => Choice::Use(wanted.to_owned()),
        Some(Answer::Keep) => Choice::Keep(current.to_owned()),
        None => Choice::Ask,
    }
}
```

## Testing Strategy

No test touches a Saltbox checkout or the network.

- **Unit:** `branch_choice` (every row, detached `HEAD` included),
  `check_branch_name` (`-x`, `--force`, `a..b`, `@{-1}`, `foo.lock`, an
  empty name, unicode, a valid `feature/x`), the retry match (all three
  phrases, each one missing, under different case), the `<op>` label.
- **Local repositories:** each test makes a bare "remote" in a temp dir
  (`git init --bare`, commits on `master` and `develop`, a submodule
  repository) and a checkout of it with `git clone --depth 1 -b master
  file://...` (the single-branch shape setup makes). Through the real
  git:
  - `fetch_and_reset` to the same branch after the remote moved: HEAD
    equals the remote's; a modified tracked file and an untracked file
    are gone; an **ignored** file (as accounts.yml is) survives.
  - switching to `develop` on the single-branch clone: the refspec is
    widened once (a second call adds nothing) and HEAD is develop's.
  - a missing branch: the error, and the worktree unchanged (a marker
    file still there).
  - a detached HEAD with `Reset`: succeeds; a local branch without an
    upstream kept: the "no upstream" error.
  - a submodule added upstream is initialised.
  - `head()` before and after differs; `current_branch()` on detached is
    `HEAD`.
  - `discards` (2026-11-19): a clean clone has none; a local commit, a
    staged and an unstaged edit, an untracked file and directory are
    each counted, an ignored file and a touched-but-unchanged file
    aren't; a switch counts the target's commits, not the current
    branch's; a detached HEAD's own commits are counted, not once a tag
    holds them; a branch only on the remote has no commits to lose; a
    kept branch without an upstream is the "no upstream" refusal; a name
    outside ASCII comes as it is; the reads change nothing (the tree
    byte for byte, the refs).
- **HTTP/1.1 retry:** a stand-in `git` first on `PATH` (as exec's tests
  use stand-in programs) that prints the three phrases and exits 128
  unless its arguments start with `-c http.version=HTTP/1.1`, then runs
  the real git. Cases: the retry succeeds (child step drawn, its labels);
  it fails the same way (the public-repo error text, exact); it fails
  otherwise (exec's error); the stand-in records `LC_ALL=C` and
  `GIT_TERMINAL_PROMPT=0`.
- **No terminal wait:** a stand-in `ssh` (through a `ssh://` remote) that
  records its environment and exits 255: the fetch fails at once and the
  environment holds exec's ssh variables (amendment below). A real-ssh
  check in an `ubuntu:<release>` container with sshd on loopback (an
  unknown host key, then a passphrase key) is recorded once in a platform
  note: ssh fails without reading the terminal.
- **Ownership (root):** `SB_TEST_ROOT`, a tree in a temp dir chowned to
  `nobody` (symlink not followed, a vanishing entry skipped, the count in
  the run log), as host.md's Testing Strategy describes.
- **Cancellation:** a stand-in `git` that sleeps; SIGINT stops the fetch
  through exec's group kill, the step ends "interrupted".
- **Gates:** CONSTRAINTS.md as always.

## Boundaries

- **Always:** run git with exec's environment plus `LC_ALL=C`; check a
  branch name before it reaches git; test against temp-dir repositories.
- **Ask first:** any git option that removes more than sb-go's commands
  do; any new crate; adding `safe.directory` entries from sb.
- **Never:** `git clean -x` or `-X` (Saltbox's config files are ignored
  files in the checkout); run git attached to the terminal; touch a real
  Saltbox checkout in tests; read `/srv/git/saltbox` for research.

## Success Criteria

1. `branch_choice` and `check_branch_name` pass every listed case, each
   test failing against a stub first.
2. On a single-branch clone, `fetch_and_reset("develop")` ends on develop
   with the remote's HEAD, the refspec widened exactly once, the ignored
   file kept, tracked changes and untracked files gone, the submodule
   initialised, ownership applied (root test).
3. A missing branch fails with the named error and a byte-identical
   worktree.
4. The HTTP/1.1 retry runs once, only on the three-phrase match, with
   sb-go's labels; a repeat gives sb-go's public-repo text exactly.
5. Every git child has `GIT_TERMINAL_PROMPT=0` and `LC_ALL=C`; a
   prompting ssh ends the fetch with an error, never a wait.
6. SIGINT during a fetch ends the step "interrupted" and sb by the signal
   within the grace period plus one second.
7. The run log has each git command (its arguments as exec logs them:
   values shown, exec's decision 14), its outcome and its output lines.
8. Binary size within the ratchet's tolerance (no new crate is expected).

## Decisions

1. **git CLI through sb-exec**, with the recorded "why not gix" and
   "why not git2" above, revisited when gix checks its checkout/reset
   orchestration item and gains async HTTP (the maintainer's answer D-Q1,
   2026-11-13).
2. **The branch prompt belongs to the command**: `repo` returns `Ask`
   (term-ui "Prompts" and decision 1; research D §6.2).
3. **Branch names are checked** with `git check-ref-format --branch`, a
   leading `-` refused, the output equal to the input (research D §6.20:
   untrusted input never reaches a child as an option).
4. **`LC_ALL=C` for git's children** (research D §6.21).
5. **Ownership through host's `ensure_owner_recursive`**, built with this
   work (host decision 1; research D §6.17).
6. **The refspec is widened inside `fetch_and_reset`** (research D §8: a
   single-branch clone otherwise can't reset to another branch; sb-go's
   2f2ac78 fixed it for `branch` only).
7. **The pre-checkout reset is to `HEAD`** (sb-go's `@{u}` there fails on
   a detached HEAD or an upstream-less branch; the final reset to `@{u}`
   still updates).
8. **A missing remote branch fails before the worktree changes** (sb-go's
   clean ran first).
9. **ssh never prompts:** exec's amendment for exec-M1 (triage DEFER
   reason: "matters with the first command that runs git or ssh"; research
   D §6.18).
10. **exec-L2 is fixed with this work** (triage DEFER reason: the first
    `stream` user; research D §6.18).
11. **git's output is streamed** into its step (exec `stream`, "`git
    pull` under the runner", exec.md "Ways to run"); the retry match reads
    the kept stderr tail.
12. **Every refusal decidable up front comes before the worktree
    changes** (2026-11-19, from a test agent's finding: `--keep-branch`
    on a branch without an upstream destroyed tracked edits and untracked
    files, then failed; decision 8's rule, extended to the upstream).

13. **What an update discards is known first** (the maintainer,
    2026-11-19, from a test agent's finding: plain `update` dropped a
    local commit, a tracked edit and two untracked files and printed the
    step backwards as a move): `discards` counts it with git's own reads,
    `git clean -dn` the dry run of the clean it predicts. The counts are
    as of the last fetch, before the fetch the update runs (Known limits).

## Known limits

- `discards` reads before the fetch: a local commit the fetch then
  finds upstream is still counted (asked about needlessly), and upstream
  commits a force-push removes aren't counted (they weren't local work).
  A switch to another branch runs a second `clean -df` after the
  checkout; a file the current branch ignores and the target doesn't is
  removed there and not counted. Saltbox's branches share their
  `.gitignore`, so it doesn't arise on Saltbox. Checked on public clones
  (Saltbox 03fce9e, Sandbox 61676e3, 2026-11-19): with the default
  configs copied in, `cache.json`, `ansible_facts.d/saltbox.fact`,
  `inventories/host_vars/localhost.yml`, logs and retry files, `git
  clean -dn` lists nothing in either checkout.
- Two sb runs updating the same checkout at once: the second fails on
  git's `index.lock` with git's message. sb-go is the same.
- A host without Saltbox's `safe.directory` entries gets git's "dubious
  ownership" error (assumption 2).
- A user's credential helper that prompts some other way than the
  terminal or ssh isn't covered (none is configured by Saltbox).
- A branch only on the remote is checked out by git's guess, which root's
  git config or the checkout can defeat: with `branch.autoSetupMerge`
  off the new branch has no upstream, with `checkout.guess` off or a
  second remote with the same branch the checkout fails. Each fails
  after the reset step's clean and reset to `HEAD`, which the user's
  reset would have done anyway; Saltbox sets none of them (decision 12,
  2026-11-19, weighed after review).
- A failed `init_from_remote` leaves the `.git` it made (and the remote,
  and whatever was fetched). sb removes nothing: a rerun of `sb setup
  --force` finds a checkout and fetches and resets it. sb-go is the same.

## Amendments to other specs

- **Capability map, module table, `repo` row:** "Depends on" becomes
  `exec, host, term-ui` (it streams into term-ui steps).
- **exec spec, "Environment rules"**, add: "`SSH_ASKPASS_REQUIRE=force`
  and `SSH_ASKPASS=/bin/false` for every child that isn't attached, so an
  ssh run by git (or by any child) asks no terminal and fails at once
  (exec-M1; ssh(1), ENVIRONMENT: with `force`, the askpass program is used
  for all passphrase input). `UNVERIFIED` for the host-key confirmation
  until the platform note's container check." And to "Differences from
  sb-go": "ssh's prompts are sent to a failing askpass (sb-go: none)."
  triage.md's exec-M1 row moves to fixed; a tool that opens `/dev/tty`
  itself is noted as remaining in exec's Known limits.
- **exec spec, "Ways to run", `stream`**, add: "the final line without
  `\n` is delivered also when the 2 s drain after exit ends the read
  (exec-L2)."
- **config spec, Scope** (the one Saltbox-user lookup; the python-env
  and ops specs point here): "`saltbox_user(settings) -> Result<String,
  ConfigError>`: `user.name` from `<saltbox root>/accounts.yml`, read as
  sb-go's `GetSaltboxUser` reads it (host/system.go:69-92): the file
  through cli-core's `read_small` and sb-config's YAML entry (ADR-022),
  no schema validation, so an unrelated issue elsewhere in the file
  doesn't stop `update`. sb-go's texts: `failed to read accounts.yml:
  <cause>`, `failed to unmarshal accounts.yml: <cause>`, `user section
  not found in accounts.yml`, `user.name not found in accounts.yml`.
  Callers add their own wrapping (`error getting saltbox user: <cause>`
  in python-env's commands, `get Saltbox user: <cause>` in setup). Its
  users: ops (`update`, `branch`, `branch-sandbox`, `setup`), which
  passes the name to repo's `fetch_and_reset` as `owner`, and
  python-env's commands."

## Suggested slices for the plan

1. **First vertical slice (M):** host's `ensure_owner_recursive` plus
   `Repo::open`, `current_branch`, `check_branch_name`,
   `ensure_fetch_all_branches` and `fetch_and_reset` with the local
   repository tests, wired as a bare `sb branch-sandbox <name>` (no tag
   cache yet), so an sb command exposes it.
2. **exec amendments (S):** exec-L2 and the ssh environment, test-first.
3. **Remote wrapper (S):** the HTTP/1.1 retry and the public-repo error
   with the stand-in git.
4. **`branch_choice` and `head` (S),** ready for ops' `update`.
5. **`clone` and `init_from_remote` (S),** with setup's plan.
