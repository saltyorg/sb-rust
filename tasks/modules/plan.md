# Implementation Plan: the module build (approved specs of 2026-11-13)

**Approval.** The maintainer approved the eight module specs below on
2026-11-13. That approval stands for the planning
skill's "the human reviews and approves the plan" step: this plan only
orders and sizes what the specs already decide, and adds no behaviour of
its own. A task that finds the specs silent or contradictory stops and
reports (lane brief, "Stop and report instead of guessing").

**Sources:**
- [tui](../../docs/specs/tui.md), [ansible](../../docs/specs/ansible.md),
  [completion](../../docs/specs/completion.md),
  [python-env](../../docs/specs/python-env.md),
  [repo](../../docs/specs/repo.md), [release](../../docs/specs/release.md),
  [ops](../../docs/specs/ops.md) and [motd](../../docs/specs/motd.md),
  each with its "Suggested slices for the plan";
- their amendments, applied to the [host](../../docs/specs/host.md),
  [docker](../../docs/specs/docker.md),
  [journal](../../docs/specs/journal.md),
  [config](../../docs/specs/config.md), [exec](../../docs/specs/exec.md),
  [term-ui](../../docs/specs/term-ui.md) and
  [cli-core](../../docs/specs/cli-core.md) specs;
- [the capability map](../../docs/specs/capability-map.md): the module
  table and build order, the Commands table, "When the binary is
  complete";
- [tasks/plan.md](../plan.md) (the hardening plan; its tasks are done
  and its Checkpoint C is open): its
  "Lanes and shared files" rules and decisions 30 to 34, which apply here
  unchanged.

Tasks: [todo.md](todo.md), numbered M1 to M87 so they never clash with
the hardening plan's numbers. This plan and its todo are separate from
tasks/plan.md and tasks/todo.md, which belong to the hardening plan and
are not edited by this work.

## Overview

Eight modules and the amendments they make to seven built ones, in 87
tasks over four lanes (six since 2026-11-16: the lane changes under
Open questions). When the last task merges, every command in the
capability map's Commands table is built except what the plan's open
items hold back (completion until its review; the announcement
viewer's full-screen form, the other item, was specified and built
with M74). Then the capability map's "When the binary
is complete" items go to the maintainer (Checkpoint Z).

The work, by module, with the build-order step it comes from:

| Module | Map step | Tasks | What the tasks build |
|---|---|---|---|
| host (amendments and the unbuilt parts of its approved spec) | 2 | M1-M4, M9-M11 | ownership, the service list, the setup checks, disk space, apt |
| repo | 2 | M5-M8 | git on the checkouts; a bare `sb branch-sandbox` first |
| config (amendments) | 4 | M12, M48, M51, M52 | `saltbox_user()`, `check_file`/`validate_file`, the facts half |
| python-env | 3 | M13-M21 | `update-venv`, `reinstall-*`, uv, the fact, deadsnakes |
| exec (amendments) | 1 | M22, M23 | `option_value`, public `shown()`, the ssh environment, exec-L2 |
| release | 2 | M24-M29 | the client, verified download, `sb self-update`, term-ui's confirm |
| ansible | 3 | M30-M35 | `diag`, `install`, the tag cache, `list` |
| motd | 5 | M36-M47 | `sb motd`, section by section |
| tui: editors | 6 | M49, M50, M53-M56 | `sb edit`, the edit picker, `sb fact` |
| completion | 5 | M57-M60 | paused until proto-complete's review |
| docker, journal (amendments) | 4 | M62-M65 | picker fields, log reads, events, `Journal::unit_query` |
| tui: log viewer | 6 | M61, M66-M73 | `sb logs`, `sb docker logs` |
| ops | 7 | M74-M87 | announcements, restore, bench, branch, gha, setup, install script, update |

## Plan decisions

1. **Foundations first.** The shared pieces later modules call are each
   the first tasks of their lane: host's ownership (M1) and services (M2);
   exec's additions (M22, M23); term-ui's confirm with release's client
   (M24-M26); config's `saltbox_user()` (M12) and per-file check (M48).
   No task starts before the tasks it depends on have merged on `main`
   (its **Dependencies** line). A task in another lane counts only once
   it has merged.
2. **Lanes, files and the cap.** Four lanes, each in its own worktree with
   its own target directory, each lane's heavy runs capped at a quarter of
   the host (hardening decision 31). Lanes 1, 2 and 4 start now. Lane 3
   starts once the hardening plan's Task 55 has merged, because that
   worktree runs it. A lane runs its tasks in the order listed, and its
   files are its own (the lane table below). The few files that pass
   between lanes do so at a named merge, never while both lanes have open
   work on them (the handoffs under the lane table). *Six lanes since
   2026-11-16 (the lane change of 2026-11-16).*
