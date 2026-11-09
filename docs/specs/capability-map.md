# Capability Map: sb-rust (approved 2026-10-29)

sb-rust is a Rust rewrite, not a line-by-line port, of the Saltbox CLI (`sb`,
currently sb-go). It keeps the user-facing behaviour of `sb` and uses idiomatic
Rust libraries in place of sb-go's internals.

> **Status:** the stack and module boundaries below have been tested by eight
> prototypes (`prototypes/*/FINDINGS.md`; those whose module is built in
> `docs/prototypes/`). Every row in the stack table names
> the prototype that backs it, or says it is untested. The verdicts are
> technical only until the maintainer reviews each prototype's look and
> feel. Reviewed: proto-logs, proto-markdown, proto-facts, proto-runner and
> proto-inline (2026-10-29; its restore prompts deferred). The maintainer
> approved this map on 2026-10-29 with proto-complete's review still open
> (see Open items). Module specs start with `cli-core`.

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
   - ratatui `unstable-rendered-line-info` (`Paragraph::line_count`): used by
     the log viewer, the announcement viewer and inline Markdown output.
   - The `cross` image version used for releases.

   See [ADR-001](../decisions/ADR-001-unstable-feature-pins.md).
10. **Colour:** screens are designed for truecolor and emit it by default,
    as sb-go does. sb-go sets `COLORTERM=truecolor` and a TrueColor
    colorprofile writer unless `SB_COLOR_PROFILE` says otherwise. sb-rust has
    its own truecolor palette, shared by every screen; it is not copied from
    sb-go or charmtone. ratatui with crossterm writes `Color::Rgb` as 24-bit
    SGR whatever the terminal reports, so the default needs no code.

    **Fallback (planned, not built):**
    - Keep sb-go's `SB_COLOR_PROFILE` (`truecolor`, `ansi256`, `ansi`,
      `ascii`) for terminals without truecolor, and honour `NO_COLOR`.
    - Use libraries, with no colour maths of ours.
      - **Recommended:** `anstyle-query` for detection and `anstyle-lossy`
        for RGB to 256/16 conversion. Both are from the rust-cli org and
        widely used. Pair them with ratatui-core's `anstyle` feature for
        conversions.
      - **Alternative:** `termprofile`, which does detection and conversion
        in one crate with a ratatui feature, but has low adoption.
    - The palette is converted once at startup for the chosen profile.
    - Decided in [ADR-002](../decisions/ADR-002-colour-fallback.md):
      forced truecolor, `SB_COLOR_PROFILE` (or the theme config's
      `color_profile`) to step down, one conversion at startup with
      `anstyle-lossy`.

    **Piped output** is plain by default (colour only on a terminal, and
    console's NO_COLOR and CLICOLOR rules). A `--color=auto|always|never` flag (clap's
    `ColorChoice`, the git/ls/cargo convention) forces it either way.
    The install script will run `sb setup` attached to the terminal instead
    (tasks/prototypes/todo.md), so it needs no flag; `--color` stays as a general
    override. Proved in proto-runner and proto-inline; the flag itself is
    `colorchoice-clap` (anstream's clap mixin), which sets anstream's
    choice, and cli-core passes `always`/`never` on to console and to
    crossterm, which otherwise drops colours under NO_COLOR on its own (so
    the ratatui screens honour NO_COLOR already). Open for cli-core: make clap's
    own `--help` follow the flag too (`Command::color`).

    The palette assumes a dark background. Light themes are an open item.

11. **Logging on the host (maintainer, 2026-10-29):** sb always writes
    useful logs on the host, by default, with rotation, so a user (or
    whoever helps them) can see afterwards what happened. sb-go writes no log
    files: its debug output goes to the screen only with `-v`.
    - **Questions the logs must answer:**
      - Which sb command ran, with what version and arguments, and when?
      - Which step failed, and why (exit code, signal)?
      - What did each step print (its complete output, not just the tail on
        screen)?
      - How long did each step take?
    - **Libraries:** `tracing` events, written by `tracing-subscriber` to a
      `tracing-appender` `RollingFileAppender` that never rotates: one file,
      `/var/log/sb/sb.log`. Saltbox's logrotate, which Saltbox already
      manages, rotates and compresses it (maintainer, 2026-10-30; replaces
      daily files pruned by `max_log_files`). The rule:
      docs/platform-notes/logrotate.md.
    - **What is logged:**
      - one record per run, with a run id and the entry point (command, and
        whether the install script started it);
      - one record per step: start, end, outcome and elapsed;
      - each step's output lines, tagged with run id and step.

      Logging is general: every run is logged and kept, successful or not.
    - **Verbose by default, no flag (maintainer, 2026-11-01):** sb's own
      output has no verbose mode (term-ui decision 9), so the log must
      hold, by default, everything needed to debug odd behaviour: every
      module logs the detail a debug flag would otherwise have turned on.
      Ansible's own verbosity for installs is passed through separately.
      Only logrotate retires old copies. A failed step's `full output:` line
      points at the log and the run id. Proved in proto-runner.
    - **Format:** to be decided in the spec. tracing-subscriber's readable
      key=value format suits users reading the file; its JSON format suits
      tools.
    - **Location:** `/var/log/sb/` when running as root (sb normally is),
      otherwise the user's state directory. To be settled in the spec, with
      file permissions that keep the logs root-only.
    - **Never logged:** secrets. That means fact values, credentials and
      tokens, and anything the fact editor masks. The secret check is shared
      with the fact editor's masking.

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
| Scrollable panes (small content) | `tui-scrollview` (ratatui org) | 0.6.8 / 2026-09 | 0.1M | bubbles viewport | proto-markdown |
| Spinners, progress | `indicatif` (printed-lines pattern) | 0.18.6 / 2026-07 | 54M | bubbles spinner/progress, terminal runner | proto-runner |
| Styled one-off lines, ANSI stripping | `console` (already pulled in by indicatif) | 0.16.6 / 2026-09 | 94M | lipgloss styles | proto-runner |
| Prompts | `dialoguer` | 0.12.0 / repo active 2026-10 | 19M | restore textinput model | proto-inline |
| Tables | `tabled` (`ansi` feature), terminal width from `console` | 0.22.0 / 2026-09 | 13M | aquasecurity/table, terminal table | proto-inline (replaced comfy-table: no border colour) |
| Colour decision | `anstyle` + `anstream` (clap's own), `colorchoice-clap` for `--color` | 1.0 / 2026 | 227M | lipgloss styles, colorprofile, charmtone | proto-inline |
| Signals | `signal-hook` | 0.4.5 / 2026-10 | 67M | signals pkg | proto-runner |
| Host OS release | `sysinfo` (`system` feature only), reading `/etc/os-release` | 0.40.0 / 2026-10 | 58M | host/ubuntu.go parser | cli-core (approved 2026-10-30; os_info rejected: runs `lsb_release`) |
| Errors | `anyhow` (bin), `thiserror` (lib modules) | 2026 | 260M / 464M | — | untested |
| Logging | `tracing` + `tracing-subscriber`, one file via `tracing-appender` (never rotating; Saltbox's logrotate rotates it) | 0.2.5 (appender) / 2026-04 | 240M / 42M | terminal/logging (screen only) | proto-runner; decision 11 |
| Async + HTTP | `tokio`, `reqwest` (rustls, aws-lc-rs) | 2026-10 | 276M / 234M | net/http, x/net | proto-musl, proto-logs |
| Docker | `bollard` | 0.21.1 / 2026-08 | 20M | moby client | proto-musl, proto-logs, proto-inline |
| systemctl JSON output | `serde_json` | 1.0.154 / 2026-10 | 389M | parsing systemctl's human-readable columns | proto-logs |
| Timestamps | `jiff` | 0.2.38 / 2026-10 | 98M | time | proto-logs |
| Atomic file saves (facts, config) | `tempfile` (`NamedTempFile::persist`), keeping owner and mode | 3.27.0 / 2026-03 | 227M | os.CreateTemp + rename | proto-facts |
| TOML (sb's own config) | `toml` (serde, parse only) | 1.1.8 / 2026-10 | 286M | viper/koanf config | cli-core (theme file) |
| YAML, typed | `serde-saphyr` (`deserialize` only for sb-config) | 2.0.0 / 2026-10 | 16M | yaml.v3 | proto-yaml, proto-markdown, config Task 1 |
| YAML, schema-validated config | `saphyr` (`MarkedYaml`, positions, duplicate-key rejection) | 0.3.3 / 2026-10 | 1.1M | yaml.v3 nodes + custom duplicate-key walker | proto-yaml, cli-core (inventory) |
| Schema `pattern` rules | `regex` | 1.13.1 / 2026-07 | 1.3B | (not enforced by sb-go) | proto-yaml, config Task 1 |
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
| `cli-core` | clap tree, global flags, root relaunch, build info, signals/cancellation (an interrupted run dies of its signal, `$?` 130), error and exit-code policy | — | main.go, cmd/root.go, buildinfo, signals, layout |
| `exec` | Child processes: streamed and captured output; PTY execution with process-group kill on cancel; secrets out of logs, output and errors | cli-core, term-ui | executor |
| `term-ui` | Inline UI only: task runner (indicatif, finished steps printed as lines), tables, prompts with a non-interactive flag for each, one colour decision feeding anstream/tabled/indicatif; ANSI stripping of child output when not a terminal | cli-core | terminal |
| `host` | Ubuntu/arch/LXC/desktop checks, disk space, apt, systemd service list, file ownership; wraps `sb-journal` for the service logs (host spec, decision 6) | exec, term-ui, journal | host |
| `journal` (`sb-journal`) | sb's own read-only systemd journal reader, pread only and pure Rust: a unit's newest entries, pages back and forward, following ([journal spec](journal.md); [ADR-013](../decisions/ADR-013-own-journal-reader.md)) | — (wrapped by host) | cmd/logs.go's `journalctl` polling |
| `repo` | Git operations on Saltbox/Sandbox checkouts, branch switching | exec, host | git |
| `release` | GitHub release lookup (with User-Agent), version compare, self-update with digest verification | cli-core | release, selfupdate |
| `python-env` | uv-managed Python, Ansible venv build/update, deadsnakes cleanup | exec, host, release | python |
| `ansible` | Playbook execution with tags, tag listing/cache, `install`/`list` | exec, repo, term-ui | ansible, cmd/install, cmd/list |
| `config` | Saltbox YAML: strict schema loading (drift fails loudly), position-carrying validation collecting all issues, `pattern` and `additionalProperties` support, `validate-config`; facts with a line-preserving writer (round-trip tests), locking and drift detection | cli-core, term-ui (validate-config); exec, host, ansible, release from the facts half and `sb update` on (config spec) | saltbox, facts |
| `docker` | ps, the container list (kept live by the event stream) and log streams via bollard; start/stop/restart via the Saltbox Docker controller's HTTP API after a `systemctl` check (docker spec) | cli-core, term-ui; exec and an HTTP client from start/stop/restart on | cmd/docker* |
| `motd` | System info plus media-server and downloader providers (thin reqwest clients), parallel collection | host, config, term-ui | motd |
| `tui` | ratatui screens: one shared streaming log viewer (docker and systemd) with an inline fallback; fact editor (modal editor, masked secrets, mouse); edit picker; announcements viewer (tui-markdown, announcement list, tui-scrollview). Mouse via recorded layout rects on all screens | docker, host, config | cmd/logs, cmd/dockerLogs, cmd/edit, factui, saltbox/announcements |
| `ops` | Workflow commands: setup, update, branch, branch-sandbox, reinstall-*, update-venv, restore, diag, bench, gha | all of the above as needed | remaining cmd/* |
| `completion` | Dynamic completion via `CompleteEnv` (one tag completer reading the tag cache); install writes the generated per-shell registration stub | cli-core, ansible | completion, cmd/tag_completion |

Arrows point one way, with no cycles. `tui` sits above the data modules so that
logs, edit and facts can be built and tested without a terminal.

**Build order:**

1. `cli-core` → `term-ui` → `exec` (exec streams into term-ui's steps)
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

## When the binary is complete (maintainer's request, 2026-11-04)

To discuss with the maintainer once every module is done, not before:

- **Who owns the config schemas.** Today Saltbox ships them in `schema/`
  and sb reads them at run time, strictly (config spec, assumption 2 and
  decision 5, whose answer to drift is that sb updates itself). The
  alternative is a schema version file in Saltbox, with the rules owned
  by the binary. A design review during the config module (plan Task 13)
  favoured today's setup: Saltbox edits its schemas on its own, mostly
  data-only and together with the matching role change, and a version
  file would make each edit an sb release too. sb would also have to
  embed every past schema version, and sb-go reads `schema/` until it is
  retired. It found one failure today's setup has, a rule key added
  after a user's sb was built breaking that sb's strict load (as
  `validate_when_true` would have when Saltbox added it in February 2026;
  sb-go supported it the same day), and two cheaper fixes for it: a
  `schema_version` key inside the schema files, or a scheduled sb-side
  check that loads upstream's schemas strictly.

## Open items carried into module specs

- **Manual real-terminal check** (two emulators): mouse use and Shift-drag
  native selection in the fact editor.
- **Logging (decision 11):** the logging spec (format, location,
  retention, secret filtering) and a logging dimension in `CONSTRAINTS.md`
  when it is created. The runner already logs every run and its step
  output (proto-runner).
- **Theme configuration:** the real CLI should let users choose or override
  the colour theme in its config (maintainer review of proto-inline,
  2026-10-29). Every screen already takes its colours from one role-named
  palette (proto-theme), so a theme is a set of values for those roles.
  The config format and where it lives go in the cli-core spec, alongside
  the colour fallback (decision 10).
- **Install script:** moves into this repository and runs `sb setup`
  attached to the terminal instead of capturing it (tasks/prototypes/todo.md, "Later:
  Install script lives in this repository").
- **Shell completion:** proto-complete's look and feel is not yet reviewed
  by the maintainer; review it before the `completion` module spec. fish
  registration is generated but untested. Completion runs without sudo
  (cli-core spec), so the tag cache stays readable by any user: `ansible`
  writes it 0644, set explicitly.
- **Runner:** nested subtasks (`insert_after`) and forwarding terminal resize
  to the PTY haven't been prototyped. Evaluate a terminal-emulator crate
  (`vt100`, or termwiz's escape parser) for the output tail instead of the
  custom line splitter.
- **Edit picker:** not prototyped on its own. It reuses the proto-facts and
  proto-logs patterns (list picker, then `$EDITOR`).
- **Release tooling:** evaluate cross from git main or `cargo-zigbuild` when CI
  is set up, to drop the old 0.2.5 images and the per-target target-dir
  workaround.
- **Announcements and unmaintained crates:** `tui-markdown`'s default
  `highlight-code` feature pulls in `syntect`, which brings `bincode` and
  `yaml-rust`, both flagged unmaintained by RustSec (cargo-deny,
  2026-10-30). The maintainer kept YAML highlighting: CONSTRAINTS.md
  exception E1, expiring 2027-01-28. Without it, `default-features =
  false` drops syntect (checked in proto-markdown: tests pass, advisories
  clean).
- **Plain/JSON output** for scripted commands (`docker ps` and others): decide
  per command.
