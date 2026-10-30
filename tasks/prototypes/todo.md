# Tasks: Stack prototypes

Plan: `tasks/prototypes/plan.md`. Each prototype task ends with `prototypes/<name>/FINDINGS.md`
in the format the plan defines.

Every task also requires:
`cargo build -p <name>` and `cargo clippy -p <name>` with no warnings.

## Task 0: Workspace scaffold

**Description:** Create the Cargo workspace, the toolchain pin, and shared lint
settings so prototypes build consistently.

**Acceptance criteria:**
- [x] `Cargo.toml` workspace (resolver 3) with `prototypes/*` members and
      workspace-level `[workspace.lints]`.
- [x] `rust-toolchain.toml` pins 1.99.1 with the `x86_64-unknown-linux-musl` and
      `aarch64-unknown-linux-musl` targets.

**Verification:**
- [x] `cargo metadata --no-deps` succeeds.

**Dependencies:** None
**Files:** `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`
**Scope:** S

## Task 1: Static musl build (`proto-musl`)

**Question:** Do clap, tokio, reqwest (rustls), bollard and portable-pty build
into one static musl binary for both architectures, at an acceptable size and
build time?

**Acceptance criteria:**
- [x] The binary makes an HTTPS GET, lists containers via bollard, and runs a
      command under a PTY.
- [x] It builds for x86_64 and aarch64 musl. `file` reports both as statically
      linked.
- [x] FINDINGS records release size (stripped), clean build time, and any
      feature flags needed.

**Verification:**
- [x] The x86_64 binary runs on Ubuntu and does all three actions.
- [x] The aarch64 build is checked under `cross` or `qemu`, or FINDINGS says
      it wasn't run.

