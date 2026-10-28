# Capability Map: sb-rust (ready for approval)

sb-rust is a Rust rewrite, not a line-by-line port, of the Saltbox CLI (`sb`,
currently sb-go). It keeps the user-facing behaviour of `sb` and uses idiomatic
Rust libraries in place of sb-go's internals.

> **Status:** the stack and module boundaries below have been tested by eight
> prototypes (`prototypes/*/FINDINGS.md`). Every row in the stack table names
> the prototype that backs it, or says it is untested. Once this map is
> approved, module specs start with `cli-core`.

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
   but every screen is redesigned instead of ported. Each screen's design is
   recorded in the `tui` spec.
4. **Fact editor:** keeps mouse support (crossterm mouse events mapped to
   ratatui layout areas).
5. **Toolchain:** pinned stable Rust, edition 2024, in `rust-toolchain.toml`.
   Bumping it is a deliberate change.
6. **Release binaries:** static musl builds for Linux amd64 and arm64 (Ubuntu,
   as sb-go checks today). TLS via rustls with aws-lc-rs. Release builds use a
   musl C toolchain (`cross` today), because every rustls crypto backend
   compiles C (proto-musl).
7. **Repository:** git repository, intended to be public under saltyorg, so
   the public-repo rules in `CLAUDE.md` apply from the first commit.
8. **Async runtime:** tokio, confirmed by proto-logs. Blocking work (PTY
   readers) runs on threads or `spawn_blocking`. No blocking-HTTP crate.
9. **Unstable-feature dependencies** are allowed only with an exact version
   pin and an ADR. There are currently three:
   - `clap_complete/unstable-dynamic` (dynamic tag completion).
   - ratatui `unstable-rendered-line-info` (only if inline Markdown output is
     kept).
   - The `cross` image version used for releases.

## Assumptions (still open to correction)

1. sb-go stays the shipping binary until sb-rust reaches the parity agreed per
   module. No consumer (Saltbox repo) changes are part of this work.

## Stack (activity checked 2026-10-28; verdicts from prototypes)

Activity data comes from crates.io (90-day downloads, latest release) and the
GitHub API (last push, archived flag).

