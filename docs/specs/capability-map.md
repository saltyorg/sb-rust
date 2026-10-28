# Capability Map: sb-rust (DRAFT: revisit after prototyping)

sb-rust is a Rust rewrite, not a line-by-line port, of the Saltbox CLI (`sb`,
currently sb-go). It keeps the user-facing behaviour of `sb` and uses idiomatic
Rust libraries in place of sb-go's internals.

> **Status:** the stack table and module boundaries are hypotheses. Prototypes
> (`prototypes/`) test them before any module spec is written. Update this map
> with what the prototypes show.

## Objective

- Replace sb-go with a Rust binary that does the same jobs for Saltbox users.
- **Primary driver:** get out of the charm (bubbletea/lipgloss/bubbles/glamour/
  fang) dependency churn. sb-go pins a pre-release `ultraviolet` commit just to
  get a working lipgloss.
- **Library policy:**
  1. Prefer highly active, widely used crates (recent releases, active repo,
     large download base).
  2. Do not own terminal rendering. Use maintained widgets/renderers instead of
     hand-rolled ANSI, width, or layout code, so their maintainers own the
     edge-case validation, not us.
  3. Hand-roll only thin HTTP API clients where no maintained crate exists
     (Arr, Plex, Emby, Jellyfin, qBittorrent, SABnzbd, NZBGet, rTorrent).

## Decisions (2026-10-28)

1. **Compatibility:** keep sb-go's binary name, subcommands, and flags by
   default. A name may change only when the change is a factual improvement,
   and each change is recorded in the module spec. On-disk files (Saltbox
   config YAML, `saltbox.fact`, venv/uv paths) stay compatible.
2. **Scope:** every sb-go command's functionality is covered. Nothing is cut.
3. **Screens:** all functionality behind sb-go's full-screen screens is kept,
   but every screen is redesigned instead of ported. Each screen's design
   (full-screen ratatui vs inline prompt/stream) is decided while prototyping
   it, and recorded in the `tui` spec.
4. **Fact editor:** keeps mouse support (crossterm mouse events mapped to
   ratatui layout areas).
5. **Toolchain:** pinned stable Rust, edition 2024, in `rust-toolchain.toml`.
   Bumping it is a deliberate change.
6. **Release binaries:** static musl builds for Linux amd64 and arm64 (Ubuntu,
   as sb-go checks today). TLS via rustls, no OpenSSL.
7. **Repository:** git repository, intended to be public under saltyorg, so
   the public-repo rules in `CLAUDE.md` apply from the first commit.

## Assumptions (still open to correction)

1. Async runtime: tokio (bollard and reqwest need it). One runtime, no
   blocking-HTTP crate alongside it.
2. sb-go stays the shipping binary until sb-rust reaches the parity agreed per
   module. No consumer (Saltbox repo) changes are part of this work.

## Proposed stack (activity checked 2026-10-28)

All data comes from crates.io (90-day downloads, latest release) and the GitHub
API (last push, archived flag).

