# Module build tasks (M1 to M87)

Plan: [plan.md](plan.md) (decisions, lanes, handoffs, the dependency
graph, risks, open questions). Approved by the maintainer on
2026-11-13, with the eight specs it comes from.

**Every task, besides what it lists:**
- the skills CLAUDE.md's table names for each phase, said when the phase
  starts (incremental-implementation, test-driven-development,
  source-driven-development for crate APIs, git-workflow-and-versioning);
- tests first where behaviour changes; library first, with full doc URLs
  in comments and `UNVERIFIED` where no official source exists;
- `scripts/check.sh fast` after edits, `task <base>` before review,
  `full <base>` before the commit, `<base>` being the commit the task
  started from;
- the reviews it lists, on the session's own model and effort, in
  parallel with `check.sh full`; findings weighed against how sb runs on
  Saltbox, real ones fixed test-first, the rest declined in the commit
  body;
- a signed Conventional Commit with its `Library-check:` and `Review:`
  lines; the capability map's Commands row of any command it wires moves
  to "built";
- sb only with `SB_LOG_DIR` in a temporary directory or `target/`; every heavy
  run under the quarter-host cap; containers with the lane's prefix.

**Gates:** free (merges when its gates pass), look (merges after the
maintainer has seen screenshots taken through `sb`), paused (waits for a
recorded maintainer action). **Shared files: yes** means the task lands
alone (plan decision 3). A task that turns out to change `Cargo.lock`
without the mark lands alone too.

---

# Lane 1: host, repo, python-env

## Task M1: host: file ownership

**Lane:** 1 · **Gate:** free · **Shared files:** yes (walkdir in sb-host)

**Description:** The host spec's "Ownership": `ensure_owner` and
`ensure_owner_recursive` over nix's user lookup and walkdir, with
`lchown` injected (M-MOCKABLE-SYSCALLS). repo (M5) and python-env (M14)
are its first users; the repo spec's slice 1 and python-env's slice 1
both name it as theirs, and this task builds it once.

**Acceptance criteria:**
- [x] A missing path or root is skipped; an empty or unknown user skips
      everything; a symlink is changed itself, never its target; an entry
      that vanishes mid-walk is skipped and the walk goes on (difference
      11).
- [x] The errors nest with sb-go's texts (`look up user <u>: ...`,
      `inspect ownership of <p>: ...`, `set ownership of <p>: ...`,
      `set recursive ownership of <root>: ...`).
- [x] The run log gets one count of changed entries per root.

**Verification:**
- [x] `cargo test -p sb-host` with the injected `lchown` and temporary trees.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/host/src/{lib.rs,owner.rs}`, `crates/host/Cargo.toml`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M2: host: the systemd service list

**Lane:** 1 · **Gate:** free · **Shared files:** yes (sb-host's dependencies)

**Description:** The host spec's "systemd services" with the tui and motd
amendments: `services(filters, token)`, `with_additional`, and sb-go's
`format_duration` and `format_next_in`. The tui picker (M68) and motd's
Services (M43) wait for it.

**Acceptance criteria:**
- [x] `list-units`, `list-unit-files` and `list-timers` run
      concurrently, then one `show --timestamp=utc` over the matched
      units for `ActiveEnterTimestamp` and `StateChangeTimestamp`; each a
      10 s capture taking the caller's `CancellationToken`, so the call
      takes at most two timeouts.
- [x] Matching: the `saltbox_managed_` prefix and `docker`, configured
      names with `.service` stripped, a name matching nothing left out,
      sorted, no duplicates, `not-found` dropped; `n/a`, empty or missing
      timestamps read as unknown (UTC text on every release: systemd 249
      refuses `--timestamp=unix`, which arrived in v251).
- [x] A failed `list-units` or `list-unit-files` is `failed to list
      systemd services: <cause>`; a failed `list-timers` or `show` leaves
      its fields empty.

**Verification:**
- [x] `cargo test -p sb-host` on stand-in `systemctl` programs (JSON
      fixtures), including cancellation mid-call and the time bound.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/host/src/{lib.rs,systemd.rs,testing.rs}`, `crates/host/Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M3: host: setup checks and `package_installed`

**Lane:** 1 · **Gate:** free · **Shared files:** yes (rustix `system` for `uname`)

**Description:** The host spec's "Release checks" (`check_setup_support`)
and "Setup checks" (`check_architecture`, `check_lxc`, `check_desktop`),
with apt's `package_installed`, which `check_desktop` and python-env's
libpq-dev check use. ansible's `install` (M31) and ops' setup (M82) are
the checks' users.

**Acceptance criteria:**
- [x] Each check passes and fails as the spec's table says, with sb-go's
      texts; `check_lxc` matches stdout and stderr together.
- [x] `package_installed` is true only for `ii`, false on exit 1, and
      `query package <name>: <cause>` otherwise (5 s for the desktop
      check, 10 s elsewhere).
- [x] Program paths are fields, set through the `testing` feature only.

**Verification:**
- [x] `cargo test -p sb-host` with stand-in `systemd-detect-virt` and
      `dpkg-query`, and an injected `uname`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/host/src/{lib.rs,setup.rs,apt.rs,testing.rs}`, `crates/host/Cargo.toml`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M4: host: disk space and `free_space`

**Lane:** 1 · **Gate:** free

**Description:** The host spec's "Disk space": `check_disk_space`,
`disk_space_error` for install's `--force-disk-full`, and the ops
amendment's `free_space`, over an injected `statvfs`.

**Acceptance criteria:**
- [x] Paths resolve to their nearest existing ancestor and each
      filesystem is checked once (by `f_fsid`); free space is
      `f_bavail × f_frsize`, used percent sb-go's formula; under 2 GiB
      fails with sb-go's text and `formatBytes` sizes.
- [x] Zero blocks and other errors nest as sb-go's; the run log gets each
      filesystem's figures and each skipped duplicate.
- [x] `free_space(path)` returns the same free figure `check_disk_space` uses.

**Verification:**
- [x] `cargo test -p sb-host` with the injected `statvfs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/host/src/{lib.rs,disk.rs}`
**Scope:** S

## Checkpoint H1: host foundations
- [x] M1-M4 merged; `check.sh full` on `main`.
- [x] No looks (library code, no new output).

## Task M5: repo: crate and a bare `sb branch-sandbox`