| Concern | Crate | Latest / released | 90d downloads | Replaces in sb-go | Backed by |
|---|---|---|---|---|---|
| CLI parsing | `clap` 4 (derive) | 4.6.7 / 2026-09 | 267M | cobra, pflag, fang | proto-musl, proto-complete |
| Completions | `clap_complete` + `unstable-dynamic`, exact pin | 4.6.11 / 2026-09 | 34M | completion pkg, tag_completion | proto-complete |
| Man pages | `clap_mangen` | 0.3.3 / 2026-08 | 4.8M | mango | untested |
| Full-screen TUI | `ratatui` + `crossterm` (`event-stream`) | 0.30.2 / 0.29 | 24M / 54M | bubbletea, bubbles, lipgloss | proto-facts, proto-logs |
| Text input / editor | `ratatui-textarea` (ratatui org) | 0.9.3 / 2026-10 | 0.6M | bubbles textinput/textarea | proto-facts, proto-logs |
| ANSI log text in TUI | `ansi-to-tui` (ratatui org) | 8.0.1 / 2026-01 | 3.4M | (lipgloss/ansi handling) | proto-logs |
| Markdown | `tui-markdown` + a small Saltbox `StyleSheet` | 0.3.12 / 2026-10 | 0.2M | glamour | proto-markdown |
| Spinners, progress | `indicatif` (printed-lines pattern) | 0.18.6 / 2026-07 | 54M | bubbles spinner/progress, terminal runner | proto-runner |
| Styled one-off lines, ANSI stripping | `console` (already pulled in by indicatif) | 0.16.6 / 2026-09 | 94M | lipgloss styles | proto-runner |
| Prompts | `dialoguer` | 0.12.0 / repo active 2026-10 | 19M | restore textinput model | proto-inline |
| Tables | `comfy-table` | 8.0.1 / 2026-09 | 22M | aquasecurity/table, terminal table | proto-inline |
| Colour decision | `anstyle` + `anstream` (clap's own) | 1.0 / 2026 | 227M | lipgloss styles, colorprofile, charmtone | proto-inline |
| Signals | `signal-hook` | 0.4.5 / 2026-10 | 67M | signals pkg | proto-runner |
| Errors | `anyhow` (bin), `thiserror` (lib modules) | 2026 | 260M / 464M | — | untested |
| Logging | `tracing` + `tracing-subscriber` | 2026 | 240M | terminal/logging | untested |
| Async + HTTP | `tokio`, `reqwest` (rustls, aws-lc-rs) | 2026-10 | 276M / 234M | net/http, x/net | proto-musl, proto-logs |
| Docker | `bollard` | 0.21.1 / 2026-08 | 20M | moby client | proto-musl, proto-logs, proto-inline |
| YAML, typed | `serde-saphyr` | 2.0.0 / 2026-10 | 10M | yaml.v3 | proto-yaml, proto-markdown |
| YAML, schema-validated config | `saphyr` (`MarkedYaml`, positions, duplicate-key rejection) | 0.3.3 / 2026-10 | 1.1M | yaml.v3 nodes + custom duplicate-key walker | proto-yaml |
| Schema `pattern` rules | `regex` | — | — | (not enforced by sb-go) | proto-yaml |
| XML-RPC (rTorrent) | `quick-xml` (thin client) | 0.42.0 / 2026-08 | 151M | go-rtorrent | untested |
| Versions | `semver` | 1.0.28 | 253M | Masterminds/semver | untested |
| Fuzzy match | `strsim` | 0.11.1 (stable, low churn) | 258M | levenshtein | untested |
| Shell quoting | `shlex` | 2.0.1 / 2026-05 | 311M | go-shellquote | untested |
| PTY | `portable-pty` (wezterm) | 0.9.0 / repo active 2026-10 | 14M | creack/pty | proto-musl, proto-runner |
| Unix syscalls | `rustix` | 1.1.6 / 2026-10 | 320M | x/sys | proto-runner |
| Restore decrypt | `aes`, `cbc`, `pbkdf2`, `sha1`, `sha2` (RustCrypto) | 2026 | 48–332M | crypto/*, x/crypto/pbkdf2 | untested |
| Self-update | hand-rolled on `reqwest` + `sha2`, sends a `User-Agent` | — | — | selfupdate pkg (keeps digest check) | proto-musl (User-Agent) |
| Testing | `insta` + ratatui `TestBackend`, `assert_cmd`, `rexpect` (PTY smoke) | 2026 | 35M / 20M / 0.4M | — | proto-facts (recommendation) |
| Release build | `cross` (musl C toolchain) | 0.2.5 | — | goreleaser-style build | proto-musl |

**Rejected, with reasons:**
- `serde_yaml`: archived and deprecated. `serde_yml`: archived.
  `serde_yaml_ng`/`serde_norway`: last release 2024. serde-saphyr also has no
  generic value type, which is why saphyr covers dynamic config.
- `tui-textarea` (rhysd): no release since 2024-10. `ratatui-textarea` is the
  maintained successor under the ratatui org.
- `termimad`: renders well inline, but it is a second rendering stack (its own
  view types and `unicode-width` 0.1 next to ratatui's 0.2). proto-markdown.
- rustls with `ring` instead of aws-lc-rs: no benefit, because ring compiles C
  too. proto-musl.
- Hand-written mouse hit-maps (sb-go's approach): layout rects plus
  `ListState::offset()` do the same in about a quarter of the code.
  proto-facts.
- Journal cursor polling (sb-go's approach): replaced by `journalctl -f` and
  bollard follow streams. proto-logs.
- `cloudflare` crate: about 86K downloads and last release 2025-03. sb-go only
  calls it in one config validator, so a single `reqwest` call is smaller.
- `self_update`: active, but pulls in archive/extraction stacks we don't need.
- `inquire`/`cliclack`: viable, but `dialoguer` shares the console-rs stack
  with `indicatif`.

## Modules

| Module id | Responsibility | Depends on | sb-go source |
|---|---|---|---|
| `cli-core` | clap tree, global flags, root relaunch, build info, signals/cancellation (exit 130 on interrupt), error and exit-code policy | — | main.go, cmd/root.go, buildinfo, signals, layout |
| `exec` | Child processes: streamed and captured output; PTY execution with process-group kill on cancel; ANSI stripping when not a terminal | cli-core | executor |
| `term-ui` | Inline UI only: task runner (indicatif, finished steps printed as lines), tables, prompts with a non-interactive flag for each, one colour decision feeding anstream/comfy-table/indicatif | cli-core | terminal |
| `host` | Ubuntu/arch/LXC/desktop checks, disk space, apt, systemd (including a `journalctl -f` log stream), file ownership | exec, term-ui | host |
| `repo` | Git operations on Saltbox/Sandbox checkouts, branch switching | exec, host | git |
| `release` | GitHub release lookup (with User-Agent), version compare, self-update with digest verification | cli-core | release, selfupdate |
| `python-env` | uv-managed Python, Ansible venv build/update, deadsnakes cleanup | exec, host, release | python |
| `ansible` | Playbook execution with tags, tag listing/cache, `install`/`list` | exec, repo, term-ui | ansible, cmd/install, cmd/list |
| `config` | Saltbox YAML: strict schema loading (drift fails loudly), position-carrying validation collecting all issues, `pattern` and `additionalProperties` support, `validate-config`; facts with a line-preserving writer (round-trip tests), locking and drift detection | exec, host, ansible, release | saltbox, facts |
| `docker` | ps/start/stop/restart and log streams via bollard | cli-core, term-ui | cmd/docker* |
| `motd` | System info plus media-server and downloader providers (thin reqwest clients), parallel collection | host, config, term-ui | motd |
| `tui` | ratatui screens: one shared streaming log viewer (docker and systemd) with an inline fallback; fact editor (modal editor, masked secrets, mouse); edit picker; announcements viewer (tui-markdown). Mouse via recorded layout rects on all screens | docker, host, config | cmd/logs, cmd/dockerLogs, cmd/edit, factui, saltbox/announcements |
| `ops` | Workflow commands: setup, update, branch, branch-sandbox, reinstall-*, update-venv, restore, diag, bench, gha | all of the above as needed | remaining cmd/* |
| `completion` | Dynamic completion via `CompleteEnv` (one tag completer reading the tag cache); install writes the generated per-shell registration stub | cli-core, ansible | completion, cmd/tag_completion |

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

## Upstream note (for the maintainer to raise; no consumer changes here)

proto-yaml found that `motd.schema.yml` uses `pattern` (regex) and
`additionalProperties` rules that sb-go's schema struct doesn't define, so
sb-go has never enforced them. sb-rust will.

## Open items carried into module specs

- **Manual real-terminal check** (two emulators): mouse use and Shift-drag
  native selection in the fact editor, and wide characters on the inline
  Markdown path.
- **fish completion:** registration is generated but untested.
- **Runner:** nested subtasks (`insert_after`) and forwarding terminal resize
  to the PTY haven't been prototyped.
- **Edit picker:** not prototyped on its own. It reuses the proto-facts and
  proto-logs patterns (list picker, then `$EDITOR`).
- **Release tooling:** evaluate cross from git main or `cargo-zigbuild` when CI
  is set up, to drop the old 0.2.5 images and the per-target target-dir
  workaround.
- **Plain/JSON output** for scripted commands (`docker ps` and others): decide
  per command.
