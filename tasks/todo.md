# Tasks: Stack prototypes

Plan: `tasks/plan.md`. Each prototype task ends with `prototypes/<name>/FINDINGS.md`
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

**Dependencies:** Task 0
**Files:** `prototypes/proto-runner/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** M

## Checkpoint A: after Tasks 1–3
- [x] All three FINDINGS have a verdict.
- [ ] Review with the user before starting the screen prototypes.

## Task 4: Fact editor, keyboard (`proto-facts`)

**Question:** Do ratatui and ratatui-textarea give a good keyboard-driven fact
editor on real facts data?

**Acceptance criteria:**
- [ ] It reads facts from a path argument and shows role → instance → key/value
      navigation.
- [ ] It edits a value inline, deletes a fact, and shows a pending-changes
      summary. Writes go to a copy, never the live file.
- [ ] It handles resize and narrow terminals without panics.

**Verification:**
- [ ] Manual session against a copy of a real facts directory.
- [ ] The diff of the copy shows only the intended changes.

**Dependencies:** Task 0
**Files:** `prototypes/proto-facts/{Cargo.toml,src/main.rs,src/model.rs,FINDINGS.md}`
**Scope:** M

## Task 5: Fact editor, mouse (`proto-facts`)

**Question:** Can crossterm mouse events mapped to ratatui layout areas
replace sb-go's hand-written hit-map, without owning ANSI or width code?

**Acceptance criteria:**
- [ ] Clicking selects rows and focuses the editor, and the scroll wheel
      scrolls lists.
- [ ] Click targets stay correct after resize and with wide (CJK/emoji)
      values.
- [ ] FINDINGS lists any gap that needs custom code and how big it is.

**Verification:**
- [ ] Manual mouse session in at least two terminal emulators, named in
      FINDINGS.

**Dependencies:** Task 4
**Files:** `prototypes/proto-facts/src/{main.rs,mouse.rs}`, `FINDINGS.md`
**Scope:** S

## Task 6: Log viewer streaming (`proto-logs`)

**Question:** Can bollard log streams and `journalctl -f` output feed a
ratatui viewer with follow, scroll, and search, without blocking input?

**Acceptance criteria:**
- [ ] It picks a running container or a Saltbox systemd service, then streams
      its logs.
- [ ] Follow mode toggles, scrolling back works, and `/` search highlights
      matches.
- [ ] Switching the source mid-stream cancels the old stream cleanly.
- [ ] FINDINGS compares this with a simpler inline design (picker, then plain
      streamed output).

**Verification:**
- [ ] Manual session against a running container and a Saltbox service.

**Dependencies:** Task 0
**Files:** `prototypes/proto-logs/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** M

## Task 7: Announcements markdown (`proto-markdown`)

**Question:** Do tui-markdown or termimad render Saltbox announcements well
enough to replace glamour?

**Acceptance criteria:**
- [ ] It renders a real announcements file with each crate, inline and in a
      scrollable view.
- [ ] Headings, lists, code, links and emphasis are compared side by side in
      FINDINGS.

**Verification:**
- [ ] Manual review at 80 columns and at a wide width.

**Dependencies:** Task 0
**Files:** `prototypes/proto-markdown/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Checkpoint B: after Tasks 4–7
- [ ] All screen FINDINGS have a verdict and a proposed screen design.
- [ ] Review with the user.

## Task 8: Inline output kit (`proto-inline`)

**Question:** Do comfy-table, anstyle and dialoguer give the non-TUI look for
`docker ps`-style tables and restore-style prompts, on a TTY and when piped?

**Acceptance criteria:**
- [ ] Renders a docker ps table from live bollard data, with colours, and
      adapts to terminal width.
- [ ] Shows username and password prompts with validation, plus a confirm
      prompt.
- [ ] When piped or with `NO_COLOR` set, output is plain, and prompts fail
      clearly without a TTY.

**Verification:**
- [ ] Manual runs on a TTY, piped, and with `NO_COLOR=1`.

**Dependencies:** Task 0
**Files:** `prototypes/proto-inline/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Task 9: Dynamic completions (`proto-complete`)

**Question:** Can clap_complete's runtime completion supply Ansible tags
dynamically in bash, zsh and fish, replacing sb-go's tag_completion?

**Acceptance criteria:**
- [ ] `install <TAB>` completes tags from a tag source (a static list in the
      prototype).
- [ ] It works in bash, zsh and fish, with the install steps recorded.
- [ ] FINDINGS notes the API's stability status (unstable feature flags, if
      any).

**Verification:**
- [ ] Manual completion in each shell, or one tested with `rexpect`.

**Dependencies:** Task 0
**Files:** `prototypes/proto-complete/{Cargo.toml,src/main.rs,FINDINGS.md}`
**Scope:** S

## Task 10: Update the capability map from the findings

**Acceptance criteria:**
- [ ] The stack table reflects every verdict, with rejected crates moved to
      "Rejected" with reasons.
- [ ] Module boundaries are adjusted where findings require it.
- [ ] The status line changes to "ready for approval".

**Verification:**
- [ ] Every FINDINGS verdict traces to a map change, or to an explicit "no
      change".

**Dependencies:** Tasks 1–9
**Files:** `docs/specs/capability-map.md`
**Scope:** XS

## Checkpoint C
- [ ] User approves the updated map. Then module specs start with `cli-core`.