**Lane:** 1 · **Gate:** free · **Shared files:** yes (new crate sb-repo; deny.toml; the library-check hook's list)

**Description:** The repo spec's first slice: `Repo::open`,
`current_branch`, `check_branch_name`, `ensure_fetch_all_branches` and
`fetch_and_reset` (with M1's recursive ownership), on local bare
repositories, wired as a bare `sb branch-sandbox <name>` (no tag cache)
so an sb command exposes it. ops replaces the wiring in M79.

**Acceptance criteria:**
- [x] The crate per the repo spec's Project Structure; deny.toml gets
      `sb-repo` (wrappers `sb`) and adds it to the wrappers of `sb-exec`,
      `sb-host` and `sb-term-ui`; the library-check hook lists `repo`.
- [x] git runs through exec with the spec's environment (`LC_ALL=C`);
      captures for reads, streams into term-ui steps for changes; sb-go's
      texts.
- [x] `sb branch-sandbox <name>` resets a local Sandbox checkout to the
      branch, owned by the given user.

**Verification:**
- [x] `cargo test -p sb-repo` on `file://` remotes; an sb test through the
      binary.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M1 (M23 before Checkpoint R, not before this task)
**Files:** `crates/repo/{Cargo.toml,src/lib.rs,src/git.rs,src/update.rs,tests/}`, `crates/sb/src/{main.rs,branch.rs}`, `crates/sb/Cargo.toml`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M6: repo: the remote wrapper

**Lane:** 1 · **Gate:** free

**Description:** The repo spec's "Remote operations": the HTTP/1.1 retry
and the public-repo error, tested with a stand-in git.

**Acceptance criteria:**
- [x] The retry happens only on the spec's conditions, once, over
      HTTP/1.1.
- [x] The public-repo error carries the spec's text.
- [x] No remote call waits on a terminal ("Never waiting on a terminal").

**Verification:**
- [x] `cargo test -p sb-repo` with the stand-in git.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M5
**Files:** `crates/repo/src/{lib.rs,remote.rs}`, `crates/repo/tests/`
**Scope:** S

## Task M7: repo: `branch_choice` and `head`

**Lane:** 1 · **Gate:** free

**Description:** `branch_choice(current, wanted, answer)` (answering
`Ask` when the branches differ and no answer was given) and `head`, ready
for ops' `update` and ansible's tag cache (M32).

**Acceptance criteria:**
- [x] `branch_choice` covers same branch, different with each answer,
      and different without one.
- [x] `head` returns the checkout's commit, and the spec's error on an
      unborn or broken repository.
- [x] Both are public in sb-repo's API as the spec names them.

**Verification:**
- [x] `cargo test -p sb-repo`.
- [x] Review: code-reviewer.

**Dependencies:** M5
**Files:** `crates/repo/src/lib.rs`, `crates/repo/tests/`
**Scope:** S

## Task M8: repo: `clone` and `init_from_remote`

**Lane:** 1 · **Gate:** free

**Description:** The two operations setup needs (`clone(url, branch,
step)`, `init_from_remote(url, branch, step)`), with setup's plan from
the ops spec.

**Acceptance criteria:**
- [x] `clone` into a missing directory and `init_from_remote` over an
      existing one behave as the spec says, streamed into the step.
- [x] Failures leave the directory as the spec says.
- [x] sb-go's texts.

**Verification:**
- [x] `cargo test -p sb-repo` on `file://` remotes.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M5
**Files:** `crates/repo/src/{lib.rs,update.rs}`, `crates/repo/tests/`
**Scope:** S

## Checkpoint R: repo
- [x] M5-M8 and M23 merged; `check.sh full` on `main`.
- [x] Move triage.md's exec-M1 and exec-L2 rows to fixed.
- [x] No looks.

## Task M9: host: apt policy, install, remove, update_lists, the lock note

**Lane:** 1 · **Gate:** free

**Description:** The host spec's "apt": the policy (`-o` options,
`DEBIAN_FRONTEND`, `NEEDRESTART_MODE`, `DPkg::Lock::Timeout=240`),
`install`, `remove`, `autoremove`, and `update_lists` with its retries
and lists-lock wait (decision 5), plus the lock-wait verification and
`docs/platform-notes/apt.md`.

**Acceptance criteria:**
- [x] Each call's argv, environment, timeout and texts as the spec says,
      on stand-in `apt-get`; an interrupted attempt is never retried.
- [x] `update_lists`: 3 attempts, 2 min each, 5 s and 10 s delays on the
      named mirror errors; the lists-lock retry every 5 s within 240 s.
- [x] The lock-wait check runs in a throwaway Ubuntu container (never the
      host's apt) with `-o Debug::Locking=1`, and the note records
      versions and method; difference 1 loses `UNVERIFIED` only if it
      holds.

**Verification:**
- [x] `cargo test -p sb-host`; the container check under the cap.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M3
**Files:** `crates/host/src/{apt.rs,testing.rs}`, `docs/platform-notes/apt.md`
**Scope:** M

## Task M10: host: `add_repositories` with the fenced cleanup

**Lane:** 1 · **Gate:** free

**Description:** `add_repositories(step)` with the ops amendment's
cleanup: the `sources.list.d` entries move into a root-only
`/var/backups/sb/apt-sources/<run id>/` through directory handles; the
jammy and DEB822 rules; atomic saves.

**Acceptance criteria:**
- [x] The codename from `lsb_release -sc`'s stdout; any codename other
      than `jammy`, `noble` or `resolute` fails before anything changes.
- [x] A symlinked, foreign-owned or group- or world-writable directory is
      refused before anything moves; entries move with `renameat`
      between handles (`EXDEV`: copy, sync, unlink); a subdirectory
      fails; names logged before they move; the backup directories made
      0700, the run's only when something moves.
- [x] jammy's `sources.list` append and rewrite, DEB822's
      `ubuntu-archive.sources` only when needed, files saved atomically
      under a name apt ignores (checked against apt's source).

**Verification:**
- [x] `cargo test -p sb-host` on temporary roots (the root arguments are
      test-only).
- [x] Review: code-reviewer, library-reviewer, security-auditor (root
      filesystem moves).

**Dependencies:** M9
**Files:** `crates/host/src/{apt.rs,apt/sources.rs}`, `crates/host/tests/`
**Scope:** M

## Task M11: host: apt queries and `remove_sources_mentioning`

**Lane:** 1 · **Gate:** free

**Description:** The python-env amendments H2 and H3: `package_list()`,
`package_policy(name)` (with `LC_ALL=C`), and
`remove_sources_mentioning(needle, backup)` on M10's fenced handles.

**Acceptance criteria:**
- [x] The two queries' argv, timeouts, row parsing and error texts.
- [x] `remove_sources_mentioning` moves only regular files directly in
      `sources.list.d` whose content holds the needle, into
      `/var/backups/sb/<backup>/<run id>/`, with M10's refusals and
      modes; subdirectories and symlinks are left alone.
- [x] It returns what it moved, and logs each name first.

**Verification:**
- [x] `cargo test -p sb-host` on temporary roots and stand-in programs.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M10
**Files:** `crates/host/src/{apt.rs,apt/sources.rs,testing.rs}`
**Scope:** S

## Checkpoint H2: host complete
- [x] M1-M4, M9-M11 merged; every part of the host spec is built.
- [x] No looks.

## Task M12: config: `saltbox_user()`

**Lane:** 1 · **Gate:** free

**Description:** The config amendment from the repo spec:
`saltbox_user(settings)` reads `user.name` from `accounts.yml` as sb-go's
`GetSaltboxUser` does, through cli-core's `read_small` and sb-config's
YAML entry, with no schema validation. python-env's commands and ops
call it.

**Acceptance criteria:**
- [x] sb-go's four texts (`failed to read accounts.yml: ...`, `failed to
      unmarshal accounts.yml: ...`, `user section not found ...`,
      `user.name not found ...`).
- [x] An unrelated schema issue elsewhere in the file doesn't stop it.
- [x] It lives in its own file, so lane 3's run.rs work doesn't touch it.

**Verification:**
- [x] `cargo test -p sb-config`.
- [x] Review: code-reviewer.

**Dependencies:** hardening Task 59 merged (plan decision 9)
**Files:** `crates/config/src/{lib.rs,user.rs}`, `crates/config/tests/`
**Scope:** S

## Task M13: python-env: crate, layout, toolchain, isolation, uv probe

**Lane:** 1 · **Gate:** free · **Shared files:** yes (new crate sb-python-env; semver, sha2; deny.toml; the hook's list)

**Description:** The first half of python-env's slice 1: the crate, the
fixed layout (decision 7), the toolchain pin files and uv minimum, the
lock digest, the isolated `Cmd` builder (decision 11), and the uv
`--version` probe with uv present.

**Acceptance criteria:**
- [x] The crate per the spec's Project Structure; deny.toml adds
      `sb-python-env` to the wrappers of `sb-exec`, `sb-term-ui`,
      `sb-host` and `sb-release` (an allow-list, so ahead of use is
      harmless), its own entry allowing `sb`; the hook lists `python-env`.
- [x] The isolated builder sets exactly the spec's environment for uv,
      python and entry points.
- [x] The probe parses uv's version and applies the minimum, with the
      spec's texts.

**Verification:**
- [x] `cargo test -p sb-python-env` with stand-in programs.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M12 (the user, for the next task); none for the code here
**Files:** `crates/python-env/{Cargo.toml,src/lib.rs,src/toolchain.rs,src/isolate.rs,src/uv.rs,src/testing.rs}`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M14: python-env: `sb update-venv` on a healthy tree

**Lane:** 1 · **Gate:** free · **Shared files:** yes (sb depends on sb-python-env)

**Description:** The rest of slice 1: manifest reading, the environment
check, the wrapper rewrite, generation cleanup, ownership (M1), libpq-dev
present (M3's `package_installed`); deadsnakes and the fact as stubs
answering "nothing to do" behind the same steps; `sb update-venv` wired.

**Acceptance criteria:**
- [x] On a healthy tree `sb update-venv` passes every step with the
      spec's step texts and changes nothing it needn't.
- [x] Wrappers are rewritten and old generations removed as the spec says.
- [x] `error getting saltbox user: <cause>` wraps M12's errors.

**Verification:**
- [x] `cargo test -p sb-python-env`; an sb test through the binary on a
      stand-in tree.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M1, M3, M12, M13
**Files:** `crates/python-env/src/{lib.rs,manifest.rs,venv.rs,wrappers.rs}`, `crates/sb/src/{main.rs,python_env.rs}`, `crates/sb/Cargo.toml`, `crates/sb/tests/python_env.rs`, `Cargo.lock`
**Scope:** M

## Task M15: python-env: the build into staging

**Lane:** 1 · **Gate:** free

**Description:** The first half of slice 2: Python install and find, `uv
venv`, `pip sync` streamed, discovery, the staged check and the manifest
write.

**Acceptance criteria:**
- [x] Each step's argv, isolation and texts as the spec's "venv
      reconcile, step by step" says.
- [x] A failed or cancelled build leaves the live venv untouched and the
      staging cleaned up.
- [x] The manifest is schema 1, written atomically.

**Verification:**
- [x] `cargo test -p sb-python-env` with stand-in uv and python.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M14
**Files:** `crates/python-env/src/{venv.rs,manifest.rs,testing.rs}`, `crates/python-env/tests/`
**Scope:** M

## Task M16: python-env: activation and the `reinstall-*` commands

**Lane:** 1 · **Gate:** free

**Description:** The second half of slice 2: wrappers, activation by
symlink swap with legacy adoption, stale wrapper removal, failure and
cancel cleanup; `sb reinstall-venv` and `sb reinstall-python` wired.

**Acceptance criteria:**
- [x] Activation swaps atomically; a legacy (sb-go) venv is adopted as
      the spec says.
- [x] Stale wrappers go; a failure or Ctrl-C leaves the previous
      generation active.
- [x] Both commands force their rebuilds with sb-go's texts.

**Verification:**
- [x] `cargo test -p sb-python-env`; sb tests through the binary.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M15
**Files:** `crates/python-env/src/{activate.rs,wrappers.rs,lib.rs}`, `crates/sb/src/{main.rs,python_env.rs}`, `crates/sb/tests/python_env.rs`
**Scope:** M

## Task M17: python-env: self-healing

**Lane:** 1 · **Gate:** free

**Description:** Slice 3 (decision 3): the wider Python check and the
clean retry on the normal path, through `reconcile_runtime`, which
`update` and `branch` call.

**Acceptance criteria:**
- [x] A broken venv found by the wider check is rebuilt once, cleanly.
- [x] A second failure is reported, not retried.
- [x] `reconcile_runtime(step, env, user)` is public, as ops needs it.

**Verification:**
- [x] `cargo test -p sb-python-env`.
- [x] Review: code-reviewer.

**Dependencies:** M16
**Files:** `crates/python-env/src/{venv.rs,lib.rs}`, `crates/python-env/tests/`
**Scope:** S

## Task M18: python-env: uv install

**Lane:** 1 · **Gate:** free · **Shared files:** yes (tar, flate2 with `rust_backend`, unless M78 added them first)

**Description:** Slice 4: uv's metadata, download, extraction, staged
`--version` and activation through release (R1-R6), and the libpq-dev
apt steps (M9's `install` and `update_lists`, their first use).

**Acceptance criteria:**
- [x] uv comes from `astral-sh`'s release through `Releases`, verified,
      extracted with tar and flate2 (size-capped), checked with a staged
      `--version`, then activated.
- [x] A missing libpq-dev is installed through host's apt steps.
- [x] The binary-size cost of tar and flate2 is measured on the
      production build.

**Verification:**
- [x] `cargo test -p sb-python-env` with release's `testing` fake server.
- [x] Review: code-reviewer, library-reviewer, security-auditor (a
      download run as root).

**Dependencies:** M9, M17, M29 (release's interface)
**Files:** `crates/python-env/{Cargo.toml,src/uv.rs}`, `crates/python-env/tests/`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M19: python-env: the fact

**Lane:** 1 · **Gate:** free

**Description:** Slice 5: saltbox.fact's version probe, the decision with
the digest rule, download, the ELF check and staged `--version`,
activation; `sb reinstall-facts` and update-venv's real fact step.

**Acceptance criteria:**
- [x] The decision follows the spec (decisions 2, 6, 8), including the
      digest rule.
- [x] `check_elf` and `sha256_file` (R7, R8) come from release, with
      sb-go's fact texts for the two ELF errors.
- [x] `sb reinstall-facts` forces it; `update-venv` no longer stubs it.

**Verification:**
- [x] `cargo test -p sb-python-env`; sb tests through the binary.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M18, M28
**Files:** `crates/python-env/src/{fact.rs,lib.rs}`, `crates/sb/src/python_env.rs`, `crates/sb/tests/python_env.rs`
**Scope:** M

## Task M20: python-env: deadsnakes

**Lane:** 1 · **Gate:** free

**Description:** Slice 6: the deadsnakes cleanup on 22.04 (decisions 10
and 22), parsing M11's queries from fixtures and moving the source files
aside through `remove_sources_mentioning`. update-venv is complete.

**Acceptance criteria:**
- [x] Parsing of `dpkg-query` and `apt-cache policy` fixtures picks
      exactly the spec's packages.
- [x] Removal and autoremove run only when there is something to remove;
      the sources move to `/var/backups/sb/deadsnakes/<run id>/`.
- [x] Not 22.04: nothing to do.

**Verification:**
- [x] `cargo test -p sb-python-env` with fixtures.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M11, M19
**Files:** `crates/python-env/src/deadsnakes.rs`, `crates/python-env/tests/fixtures/`
**Scope:** S

## Task M21: python-env: live acceptance

**Lane:** 1 · **Gate:** free

**Description:** Slice 7: the container test on 22.04, 24.04 and 26.04,
both directions with sb-go (sb-go's tree read by sb, sb's read by sb-go).

**Acceptance criteria:**
- [x] `update-venv`, the `reinstall-*` commands and the fact pass in each
      container.
- [x] sb adopts an sb-go-built tree, and sb-go accepts an sb-built one.
- [x] Results go in the spec's verification items without environment
      details.

**Verification:**
- [x] The container script under the cap, lane-prefixed containers.
- [x] Review: code-reviewer.

**Dependencies:** M20
**Files:** `crates/python-env/tests/live.rs` (and its script)
**Scope:** S

## Checkpoint PE: python-env
- [x] M12-M21 merged; `check.sh full`; the live acceptance green on three
      releases.
- [x] No looks (sb-go's step texts).

---

# Lane 2: exec, release, ansible, motd

*motd's tasks (M36-M47) moved to lane 3 on 2026-11-13 (plan.md, the
lane change of 2026-11-13); their Lane lines say so.*

## Task M22: exec: `option_value` and public `shown()`

**Lane:** 2 · **Gate:** free

**Description:** The ansible amendment to exec: `option_value(&'static
str, String)` joins a literal option to a user value in one argv element
(`--extra-vars=<value>`), shown as `--extra-vars=[ARG]` in logs and
errors; `Cmd::shown()` becomes public.
*As built, then changed (the maintainer, 2026-11-16): 69701ad removed
`option_value` and the `[ARG]` rendering after 6e12a38 left them with
no production caller; a joined option is one `shown_value` the caller
builds, or one `secret` (exec decision 14), and `shown()` stays
public.*

**Acceptance criteria:**
- [x] The child receives the joined element byte for byte, JSON and a
      leading `-` included.
- [x] Logs and errors show the literal with `[ARG]`.
- [x] `shown()` is public with its doc, returning the masked rendering.

**Verification:**
- [x] `cargo test -p sb-exec`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/exec/src/{lib.rs,mask.rs}`
**Scope:** S

## Task M23: exec: the ssh environment and exec-L2

**Lane:** 2 · **Gate:** free

**Description:** The repo amendments to exec: `SSH_ASKPASS_REQUIRE=force`
and `SSH_ASKPASS=/bin/false` for every child that isn't attached
(exec-M1), and `stream` delivering a final line without `\n` when the
2 s drain ends the read (exec-L2). Test-first.

**Acceptance criteria:**
- [x] Every non-attached child gets both variables; attached children
      don't.
- [x] The last unterminated line arrives after the drain.
- [x] The spec's Differences and Known limits wording is in place;
      the host-key case stays `UNVERIFIED` until a container check.

**Verification:**
- [x] `cargo test -p sb-exec` (failing tests first).
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/exec/src/{run.rs,lib.rs}`, `crates/exec/tests/`, `docs/specs/exec.md`
**Scope:** S

## Task M24: release: crate, client, lookup, asset checks

**Lane:** 2 · **Gate:** free · **Shared files:** yes (new crate sb-release; semver, sha2, hex; rcgen test only; deny.toml; the hook's list)

**Description:** The start of release's slice 1: `Releases::production`
and `testing` (R1), the lookup through the SVM proxy then GitHub with
sb-go's warning (R2), and the asset checks with the pinned URL (R3), on
the fake TLS server (axum with rcgen's loopback certificate).

**Acceptance criteria:**
- [x] The client is reqwest with rustls/aws-lc-rs, `https_only`, connect
      and per-read timeouts, and a `User-Agent`.
- [x] An asset passes only with its name, size > 0, a `sha256:<64 hex>`
      digest and the pinned URL prefix; a missing one is `release <tag>
      is missing <name>`.
- [x] deny.toml: `sb-release` (wrappers `sb`), added to the wrappers of
      `sb-cli-core`, `sb-term-ui` and `sb-exec`; no key in the repository.

**Verification:**
- [x] `cargo test -p sb-release` on the fake server.
- [x] Review: code-reviewer, library-reviewer, security-auditor
      (downloads that replace a binary).

**Dependencies:** None
**Files:** `crates/release/{Cargo.toml,src/lib.rs,src/lookup.rs,src/asset.rs,src/testing.rs}`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M25: release: verified download and activation

**Lane:** 2 · **Gate:** free

**Description:** `Releases::download(&Asset, Stage, &mut Step) ->
Staged` (R5), streamed with the digest check and the size cap, removed on
drop; `activate(staged, target, mode)` (R6).

**Acceptance criteria:**
- [x] A wrong digest, an oversize body or a short body fails and leaves
      nothing staged.
- [x] Activation replaces the target atomically with its mode, syncing
      the directory.
- [x] A caller's own staged file in the target directory activates too.

**Verification:**
- [x] `cargo test -p sb-release`.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M24
**Files:** `crates/release/src/{download.rs,activate.rs,lib.rs}`
**Scope:** M

## Task M26: release: `sb self-update`, `BuildInfo::released`, term-ui's confirm

**Lane:** 2 · **Gate:** look (the confirm prompt) · **Shared files:** yes (dialoguer in sb-term-ui; sb depends on sb-release)

**Description:** The rest of release's slice 1: self-update's flow end
to end on the fake server, the disabled and raw-build paths through the
cli-core amendment's `BuildInfo::released`, `--yes`, `--force-update`,
and term-ui's confirm prompt (term-ui spec "Prompts", built with its
first user).

**Acceptance criteria:**
- [x] `sb self-update` looks up, checks, downloads, verifies and
      activates; a build without `SB_VERSION` is treated as disabled;
      `sb version` is unchanged.
- [x] The confirm reads the terminal, not stdin; `--yes` answers it; no
      terminal and no answer fails as term-ui's criterion 10 says.
- [x] Screenshots of the confirm, and of a full self-update run, through
      `sb self-update` against the fake server.

**Verification:**
- [x] `cargo test -p sb-release -p sb-term-ui -p sb-cli-core -p sb`;
      `crates/sb/tests/self_update.rs`.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M25
**Files:** `crates/release/src/self_update.rs`, `crates/term-ui/{Cargo.toml,src/prompt.rs,src/lib.rs}`, `crates/cli-core/src/build_info.rs` (and its build script), `crates/sb/src/{main.rs,self_update.rs}`, `crates/sb/tests/self_update.rs`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M27: release: retries and timeouts

**Lane:** 2 · **Gate:** free

**Description:** Slice 2: the retry policy for metadata and downloads (4
attempts, sb-go's backoff, `Retry-After`), and the per-read timeout.

**Acceptance criteria:**
- [x] The fake server's 5xx, 429 with `Retry-After`, and a stalled body
      each produce the spec's retries and waits.
- [x] A non-retryable status fails at once.
- [x] The "why not reqwest::retry" comment is in the code.

**Verification:**
- [x] `cargo test -p sb-release` with paused tokio time where it fits.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M26
**Files:** `crates/release/src/{retry.rs,lookup.rs,download.rs}`
**Scope:** S

## Task M28: release: staged binary checks

**Lane:** 2 · **Gate:** free · **Shared files:** yes (object with `read_core`, `elf`)

**Description:** Slice 3: `check_elf(path, Arch)` with object (R7),
`Arch` (R4), and the staged `version --json` run before activation.

**Acceptance criteria:**
- [x] An ELF of the wrong architecture, a non-ELF and a truncated file
      fail with the two `ElfError` variants.
- [x] The staged binary's `version --json` must run and parse before
      activation.
- [x] object's size cost is measured on the production build.

**Verification:**
- [x] `cargo test -p sb-release`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M27
**Files:** `crates/release/{Cargo.toml,src/activate.rs,src/lib.rs,src/self_update.rs}`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M29: release: the python-env interface

**Lane:** 2 · **Gate:** free

**Description:** Slice 4: `release(GithubRepo, Query)`, `Asset::sha256`,
`Asset::size`, `Asset::pinned`, `activate` on a caller's file,
`sha256_file`, `Arch::current`, `rust_name`, `go_name`, with a stub
caller in the tests (deny.toml already lets `sb-python-env` use it,
from M13).

**Acceptance criteria:**
- [x] Each of R1-R8 is public under the release spec's names.
- [x] `GithubRepo` takes the owner as a parameter (uv's `astral-sh`).
- [x] A stub caller exercises the interface as python-env will.

**Verification:**
- [x] `cargo test -p sb-release`.
- [x] Review: code-reviewer.

**Dependencies:** M28
**Files:** `crates/release/src/{lib.rs,asset.rs}`, `crates/release/tests/`
**Scope:** S

## Checkpoint X: exec and release
- [x] M22-M29 merged; `check.sh full`.
- [x] Look: the confirm prompt and a self-update run (M26), seen by the
      maintainer.

## Task M30: ansible: crate and `sb diag`

**Lane:** 2 · **Gate:** free · **Shared files:** yes (new crate sb-ansible; deny.toml; the hook's list)

**Description:** The ansible spec's slice 1: playbook paths, the argv
builder on M22's rendering, the attached run, the success line and
pass-through, Ctrl-C and 99, `SB_ANSIBLE_PLAYBOOK` (cli-core
amendment), `sb diag` through the binary with the fake playbook.

**Acceptance criteria:**
- [x] The crate per the spec's Project Structure; deny.toml per the
      capability map's row (exec, host, repo, term-ui, cli-core).
- [x] The fake `ansible-playbook` records argv NUL-separated; `diag`'s
      argv and `Executing...` line (through `Cmd::shown()`) match.
- [x] Ctrl-C ends with 130, and the playbook's exit code passes through.

**Verification:**
- [x] `cargo test -p sb-ansible -p sb`; `crates/sb/tests/ansible.rs`,
      `ansible_signals.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M22
**Files:** `crates/ansible/{Cargo.toml,src/lib.rs,src/run.rs,tests/fixtures/}`, `crates/sb/src/{main.rs,ansible.rs}`, `crates/sb/Cargo.toml`, `crates/sb/tests/{ansible.rs,ansible_signals.rs}`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M31: ansible: `install` without the cache

**Lane:** 2 · **Gate:** free

**Description:** Slice 2: clap flags, tag parsing, routing, run order,
the counted `-v` and whole `-e` values (both fixtures, the argparse one
included), `--skip-tags`, `--no-cache` as the only path for now, host's
LXC and disk checks (`--force-disk-full`).

**Acceptance criteria:**
- [x] `-v` to `-vvvv` and `-v -v` reach every run as one `-v...` element.
- [x] Each `-e` reaches Ansible as one `--extra-vars=<value>`: multi-key
      JSON, nested quotes, unicode, a leading `-`, several in order (the
      capability map's open item).
- [x] The LXC and disk checks run before any playbook, with host's texts.

**Verification:**
- [x] `cargo test -p sb-ansible -p sb` with both fixtures.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M30, M3, M4
**Files:** `crates/ansible/src/{args.rs,run.rs,lib.rs}`, `crates/sb/src/ansible.rs`, `crates/sb/tests/ansible.rs`
**Scope:** M

## Task M32: ansible: the tag cache

**Lane:** 2 · **Gate:** free

**Description:** Slice 3: load, the listing step, the `TASK TAGS:` parse,
commit validation through repo's `head` (M7), the atomic 0644 write,
corrupt files, sb-go-written fixtures; `TagCache::refresh(repo, step)`
for ops.

**Acceptance criteria:**
- [x] sb-go's `cache.json` loads unchanged; a stale commit rebuilds.
- [x] Writes are atomic and 0644, set explicitly (completion reads
      without sudo).
- [x] A corrupt file rebuilds with the spec's notice.

**Verification:**
- [x] `cargo test -p sb-ansible` with sb-go fixtures.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M31, M7
**Files:** `crates/ansible/src/{cache.rs,lib.rs}`, `crates/ansible/tests/`
**Scope:** M

## Task M33: ansible: validation and suggestions

**Lane:** 2 · **Gate:** free · **Shared files:** yes (strsim, plan decision 8)

**Description:** Slice 4: the four suggestion kinds, sorting, the fatal
and lenient repo failures, the error layout.

**Acceptance criteria:**
- [x] Each kind's cases from the spec, with strsim's `levenshtein`.
- [x] Fatal and lenient failures as the spec says.
- [x] The error layout matches the spec's texts.

**Verification:**
- [x] `cargo test -p sb-ansible -p sb`.
- [x] Review: code-reviewer.

**Dependencies:** M32
**Files:** `crates/ansible/{Cargo.toml,src/suggest.rs}`, `crates/sb/tests/ansible.rs`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M34: ansible: `sb list`

**Lane:** 2 · **Gate:** look (`list`'s columns) · **Shared files:** yes (uutils_term_grid)

**Description:** Slice 5: the tag columns with uutils_term_grid,
`(cached)`, `-m`/`--include-mod`, the search table with spans through the
term-ui amendment `Table::spanning_row`, error collection.
*As built, then changed (the maintainer, 2026-11-16): `sb list` took the
refined columns layout (da6f848, 6841fa0; ansible spec decision 19):
a heading per repository, columns of one shared width, still through
uutils_term_grid, and a footer hint; the `(cached)` note became a
run-log record.*

**Acceptance criteria:**
- [x] Columns fill as many as the terminal width allows, through
      uutils_term_grid (its size measured).
- [x] `sb list <query>` prints the search table with bold spanning rows
      for the later repos.
- [x] Screenshots of `sb list`, `sb list -m` and a search, on a fixture
      cache.

**Verification:**
- [x] `cargo test -p sb-ansible -p sb-term-ui -p sb` with insta snapshots.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M33
**Files:** `crates/ansible/{Cargo.toml,src/list.rs}`, `crates/term-ui/src/table.rs`, `crates/sb/src/ansible.rs`, `crates/sb/tests/ansible.rs`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M35: ansible: the container check by hand

**Lane:** 2 · **Gate:** free

**Description:** Slice 6: real Ansible in a container with the `-e`
cases and Ctrl-C; the `--list-tags` fixture recorded from it.

**Acceptance criteria:**
- [x] Every `-e` case reaches the play intact.
- [x] Ctrl-C stops the play, and sb exits 99 (Ansible catches the interrupt and exits 99; ansible spec step 7, difference 6, decision 7).
- [x] The fixture is the real `--list-tags` output, with no environment
      details.

**Verification:**
- [x] The container run under the cap.
- [x] Review: code-reviewer.

**Dependencies:** M34
**Files:** `crates/ansible/tests/fixtures/`, the spec's verification notes
**Scope:** S

## Checkpoint A: ansible
- [x] M30-M35 merged; `check.sh full`.
- [x] Look: `sb list`'s columns and search table (M34).

## Task M36: motd: crate, flags, runner, layout, distro and kernel

**Lane:** 3 · **Gate:** look (motd's layout) · **Shared files:** yes (new crate sb-motd; deny.toml; the hook's list)

**Description:** The first half of motd's slice 1: all 32 clap flags,
the selection and `--all` rules with their texts, the runner (30 s cap,
panics, fixed order), the layout writer, the run log, and the distro and
kernel sections, so `sb motd --distro --kernel` shows the layout.

**Acceptance criteria:**
- [x] The contract test sees every flag of cmd/motd.go:68-100; deny.toml
      has `sb-motd` (wrappers `sb`) and adds it to the wrappers of
      `sb-cli-core`, `sb-term-ui`, `sb-exec`, `sb-host`, `sb-config` and
      `sb-docker`, all at once.
- [x] A panicking or slow section shows the spec's line, and the rest
      still print in order.
- [x] Screenshots of `sb motd --distro --kernel`.

**Verification:**
- [x] `cargo test -p sb-motd -p sb`; `crates/sb/tests/motd.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None (the docker and config parts come later)
**Files:** `crates/motd/{Cargo.toml,src/lib.rs,src/run.rs,src/layout.rs,src/system.rs}`, `crates/sb/src/{main.rs,motd.rs}`, `crates/sb/Cargo.toml`, `crates/sb/tests/motd.rs`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M37: motd: the system sections

**Lane:** 3 · **Gate:** free (M36's layout) · **Shared files:** yes (procfs, no default features)

**Description:** The rest of slice 1: uptime, CPU (load), memory,
processes, cpu-info and reboot from procfs, rustix and os-release, with
fixture tests; colour with `CLICOLOR_FORCE` piped; the fast-path timing
baseline beside sb-go.

**Acceptance criteria:**
- [x] Each section matches sb-go's values on fixtures.
- [x] Piped output is plain unless `CLICOLOR_FORCE` is set.
- [x] The baseline timing is recorded in the spec's verification notes;
      procfs's size is measured.

**Verification:**
- [x] `cargo test -p sb-motd -p sb`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M36
**Files:** `crates/motd/{Cargo.toml,src/system.rs}`, `crates/motd/tests/`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M38: motd: Disk Usage and term-ui's `bar`

**Lane:** 3 · **Gate:** look (the bars)

**Description:** Slice 3: mountinfo, statvfs, df's rules, and term-ui's
`bar(theme, role, on, ratio, width)` (the motd amendment), with the
library-reviewer.

**Acceptance criteria:**
- [x] The filesystems and percentages match df's rules on fixtures.
- [x] `bar` draws `round(width × ratio)` filled cells in the role and the
      rest in Faint, through `paint`.
- [x] Screenshots of `sb motd --disk`.

**Verification:**
- [x] `cargo test -p sb-motd -p sb-term-ui`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M37
**Files:** `crates/motd/src/disk.rs`, `crates/term-ui/src/{style.rs,lib.rs}`
**Scope:** S

## Task M39: motd: packages and GPU

**Lane:** 3 · **Gate:** free (the approved layout; seen at Checkpoint M)

**Description:** Slice 4: update-notifier's sources and children; sysfs
and the host-pci.ids parser (decision 2) with fixture tests.

**Acceptance criteria:**
- [x] Package and reboot status as sb-go on fixtures.
- [x] The pci.ids parser reads the host's file at run time; no embedded
      database.
- [x] A missing file or GPU leaves the section as the spec says.

**Verification:**
- [x] `cargo test -p sb-motd`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M38
**Files:** `crates/motd/src/{packages.rs,gpu.rs}`, `crates/motd/tests/fixtures/`
**Scope:** S

## Task M40: motd: logins

**Lane:** 3 · **Gate:** free (seen at Checkpoint M)

**Description:** Slice 5: the container matrix first (logind JSON and
`last` on 22.04, 24.04 and 26.04, `docs/platform-notes/motd-sources.md`),
then sessions and the last login.

**Acceptance criteria:**
- [x] The platform note records versions and method, no environment
      details.
- [x] Sessions and last login match each release's fixtures.
- [x] A missing `loginctl` or `last` leaves the section as the spec says.

**Verification:**
- [x] `cargo test -p sb-motd`; the container matrix under the cap.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M39
**Files:** `crates/motd/src/logins.rs`, `crates/motd/tests/matrix.sh`, `docs/platform-notes/motd-sources.md`
**Scope:** M

## Task M41: motd: Docker and Traefik

**Lane:** 3 · **Gate:** free (seen at Checkpoint M)

**Description:** Slice 6: containers through sb-docker's listing, and the
Traefik client.

**Acceptance criteria:**
- [x] Container counts and states from the fake Engine API.
- [x] Traefik's status from a fake server, with the spec's errors.
- [x] Docker unreachable shows the spec's line.

**Verification:**
- [x] `cargo test -p sb-motd`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M40
**Files:** `crates/motd/src/docker.rs`, `crates/motd/tests/`
**Scope:** S

## Task M42: motd: motd.yml

**Lane:** 3 · **Gate:** look (colours, the `Config:` line)

**Description:** Slice 2, after config's `check_file` exists: the typed
model read through `check_file` (motd decision 17), the `Config:` line,
colours, `--generate-config` and its golden file.

**Acceptance criteria:**
- [x] motd.yml is read once through `check_file`; issues show on the
      `Config:` line as the spec says.
- [x] `--generate-config` matches its golden file.
- [x] Screenshots of `sb motd` with a configured motd.yml and with a
      broken one.

**Verification:**
- [x] `cargo test -p sb-motd -p sb`.
- [x] Review: code-reviewer.

**Dependencies:** M41, M48
**Files:** `crates/motd/src/config.rs`, `crates/motd/tests/`, `crates/sb/tests/motd.rs`
**Scope:** M

## Task M43: motd: services

**Lane:** 3 · **Gate:** free (seen at Checkpoint M)

**Description:** Slice 7: host's `services` (M2) with motd's configured
names and display names.

**Acceptance criteria:**
- [x] Configured names join the defaults through `with_additional`.
- [x] The section's cap reaches `services` through the token.
- [x] Display names as the spec says.

**Verification:**
- [x] `cargo test -p sb-motd` with stand-in `systemctl`.
- [x] Review: code-reviewer.

**Dependencies:** M42, M2
**Files:** `crates/motd/src/services.rs`
**Scope:** S

## Task M44: motd: HTTP providers, media servers and downloaders

**Lane:** 3 · **Gate:** free (seen at Checkpoint M)

**Description:** The first half of slice 8: the shared client on
`sb_config::http::Http`, the redaction helper and the secret canary
test, with Plex, Emby, Jellyfin, SABnzbd and NZBGet.

**Acceptance criteria:**
- [x] Each provider's values from its fake server match sb-go's.
- [x] The canary test proves no credential reaches output, errors or the
      run log.
- [x] A slow or failing provider stays inside its budget.

**Verification:**
- [x] `cargo test -p sb-motd`.
- [x] Review: code-reviewer, library-reviewer, security-auditor (untrusted
      responses, credentials).

**Dependencies:** M43
**Files:** `crates/motd/src/{redact.rs,providers/}`, `crates/motd/tests/`
**Scope:** M

## Task M45: motd: HTTP providers, queues and torrents

**Lane:** 3 · **Gate:** free (seen at Checkpoint M) · **Shared files:** yes (quick-xml, plan decision 8)

**Description:** The second half of slice 8: the Arr queues,
qBittorrent, and rTorrent's XML-RPC with quick-xml.

**Acceptance criteria:**
- [x] Each provider's values from its fake server match sb-go's.
- [x] rTorrent's `methodCall` and `methodResponse` (int, i4, i8, string,
      array, fault) round-trip through quick-xml.
- [x] The canary test covers these providers too.

**Verification:**
- [x] `cargo test -p sb-motd`.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M44
**Files:** `crates/motd/{Cargo.toml,src/providers/}`, `crates/motd/tests/`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M46: motd: the banner

**Lane:** 3 · **Gate:** look (the banner) · **Shared files:** yes (sb-motd gains shlex, a workspace dependency since M49's 3d0a71b; plan decision 8)

**Description:** Slice 9: the toilet and boxes children, fallbacks, the
type and font checks, `--banner-file-toilet`'s arguments through shlex.

**Acceptance criteria:**
- [x] With toilet and boxes present, the banner as the spec says;
      without them, the fallbacks.
- [x] Unknown types and fonts refused with the spec's texts.
- [x] Screenshots of the banner and a fallback.

**Verification:**
- [x] `cargo test -p sb-motd` with stand-in programs.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M45
**Files:** `crates/motd/{Cargo.toml,src/banner.rs}`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M47: motd: `--share` and the budgets

**Lane:** 3 · **Gate:** free

**Description:** Slice 10: `--share`'s filter, then the spec's criteria
9 to 11 measured on the production build.

**Acceptance criteria:**
- [x] `--share` hides the spec's candidates (IP addresses confirmed by
      `IpAddr`'s parser, host names, credentials).
- [x] The fast path and the 30 s cap meet criteria 9 to 11 on the
      production build.
- [x] Checkpoint M records the numbers.

**Verification:**
- [x] `cargo test -p sb-motd -p sb`; the timing runs.
- [x] Review: code-reviewer.

**Dependencies:** M46
**Files:** `crates/motd/src/{redact.rs,lib.rs}` (redact.rs holds M44's credential hiding, a66b102), `crates/sb/tests/motd.rs`
**Scope:** S

## Checkpoint M: motd
- [x] M36-M47 merged; `check.sh full`; the budgets' numbers recorded.
      Done: full passed on main's tree at 03fcccc (2026-11-17). The
      budgets (motd spec, criteria 9 to 11 and Testing, "Timing"),
      measured on 2026-11-17:
      - Method: the production build of sb and sb-go at 35a7acc
        (`CGO_ENABLED=0`), each run in one ubuntu:22.04 container with
        no network, capped at 2 CPUs and 2 GB, timed by hyperfine
        2.0.0. The flags are Saltbox's login flags (`--all --title
        Saltbox --font ivrit --type parchment`), and motd.yml has one
        instance per provider. The fakes are a socat handler on the
        loopback answering every provider at once (its listen backlog
        raised, so a burst of connections isn't dropped), a port that
        accepts and never answers, and a closed port. toilet and boxes
        are the release's packages, and update-notifier's summary file
        is in place.
      - Criterion 9 (every source answering at once, 30 runs each):
        sb's median 50.2 ms, sb-go's 67.3 ms.
      - Criterion 10 (20 runs of sb, 10 of sb-go): with one instance
        refused, sb 47.4 ms against 47.0 ms with none (sb-go 69.2 ms
        against 68.3 ms); with one instance accepting and never
        answering at the default 1 s timeout, sb 1025.8 ms (sb-go
        1044.2 ms). In that run every other section ended within
        30 ms.
      - Criterion 11 (every source hung: the fakes, stand-ins that
        never end for toilet, boxes, loginctl, last, systemctl and
        update-notifier's scripts, Docker and Traefik pointed at the
        port that never answers): three runs of sb, each ending in
        10.0 s with every section's line shown and no stand-in left.
        The 10 s is Services' two systemctl calls at their 5 s timeout
        each.
      - The container matrix (`crates/motd/tests/matrix.sh`) passed on
        22.04, 24.04 and 26.04, after its `sb motd` calls took
        `--title ''` (M46's banner was in their values).
- [ ] Look: `sb motd --all` with every section (M36, M38, M42, M46 seen
      at their tasks; the rest here).

---

# Lane 3: config and the editors, completion (starts after hardening Task 55)

*The editors' screens (M53-M56) moved to lane 6 on 2026-11-16 (plan.md,
the lane change of 2026-11-16).*

## Task M48: config: `check_file` and `validate_file`

**Lane:** 3 · **Gate:** free

**Description:** The tui and motd amendment to config: `check_file(settings,
file) -> Result<Checked, FileError>` (no network) and
`run::validate_file(step, settings, file, checks)` with `Checks::Run` or
`Checks::Skip`; `run` moved onto them.

**Acceptance criteria:**
- [x] `check_file` and `validate_file` give the same issues as `run` over
      the fixture sets (config criterion 13).
- [x] `Checks::Skip` makes no request, and adds the notice when the file
      has credentials.
- [x] `validate-config`'s output is unchanged (its snapshots pass).

**Verification:**
- [x] `cargo test -p sb-config -p sb`.
- [x] Review: code-reviewer.

**Dependencies:** hardening Task 55 merged (lane 3's start)
**Files:** `crates/config/src/{run.rs,lib.rs}`, `crates/config/tests/`
**Scope:** M

## Task M49: tui editors: `sb edit <sub>` direct

**Lane:** 3 · **Gate:** free

**Description:** The editors' slice 7 (no ratatui): the six
subcommands, the target checks, the editor choice, exec's `attached`,
exit codes, the log record; fake-editor tests through the binary.

**Acceptance criteria:**
- [x] Each subcommand opens its file in the chosen editor (`VISUAL`,
      `EDITOR`, the spec's fallback).
- [x] The target checks refuse what the spec refuses.
- [x] Exit codes and the run-log record as the spec says.

**Verification:**
- [x] `cargo test -p sb`; `crates/sb/tests/edit.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M48 (lane order)
**Files:** `crates/sb/src/{main.rs,edit.rs}`, `crates/sb/tests/edit.rs`
**Scope:** S

## Task M50: tui editors: validation after the edit, "edit again?"

**Lane:** 3 · **Gate:** look (the post-edit report and prompt)

**Description:** The editors' slice 8: `sb edit`'s validation step on
`validate_file` with `Checks::Skip`, the report, "edit again?" through
term-ui's confirm, and `--no-edit-again`.

**Acceptance criteria:**
- [x] After the editor exits, the file's issues show under the step as
      validate-config shows them.
- [x] With issues, the confirm offers another edit; `--no-edit-again`
      answers no.
- [x] Screenshots of a clean edit and of one with issues.

**Verification:**
- [x] `cargo test -p sb`; `crates/sb/tests/edit.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M49, M26 (the confirm)
**Files:** `crates/sb/src/edit.rs`, `crates/sb/tests/edit.rs`
**Scope:** M

## Task M51: config facts: parser and writer

**Lane:** 3 · **Gate:** free

**Description:** The editors' slice 10: `facts/document.rs`, the
line-preserving parser and writer, with the ported round-trip corpus
and the property test.

**Acceptance criteria:**
- [x] Every corpus file round-trips byte for byte (CRLF, continuations,
      comments).
- [x] The property test holds over generated documents.
- [x] Edits change only the lines they touch.

**Verification:**
- [x] `cargo test -p sb-config`.
- [x] Review: code-reviewer.

**Dependencies:** M50
**Files:** `crates/config/src/facts/{mod.rs,document.rs}`, `crates/config/tests/fixtures/facts/`, `crates/config/src/lib.rs`
**Scope:** M

## Task M52: config facts: session and apply

**Lane:** 3 · **Gate:** free

**Description:** The editors' slice 11: the pinned directory, the editor
lock, role locks shared with Saltbox taken at Save (decision 38), drift,
prepare and commit with owner and mode (a temp file on the pinned
directory fd, `O_EXCL`), the outcomes, `is_secret_key`.

**Acceptance criteria:**
- [x] A save writes through the pinned fd and keeps owner and mode.
- [x] A held role lock waits or refuses as the spec says; drift is
      detected before commit.
- [x] Each outcome is reported as the spec names it.

**Verification:**
- [x] `cargo test -p sb-config` on temporary roots.
- [x] Review: code-reviewer, library-reviewer, security-auditor (root
      writes to Saltbox files, locks).

**Dependencies:** M51
**Files:** `crates/config/src/facts/{session.rs,apply.rs,secret.rs}`, `crates/config/tests/`
**Scope:** M

## Task M53: tui editors: the edit picker

**Lane:** 6 · **Gate:** look (the picker)

**Description:** The editors' slice 9 on M61's base: the list with
mouse, restore, and the hand-off to M49's path, so `sb edit` without a
file shows the picker.

**Acceptance criteria:**
- [ ] The picker returns the chosen file and restores the terminal; sb's
      `edit.rs` runs the editor, so both paths share one.
- [ ] Mouse and keys choose a row; Esc and `q` leave.
- [ ] Screenshots of `sb edit`.

**Verification:**
- [ ] `cargo test -p sb-tui -p sb` with TestBackend snapshots.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M52, M74 (crates/tui handoff)
**Files:** `crates/tui/src/{edit_picker.rs,lib.rs}` (`picker.rs` is the log viewer's, M70), `crates/sb/src/edit.rs`, `crates/sb/tests/edit.rs`
**Scope:** S

## Task M54: tui editors: the fact editor, read-only page

**Lane:** 6 · **Gate:** look (the fact editor) · **Shared files:** yes (tui-popup; tui-scrollview is in sb-tui since c8f73c7)

**Description:** The editors' slice 12: `sb fact` loads the role files
and shows headings, search, folding, masking, Show and Copy.

**Acceptance criteria:**
- [ ] Secrets are masked by `is_secret_key` until Show.
- [ ] Search and folding as the spec says.
- [ ] Screenshots of `sb fact` on fixture role files.

**Verification:**
- [ ] `cargo test -p sb-tui -p sb` with TestBackend snapshots.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M53
**Files:** `crates/tui/{Cargo.toml,src/fact/}`, `crates/sb/src/{main.rs,fact.rs}`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M55: tui editors: the fact editor's changes

**Lane:** 6 · **Gate:** look (the panel and dialogs)

**Description:** The editors' slice 13: edit, add key, delete at three
levels, the changes panel with undo, save, quit, and the lock-wait,
drift and outcome dialogs.

**Acceptance criteria:**
- [ ] Each change shows in the panel and undoes.
- [ ] Save goes through M52's apply; each outcome has its dialog.
- [ ] Screenshots of the panel and each dialog. If it grows past M, the
      delete levels split into a task of their own.

**Verification:**
- [ ] `cargo test -p sb-tui -p sb`.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M54
**Files:** `crates/tui/src/fact/`, `crates/sb/tests/fact.rs`
**Scope:** M

## Task M56: tui editors: mouse, the menu, the PTY smoke test

**Lane:** 6 · **Gate:** look (the menu; the manual check)

**Description:** The editors' slice 14: mouse through recorded layout
rects, the right-click menu, the PTY smoke test, then the manual
two-emulator check (Shift-drag selection) and the maintainer's look.

**Acceptance criteria:**
- [ ] Clicks and the menu act on the rows under them.
- [ ] The PTY smoke test drives `sb fact` end to end.
- [ ] The two-emulator check is recorded; screenshots of the menu.

**Verification:**
- [ ] `cargo test -p sb-tui -p sb`; `crates/sb/tests/fact.rs`.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M55
**Files:** `crates/tui/src/fact/`, `crates/sb/tests/fact.rs`
**Scope:** M

## Checkpoint E: editors
- [ ] M48-M56 merged; `check.sh full`.
- [ ] Look: the post-edit report (M50), the edit picker (M53), the fact
      editor's page, panel, dialogs and menu (M54-M56).

## Task M57: completion: `CompleteEnv`, the tag completer, `generate`

**Lane:** 3 · **Gate:** unpaused by the maintainer, 2026-11-17 (they test sb's real completion once the CLI is complete, instead of reviewing proto-complete first) · **Shared files:** yes (new crate sb-completion; clap_complete =4.6.11 with `unstable-dynamic`; deny.toml)

**Description:** Completion's slice 1: `CompleteEnv` first in `main`
(the cli-core amendment), the tag completer with sb-go's cases,
`completion generate`, and the container check for bash, zsh and fish
settling verification items 1-6.

**Acceptance criteria:**
- [ ] A run with `SB_COMPLETE` prints only candidates and is never logged.
- [ ] The tag completer reads ansible's cache and answers sb-go's cases.
- [ ] Verification items 1-6 settled in each shell's container (2's symlink name and 5, the completer path, needed `completion install`: settled with M58).

**Verification:**
- [ ] `cargo test -p sb-completion -p sb`; `tests/shells.sh` under the cap.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** the maintainer's proto-complete review; M32
**Files:** `crates/completion/{Cargo.toml,src/lib.rs,src/tags.rs,tests/}`, `crates/sb/src/main.rs`, `crates/sb/Cargo.toml`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M58: completion: `completion install`

**Lane:** 3 · **Gate:** paused

**Description:** Slice 2: names, probes, paths, stubs per name, markers,
atomic writes, result lines, `--allow-unavailable`, the three spellings.

**Acceptance criteria:**
- [ ] Each shell's stub lands where its probe says, atomically.
- [ ] Markers and result lines as the spec says.
- [ ] `--allow-unavailable` and the three spellings behave as specified.

**Verification:**
- [ ] `cargo test -p sb-completion -p sb` under a temporary `Paths` root.
- [ ] Review: code-reviewer, library-reviewer, security-auditor (files
      written as root into shell directories).

**Dependencies:** M57
**Files:** `crates/completion/src/install.rs`, `crates/sb/src/main.rs`
**Scope:** M

## Task M59: completion: migration and cleanup

**Lane:** 3 · **Gate:** paused

**Description:** Slice 3: sb-go's files and markers, the old outputs,
live-executable preservation, from sb-go-written fixtures.

**Acceptance criteria:**
- [ ] sb-go's files are found and replaced as the spec says.
- [ ] A live executable is never removed.
- [ ] The fixtures are sb-go-written.

**Verification:**
- [ ] `cargo test -p sb-completion`.
- [ ] Review: code-reviewer.

**Dependencies:** M58
**Files:** `crates/completion/src/install.rs`, `crates/completion/tests/`
**Scope:** S

## Task M60: completion: `refresh(step)`

**Lane:** 3 · **Gate:** paused

**Description:** Slice 4: the per-shell refresh steps for ops' `update`
and `setup`, with a warning on failure.

**Acceptance criteria:**
- [ ] One step per shell (every shell, as sb-go; one not installed is checked).
- [ ] A failure warns and doesn't fail the caller.
- [ ] `refresh(step)` is public as ops calls it.

**Verification:**
- [ ] `cargo test -p sb-completion`.
- [ ] Review: code-reviewer.

**Dependencies:** M59
**Files:** `crates/completion/src/lib.rs`
**Scope:** S

## Checkpoint C: completion
- [ ] M57-M60 merged; `check.sh full`; the shell containers pass.
- [ ] Look: covered by the proto-complete review that unpaused it.

---

# Lane 4: docker, journal, the log viewer, ops

*On 2026-11-16 bench (M77, M78) moved to lane 5 and M79-M86 to lane 1;
M87 has no lane until completion (M57-M60) is unpaused. Restore (M75,
M76) stays in lane 4 (plan.md, the lane change of 2026-11-16).*

## Task M61: tui: groundwork

**Lane:** 4 · **Gate:** free · **Shared files:** yes (new crate sb-tui; ratatui =0.30.2 with its features, ratatui-core =0.1.2 `anstyle`, crossterm `event-stream`, ansi-to-tui; deny.toml; the hook; `MUTATION_TESTERS`)

**Description:** The tui spec's slice 0, no user-visible behaviour: the
workspace entries, the crate, the gate bookkeeping, terminal setup and
restore through `ratatui::init`/`restore` chained with cli-core's
restore hook and panic path, the async event loop on the current-thread
runtime with the PTY input-latency measurement, and the size ADR.

**Acceptance criteria:**
- [x] deny.toml: `sb-tui` (wrappers `sb`), added to the wrappers of
      `sb-cli-core`, `sb-term-ui`, `sb-host`, `sb-docker`; the hook lists
      `tui`; `MUTATION_TESTERS[sb-tui]="sb"` and `sb` joins sb-journal's
      entry.
- [x] A panic or signal restores the terminal (mouse off) through the
      chained hooks.
- [x] The size ADR records the production binary's growth; the
      input-latency figure is in the spec's verification notes.

**Verification:**
- [x] `cargo test -p sb-tui`; the production build's size.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** None
**Files:** `crates/tui/{Cargo.toml,src/lib.rs,src/term.rs}`, `deny.toml`, `.claude/hooks/library-check.sh`, `scripts/check.sh`, `docs/decisions/` (the size ADR), `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M62: docker: the picker's fields and `picker_status`

**Lane:** 4 · **Gate:** free

**Description:** The tui amendment to docker: `Row` gains
`restart_count`, `created`, `started_at` and `finished_at` from the
inspect; `picker_status(row, now, format)` returns symbol, text, role and
attention.

**Acceptance criteria:**
- [x] Docker's zero time and unparsable values read as `None`;
      `--json`'s keys are unchanged.
- [x] The status text follows `ps::status` with the spec's suffixes, the
      10-minute restart rule, `↻` and attention.
- [x] Durations come from the caller's function (sb-docker doesn't
      depend on sb-host).

**Verification:**
- [x] `cargo test -p sb-docker -p sb` (ps's snapshots unchanged).
- [x] Review: code-reviewer.

**Dependencies:** M61 (lane order only)
**Files:** `crates/docker/src/{ps.rs,connect.rs}`
**Scope:** S

## Task M63: docker: log reads and the daemon probe

**Lane:** 4 · **Gate:** free

**Description:** The daemon probe and `docs/platform-notes/docker-logs.md`
first (`tail` before `since`/`until`, the order of equal timestamps, 16
KiB partial lines, `until`'s inclusiveness), then `logs.rs`: `Position`,
`LogLine`, `tail`, `older`, `newer`, `follow`, and the fake Engine API's
`/containers/{id}/logs` (multiplexed and TTY).

**Acceptance criteria:**
- [x] The platform note records Docker's version and method, no
      environment details; the probe runs on a lane-prefixed container
      only.
- [x] Each read parses the RFC 3339 prefix with jiff, filters its window
      itself, and counts ordinals among equal timestamps; an unparsable
      line keeps the previous position with the next ordinal.
- [x] `older` widens its window from 60 s, doubling, down to `created`;
      `newer` sets `more_newer` from one extra entry.

**Verification:**
- [x] `cargo test -p sb-docker` on the fake; the probe under the cap.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M62
**Files:** `crates/docker/src/{logs.rs,lib.rs,testing/}`, `docs/platform-notes/docker-logs.md`
**Scope:** M

## Task M64: docker: container events

**Lane:** 4 · **Gate:** free

**Description:** `events.rs`: `container_events(daemon)` through
bollard's `events` with `type=container`, and the fake's `/events`.

**Acceptance criteria:**
- [x] The named actions map; others pass through.
- [x] Each event carries the container's ID and name.
- [x] The stream ends cleanly when the daemon ends it.

**Verification:**
- [x] `cargo test -p sb-docker`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M63
**Files:** `crates/docker/src/{events.rs,lib.rs,testing/}`
**Scope:** S

## Task M65: journal: `Journal::unit_query`

**Lane:** 4 · **Gate:** free

**Description:** The tui amendment to journal: `unit_query(&self, unit,
coredump_uid) -> Query`, journalctl's `-u` for the release and source the
journal was opened with (page-L5).

**Acceptance criteria:**
- [x] It equals `Query::unit` with the journal's own release and source.
- [x] The doc states the follow rule (a follow's `from` is the `end` of a
      page of the same query).
- [x] `Query::unit` stays for the test tools.

**Verification:**
- [x] `cargo test -p sb-journal -p sb-host`.
- [x] Review: code-reviewer.

**Dependencies:** M64 (lane order only)
**Files:** `crates/journal/src/lib.rs`, `crates/journal/tests/`
**Scope:** S

## Task M66: tui: the journal adapter and the service status rule

**Lane:** 4 · **Gate:** free

**Description:** From the tui spec's slice 1, the parts with no screen:
the source protocol, the journal adapter (tail, older, newer, the cycle
guard, the follow with `FollowWait`, one query per source), and the pure
service status rule.

**Acceptance criteria:**
- [x] The adapter pages and follows on crafted journals, giving every
      entry once across the handover.
- [x] The cycle guard stops a page that returns where it began.
- [x] The status rule's table from the spec, with "failed 2h ago" from
      `StateChangeTimestamp`.

**Verification:**
- [x] `cargo test -p sb-tui` on sb-journal's `testing` builder.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M61, M65, M2
**Files:** `crates/tui/src/{source.rs,source/journal.rs,status.rs,testing.rs}`
**Scope:** M

## Task M67: tui: the viewer

**Lane:** 4 · **Gate:** look (shown through `sb logs` in M68; the two merge together)

**Description:** The viewer of slice 1: the buffer, windowed scrolling,
wrap, paging, follow, markers, `t`, the line format and the record cap
with the small limits; Esc, `q`, Ctrl-C 130.

**Acceptance criteria:**
- [x] Scrolling, paging and follow on a fake source, with TestBackend
      snapshots.
- [x] Lines through `DisplayText` and ansi-to-tui; the record cap holds.
- [x] Ctrl-C ends with 130 and the terminal restored.

**Verification:**
- [x] `cargo test -p sb-tui` with snapshots.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M66
**Files:** `crates/tui/src/{viewer.rs,line.rs,lib.rs}`
**Scope:** M

## Task M68: tui: `sb logs`, the Services picker

**Lane:** 4 · **Gate:** look (the picker and viewer) · **Shared files:** yes (sb depends on sb-tui)

**Description:** The rest of slice 1: the picker's Services section (no
filter yet) on host's `services`, `sb logs`, `SB_JOURNAL_DIR`, the empty
and failure cases, and the PTY smoke test on crafted journals.

**Acceptance criteria:**
- [x] `sb logs` lists the services and opens the viewer on the chosen
      one.
- [x] No services, an unreadable journal and no terminal give the spec's
      texts.
- [x] Screenshots of the picker and the viewer (M67's look with this one).

**Verification:**
- [x] `cargo test -p sb-tui -p sb`; `crates/sb/tests/logs*.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M67
**Files:** `crates/tui/src/{picker.rs,lib.rs}`, `crates/sb/src/{main.rs,logs.rs}`, `crates/sb/Cargo.toml`, `crates/sb/tests/logs.rs`, `Cargo.lock`
**Scope:** M

## Task M69: tui: the docker source and `sb docker logs`

**Lane:** 4 · **Gate:** look (container rows)

**Description:** Slice 2: the docker adapter with follow across
restarts, the Containers section kept live by events, `sb docker logs`,
the Docker-unreachable row.

**Acceptance criteria:**
- [x] A container's logs page and follow across a restart on the fake.
- [x] The section reloads on events; picker rows use `picker_status`.
- [x] Screenshots of the Containers section and a container's log.

**Verification:**
- [x] `cargo test -p sb-tui -p sb`; `crates/sb/tests/logs_docker.rs`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M68, M64
**Files:** `crates/tui/src/{source/docker.rs,picker.rs}`, `crates/sb/src/docker_logs.rs`, `crates/sb/tests/logs_docker.rs`
**Scope:** M

## Task M70: tui: the unified picker

**Lane:** 4 · **Gate:** look (Needs attention, the filter line) · **Shared files:** yes (ratatui-textarea)

**Description:** Slice 3: Needs attention, the name filter
(ratatui-textarea), the kind filter and `Tab`, mouse on both screens,
the services' 5 s refresh.

**Acceptance criteria:**
- [x] Rows with attention head the list.
- [x] The filters narrow the list; `Tab` moves between sections.
- [x] Screenshots of the filtered picker.

**Verification:**
- [x] `cargo test -p sb-tui` with snapshots.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M69
**Files:** `crates/tui/{Cargo.toml,src/picker.rs}`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M71: tui: search

**Lane:** 4 · **Gate:** look (highlights)

**Description:** Slice 4: smart-case matches, highlights across spans,
n/N, horizontal scrolling with wrap off.

**Acceptance criteria:**
- [x] Smart case: lowercase matches any case, capitals match exactly.
- [x] A match spanning styled spans highlights whole.
- [x] Screenshots of a search with matches.

**Verification:**
- [x] `cargo test -p sb-tui`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M70
**Files:** `crates/tui/src/{search.rs,viewer.rs}`
**Scope:** S

## Task M72: tui: pasteable output and the inline fallback

**Lane:** 4 · **Gate:** look (the inline forms)

**Description:** Slice 5: `s` and `p`; the picker on `/dev/tty` with the
plain stream when stdout isn't a terminal; the no-terminal error.

**Acceptance criteria:**
- [x] `s` saves and `p` prints as the spec says.
- [x] Piped `sb logs` picks on `/dev/tty` and streams plain lines.
- [x] Screenshots of the inline picker and of a saved file's head.

**Verification:**
- [x] `cargo test -p sb-tui -p sb`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M71
**Files:** `crates/tui/src/{save.rs,lib.rs}`, `crates/sb/tests/logs.rs`
**Scope:** M

## Task M73: tui: hardening

**Lane:** 4 · **Gate:** free

**Description:** Slice 6: the frame-cost test and its timing,
colour-profile guards on log spans, RLIMIT_NOFILE, follow-L1 and
follow-L4, the security-auditor pass (journal files and container output
are untrusted).

**Acceptance criteria:**
- [x] The frame-cost test bounds a frame's time as the spec says.
- [x] Log spans step down with the colour profile.
- [x] follow-L1 and follow-L4 fixed test-first.

**Verification:**
- [x] `cargo test -p sb-tui -p sb`.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M72
**Files:** `crates/tui/src/{line.rs,term.rs,source/journal.rs}`, `crates/tui/tests/`
**Scope:** S

## Task M74: ops: crate, announcement rules, `sb announcements`, tui's inline Markdown

**Lane:** 4 · **Gate:** look (the inline Markdown) · **Shared files:** yes (new crate sb-ops; tui-markdown in sb-tui, plan decision 8; deny.toml; the hook's list)

**Description:** ops' slice 10 with tui's inline announcement form: the
crate, the announcement rules (load through the YAML entry, diff, gate,
sort, the migration list), `sb announcements` (planned hidden, with
`--migrations`; see as built), and tui's inline Markdown output (each announcement
under `<Repo> Announcement - <Month D, YYYY>`, messages through
`DisplayText`). After this task merges, `crates/tui` passes to lane 3.
*As built (the maintainer, 2026-11-15 and 2026-11-16): `sb
announcements` is shown and read-only, with no `--migrations`
(migrations run during `sb update` only); it opens tui's announcement
viewer on a terminal (M87's viewer half, built with it) and prints
inline otherwise; without `-r` it reads both repositories, a found file
that fails warns and the other's show, and the step line counts what is
shown.*

**Acceptance criteria:**
- [x] The rules on fixtures: new entries, the required folder and file,
      the stable date sort, Saltbox's before Sandbox's.
- [x] `sb announcements --before --after` prints inline and runs nothing.
- [x] Screenshots of the inline output.

**Verification:**
- [x] `cargo test -p sb-ops -p sb-tui -p sb`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M73
**Files:** `crates/ops/{Cargo.toml,src/lib.rs,src/announcements.rs}`, `crates/tui/{Cargo.toml,src/markdown.rs}`, `crates/sb/src/{main.rs,announcements.rs}`, `crates/sb/Cargo.toml`, `deny.toml`, `.claude/hooks/library-check.sh`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Checkpoint V: log viewer
- [x] M61-M74 merged; `check.sh full`. Done: full passed on main's
      tree at 798cbdd (2026-11-17).
- [x] Look: the picker and viewer (M67, M68), container rows (M69), the
      unified picker (M70), search (M71), the inline forms (M72), the
      inline announcements (M74). Done: each approved by the maintainer
      at its task's look.
- [x] The announcement viewer's spec section asked for (plan open
      question 1). Done: written at M74's look (tui.md, "The
      announcement viewer", 397f079) and built in c8f73c7.

## Task M75: ops: restore core

**Lane:** 4 · **Gate:** free · **Shared files:** yes (aes, cbc, pbkdf2 with hmac, sha1, zeroize)

**Description:** ops' slice 1: `user_key`, `decrypt`, the openssl-made
fixtures (how they were made, recorded), and the placement layer on a
temporary root.

**Acceptance criteria:**
- [x] Fixtures made by openssl decrypt; a wrong password is the padding
      error.
- [x] Keys, IVs, passwords and plaintext are zeroized.
- [x] Placement writes files with the spec's modes under the root.

**Verification:**
- [x] `cargo test -p sb-ops`; the crates' size measured.
- [x] Review: code-reviewer, security-auditor (secrets, crypto).

**Dependencies:** M74
**Files:** `crates/ops/{Cargo.toml,src/restore/}`, `crates/ops/tests/fixtures/`, `Cargo.toml`, `Cargo.lock`
**Scope:** S

## Task M76: ops: `sb restore`, term-ui's text and password prompts

**Lane:** 4 · **Gate:** look (the text and password prompts)

**Description:** ops' slice 2: term-ui's text and password prompts (their
first user; no cancel key, Ctrl-C ends), `--user` and `--password-file`,
the fetch against the fake service, the output lines, the secrets log
test, and the one read-only check of the real service's 404.

**Acceptance criteria:**
- [x] The prompts read the terminal; `--user` and `--password-file`
      answer them; no terminal and no answer fails.
- [x] No password or plaintext reaches output, errors or the run log.
- [x] Screenshots of both prompts and a restore run on the fake service.

**Verification:**
- [x] `cargo test -p sb-ops -p sb-term-ui -p sb`; the contract test's new
      `EXTENSIONS` lines.
- [x] Review: code-reviewer, library-reviewer, security-auditor.

**Dependencies:** M75, M26 (prompt.rs handoff)
**Files:** `crates/ops/src/restore/`, `crates/term-ui/src/prompt.rs`, `crates/sb/src/{main.rs,restore.rs,contract.rs}`, `crates/sb/tests/restore.rs`
**Scope:** M

## Task M77: ops: bench, system and disk

**Lane:** 5 · **Gate:** look (bench's new format) · **Shared files:** yes (sysinfo's `disk` feature)

**Description:** ops' slice 3: sysinfo rows, cpuinfo parsing, host's
`free_space`, the in-process disk test, the tables, `sb bench`'s help
text.

**Acceptance criteria:**
- [ ] The system rows and Total Disk found as bench.sh finds them (ops
      decision 33); cpuinfo parsed from fixtures.
- [ ] The disk test writes in-process with the spec's sizes
      and cleans up.
- [ ] Screenshots of `sb bench` without the network part.

**Verification:**
- [ ] `cargo test -p sb-ops -p sb`.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M4 (M76 was lane order, dropped with the lane change of 2026-11-16)
**Files:** `crates/ops/src/bench/{mod.rs,system.rs,disk.rs}`, `crates/sb/src/{main.rs,bench.rs}`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M78: ops: bench, network

**Lane:** 5 · **Gate:** look (the network rows) · **Shared files:** yes (sb-ops gains tar and flate2, which M18 added to the workspace in 5ab54b2)

**Description:** ops' slice 4: release's verified download of both
pinned Speedtest assets, the tar extraction (size-capped, one entry),
the Speedtest JSON with its platform note from one run, icanhazip and
ipinfo.

**Acceptance criteria:**
- [ ] The Speedtest asset is pinned and verified before it runs.
- [ ] The JSON parse matches the platform note's recorded shape
      (version, method, no environment details).
- [ ] Screenshots of a full `sb bench`.

**Verification:**
- [ ] `cargo test -p sb-ops` with the `testing` fakes; one real run.
- [ ] Review: code-reviewer, library-reviewer, security-auditor (a
      downloaded program run as root).

**Dependencies:** M77, M29
**Files:** `crates/ops/src/bench/{speedtest.rs,network.rs}`, `crates/ops/src/testing.rs`, `docs/platform-notes/speedtest.md`, `Cargo.toml`, `Cargo.lock`
**Scope:** M

## Task M79: ops: `sb branch`, `sb branch-sandbox`

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 9, the update part's first slice: both
branch commands on repo, replacing M5's bare wiring, with
`saltbox_user()` as the owner; runtime and tags left out until those
land (each added in M85).

**Acceptance criteria:**
- [x] `<name>` is required and checked by `check_branch_name`.
- [x] Each resets its checkout through `fetch_and_reset` with the Saltbox
      user as owner.
- [x] sb-go's texts and help.

**Verification:**
- [x] `cargo test -p sb-ops -p sb` on local bare repositories.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M8, M12 (M78 was lane order, dropped with the lane change of 2026-11-16)
**Files:** `crates/ops/src/branch.rs`, `crates/sb/src/{main.rs,branch.rs}`, `crates/sb/tests/branch.rs`
**Scope:** M

## Task M80: ops: setup's and gha's shared pieces

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 5: `prerequisites`, `locale`,
`copy_default_configs`, `secure_existing_configs`, `git_hooks`, and the
stub-and-namespace harness the later ops tests use.

**Acceptance criteria:**
- [x] Each piece runs host's apt and file calls in sb-go's order with its
      texts and modes.
- [x] The harness runs them against stand-in programs and a temporary
      root.
- [x] `add_repositories` is host's (M10), not re-implemented.

**Verification:**
- [x] `cargo test -p sb-ops`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M79, M9, M10
**Files:** `crates/ops/src/{prereq.rs,locale.rs,configs.rs,hooks.rs,testing.rs}`
**Scope:** M

## Task M81: ops: `sb gha`

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 6: gha's sequence, `SUDO_USER` ownership, the
symlinked root, and the container matrix on 22.04, 24.04 and 26.04.

**Acceptance criteria:**
- [x] A relaunched `runner` user's run leaves the venv, uv, the fact and
      the six configs owned as the spec says.
- [x] The symlinked Saltbox root works.
- [x] The matrix passes on three releases.

**Verification:**
- [x] `cargo test -p sb-ops -p sb`; `gha-matrix.sh` under the cap.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M80, M16, M19
**Files:** `crates/ops/src/gha.rs`, `crates/ops/tests/gha-matrix.sh`, `crates/sb/src/{main.rs,gha.rs}`
**Scope:** S

## Task M82: ops: `sb setup`

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 7: the guard, the phases, the branch prompt
with `--keep-branch`/`--reset-branch`, hidden `-v`; the completion
refresh is left to M87.

**Acceptance criteria:**
- [x] The guard's five cases with their texts.
- [x] The phases run in sb-go's order and names; no terminal and no
      branch flag fails before any step.
- [x] The contract test's setup `EXTENSIONS` lines.

**Verification:**
- [x] `cargo test -p sb-ops -p sb` on the harness.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M81, M3, M8
**Files:** `crates/ops/src/setup.rs`, `crates/sb/src/{main.rs,setup.rs,contract.rs}`, `crates/sb/tests/setup.rs`
**Scope:** M

## Task M83: ops: the install script

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 8: `install/install.sh` from sb-go's plus
`-b`, its container tests (sb-go's test-install cases) and the
acceptance checks. Publishing it is a separate, explicitly authorised
step, not part of this task.

**Acceptance criteria:**
- [x] The script installs sb and runs `sb setup` attached to the
      terminal, passing `-b`.
- [x] sb-go's test-install cases pass in containers.
- [x] ShellCheck clean, if the gate runs it.

**Verification:**
- [x] `install/tests/` under the cap.
- [x] Review: code-reviewer, security-auditor (a script run as root).

**Dependencies:** M82
**Files:** `install/install.sh`, `install/tests/`
**Scope:** M

## Task M84: ops: `update`, part 1

**Lane:** 1 · **Gate:** free

**Description:** The first half of ops' slice 11: the up-front flag
check, disk space, self-update (exit 0 with the "Re-run" warning), the
branch questions, apt, both repositories.

**Acceptance criteria:**
- [x] The flag check refuses conflicting flags before any step.
- [x] Self-update inside `update` ends the run with exit 0 and the
      warning.
- [x] Both repositories update with the branch rules.

**Verification:**
- [x] `cargo test -p sb-ops -p sb`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M83, M26, M79
**Files:** `crates/ops/src/update.rs`, `crates/sb/src/{main.rs,update.rs,contract.rs}`, `crates/sb/tests/update.rs`
**Scope:** M

## Task M85: ops: `update`, part 2

**Lane:** 1 · **Gate:** free

**Description:** The second half of slice 11: secure files, validation
through `validate_file` with `Checks::Run`, the runtime through
`reconcile_runtime`, the tag cache refresh, announcements inline (M74);
the branch commands gain runtime and tags too.

**Acceptance criteria:**
- [x] Validation issues show as validate-config shows them.
- [x] The venv and the tag cache refresh after each pull.
- [x] New announcements print inline.

**Verification:**
- [x] `cargo test -p sb-ops -p sb`.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M84, M48, M17, M32
**Files:** `crates/ops/src/{update.rs,branch.rs}`, `crates/sb/tests/update.rs`
**Scope:** M

## Task M86: ops: migrations

**Lane:** 1 · **Gate:** free

**Description:** ops' slice 12: the migration table, the confirm (or
`--migrations`), ansible's `run` by tag per repository.

**Acceptance criteria:**
- [x] All or none; declined ones are warned about by name.
- [x] A failing migration stops with the spec's text and exit 1.
- [x] `--migrations` answers the confirm.

**Verification:**
- [x] `cargo test -p sb-ops -p sb` with the fake playbook.
- [x] Review: code-reviewer, library-reviewer.

**Dependencies:** M85, M31
**Files:** `crates/ops/src/{update.rs,announcements.rs}`, `crates/sb/tests/update.rs`
**Scope:** S

## Task M87: ops: the announcement viewer and the completion refresh

**Lane:** none until completion (M57-M60) is unpaused · **Gate:** paused (the proto-complete review; the announcement viewer's tui spec section came with M74, 397f079)

**Description:** ops' slice 13: `update` and `setup` show announcements
in tui's viewer on a terminal and refresh completion, once both exist.
*The viewer itself is built (with M74, 2026-11-16); what is left is
calling it from `update` and `setup`, and the completion refresh.*

**Acceptance criteria:**
- [ ] On a terminal, `update` opens the announcement viewer; piped, the
      inline form stays.
- [ ] `update` and `setup` call `refresh(step)`.
- [ ] A refresh failure warns only.

**Verification:**
- [ ] `cargo test -p sb-ops -p sb`.
- [ ] Review: code-reviewer, library-reviewer.

**Dependencies:** M86, M60 (the announcement viewer's spec section and build came with M74: 397f079, c8f73c7)
**Files:** `crates/ops/src/{update.rs,setup.rs}`
**Scope:** S

## Checkpoint O: ops
- [ ] M74-M87 merged; `check.sh full`; the gha matrix and install-script
      containers pass.
- [ ] Look: restore's prompts (M76), bench's format (M77, M78).

## Checkpoint Z: the binary is complete
- [ ] Every M task merged; every Commands row "built".
- [ ] The by-hand container scripts pass on the final tree: the motd,
      gha and setup matrices, the install-script containers, and
      `crates/sb/tests/hangup-systemd.sh` (hangup's session-scope test on
      a systemd of its own, which the gate's sandbox hides; added
      2026-11-17).
- [ ] The capability map's "When the binary is complete" items go to the
      maintainer: the schemas' owner, the ratchet rows re-measured, glibc
      and musl side by side (settled 2026-11-17, ahead of the checkpoint:
      glibc with its own malloc; ADR-020, "Confirmed").

## Follow-ups found during the build

- [ ] **Paste arrives as one input** (lane 6 after M56; the maintainer,
      2026-11-16). Bracketed paste is off in sb-tui's shared screen
      setup, so a paste reaches every text box as keystrokes and a
      pasted newline stages a value early. Turn it on (crossterm's
      `EnableBracketedPaste`, off again on every way out) and insert a
      paste whole (ratatui-textarea's `insert_str`) on every screen that
      takes text.
- [x] **First sight far right meets 5 ms** (the maintainer, 2026-11-16).
      With wrap off far to the right, a line the viewer has never drawn
      there (paging to new lines, a jump from far away) is still parsed
      from its start once: 5.5 to 23.5 ms for a screen at column 4,000,
      after ed95149 brought rebuilds to 2.5 to 4 ms. Bring the first
      frame under 5 ms too, for example by parsing places ahead in the
      background.
      Done: 37aceeb (a page up, Home or End far right 1.8 to 4.8 ms).
- [ ] **One enum for ops' two repositories** (found by lane 1's M79,
      2026-11-16). `branch::Checkout` (M79) and `announcements::Repo` (M74)
      both name Saltbox and Sandbox. Merge them when update needs both
      (M84 or M85).
- [x] **"Refused" test ports stay reserved** (found by lane 3's gate,
      2026-11-16). `refused()` in crates/sb/tests/docker_control.rs and
      crates/config/tests/run_log.rs binds port 0, drops the listener and
      uses the freed port as one nothing listens on; under the gate's
      parallel load another test's fake server can take that port, and
      `each_failure_shows_its_block_and_exits_1` got a 404 instead of a
      refused connection. Hold the port for the test's lifetime with a
      socket that is bound but never listens (a connect to it is refused),
      for example socket2's `Socket::bind` without `listen`.
      Done: 798cbdd (a tokio `TcpSocket` bound but never listening, in
      all eight helpers, `refused_url` held for the test binary's life).
- [x] **The viewer's far-right rebuild frame meets 5 ms** (lane 2; the
      maintainer, 2026-11-15). With wrap off far to the right, a frame that
      rebuilds every line in view (a jump there, a search, a theme or `t`
      change, a resize) still grows with the column: about 6 ms on plain
      records and 19 ms on CJK or colour-per-word records at column 4,000
      (ADR-023, tui.md). The text left of the column has to be parsed for
      its colours, and neither sb's text parser nor ansi-to-tui can resume
      mid-line: make the colour parse resumable (checkpoints of the SGR
      state every N columns, cached per line) so the rebuild starts near
      the column.
      Done: `DisplayText::resumable` and `from_bytes_at` (term-ui) resume
      the parse; each line built far right keeps its places (byte, column,
      style, about every 64 columns) in the frame cache, within its 4 MiB
      and across theme, `t` and search changes (`resume.rs`). A `t`
      change or a search at columns 1,000 and 4,000 takes 2.3 to 4.1 ms on
      plain, CJK and colour-per-word records, a resize there 3.8 to 4.9
      ms (ADR-023, the tui spec's "Hardening as built"). A line first
      seen far right (a page up or a jump there) is still parsed from its
      start once: 5.5 to 23.5 ms a screen of them at 4,000.
- [x] **Every run log line carries the run id** (lane 4; the maintainer,
      2026-11-15). sb-host's ownership lines (`sb_host::owner: ownership
      ...`, `recursive ownership ...`) are written outside the run's span,
      so grepping a run id misses them; carry the span into every thread
      (`Span::current()` and `in_scope`, or `Instrument`). Done:
      c311a7f, and ff5ed2d's `sb_cli_core::spawn` with a clippy rule
      against bare spawns (the maintainer, 2026-11-16).
- [x] **Values sb computes are shown, not `[ARG]`** (lane 4; the
      maintainer, 2026-11-15). python-env's children log sb's own paths
      as `[ARG]` (`uv pip sync --python [ARG] ... [ARG]`); show every value
      sb computes with exec's `shown_value`, and mask only real secrets.
      Done: 6e12a38 (exec decision 14).
- [x] **Mutation passes run in a sandbox** (the gate; the maintainer,
      2026-11-15). The passes run tests as root, so a mutant that bends a
      path or drops a safety flag writes on the host: on 2026-11-15 a
      mutant of the motd.yml migration's write wrote /nonexistent through
      a test's planted link, and a mutant of its backups lookup left two
      test backups and a root-owned /opt/saltbox/backups (all removed with
      the maintainer's approval). Run cargo-mutants in a private mount
      namespace where the host is read-only except the pass's own
      directory and /tmp (unshare, / bind-mounted read-only), so root
      tests keep their temp roots and nothing else can change. Record it
      in ADR-021; floor-guard tests show a mutant's write to a host path
      fails inside the pass.
      Done: 984d853.
- [x] **The viewer's wrap-off frame meets 5 ms** (the maintainer,
      2026-11-15). M73 measured a wrap-off frame at about 5.6 ms (9.9 ms
      for the frame that switches wrap off) at 240x70 on the spec's
      worst-case records, against the tui spec's 5 ms (success criterion
      8); wrap-on frames take 2-4 ms. The cost is building the coloured
      text of the ~70 long lines on screen each frame: cache it per line,
      as M73 cached the wrapped height, within the 32 MiB buffer cap.
      Done: wrap-off heights counted from the bytes, texts built only as
      far as the window reaches, and each frame's texts kept for the next
      (bounded to the entries in view); the switch frame 3.1 ms, other
      wrap-off frames 1.3-1.8 ms (ADR-023, the tui spec's "Hardening as
      built"). Far right stays over 5 ms past about column 1,000.
- [x] **exec's remaining fixed bounds in groups.rs** (found reviewing
      3f9e8f9). `took < GRACE` at crates/exec/tests/groups.rs:264, :322
      and :348, `start.elapsed() < GRACE` at :363 and :454, and `< BOUND`
      at :233, :277 and :301 are fixed wall-clock bounds of the kind
      3f9e8f9 replaced; give them baseline-derived bounds the same way.
      Done: 6c0c747.
- [x] **Leaked exec helper shells after 517de92** (the gate). On
      2026-11-14, 185 orphaned `dash` helper shells (exec's tests' `sh -c
      'trap ... exit'`) were found with their working directory in five
      deleted mutation-pass copies of one lane's worktree, though both
      sweeps match a process's working directory since 517de92. Either
      those gate runs were killed without their EXIT trap, or the sweep
      misses a case (a shell started after the last sweep, by a process
      the sweep killed?). Reproduce with a pass over sb-exec, find which,
      and fix or document it.
      Done: 61af0ef (the shells came from gate runs killed mid-pass).
- [x] **sb-ansible's log test is flaky under load** (lane 4, with its next
      task). `cache::tests::a_corrupt_or_unreadable_file_is_logged`
      (crates/ansible/src/cache.rs) captured an empty log once, during the
      instrumented coverage run on a loaded host; eight plain reruns
      passed. Likely tracing's per-callsite interest cache racing a
      `with_default` subscriber on another test thread, the cause of M10's
      flake. Fix it as M10's was: one global test subscriber with a
      per-thread buffer.
- [x] **sb-release's ETXTBSY in `a_binary_reporting_another_version_is_refused`**
      (lane 2). One full run failed with "Text file busy" (the fork race
      ADR-021 describes); five reruns passed. Write the stand-in, then
      run it, so that no other test's fork holds its write descriptor
      (the SPAWNS lock pattern).
- [x] **`--help` shows a flag's long help** (from M31, lane 2 with M70).
      sb's help renderer (cli-core's `flag_row`) shows only the short help,
      so `-e`'s long help (ansible spec, lines 75-77: "Each value reaches
      Ansible whole...") never reaches `sb install --help`. cli-core shows
      the long help on `--help`, as clap does
      (https://docs.rs/clap/latest/clap/struct.Arg.html#method.long_help).
- [x] **M33: does install and list need a top-level cache form** (from
      M32). `tags` and `refresh` open "Updating <Repo> tags cache" as a
      sub-step of a parent `Step`, which ops needs; sb-go's install and
      list have no parent step. M33 decides whether a `Runner` form is
      needed.
- [x] **Before M20: symlinked sources and the re-check** (from M11, lane
      1). Two things M20 needs settled first (the maintainer decided
      both on 2026-11-15: the link moves; sb-host adds the read-only
      query):
      - [x] The specs disagree on a symlink in `sources.list.d`.
        python-env.md's Testing list wants "a symlinked entry moved as a
        link and never followed". host.md H3, and M11 as built, leave a
        symlink alone. apt follows such a link, so a symlinked deadsnakes
        list would stay active. Either M11's `remove_sources_mentioning`
        moves a link whose target holds the needle (the link, never the
        target), or python-env.md is amended and the case is recorded as a
        difference from sb-go. Check sb-go's behaviour first.
      - [x] python-env's re-check after the cleanup has no host function. A
        second `remove_sources_mentioning` with the same run id is
        refused ("already exists") when something matches. Decide whether
        sb-host adds a read-only `sources_mentioning(needle)`.
- [x] **At M67's look: weigh a side-effect-free viewer core** (closed
      2026-11-14: the maintainer approved M67 as built, choosing "Approve,
      merge" over "Approve, rework core later") (the
      maintainer, 2026-11-13, from a comparison with bootvine). The
      maintainer asked for it to be weighed in the viewer's spec, but the
      tui spec is approved and M67 is built (lane 4, unmerged): its
      viewer state holds tokio's `Notify` and `mpsc` and crossterm's key
      events. The alternative is ratatui's Elm pattern
      (https://ratatui.rs/concepts/application-patterns/the-elm-architecture/):
      `update(model, message, now)` returns its effects as data, in a
      crate without tokio or crossterm, with the runtime apart. Pros:
      plain synchronous unit tests, no tokio test-util, an easier
      mutation bar. Cons: another crate (deny.toml wrappers, capability
      map rows), and rework of the built viewer. The maintainer decides
      at the look.
- [x] **exec's PTY timing tests fail under load** (lane 2, with or after
      M23, which also touches crates/exec). `crates/exec/tests/pty.rs`'s
      `a_grandchild_holding_the_pty_is_left_after_the_drain` (3.36 s
      against its 3 s bound) and `cancellation_and_timeouts_reach_the_group`
      (1.26 s against 1 s) failed in M62's full check with the host fully loaded,
      and passed on a rerun. Make each
      bound robust under load without losing what it proves: measure
      against the test's own baseline, or a margin derived from the
      drain's own timeout, never a bigger magic number alone. Lanes now
      run at 12 cores each, which leaves headroom.

- [x] **The inventory read's memory** (lane 3, which owns cli-core's `config.rs` and `config/yaml.rs` for this). An
      uncapped read of localhost.yml peaks at about 30 times the file's size
      (3.5 MB: 0.75 s and 105 MB; 56 MB: 10-12 s and 1.55 GB, release
      build, Task 61's review). No length cap goes back (plan decision 41);
      cut the cost instead: rewrite only when the pre-pass must, iterate
      lines lazily, and read `server_appdata_path` from the event stream
      rather than a tree. Real inventories are kilobytes, so this is not
      urgent.
- [x] **The duplicate-key report grows quadratically** (fixed in 6012fe7: each duplicate cites the first position only; 10,000 repeats now log about 600 KB, so no separate log cap) (config; lane 3).
      One key repeated N times gives N issues, each listing every earlier
      position: 10,000 repeats (50 KB) took 102 s and 5.8 GB and logged
      about 1.1 GB (Task 61's review). Report each duplicate with the
      first position only, or one issue per key, and cap what's logged.
- [x] **A top-level block scalar before a column-0 comment** (lane 3, in cli-core's `config/yaml.rs`; lane
      3, with the next YAML change). sb reads the comment as content, where
      libyaml ends the scalar there. No Saltbox file has a top-level block
      scalar; add the corpus case and match libyaml.

- [x] **exec's other fixed timing bounds** (lane 2, with its next exec
      change). `crates/exec/tests/stream.rs:203`, `:287` and
      `groups.rs:163-164` use fixed wall-clock bounds of the kind c06f497
      replaced; give them the same baseline-derived bounds.
- [x] **release: a total timeout on the two metadata requests** (lane 2,
      with M25 or M26). A proxy that trickles bytes keeps under the per-read
      timeout and can stall `sb update` without end. Cap the metadata
      requests at 60 s in total (downloads keep no total, release spec
      decision 5). Decided: hardening that costs no
      feature. Declined with it: a charset rule for tags and asset names
      (the pinned URL and the digest already hold, and remote tags reach
      messages cleaned).

- [x] **docker's poll-tolerance test times out under load** (lane 4, with
      its next docker change). `control::client::tests::a_few_failed_polls_in_a_row_are_tolerated`
      (crates/docker/src/control/client.rs:641) timed out once in four
      runs under load in M64's review and passed on a rerun. Give it a
      bound that holds under load, as c06f497 did for exec's PTY tests.

- [x] **A byte order mark after the stream's start** (lane 3; cli-core's
      `config/yaml.rs`). Found by 985b5e7's security audit, and older than
      it: sb's pre-pass and saphyr read a U+FEFF past the first character
      differently, so sb accepts files Ansible refuses (a second BOM before
      a top-level scalar or a mapping). Decision 38: refuse U+FEFF anywhere
      but the stream's start, with corpus cases whose libyaml verdicts
      come from scripts/yaml-corpus.py. No Saltbox file has one.

- [x] **sb-config's hung-lookup test times out under load** (lane 3, with
      its next sb-config change). `cloudflare::tests::a_hung_lookup_times_out_after_its_retries`
      made 1 attempt instead of 3 once during M67's full run under load,
      and passed alone three times. Give its timing a bound that holds
      under load, as c06f497 did for exec.

- [x] **Mutation runs leak sb processes** (the gate; lane 3, next, while M50
      waits for M26). A mutant that ignores signals outlives its test: the
      timeout kills the test without its kill-on-drop cleanup, and the sb
      it started under `script` runs on, orphaned, at about 90% CPU for
      hours (19 found from lane 3's runs and 4 from lane 4's on
      2026-11-13, all stopped by hand). This load is the likely cause of
      the PTY timing failures. Make check.sh's mutation pass leave nothing
      behind: run each pass so that every process it started ends with it
      (a systemd scope ended after the pass, or a sweep of processes whose
      executable lies in the pass's own temp copy), and test that a
      signal-ignoring child is gone after the pass. A gate change: one
      code-reviewer pass, and an ADR line in ADR-021's consequences.
- [x] **shlex reads an unquoted `#` word as a comment** (lane 3, with its
      next tui-spec edit). go-shellquote didn't, so `EDITOR='vi #x'`
      differs from sb-go. List it in the tui spec's Differences.

- [x] **The hung-lookup test's baseline is too wide** (superseded by 67cfbfe, which counts retries client-side) (lane 3, after M36;
      sb-config). fa72214's baseline times a whole healthy `run()`,
      including `Http::new` and two requests, and the bound is 13 of
      those, so under heavy load the test took a median 72 s and up to
      117 s, past the mutation profile's 60 s per-test limit: a spurious
      baseline failure, so no verdict (a review that arrived after the
      merge). Time only what one attempt must outlast: build the `Http`
      and `Check` by hand as `a_named_delay_is_taken_from_the_answer`
      does, start the clock after `Http::new`, time one request. Keep the
      200 ms floor and the multiplier. Re-run under spinner load and
      check that it stays a small multiple of the old test's 2-8 s and
      well under 60 s; clamp it with a stated reason if not.

- [x] **Leaked mutant processes during a pass** (the gate; lane 3, now).
      8d9ed8b's sweep runs when a pass ends, but a long pass still leaks
      signal-ignoring, busy mutant `sb` processes while it runs: 19 were
      found at once on 2026-11-13, each over 100% CPU, with the host's load
      far above its core count, which slowed every lane and broke timing
      tests. Sweep during the pass too: a background loop in check.sh,
      every few seconds, that kills only processes of this pass's own
      TMPDIR that have been reparented to init (ppid 1), so the pass's
      live tests are never touched; stopped when the pass ends. Test it
      in the floor guard's stub as 8d9ed8b's test does.
- [x] **The hung-lookup test: count attempts on the client side** (lane 3,
      after the sweep; replaces the uncommitted narrower-baseline change in
      lane 3, which a review showed failing 5 of 312 runs under load). Two
      timing bounds failed under load. Decided: count
      the retries client-side with a counting backoff closure, as
      `the_backoff_is_asked_per_retry` does, and assert it was asked
      twice, so the count no longer depends on load; keep the held answer,
      "timed out" and the guidance assertions; the fake's `seen()` count
      may stay as a lower bound or go. The test's timeout can then be a
      fixed small value again.

- [x] **The follow may miss a journal file with an unusual name** (lane 4;
      sb-journal's follow). In M68's end-to-end test a crafted second file
      named `next.journal` was not picked up while following, where the
      same file named `user-1000.journal` was (crafted files only, not
      checked against journald). journalctl reads every `*.journal` in a
      directory. Find whether the follow filters by name or orders files
      by name, compare with journalctl on 22.04, 24.04 and 26.04 in the
      matrix, and fix or list it as a Difference.

- [x] **The mutation sweep misses leaked shells** (the gate; lane 3).
      exec's tests leave helper shells (`sh -c 'trap … exit'`) that run
      `/bin/sh`, with their working directory in the pass's deleted copy.
      The sweeps match a process's executable only, so these survived,
      hundreds of them, each holding its gate run's systemd scope alive
      after the run ended. Match a process whose executable or working
      directory lies under the pass's TMPDIR, in both the in-pass and
      end-of-pass sweeps, and test it with a shell decoy as fe204d5's test
      does.

## Ready to start

Started right after this plan's commit:

| Lane | First task | Why it can start now |
|---|---|---|
| 1 | M1 host: file ownership | no dependency; repo and python-env wait for it |
| 2 | M22 exec: `option_value` and public `shown()` | no dependency; ansible waits for it |
| 4 | M61 tui: groundwork | no dependency; the viewer and editors build on it |
| 3 | M48 config: `check_file` and `validate_file` | when hardening Task 55 has merged |

M1 and M61 are both **Shared files: yes**. They can run together, but
they merge one at a time.