3. **Shared files land alone** (hardening "Lanes and shared files"). A
   task is marked **Shared files: yes** when its diff touches the
   workspace `Cargo.toml`, `Cargo.lock`, `deny.toml`,
   `rust-toolchain.toml`, or the gate (`scripts/`, `CONSTRAINTS.md`,
   `CLAUDE.md`, `.claude/`, `.github/`). Every new crate and every new
   third-party dependency does. Such a task merges while
   no other merged-but-unrebased task touches those files; the other lanes
   then rebase and, because a gate file changed, run `check.sh full`
   again (hardening decision 30). The marks list the changes each task
   is expected to make; a task whose diff turns out to change
   `Cargo.lock` without the mark lands alone too. A `Cargo.lock` conflict on rebase takes
   `main`'s side and lets cargo re-resolve; it is never hand-merged.
4. **Files many lanes add to, by union.** These take both sides' lines on
   a rebase conflict, as `scripts/floor-acknowledged.txt` does:
   - `crates/sb/src/main.rs`: a command's code lives in its own file
     under `crates/sb/src/` (as the tui spec's `edit.rs`); `main.rs` gains
     only the subcommand's variant, its dispatch arm and the `mod` line;
   - `crates/sb/src/contract.rs`'s `EXTENSIONS` lines and
     `crates/sb/Cargo.toml`'s dependency lines (which make the task
     **Shared files: yes** through `Cargo.lock`);
   - a crate's `lib.rs` `mod`/`pub use` lines where two lanes add modules
     to one crate (sb-config: M12 and M48; sb-tui: lane 4 and lane 6);
   - the capability map's Commands rows: each command's row moves to
     "built" in the task that wires it, and no task edits another row.
5. **Gates.** Each task carries one:
   - **look:** a new screen or output look. The task ends with the
     maintainer's screenshots, taken through the `sb` binary , and it merges only after the maintainer has
     seen them. Where the look first shows through a later task's
     command, the two merge together (said in the task).
   - **paused:** waits for a recorded maintainer action: completion's
     slices wait for the proto-complete review (completion spec, slice 0);
     M87 waits for that (its other wait, the announcement viewer's spec
     section, came with M74: 397f079).
   - **free:** merges when its gates pass.
6. **A lane keeps going past a look.** The next task in the lane starts
   on top of the look task's commit, so a lane doesn't stall on
   screenshots. Commits merge in lane order: nothing merges before the
   look task ahead of it. A change the maintainer asks for at a look is a
   new commit on top, not a rewrite of the stacked ones. A **free** task
   stacked behind a look waits for it.
7. **Reviews by tier** (CLAUDE.md, ADR-016), each on the session's own
   model and effort (the maintainer dropped the Fable model and the medium
   effort for reviewers on 2026-11-13), in parallel with
   `scripts/check.sh full <base>`: one `agent-skills:code-reviewer` run on
   the task's whole diff for every task; `library-reviewer` where the task
   says so (terminal, UI, input, event-loop, process or I/O code);
   `agent-skills:security-auditor` where the task says so (untrusted
   input, privileges, secrets, downloads run as root). Reviewers are
   briefed with the range and the goal, edit nothing, and run sb only with
   `SB_LOG_DIR` in a temporary directory.
8. **Crates.** New third-party crates come from the maintainer's list,
   each added by the task that first needs it: ratatui `=0.30.2`
   (ADR-001), crossterm, ratatui-textarea, tui-popup, tui-scrollview,
   ansi-to-tui (M61, M70, M55); dialoguer (M26); object (M28); procfs
   (M37); tar and flate2 (M18, or M78 if it lands first); uutils_term_grid
   (M34); aes, cbc, pbkdf2 with hmac, sha1 (M75); rcgen, test only (M24).
   *As built, tui-scrollview came with M74's announcement viewer
   (c8f73c7).*
   The specs also need crates that an approved decision already names and
   that are already in `Cargo.lock`. They are added as direct
   dependencies by their first task, citing the decision:
   - `ratatui-core =0.1.2` (ADR-001 decision 1, part of the ratatui pin):
     M61;
   - `walkdir` (host spec decision 2, maintainer, 2026-11-08): M1;
   - `semver`, `sha2`, `hex` (stack table "Versions", "Self-update"):
     M13, M24;
   - `strsim` (stack table "Fuzzy match"): M33;
   - `shlex` (stack table "Shell quoting"): M46 (as built, M49: 3d0a71b);
   - `quick-xml` (stack table "XML-RPC", the motd spec): M45;
   - `tui-markdown` (stack table "Markdown", CONSTRAINTS exception E1):
     M74;
   - `clap_complete =4.6.11` with `unstable-dynamic` (ADR-001): M57;
   - `zeroize` (RustCrypto's own, the ops spec's Tech Stack): M75.

   Any other crate stops the task and goes to the maintainer.
