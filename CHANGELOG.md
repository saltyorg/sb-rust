# Changelog

All notable changes to sb are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and sb uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Prereleases
are tagged `X.Y.Z-rc.N`.

## [Unreleased]

## [1.0.0] - unreleased

The first release of sb in Rust. It replaces [sb-go](https://github.com/saltyorg/sb-go)
with the same commands. [MIGRATING.md](MIGRATING.md) lists the changes
you could trip over.

### Added

- **Release channels:** `release` follows stable releases, and `beta`
  follows prereleases too. `sb self-update --channel` switches once, and
  `install.sh --beta` installs beta.
- **arm64 builds** of sb, published for when Saltbox supports arm64. The
  install script and Saltbox stay x86_64 only until then; a hand-installed
  arm64 sb updates itself on its channel.
- **A run log,** `/var/log/sb/sb.log`, for every root run, with a run id in
  each record and a pointer to it when a command fails (not under a
  failed playbook, whose own output on the screen says why).
- **A new log viewer** for `sb logs`: one live picker for services and
  containers, search across the whole log, wrap and mouse support. A log's text comes from `journalctl -u` or `docker
  logs`, as Saltbox's docs use.
- **`sb fact`'s editor**, a settings page with search, a changes panel
  and Copy (clipboard through OSC 52).
- **`sb edit` validates the file you edited** and offers to edit it again.
- **A Certificates line in `sb motd`** (with `--traefik`), from Traefik's
  API.
- **A theme file,** `<appdata>/sb/config.toml`: the built-in theme, with
  the colour depth and colours per role.
- **`sb docker ps --json`.**
- **`sb update --verify-venv`** (hidden while its cost is tried): checks
  every file of the Ansible virtual environment against its package's
  record, and the Python against a manifest sb writes when it installs
  it, and rebuilds what differs. It catches damage the health checks
  don't, such as a package Saltbox doesn't import at startup.
- **`sb update` says what moved:** each checkout's commit range, the
  tags its pull added or removed, and how to apply the update (`sb
  install saltbox`, or `core` plus your apps). The run log keeps the
  commits and the tag changes.
- **`sb update` asks before discarding local work** in the Saltbox and
  Sandbox checkouts: local commits, tracked changes and untracked files.
  Declining leaves that checkout as it is; without a terminal sb stops
  and names `--discard-local`. `sb branch`, `sb branch-sandbox` and `sb
  setup --force` do the same.
- **Flags for every question,** so commands run unattended:
  `update --migrations` and `--discard-local`, `setup --keep-branch` and `--reset-branch`,
  `restore --user` and `--password-file`, `edit --no-edit-again`.
- **Host-changing commands run one at a time.** A second one waits for the
  first, and says so.
- **Longer help with examples,** and parse errors laid out with hints
  (the best guess first, `sb list` for `sb install` without tags, and
  Docker's own command for one container), wrapped to the terminal.
- **`sb edit` takes the file's name** too: `sb edit accounts.yml`, and
  `sb edit localhost` for the inventory.
- **`version` and `self-update` on an unsupported Ubuntu release,** with a
  warning, so such a host can still update sb.

### Changed

- **Prompts read the terminal, not stdin.** With no terminal and no
  answering flag, sb stops and names the flag.
- **validate-config is stricter.** It checks every schema rule
  (`pattern`, `additionalProperties`), refuses YAML that libyaml refuses,
  and reports each issue with its position. A failed run ends with the
  `sb edit` command for each file with issues (`Fix with: sb edit
  accounts`).
- **Parse errors exit 2** (sb-go exited 1).
- **A failed playbook exits with Ansible's own code** (sb-go exited 1), and
  Ctrl-C during one exits 99, Ansible's (sb-go exited 130): `sb install`
  and update's migrations.
- **`-v`/`--verbose` no longer changes the output** of the commands that
  had it; it is accepted with a note, and the run log keeps the detail.
  `install` keeps `-v` to `-vvvv` for Ansible.
- **A blank line before the prompt:** on a terminal, every run ends
  with one blank line after its last line, unless it ends in one
  already. Pipes and logs get the same bytes as before.
- **One tree per command run,** printed once, at the end, with every
  time in one column: `sb update` and `sb self-update` no longer draw a
  tree per phase. Their questions open on the alternate screen, and
  each answer is noted in the tree.
- **The Ansible virtual environment's checks and last jobs are steps:**
  inspecting the active environment, validating a new one, cleaning up
  old environments and setting ownership are steps of their own in `sb
  reinstall-venv` and `sb reinstall-python`, and in `sb update`, `sb
  update-venv` and `sb setup` (the last three when they build one), so
  the run no longer sits silent between steps.
- **New layouts** for `sb list`, `sb docker ps`, `sb motd`'s Docker and
  queue lines, `sb bench` and `sb fact`.
- **`sb motd`'s default colours** are the GitHub Dark Colorblind palette
  (GitHub Primer's dark colorblind terminal colours) wherever motd.yml
  sets none: blue labels, values, successes and low bars, purple app
  names, yellow warnings and high bars, and orange errors and critical
  bars; no green or red is left. `sb motd --generate-config` writes
  them in its `colors` block, in place of sb-go's charmtone colours.
- **Cancellation:** Ctrl-C and a dropped SSH session stop the whole
  process group of what sb started, with a grace period, and the run ends
  by its signal (an attached Ansible run ends with Ansible's own code).
- **Self-update is stricter.** Downloads come only from the release's own
  URL, and the new binary must report the release's version and channel
  before it replaces sb. There is no total timeout for slow links.
- **`sb branch` switches without asking** about the branch; it asks
  before discarding local changes.
- **Redirects are not followed** by motd's providers, the credential
  checks, the Docker controller or restore.
- **Restore decrypts in memory** and writes atomically, so a wrong password
  can't damage files.
- **Releases come from saltyorg/sb-rust.**

### Removed

- **`sb docker logs`.** `sb logs` shows Saltbox's services and Docker
  containers in one picker; `sb docker logs` is an error that names it.
- **`sb announcements`' `-b`, `-a` and `-r`.**
- **`DOCKER_HOST` with TLS or ssh,** and `DOCKER_API_VERSION`.

[Unreleased]: https://github.com/saltyorg/sb-rust/compare/1.0.0...HEAD
[1.0.0]: https://github.com/saltyorg/sb-rust/releases/tag/1.0.0