| Concern | Crate | Latest / released | 90d downloads | Replaces in sb-go |
|---|---|---|---|---|
| CLI parsing | `clap` 4 (derive) | 4.6.7 / 2026-09 | 267M | cobra, pflag, fang |
| Completions, man | `clap_complete`, `clap_mangen` | 2026-09 / 2026-08 | 34M / 4.8M | completion pkg, mango |
| Full-screen TUI | `ratatui` + `crossterm` | 0.30.2 / repo active 2026-10 | 24M / 54M | bubbletea, bubbles, lipgloss |
| Text input / editor widget | `ratatui-textarea` (ratatui org) | 0.9.3 / 2026-10 | 0.6M | bubbles textinput/textarea |
| Markdown in TUI | `tui-markdown` (ratatui maintainer) on `pulldown-cmark` | 2026-10 | 0.2M / 62M | glamour |
| Spinners, progress | `indicatif` | 0.18.6 / 2026-07 | 54M | bubbles spinner/progress, terminal runner |
| Prompts | `dialoguer` | 0.12.0 / repo active 2026-10 | 19M | restore textinput model |
| Tables | `comfy-table` | 8.0.1 / 2026-09 | 22M | aquasecurity/table, terminal table |
| Colour / styling | `anstyle` + `anstream` (clap's own) | 2026 | 227M | lipgloss styles, colorprofile, charmtone |
| Errors | `anyhow` (bin), `thiserror` (lib modules) | 2026 | 260M / 464M | — |
| Logging | `tracing` + `tracing-subscriber` | 2026 | 240M | terminal/logging |
| Async + HTTP | `tokio`, `reqwest` (rustls) | 2026-10 | 276M / 234M | net/http, x/net |
| Docker | `bollard` | 0.21.1 / 2026-08 | 20M | moby client |
| YAML | `serde-saphyr` | 2.0.0 / 2026-10 | 10M | yaml.v3 + custom duplicate-key walker |
| XML-RPC (rTorrent) | `quick-xml` (thin client) | 0.42.0 / 2026-08 | 151M | go-rtorrent |
| Versions | `semver` | 1.0.28 | 253M | Masterminds/semver |
| Fuzzy match | `strsim` | 0.11.1 (stable, low churn) | 258M | levenshtein |
| Shell quoting | `shlex` | 2.0.1 / 2026-05 | 311M | go-shellquote |
| PTY | `portable-pty` (wezterm) | 0.9.0 / repo active 2026-10 | 14M | creack/pty |
| Unix syscalls | `rustix` | 1.1.6 / 2026-10 | 320M | x/sys |
| Restore decrypt | `aes`, `cbc`, `pbkdf2`, `sha1`, `sha2` (RustCrypto) | 2026 | 48–332M | crypto/*, x/crypto/pbkdf2 |
| Self-update | hand-rolled on `reqwest` + `sha2` | — | — | selfupdate pkg (keeps digest check) |
| Testing | `insta`, `assert_cmd`, `rexpect` | 2026 | 35M / 20M / 0.4M | — |

**Rejected, with reasons:**
- `serde_yaml`: archived and deprecated. `serde_yml`: archived.
  `serde_yaml_ng`/`serde_norway`: last release 2024. `serde-saphyr` is active and
  rejects duplicate keys by default, which deletes sb-go's hand-written
  duplicate-key validator.
- `tui-textarea` (rhysd): no release since 2024-10. `ratatui-textarea` is the
  maintained successor under the ratatui org.
- `cloudflare` crate: about 86K downloads and last release 2025-03. sb-go only
  calls it in one config validator, so a single `reqwest` call is smaller.
- `self_update`: active, but pulls in archive/extraction stacks we don't need.
  sb-go's updater is about 280 lines with a digest check, so we keep it
  hand-rolled.
- `inquire`/`cliclack`: viable, but `dialoguer` shares the console-rs stack
  with `indicatif`, so one rendering family covers both.

## Modules

| Module id | Responsibility | Depends on | sb-go source |
|---|---|---|---|
| `cli-core` | clap tree, global flags, root relaunch, build info, signals/cancellation, error and exit-code policy, styling tokens | — | main.go, cmd/root.go, buildinfo, signals, layout |
| `exec` | Child processes: streamed and captured output, PTY execution, cancellation | cli-core | executor |
| `term-ui` | Inline UI only: task runner with spinners (indicatif), tables, prompts, TTY detection | cli-core | terminal |
| `host` | Ubuntu/arch/LXC/desktop checks, disk space, apt, systemd, file ownership | exec, term-ui | host |
| `repo` | Git operations on Saltbox/Sandbox checkouts, branch switching | exec, host | git |
| `release` | GitHub release lookup, version compare, self-update with digest verification | cli-core | release, selfupdate |
| `python-env` | uv-managed Python, Ansible venv build/update, deadsnakes cleanup | exec, host, release | python |
| `ansible` | Playbook execution with tags, tag listing/cache, `install`/`list` | exec, repo, term-ui | ansible, cmd/install, cmd/list |
| `config` | Saltbox YAML load, schema and custom validation, `validate-config`, `saltbox.fact` handling | exec, host, ansible, release | saltbox, facts |
| `docker` | ps/start/stop/restart via bollard | cli-core, term-ui | cmd/docker* |
| `motd` | System info plus media-server and downloader providers (thin reqwest clients), parallel collection | host, config, term-ui | motd |
| `tui` | Interactive screens covering docker logs, systemd logs, the edit picker, the fact editor (with mouse), and the announcements viewer. Each is redesigned while prototyping. | docker, host, config | cmd/logs, cmd/dockerLogs, cmd/edit, factui, saltbox/announcements |
| `ops` | Workflow commands: setup, update, branch, branch-sandbox, reinstall-*, update-venv, restore, diag, bench, gha | all of the above as needed | remaining cmd/* |
| `completion` | Generated completions (clap_complete) plus dynamic tag completion | cli-core, ansible | completion, cmd/tag_completion |

Arrows point one way, with no cycles. `tui` sits above the data modules so that
logs, edit and facts can be built and tested without a terminal.

**Build order:**

1. `cli-core` → `exec` → `term-ui`
2. `host` → `repo`, `release`
3. `python-env`, `ansible`
4. `config`, `docker`
5. `motd`, `completion`
6. `tui`
7. `ops`

**First vertical slice (proves the stack end to end):**
`sb version`, `sb docker ps`, and `sb validate-config` on real Saltbox config,
built in that order: `cli-core` → `term-ui` → `docker` + `config`.

## Open questions

None at map level. Per-module questions go in each module spec.