9. **The hardening plan's last tasks.** Task 59 (crates/config) and Task
   55 (the toolchain, across six crates and the gate) were still open
   when this plan was written; both merged on 2026-11-13 (Task 59:
   a868b19; Task 55: a08f66e).
   - M12, the first edit to crates/config, waits for Task 59 to merge.
   - Task 55 doesn't wait for this plan's lanes: its "every open lane
     task merged" meant the hardening lanes. It lands alone (rule 3) when
     it is ready. Every lane then rebases onto it and runs `check.sh full`.
     New code keeps to the 1.100 lints Task 55 lists (`map_unwrap_or`,
     including `map(..).unwrap_or_default()`, and `needless_range_loop`),
     so its rebase finds no new sites.
   - No module task edits the files Task 55 lists as its lint sites
     (cli-core's `exit.rs`, `config.rs`, `help.rs`) before Task 55 merges.
10. **Rebase, all lanes at once, caps, mutation runner:** hardening
    decisions 30, 31 and 34 apply as written: a rebased task runs
    `check.sh full` again only on overlap; every lane runs at once inside
    the quarter-host cap; cargo-mutants runs under nextest's per-test
    timeout. New crates join `MUTATION_TESTERS` where their spec says
    (sb-tui: `sb-tui sb`).
11. **Live and container runs.** Container tests use a prefix of the
    lane's name and touch no other container or image on the host.
    apt, git, uv and Ansible checks run in throwaway Ubuntu containers,
    never against the host's own packages (host M9's lock-wait check
    included). Network calls are read-only: release lookups, the restore
    service's 404, one Speedtest run.

## Lanes