**Dependencies:** Task 0
**Files:** `prototypes/proto-musl/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Task 2: YAML on real config (`proto-yaml`)

**Question:** Can serde-saphyr cover validate-config: typed load, duplicate-key
rejection, and line/column errors, on the real Saltbox config files?

**Acceptance criteria:**
- [x] It loads the main Saltbox config files from a directory argument into
      typed structs.
- [x] Synthetic fixtures prove duplicate keys, wrong types, and missing
      required keys each give an error with file, line and column.
- [x] FINDINGS compares the error messages with sb-go's for the same fixtures.

**Verification:**
- [x] It runs clean against a real Saltbox config directory.
- [x] Each synthetic fixture produces the expected error.

**Dependencies:** Task 0
**Files:** `prototypes/proto-yaml/{Cargo.toml,src/main.rs,fixtures/*.yml,FINDINGS.md}`
**Scope:** M

## Task 3: Task runner and PTY (`proto-runner`)

**Question:** Can indicatif and portable-pty reproduce sb-go's task runner:
spinner per step, live child output under a PTY, a tail of output on failure,
and clean Ctrl-C?

**Acceptance criteria:**
- [x] It runs a sequence of steps, including an `ansible-playbook --version`
      style command under a PTY, keeping colours.
- [x] A failing step shows the last N output lines and a non-zero exit.
- [x] Ctrl-C kills the child process group and restores the terminal.
- [x] Output is sensible when piped (no spinner control codes).

**Verification:**
- [x] Manual runs on a TTY, piped to `cat`, and interrupted with Ctrl-C.
- [x] Look and feel reviewed and marked done for now by the maintainer
      (2026-10-29).

**Dependencies:** Task 0
**Files:** `prototypes/proto-runner/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** M

## Checkpoint A: after Tasks 1–3
- [x] All three FINDINGS have a verdict.
- [x] Review with the user before starting the screen prototypes.

## Task 4: Fact editor, keyboard (`proto-facts`)

**Question:** Do ratatui and ratatui-textarea give a good keyboard-driven fact
editor on real facts data?

**Acceptance criteria:**
- [x] It reads facts from a path argument and shows role → instance → key/value
      navigation.
- [x] It edits a value inline, deletes a fact, and shows a pending-changes
      summary. Writes go to a copy, never the live file.
- [x] It handles resize and narrow terminals without panics.

**Verification:**
- [x] Manual session against a copy of a real facts directory.
- [x] The diff of the copy shows only the intended changes.

**Dependencies:** Task 0
**Files:** `prototypes/proto-facts/{Cargo.toml,src/main.rs,src/model.rs,FINDINGS.md}`
**Scope:** M

## Task 5: Fact editor, mouse (`proto-facts`)

**Question:** Can crossterm mouse events mapped to ratatui layout areas
replace sb-go's hand-written hit-map, without owning ANSI or width code?

**Acceptance criteria:**
- [x] Clicking selects rows and focuses the editor, and the scroll wheel
      scrolls lists.
- [x] Click targets stay correct after resize and with wide (CJK/emoji)
      values.
- [x] FINDINGS lists any gap that needs custom code and how big it is.

**Verification:**
- [ ] Manual mouse session in at least two terminal emulators, named in
      FINDINGS.

- [x] Look and feel reviewed by the maintainer (2026-10-29); the
      two-emulator manual check above is still open.

**Dependencies:** Task 4
**Files:** `prototypes/proto-facts/src/{main.rs,model.rs}`, `FINDINGS.md`
**Scope:** S

## Task 6: Log viewer streaming (`proto-logs`)

**Question:** Can bollard log streams and `journalctl -f` output feed a
ratatui viewer with follow, scroll, and search, without blocking input?

**Acceptance criteria:**
- [x] It picks a running container or a Saltbox systemd service, then streams
      its logs.
- [x] Follow mode toggles, scrolling back works, and `/` search highlights
      matches.
- [x] Switching the source mid-stream cancels the old stream cleanly.
- [x] FINDINGS compares this with a simpler inline design (picker, then plain
      streamed output).

**Verification:**
- [x] Manual session against a running container and a Saltbox service.
- [x] Look and feel reviewed and verified by the maintainer (2026-10-29).

**Dependencies:** Task 0
**Files:** `prototypes/proto-logs/{Cargo.toml,src/main.rs,src/sources.rs,src/theme.rs,FINDINGS.md}`
**Scope:** M

## Task 7: Announcements markdown (`proto-markdown`)

**Question:** Do tui-markdown or termimad render Saltbox announcements well
enough to replace glamour?

**Acceptance criteria:**
- [x] It renders a real announcements file with each crate, inline and in a
      scrollable view.
- [x] Headings, lists, code, links and emphasis are compared side by side in
      FINDINGS.

**Verification:**
- [x] Manual review at 80 columns and at a wide width.

**Dependencies:** Task 0
**Files:** `prototypes/proto-markdown/{Cargo.toml,src/main.rs,FINDINGS.md}`

- [x] Look and feel reviewed and marked ready by the maintainer (2026-10-29).
**Scope:** S

## Checkpoint B: after Tasks 4–7
- [x] All screen FINDINGS have a verdict and a proposed screen design.
- [x] Review with the user.

## Task 8: Inline output kit (`proto-inline`)

**Question:** Do comfy-table, anstyle and dialoguer give the non-TUI look for
`docker ps`-style tables and restore-style prompts, on a TTY and when piped?

**Acceptance criteria:**
- [x] Renders a docker ps table from live bollard data, with colours, and
      adapts to terminal width.
- [x] Shows username and password prompts with validation, plus a confirm
      prompt.
- [x] When piped or with `NO_COLOR` set, output is plain, and prompts fail
      clearly without a TTY.

**Verification:**
- [x] Manual runs on a TTY, piped, and with `NO_COLOR=1`.
- [x] Look and feel of the `docker ps` table reviewed by the maintainer
      (2026-10-29): liked. The table follows sb-go's `docker ps` on tabled.
      Theme configuration for the real CLI is noted in the capability map.
- Deferred: the restore prompts. The restore backend is being rewritten,
  so its CLI takes shape after that work.

**Dependencies:** Task 0
**Files:** `prototypes/proto-inline/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Task 9: Dynamic completions (`proto-complete`)

**Question:** Can clap_complete's runtime completion supply Ansible tags
dynamically in bash, zsh and fish, replacing sb-go's tag_completion?

**Acceptance criteria:**
- [x] `install <TAB>` completes tags from a tag source (a static list in the
      prototype).
- [ ] It works in bash, zsh and fish, with the install steps recorded.
- [x] FINDINGS notes the API's stability status (unstable feature flags, if
      any).

**Verification:**
- [x] Manual completion in each shell, or one tested with `rexpect`.

**Dependencies:** Task 0
**Files:** `prototypes/proto-complete/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Task 10: Update the capability map from the findings

**Acceptance criteria:**
- [x] The stack table reflects every verdict, with rejected crates moved to
      "Rejected" with reasons.
- [x] Module boundaries are adjusted where findings require it.
- [x] The status line changes to "ready for approval".

**Verification:**
- [x] Every FINDINGS verdict traces to a map change, or to an explicit "no
      change".

**Dependencies:** Tasks 1–9
**Files:** `docs/specs/capability-map.md`
**Scope:** XS

## Checkpoint C
- [x] User approves the updated map (2026-10-29), with proto-complete's
      review carried as an open item. Module specs start with `cli-core`.

## Later: Install script lives in this repository

Not part of the prototype plan; recorded so it isn't lost. To be specified
once cli-core and the runner module exist.

**Why:** today's installer (`sb_install.sh` in the `sb` repository) captures
`sb setup`'s output in a shell variable when not verbose. sb then sees no
terminal, so the user gets no progress and no colour, only a dump of the
captured text if setup fails. sb-rust draws its own progress and keeps a
failed step's last lines, so the installer should stop capturing.

**How it must work** (documented usage, step 1 of
https://docs.saltbox.dev/saltbox/install/install/#step-1-dependencies):
- Run as root via `curl -sL https://install.saltbox.dev | sudo -H bash`, or
  the `wget -qO-` equivalent, then `&& cd /srv/git/saltbox` on success.
- Verbose via `bash -s -- -v` (`-v`), for both curl and wget.

**Design latitude (maintainer, 2026-10-29):** we own both the script and
sb, so sb may gain flags that let the script state what it wants instead of
sb guessing from its environment. Use them only where they remove a guess.
Candidates to weigh in the spec, not decisions:
- an explicit non-interactive mode (fail instead of prompting);
- `--color` set from the script's own terminal check;
- distinct exit codes, or a final summary line the script can rely on.

**Acceptance criteria:**
- [ ] The script lives in sb-rust, replacing `sb_install.sh` in the `sb`
      repository. Publishing it at install.saltbox.dev is a separate,
      explicitly authorised step.
- [ ] Both documented forms work, with and without `-v`, and exit non-zero
      on failure so `&& cd /srv/git/saltbox` only runs after success.
- [ ] `sb setup` runs attached to the terminal, never captured, so sb draws
      its own progress and failure output. Without `-v` sb shows its normal
      tree; `-v` gets sb's verbose output.
- [ ] Nothing reads stdin. Under `curl … | bash`, stdin is the script itself:
      a read would swallow the rest of it. Any prompt uses `/dev/tty` or is
      avoided.
- [ ] With no terminal (CI, output redirected to a log), sb's own detection
      gives plain output, and no `--color` flag is needed.

**Verification:**
- [ ] Each documented command run against a test server (one at a time,
      endpoint from environment variables), with and without `-v`, plus a
      forced failure to check the exit code and failure output.

**Dependencies:** cli-core, runner module
**Files:** a new install script in this repository (path decided in the spec)
**Scope:** S