| Lane | Tasks | Starts | Files (its own while the lane runs) |
|---|---|---|---|
| 1: host, repo, python-env; ops' branch, gha, setup, install script and update | M1-M21; M79-M86 (since 2026-11-16) | now | `crates/host` (not `journal.rs`), `crates/repo`, `crates/python-env`, `crates/config/src/user.rs`; `crates/ops/src/{branch,prereq,locale,configs,hooks,testing,gha,setup,update}.rs`, `install/`; `crates/sb/src/{branch,python_env,gha,setup,update}.rs` and their tests; `docs/platform-notes/apt.md`; the host, repo and python-env specs, and the ops spec's parts for its tasks |
| 2: exec, release, ansible | M22-M35 (motd's M36-M47 moved to lane 3 on 2026-11-13) | now | `crates/exec`, `crates/release`, `crates/ansible`, `crates/cli-core/src/build_info.rs` (and its build script), `crates/term-ui/src/{prompt.rs,table.rs}`; `crates/sb/src/{self_update,ansible}.rs` and their tests; the exec, release, ansible and term-ui specs |
| 3: config, motd, the editors' first half, completion | M36-M52, M57-M60 | after hardening Task 55 merges | `crates/config` (but `user.rs`), `crates/motd`, the file holding term-ui's bar, `crates/completion`; `crates/sb/src/motd.rs`, `crates/sb/src/edit.rs` (until M50 merges), CompleteEnv in `main.rs`; their tests; `docs/platform-notes/motd-sources.md`; the config, motd and completion specs |
| 4: docker, journal, the log viewer, announcements, restore | M61-M76 | now | `crates/docker`, `crates/journal/src/lib.rs` (`unit_query`), `crates/tui` (until M74 merges, then keeping only its own files), `crates/ops/src/{lib.rs,announcements.rs,restore/}`; `crates/sb/src/{logs,docker_logs,restore,announcements}.rs` and their tests; `docs/platform-notes/docker-logs.md`; the docker, journal and tui (viewer) specs, and the ops spec's parts for its tasks |
| 5: bench | M77, M78 (since 2026-11-16) | 2026-11-16 | `crates/ops/src/bench/`; `crates/sb/src/bench.rs` and its tests; `docs/platform-notes/speedtest.md`; the ops spec's bench part |
| 6: the editors' screens | M53-M56 (since 2026-11-16) | 2026-11-16 | `crates/tui/src/{edit_picker.rs,fact/}`; `crates/sb/src/{edit,fact}.rs` and their tests; the tui spec's editor parts |

M87 has no lane until completion (M57-M60) is unpaused.

**Handoffs** (a file passes once, at a merge, never while both lanes have
work open on it):
- `crates/term-ui/src/prompt.rs`: lane 2 creates it in M26; lane 4 adds
  the text and password prompts in M76. Lane 2 doesn't touch it after M26.
- `crates/tui`: lane 4 builds the crate (M61-M74); lane 6's editor tasks
  (M53-M56, lane 3's until 2026-11-16) start after M74 merges and add
  `edit_picker.rs` and `fact/` (`picker.rs` is the log viewer's, M70).
  Lane 4 no longer edits sb-tui after M74, except its own files through
  union rule 4 (the announcement viewer, c8f73c7; the viewer's
  performance follow-ups on those files, f59cd55 and 37aceeb, were done
  in lane 2).
- `crates/sb/src/edit.rs`: lane 3's M49 and M50 pass it to lane 6's M53.
- `crates/sb/src/branch.rs`: lane 1's bare `sb branch-sandbox` (M5) is
  replaced by ops' branch commands in M79 (lane 1 since 2026-11-16), after
  M8 merges.
- `crates/config`: lane 1's `user.rs` (M12) is done before lane 3
  starts; `lib.rs` follows union rule 4.

**Lane 3 is lighter on purpose.** It starts last, and four of its tasks
are paused. *Since 2026-11-13 it also holds motd (M36-M47); since
2026-11-16 the editors' screens (M53-M56) are lane 6's.*

## Dependency graph

```
hardening Task 59 ──► M12 saltbox_user ─────────────┐
                                                    ▼
M1 owner ─► M5 repo ─► M6, M7 head ─► M8 clone   M13-M17 python-env ─► M18 uv ─► M19 fact ─► M20 deadsnakes ─► M21
   │            │          │             │          ▲                      ▲           ▲            ▲
   │            │          │             │          │ M3 checks            │ M9 apt    │            │ M11 apt queries
M2 services ────┼──────────┼─────────────┼──► M43 motd services            │           │
   │            │          │             │    M66-M68 sb logs              │           │
M3 checks, M4 disk ─► M31 install        │                                 │           │
                       M7 ─► M32 cache ─► M57 completion (paused)          │           │
M22 exec opts ─► M30 diag                │                                 │           │
M23 ssh env                              │                                 │           │
M24 client ─► M25 download ─► M26 self-update+confirm ─► M27 ─► M28 ELF ─► M29 interface
                                   │
                                   └─► M50 edit report, M85 update
hardening Task 55 ─► lane 3: M48 check_file ─► M42 motd.yml, M50 edit report, M86 update
M61 tui base ─► M62-M65 docker, journal ─► M66 adapter ─► M67 viewer ─► M68 sb logs ─► M69-M73
                                                                                    ─► M74 inline md + announcements
M74 ─► M53-M56 editors' screens (lane 3)
M75 restore core ─► M76 sb restore (+ text/password prompts, after M26)
M4 ─► M77 bench ─► M78 (after M28/M29)
M5-M8, M12 ─► M79 branch ─► M84, M85 update ─► M86 ─► M87 (paused)
M9, M10 ─► M80 shared ─► M81 gha, M82 setup ─► M83 install script
```

## Task list

Full tasks in [todo.md](todo.md), which alone keeps their status.
Gates: **F** free, **L** look, **P** paused. **S** marks Shared files.
Grouped by the lanes as first planned; the lane table and the lane
changes under Open questions say where each task runs now.

### Lane 1: host, repo, python-env
- M1 host: file ownership (F, S)
- M2 host: the systemd service list (F, S)
- M3 host: setup checks and `package_installed` (F, S)
- M4 host: disk space and `free_space` (F)
- Checkpoint H1: host foundations
- M5 repo: crate and a bare `sb branch-sandbox` (F, S)
- M6 repo: the remote wrapper (F)
- M7 repo: `branch_choice` and `head` (F)
- M8 repo: `clone` and `init_from_remote` (F)
- Checkpoint R: repo
- M9 host: apt policy, install, remove, update_lists, the lock note (F)
- M10 host: `add_repositories` with the fenced cleanup (F)
- M11 host: apt queries and `remove_sources_mentioning` (F)
- Checkpoint H2: host complete
- M12 config: `saltbox_user()` (F)
- M13 python-env: crate, layout, toolchain, isolation, uv probe (F, S)
- M14 python-env: `sb update-venv` on a healthy tree (F, S)
- M15 python-env: the build into staging (F)
- M16 python-env: activation and the `reinstall-*` commands (F)
- M17 python-env: self-healing (F)
- M18 python-env: uv install (F, S)
- M19 python-env: the fact (F)
- M20 python-env: deadsnakes (F)
- M21 python-env: live acceptance (F)
- Checkpoint PE: python-env

### Lane 2: exec, release, ansible, motd
- M22 exec: `option_value` and public `shown()` (F)
- M23 exec: the ssh environment and exec-L2 (F)
- M24 release: crate, client, lookup, asset checks (F, S)
- M25 release: verified download and activation (F)
- M26 release: `sb self-update`, `BuildInfo::released`, term-ui's confirm (L, S)
- M27 release: retries and timeouts (F)
- M28 release: staged binary checks (F, S)
- M29 release: the python-env interface (F)
- Checkpoint X: exec and release (the confirm's look)
- M30 ansible: crate and `sb diag` (F, S)
- M31 ansible: `install` without the cache (F)
- M32 ansible: the tag cache (F)
- M33 ansible: validation and suggestions (F, S)
- M34 ansible: `sb list` (L, S)
- M35 ansible: the container check by hand (F)
- Checkpoint A: ansible (`list`'s columns)
- M36 motd: crate, flags, runner, layout, distro and kernel (L, S)
- M37 motd: the system sections (F, S)
- M38 motd: Disk Usage and term-ui's `bar` (L)
- M39 motd: packages and GPU (F)
- M40 motd: logins (F)
- M41 motd: Docker and Traefik (F)
- M42 motd: motd.yml (L)
- M43 motd: services (F)
- M44 motd: HTTP providers, media servers and downloaders (F)
- M45 motd: HTTP providers, queues and torrents (F, S)
- M46 motd: the banner (L, S)
- M47 motd: `--share` and the budgets (F)
- Checkpoint M: motd (its output)

### Lane 3: config and the editors, completion (after Task 55)
- M48 config: `check_file` and `validate_file` (F)
- M49 tui editors: `sb edit <sub>` direct (F)
- M50 tui editors: validation after the edit, "edit again?" (L)
- M51 config facts: parser and writer (F)
- M52 config facts: session and apply (F)
- M53 tui editors: the edit picker (L)
- M54 tui editors: the fact editor, read-only page (L, S)
- M55 tui editors: the fact editor's changes (L)
- M56 tui editors: mouse, the menu, the PTY smoke test (L)
- Checkpoint E: editors (the edit report, picker, fact editor)
- M57 completion: `CompleteEnv`, the tag completer, `generate` (P, S)
- M58 completion: `completion install` (P)
- M59 completion: migration and cleanup (P)
- M60 completion: `refresh(step)` (P)
- Checkpoint C: completion

### Lane 4: docker, journal, the log viewer, ops
- M61 tui: groundwork (F, S)
- M62 docker: the picker's fields and `picker_status` (F)
- M63 docker: log reads and the daemon probe (F)
- M64 docker: container events (F)
- M65 journal: `Journal::unit_query` (F)
- M66 tui: the journal adapter and the service status rule (F)
- M67 tui: the viewer (L, shown with M68)
- M68 tui: `sb logs`, the Services picker (L, S)
- M69 tui: the docker source and `sb docker logs` (L)
- M70 tui: the unified picker (L, S)
- M71 tui: search (L)
- M72 tui: pasteable output and the inline fallback (L)
- M73 tui: hardening (F)
- M74 ops: crate, announcement rules, `sb announcements`, tui's inline Markdown (L, S)
- Checkpoint V: log viewer (picker, viewer, search, inline forms)
- M75 ops: restore core (F, S)
- M76 ops: `sb restore`, term-ui's text and password prompts (L)
- M77 ops: bench, system and disk (L, S)
- M78 ops: bench, network (L, S)
- M79 ops: `sb branch`, `sb branch-sandbox` (F)
- M80 ops: setup's and gha's shared pieces (F)
- M81 ops: `sb gha` (F). M81 owns the ops spec's "PATH stubs in a
  private mount namespace" harness for the built binary
  (`sb_ops::testing::Host::namespaced`, on M80's `Host`); M82 and
  update's binary tests run on it (coordinator, 2026-11-16).
- M82 ops: `sb setup` (F)
- M83 ops: the install script (F)
- M84 ops: `update`, part 1 (F)
- M85 ops: `update`, part 2 (F)
- M86 ops: migrations (F)
- M87 ops: the announcement viewer and the completion refresh (P)
- Checkpoint O: ops (bench's format, restore's prompts)

### Checkpoint Z: the binary is complete
After every task, including the paused ones, has merged: the capability
map's "When the binary is complete" items go to the maintainer, to
discuss, not as tasks: who owns the config schemas; re-measuring the
ratchet rows and how far to cut them; glibc and musl side by side.

**Gate counts:** 61 free, 21 look, 5 paused (87 tasks).

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Looks stack up and a requested change ripples through the stacked commits | Med | Decision 6: changes go on top as new commits. Look tasks come early in each module, so the layout is settled before the sections that reuse it. |
| Manifest tasks pile up behind one another | Med | Decision 3: they merge one at a time and are small. A lane whose manifest task waits starts its code on the unmerged manifest commit. |
| Task 55's toolchain move lands mid-build | Med | Decision 9: lanes rebase and run `check.sh full`; new code keeps to the 1.100 lints already listed. |
| `crates/sb/src/main.rs` conflicts across four lanes | Low | Decision 4: command code in its own file, main.rs lines merged by union. |
| A container or live test touches the host's own services | High | Decision 11: lane-prefixed containers, nothing on the host's apt, Docker or `/srv`. sb only with `SB_LOG_DIR`. |
| The fenced apt moves (M10, M11) or the root saves (M52) lose a user's file | High | The security-auditor runs on them. Tests run on temporary roots; the fixed paths are reached only in containers. |
| Binary size grows past the ratchet with ratatui, procfs, object and RustCrypto | Med | Each crate's task measures its cost on the production build (the specs ask for it). The tui size ADR (M61) comes before the screens. |
| A cross-lane dependency stalls a lane (M42 on M48, M18 on M24-M29) | Med | Lane order puts those tasks late. motd's sections that need no config come before M42. |

## Open questions

1. **The announcement viewer's full-screen form isn't specified yet.**
   The tui spec lists it under "Later in this module, own sections and
   plans", and the ops spec's `update` shows announcements through it
   on a terminal. This plan builds the inline form (M74), which `update`
   uses until then, and leaves the viewer to M87, paused until the tui
   spec gains the section. *Recommendation:* the maintainer asks for that
   section with Checkpoint V's looks. *Resolved 2026-11-15: the
   maintainer asked for it at M74's look; the tui spec's "The
   announcement viewer" (397f079), built in c8f73c7.*
2. **Crates that approved decisions name but the new-crate list doesn't**
   (decision 8). The plan treats them as approved because each is named
   by an approved decision and already in Cargo.lock. If the maintainer
   wants each confirmed, its task waits, and M1 (walkdir) and M61
   (ratatui-core) are first. *Closed: both were added as the plan
   treats them, walkdir in M1 (9ad1b23) and ratatui-core in M61
   (2052c30).*

**Lane change (2026-11-13):** motd's tasks M36-M47
move from lane 2 to lane 3. Lane 3 was idle, waiting on M26 and M74; motd
depends only on M2 and M48, both merged; and its files (`crates/motd`,
`crates/sb/src/motd.rs`, the file holding term-ui's `bar`,
`docs/platform-notes/motd-sources.md`) belong to no other open task.
Lane 2 keeps exec, release and ansible (M22-M35).

**Lane change (2026-11-16):** lane 1 takes ops' M79-M86
(branch, setup's and gha's shared pieces, gha, setup, the install
script, update and migrations); a new lane 5 takes bench (M77, M78)
and a new lane 6 the editors' screens (M53-M56). Lane 4 keeps restore
(M75, M76). M87 has no lane until completion (M57-M60) is unpaused.
M79's dependency on M78 and M77's on M76 were lane order and are
dropped.

## Ready to start

These started right after this plan's commit, one per
lane:

| Lane | First task | Why it can start now |
|---|---|---|
| 1 | M1 host: file ownership | no dependency; repo and python-env wait for it |
| 2 | M22 exec: `option_value` and public `shown()` | no dependency; ansible waits for it |
| 4 | M61 tui: groundwork | no dependency; the viewer and the editors build on it |
| 3 | M48 config: `check_file` and `validate_file` | once hardening Task 55 has merged |

M1 and M61 both change shared files. They can run together but merge one
at a time (decision 3).
