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
   Bumping it is a deliberate change. 2024 is the latest stable edition
   (checked 2026-11-13 on 1.99.1 and 1.100.0); revisit when a release
   makes the next one stable.
6. **Release binaries:** glibc builds for Linux amd64 and arm64 (Ubuntu,
   as sb-go checks today), built in an image of the oldest release on the
   host spec's runtime list (`ubuntu:22.04` today), so one binary runs on
   every supported release. TLS via rustls with aws-lc-rs, whose C the
   image's GCC compiles. *Amended 2026-11-13 from static musl through
   `cross` ([ADR-020](../decisions/ADR-020-glibc-builds-on-the-oldest-release.md),
   which supersedes ADR-008; built from hardening Tasks 53 and 54).
   musl is revisited when the binary is complete (below).*
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
| Full-screen TUI | `ratatui` + `crossterm` (`event-stream`); exact pins `ratatui =0.30.2` (`crossterm_0_29`, `layout-cache`, `underline-color`, `unstable-rendered-line-info`) and `ratatui-core =0.1.2` with its `anstyle` feature (tui spec). *Amended 2026-11-13 by the tui spec.* | 0.30.2 / 0.29 | 24M / 54M | bubbletea, bubbles, lipgloss | proto-facts, proto-logs |
| Text input / editor | `ratatui-textarea` (ratatui org), staying at 0.9.3 (tui spec). *Amended 2026-11-13 by the tui spec.* | 0.9.3 / 2026-10 | 0.6M | bubbles textinput/textarea | proto-facts, proto-logs |
| ANSI log text in TUI | `ansi-to-tui` (ratatui org) | 8.0.1 / 2026-01 | 3.4M | (lipgloss/ansi handling) | proto-logs |
| Markdown | `tui-markdown` + a small Saltbox `StyleSheet` | 0.3.12 / 2026-10 | 0.2M | glamour | proto-markdown |
| Scrollable panes (small content) | `tui-scrollview` (ratatui org) | 0.6.8 / 2026-09 | 0.1M | bubbles viewport | proto-markdown |
| Dialogs | `tui-popup` (ratatui org). *Amended 2026-11-13 by the tui spec.* | 0.7.7 | — | bubbletea overlays | proto-facts |
| Spinners, progress | `indicatif` (printed-lines pattern) | 0.18.6 / 2026-07 | 54M | bubbles spinner/progress, terminal runner | proto-runner |
| Text width, terminal size | `console` (already pulled in by indicatif): term-ui's text width, padding and truncation (`measure_text_width`, `pad_str`, `truncate_str`), `Term` (size, whether a terminal) and its colour switch. ANSI stripping is term-ui's `DisplayText` on `anstyle-parse` (term-ui spec, "Escape handling"); sb's and term-ui's tests still strip with `strip_ansi_codes` | 0.16.6 / 2026-09 | 94M | lipgloss styles | proto-runner |
| Prompts | `dialoguer` | 0.12.0 / repo active 2026-10 | 19M | restore textinput model | proto-inline |
| Tables | `tabled` (`ansi` feature), terminal width from `console` | 0.22.0 / 2026-09 | 13M | aquasecurity/table, terminal table | proto-inline (replaced comfy-table: no border colour) |
| Colour decision | `anstyle` + `anstream` (clap's own), `colorchoice-clap` for `--color` | 1.0 / 2026 | 227M | lipgloss styles, colorprofile, charmtone | proto-inline |
| Signals | `signal-hook` | 0.4.5 / 2026-10 | 67M | signals pkg | proto-runner |
| Host OS release | `sysinfo` (`system` feature), reading `/etc/os-release`. *Amended 2026-11-13 by the ops spec; its `disk` feature withdrawn 2026-11-16 (ops decision 32: bench lists the mounts through procfs).* | 0.40.0 / 2026-10 | 58M | host/ubuntu.go parser | cli-core (approved 2026-10-30; os_info rejected: runs `lsb_release`) |
| Errors | `anyhow` (bin), `thiserror` (lib modules) | 2026 | 260M / 464M | — | untested |
| Logging | `tracing` + `tracing-subscriber`, one file via `tracing-appender` (never rotating; Saltbox's logrotate rotates it) | 0.2.5 (appender) / 2026-04 | 240M / 42M | terminal/logging (screen only) | proto-runner; decision 11 |
| Async + HTTP | `tokio`, `reqwest` (rustls, aws-lc-rs); sb-host runs the journal reads and the follow's wake wait on tokio's blocking pool, through cli-core's `spawn`, which carries the run's span (host spec, Tech Stack; cli-core spec, "Logging") | 2026-10 | 276M / 234M | net/http, x/net | proto-musl, proto-logs, sb-host |
| Docker | `bollard` | 0.21.1 / 2026-08 | 20M | moby client | proto-musl, proto-logs, proto-inline |
| systemctl JSON output | `serde_json` | 1.0.154 / 2026-10 | 389M | parsing systemctl's human-readable columns | proto-logs |
| Timestamps | `jiff` | 0.2.38 / 2026-10 | 98M | time | proto-logs |
| Atomic file saves (facts, config) | `tempfile` (`NamedTempFile::persist`), keeping owner and mode; fact files: a temp file on the pinned directory fd (`rustix::fs::openat`, `O_EXCL`), not tempfile, which re-resolves the path (tui spec, Editors). *Amended 2026-11-13 by the tui spec.* | 3.27.0 / 2026-03 | 227M | os.CreateTemp + rename | proto-facts |
| TOML (sb's own config) | `toml` (serde, parse only) | 1.1.8 / 2026-10 | 286M | viper/koanf config | cli-core (theme file) |
| YAML, typed | `serde-saphyr` (`deserialize` only for sb-config) | 2.0.0 / 2026-10 | 16M | yaml.v3 | proto-yaml, proto-markdown, config Task 1 |
| YAML, schema-validated config | `saphyr` (`MarkedYaml`, positions, duplicate-key rejection) | 0.3.3 / 2026-10 | 1.1M | yaml.v3 nodes + custom duplicate-key walker | proto-yaml, cli-core (inventory) |
| Schema `pattern` rules | `regex` | 1.13.1 / 2026-07 | 1.3B | (not enforced by sb-go) | proto-yaml, config Task 1 |
| XML-RPC (rTorrent) | `quick-xml` (thin client) | 0.42.0 / 2026-08 | 151M | go-rtorrent | the motd spec. *Amended 2026-11-13 by the motd spec.* |
| System facts from /proc (motd) | `procfs` (no default features). *Amended 2026-11-13 by the motd spec.* | 0.18.0 | 23.7M | /proc parsing and `uptime`, `ps` children (motd/info.go) | the motd spec |
| ELF header check (staged sb, saltbox.fact) | `object` (`read_core`, `elf`; no default features). *Amended 2026-11-13 by the release spec.* | 0.41.0 | — | debug/elf (release/asset.go, saltbox/fact.go) | untested (release and python-env specs) |
| Archives (uv, Speedtest) | `tar` + `flate2` (`rust_backend`). *Amended 2026-11-13 by the ops spec.* | 0.4.46 / 1.1.10 | — | archive/tar, compress/gzip | untested (python-env and ops specs) |
| Versions | `semver` | 1.0.28 | 253M | Masterminds/semver | untested |
| Fuzzy match | `strsim` | 0.11.1 (stable, low churn) | 258M | levenshtein | untested |
| Shell quoting | `shlex` | 2.0.1 / 2026-05 | 311M | go-shellquote | untested |
| PTY | `portable-pty` (wezterm) | 0.9.0 / repo active 2026-10 | 14M | creack/pty | proto-musl, proto-runner |
| Unix syscalls | `rustix` (the workspace's `fs`, `process`, `stdio`, `system` (sb-host's `uname(2)`, host spec assumption 3) and `termios`; sb-host adds `event`, for the follow's wake wait, `journal::FollowWait`, journal spec decision 12, and so does sb-journal's `testing` feature) | 1.1.6 / 2026-10 | 320M | x/sys | proto-runner, sb-host |
| User lookup | `nix` (`user` only, no default features), the 0.28 portable-pty already links | 0.31.3 / 2026-05 (checked 2026-11-12; sb: 0.28.0) | 244M | os/user | sb-host (the systemd-coredump user's uid, `journal::coredump_uid`; the ownership user's uid and gid, `owner`; host spec, Tech Stack) |
| Directory walk (recursive ownership) | `walkdir`, following no link, the root's included (`follow_links(false)`, `follow_root_links(false)`) | 2.5.0 / 2024-03 (latest, checked 2026-11-13) | 205M | filepath.WalkDir | sb-host (`owner`; host spec, decision 2) |
| systemd journal files (service logs) | sb's own reader, `sb-journal` (pread only, pure Rust; [ADR-013](../decisions/ADR-013-own-journal-reader.md)), on `siphasher` (siphash24), `lz4_flex` (`safe-decode`), `ruzstd`, `lzma-rust2` (`xz`, without `optimization`), `rustix` (inotify, safe opening) and `itertools` (the merge) | siphasher 1.0.4, lz4_flex 0.14.0, ruzstd 0.9.1, lzma-rust2 0.25.0, rustix 1.1.6, itertools 0.14.0 | — | cmd/logs.go's `journalctl -o json --cursor` polling | proto-journal ([findings](../prototypes/proto-journal.md)), the crate's container matrix on 22.04, 24.04 and 26.04 |
| Restore decrypt | `aes`, `cbc`, `pbkdf2`, `sha1`, `sha2` (RustCrypto) | 2026 | 48–332M | crypto/*, x/crypto/pbkdf2 | untested |
| Self-update | hand-rolled on `reqwest` + `sha2`, sends a `User-Agent` | — | — | selfupdate pkg (keeps digest check) | proto-musl (User-Agent) |
| Testing | `insta` + ratatui `TestBackend`, `assert_cmd`, `rexpect` (PTY smoke) | 2026 | 35M / 20M / 0.4M | — | proto-facts (recommendation) |
| Release build | an `ubuntu:22.04` build image (through `cross` 0.2.5 with a custom image, or `docker run`; Task 53 chooses) | — | — | goreleaser-style build | ADR-020; the glibc build in libsystemd-reader.md |

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
- Journal cursor polling (sb-go's approach): replaced by sb's own journal
  reader (`sb-journal`, ADR-013) for systemd units, and by bollard follow
  streams for containers. proto-logs first replaced it with a `journalctl
  -f` child; the host spec's decision 4 replaced that child: a host CLI
  whose JSON has quirks of its own (fields of 4096 bytes or more `null`
  without `--all`, byte-array lines) and whose `-f -n 0` loses writes made
  while it starts. proto-logs, proto-journal.
- `sdjournal` (0.1.22, run hands-on): skips `.journal~` files without an
  error, its follow can die on a rotation race, it can't page back from a
  cursor, its unit match isn't journalctl's `-u` rule, it carries a RustSec
  advisory through ruzstd 0.8, its bus factor is 1, and it adds about
  640 KB to the binary. proto-journal, ADR-013.
- netdata's `systemd-journal-sdk`: reads through an mmap window with a
  SIGBUS handler and links the C `zstd` crate, against the reader's
  pread-only, pure-Rust and `unsafe_code = "forbid"` constraints. Not run.
  ADR-013.
- The libsystemd `systemd` crate: couldn't link into the static musl
  binary, and systemd's musl support is "incomplete" (ADR-013). With
  glibc builds (ADR-020) it could link. It stays rejected for
  libsystemd-reader.md's reasons, which don't depend on musl (hardening
  plan decision 27).
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
| `term-ui` | Inline UI only: task runner (indicatif, finished steps printed as lines), tables, prompts with a non-interactive flag for each, one colour decision feeding anstream/tabled/indicatif; one shared sanitizer, `DisplayText`, for every line drawn from given text and every output and notice line logged (term-ui spec, "Escape handling") | cli-core | terminal |
| `host` | Ubuntu/arch/LXC/desktop checks, disk space, apt, systemd service list, file ownership; wraps `sb-journal` for the service logs, its blocking reads under tokio's `spawn_blocking` (host spec, decision 6) | exec, term-ui, journal | host |
| `journal` (`sb-journal`) | sb's own read-only systemd journal reader, pread only and pure Rust: a unit's newest entries, pages back and forward, following ([journal spec](journal.md); [ADR-013](../decisions/ADR-013-own-journal-reader.md)) | — (wrapped by host) | cmd/logs.go's `journalctl` polling |
| `repo` | Git operations on Saltbox/Sandbox checkouts, branch switching ([repo spec](repo.md)) | exec, host, term-ui. *Amended 2026-11-13 by the repo spec.* | git |
| `release` | GitHub release lookup (with User-Agent), version compare, self-update with digest verification ([release spec](release.md)) | cli-core, term-ui, exec. *Amended 2026-11-13 by the release spec.* | release, selfupdate |
| `python-env` | uv-managed Python, Ansible venv build/update, deadsnakes cleanup, saltbox.fact install ([python-env spec](python-env.md)). *Amended 2026-11-13 by the python-env spec.* | exec, term-ui, host, release | python |
| `ansible` | Playbook execution with tags, tag listing/cache, `install`/`list` ([ansible spec](ansible.md)) | exec, host, repo, term-ui, cli-core; deny.toml follows. *Amended 2026-11-13 by the ansible spec.* | ansible, cmd/install, cmd/list |
| `config` | Saltbox YAML: strict schema loading (drift fails loudly), position-carrying validation collecting all issues, `pattern` and `additionalProperties` support, `validate-config`; facts with a line-preserving writer (round-trip tests), locking and drift detection | cli-core, term-ui (validate-config); exec, host, ansible, release from the facts half and `sb update` on (config spec) | saltbox, facts |
| `docker` | ps, the container list (kept live by the event stream) and log streams via bollard; start/stop/restart via the Saltbox Docker controller's HTTP API after a `systemctl` check (docker spec) | cli-core, term-ui; exec and an HTTP client from start/stop/restart on | cmd/docker* |
| `motd` | System facts (/proc, sysfs, syscalls), packages, logins, disks, services, containers and Traefik, plus media-server and downloader providers (thin reqwest clients), collected in parallel under the login's time budget ([motd spec](motd.md)). *Amended 2026-11-13 by the motd spec.* | cli-core, term-ui, exec, host, config, docker | motd |
| `tui` | ratatui screens: one shared streaming log viewer (docker and systemd) with an inline fallback; fact editor (modal editor, masked secrets, mouse); edit picker; announcements viewer (tui-markdown, announcement list, tui-scrollview). Mouse via recorded layout rects on all screens ([tui spec](tui.md)) | docker, host, config | cmd/logs, cmd/dockerLogs, cmd/edit, factui, saltbox/announcements |
| `ops` | Workflow commands: setup, update, branch, branch-sandbox, reinstall-*, update-venv, restore, diag, bench, gha ([ops spec](ops.md)) | all of the above as needed | remaining cmd/* |
| `completion` | Dynamic completion via `CompleteEnv` (one tag completer reading the tag cache); install writes the generated per-shell registration stub ([completion spec](completion.md)) | cli-core, exec, term-ui, ansible; deny.toml's wrappers follow. *Amended 2026-11-13 by the completion spec.* | completion, cmd/tag_completion |

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

## Commands: sb-go's surface and sb-rust's coverage

Every command sb-go offers, read from its `cmd/` package (2026-11-13),
so the planning covers all of them. A row isn't a promise to port the
command as it is: each is decided when its module is specified. Some
may fold into others or go (for example, a solid saltbox-facts install
may make `reinstall-facts` unnecessary). Update a row when a command's
spec is approved or its build lands. "Hidden" means sb-go leaves it out
of `--help`.

| sb-go command | What it does in sb-go | Module | sb-rust | Planning note |
|---|---|---|---|---|
| `version` (`--json`) | Prints the CLI's version | cli-core | built | |
| `validate-config` | Validates Saltbox's config files against their schemas | config | built | `-v` dropped (config decision 1: the run log has the detail) |
| `docker ps` | Lists containers with their port mappings | docker | built | |
| `docker start` | Starts Saltbox's containers through the Docker controller | docker | built | `-v` dropped (docker decision 6) |
| `docker stop` (`--ignore`) | Stops them | docker | built | as above |
| `docker restart` (`--ignore`) | Restarts them | docker | built | as above |
| `docker logs` | Shows container logs | tui, docker | built (module build M69; the unified picker, M70) | one picker for docker and journal logs (journal spec decision 8; idea: [log-picker](../ideas/log-picker.md)). *Amended 2026-11-13 by the tui spec.* |
| `logs` | Shows logs of Saltbox's systemd services | tui, host, journal | built: services (module build M68), containers (M69), the unified picker's filters (M70) and the inline fallback (M72) | as above; sb-journal and sb-host's follow are built. *Amended 2026-11-13 by the tui spec.* |
| `fact` | Browses and edits saltbox facts interactively | tui | built (module build M54-M56: the page, changes and saving, the mouse and the menu); the manual two-emulator check waits for the maintainer | edits `<appdata>/saltbox/<role>.ini`, not `saltbox.fact`; the role lock only at Save (tui spec decision 38; idea: [fact-editor](../ideas/fact-editor.md)). *Amended 2026-11-13 by the tui spec.* |
| `edit <file>` | Opens one config file in an editor: `accounts`, `adv_settings`, `backup_config`, `hetzner_vlan`, `settings`, `inventory` | tui (edit picker) | built (module build M49, M50; the picker of bare `sb edit`, M53) | validates the file after the editor exits (tui spec). *Amended 2026-11-13 by the tui spec.* |
| `install [tags]` (`-v` to `-vvvv`, `-e`, `--skip-tags`, `--no-cache`) | Runs Saltbox, Sandbox or Saltbox-mod playbooks by tag | ansible | built (module build M31; the tag cache and validation, M32 and M33) | `-v` is counted and passed to `ansible-playbook` as `-v` to `-vvvv` (it stays here, unlike the dropped `-v` elsewhere); `-e` passes each value whole, so JSON reaches Ansible intact (open items below); the tag cache is shared with `list` and completion |
| `list [query]` (`--include-mod`) | Lists the available tags | ansible | built (module build M34) | |
| `update` (`--keep-branch`, `--reset-branch`, `--skip-self-update`, `--migrations`) | Updates Saltbox and Sandbox: git, venv, self-update, announcements | ops, repo, release | specified ([ops](ops.md)) | *Amended 2026-11-13 by the ops spec.* |
| `branch <name>` (required) | Switches the branch Saltbox uses | ops, repo | built per slice (module build M79): runtime and tags join in M85 | *Amended 2026-11-13 by the ops spec.* |
| `branch-sandbox <name>` (required) | Switches the branch Sandbox uses | ops, repo | built per slice (module build M5, on ops since M79): runtime and tags join in M85 | *Amended 2026-11-13 by the ops spec.* |
| `diag` | Runs the diagnostics role | ansible | built (module build M30) | *Amended 2026-11-13 by the ansible spec.* |
| `motd` (about 30 section and banner flags, `--share`, `--generate-config`) | Shows system, media-server and downloader status | motd | built per slice: Distribution and Kernel, the selection, runner and layout (module build M36); the other sections, motd.yml and `--generate-config` (M37-M45); the banner (M46) and `--share` (M47) follow | spec approved; `-v` dropped (motd decision 9). *Amended 2026-11-13 by the motd spec.* |
| `reinstall-facts` | Reinstalls the saltbox.fact file | python-env | built (module build M19) | forced variants; the normal path heals itself (python-env decisions 1-3). *Amended 2026-11-13 by the python-env spec.* |
| `reinstall-python` | Reinstalls Saltbox's Python (uv) and the Ansible venv | python-env | built (module build M16) | forced variants; the normal path heals itself (python-env decisions 1-3). *Amended 2026-11-13 by the python-env spec.* |
| `reinstall-venv` | Reinstalls the Ansible venv | python-env | built (module build M16) | forced variants; the normal path heals itself (python-env decisions 1-3). *Amended 2026-11-13 by the python-env spec.* |
| `bench` | Runs the bundled bench.sh benchmark | ops | built per slice: system and disk (module build M77); Speedtest and the network rows follow (M78) | rewritten in Rust (ops spec decision 11): sysinfo, an in-process disk test, the pinned Ookla Speedtest; no vendored script. *Amended 2026-11-13 by the ops spec.* |
| `completion install [bash\|zsh\|fish]`, `completion <shell>` (hidden) | Installs the shell completion registration | completion | built (module build M58; sb-go's migration and cleanup, M59) | dynamic completion through `CompleteEnv` (proto-complete); unpaused by the maintainer, 2026-11-17, who tests sb's real completion once the CLI is complete. *Amended 2026-11-13 by the completion spec.* |
| `completion generate <shell>` | Writes a completion script to stdout | completion | built (module build M57; Tab completion with it) | as above. *Amended 2026-11-13 by the completion spec.* |
| `setup` (hidden; `--branch`, `--force`, `--keep-branch`, `--reset-branch`, `-v` hidden and ignored) | Installs Saltbox and its dependencies | ops | built per slice (module build M82): the completion refresh joins in M87 | the first command on a new host. *Amended 2026-11-13 by the ops spec.* |
| `update-venv` (hidden) | Updates the Ansible venv and saltbox.fact | python-env | built (module build M14-M20: a healthy venv kept, the rebuild, uv's install, the fact and deadsnakes) | forced variants; the normal path heals itself (python-env decisions 1-3). *Amended 2026-11-13 by the python-env spec.* |
| `self-update` (hidden; `--yes`, and `--force-update` in builds with self-update disabled, raw builds included) | Updates the CLI itself | release | built | sends a User-Agent; keeps the digest check (stack table). *Amended 2026-11-13 by the release spec.* |
| `announcements` (no flags) | Shows Saltbox's and Sandbox's announcements, each labelled with its repository | ops, tui | built (module build M74: the inline form, then tui's viewer on a terminal, M87's viewer half) | reads Saltbox's and Sandbox's announcements, every one that applies: tui's viewer on a terminal, inline otherwise; "new" and migrations stay with update (ops spec, decisions 27 and 30, difference 45). *Amended 2026-11-13 by the ops spec, 2026-11-15, 2026-11-17 (no flags, both repositories, labelled).* |
| `restore` (hidden) | Fetches and decrypts a user's backed-up files | ops | built (module build M75, M76) | RustCrypto stack (stack table); --user and --password-file, no -v (ops spec). On a terminal tui's restore screen: the form, the live list, the file view (ops decision 32). *Amended 2026-11-17.* |
| `gha` (hidden) | Installs the dependencies Saltbox's GitHub Actions jobs need | ops | built (module build M81) | CI-only |

29 rows: every command sb-go registers, with `docker` and `completion`
listed by their subcommands. On 2026-11-17, 18 are built, 5 are built
per slice (`edit`, `branch`, `branch-sandbox`, `motd`, `setup`) and 6
are specified (`fact`, `update`, `bench`, both `completion` rows and
`restore`).

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
- **The ratchet rows** (CONSTRAINTS.md, "Measured, not yet enforced";
  maintainer, 2026-11-13). They stay as they are until then. With every
  command in place they are re-measured, and the maintainer decides how
  far they can be cut down. The fixed limits (the 10 ms startup budget,
  the size ceiling) gate every change meanwhile. Last measured on main
  at 90e9d4f (2026-11-17, glibc build), the way the gate and ADR-014
  measure: coverage 98.51% of lines (69,601 of 70,657) and 97.67% of
  functions (8,632 of 8,838) over `crates/`, production binary
  20,307,264 bytes, `sb --help` median 3.99 ms and `sb version` 4.23 ms.
  Timed side by side on one host and load, 90e9d4f's release build
  starts in 4.11 ms (`--help`) and 4.47 ms (`version`) against
  964f963's 2.57 ms and 2.96 ms. Measured before at the hardening
  plan's Checkpoint B (2026-11-13, 964f963): 98.63% of lines, 98.119% of
  functions, 11,843,920 bytes, `sb --help` 2.30 ms. The rows' own
  figures came from older hardware.
- **musl or glibc** (maintainer, 2026-11-13). glibc and musl builds of
  the feature-complete CLI are measured side by side on one host
  (startup, peak memory, the commands' own run times), and the
  maintainer decides whether the difference matters. The figures so far
  are in [ADR-020](../decisions/ADR-020-glibc-builds-on-the-oldest-release.md)'s
  Consequences; a move back to musl supersedes that ADR. **Settled
  (maintainer, 2026-11-17, ahead of Checkpoint Z): glibc with its own
  malloc.** Measured on 90e9d4f, six production builds (glibc and
  static musl, each with its own malloc, mimalloc and jemalloc): glibc
  with its own malloc is the smallest (20.3 MB) and ties for fastest
  with musl and jemalloc on startup (`sb --help` 2.78 against 2.54 ms)
  and on `list`, `motd`, `docker ps` and `validate-config`, at 1 to
  2 MiB more peak memory than musl. Plain musl's malloc costs up to
  12 ms on `motd`; mimalloc 6 to 10 MiB of memory. The method and the
  figures are in ADR-020's "Confirmed" note.
  - **Memory allocators too** (maintainer, 2026-11-16). Each libc build
    is measured with its own malloc and with the widely used Rust global
    allocators (for example mimalloc and jemalloc, through
    `#[global_allocator]`), on the same figures. musl's own malloc is
    the one most often replaced, so a musl build with another allocator
    may land somewhere else than plain musl does. The pair the
    maintainer picks, and its effect on size and startup, goes in the
    ADR that records the choice. **Settled (maintainer, 2026-11-17):
    the libc's own malloc, with glibc; neither mimalloc nor jemalloc
    beats it there** (ADR-020, "Confirmed").
- **A performance pass** (maintainer, 2026-11-16), once the libc and
  allocator choice is settled, so it profiles the build that ships. Each
  command's hot paths are measured and profiled (CPU time, allocations,
  syscalls and I/O, startup) to find where sb's own code could be faster
  or lighter, then changed only where a measurement shows a gain and
  correctness holds: every test, the mutation score and the specs'
  behaviour stay as they are, and each change carries its before and
  after figures. What it finds can also set tighter ratchet rows.
  - **Size: the build profile stays as it is** (maintainer, 2026-11-17).
    The production binary of 90e9d4f is 20,303,872 bytes (11,843,920 on
    964f963). The growth is the features added since, spread over the
    new crates and their use of std, tokio and serde; no single crate
    accounts for it. Three changes were measured in scratch builds,
    with the four offline commands' and a scripted `sb logs` session's
    user-space instructions (`perf stat`):

    | change | size | instructions |
    |---|---:|---|
    | `opt-level = "s"` | 13,685,408 (−32.6%) | +7 to +13% |
    | `panic = "abort"` | 18,279,584 (−10.0%) | −0.1 to −3.4% |
    | no syntax highlighting (tui-markdown without its default feature) | 19,240,408 (−5.2%) | unchanged |

    Kept: opt-level 3 (speed), panic unwinding (motd's sections and the
    runner's steps report a panic and go on; an abort would end the
    run), and syntax highlighting (CONSTRAINTS.md exception E1).
- **A phase exploring alternative output designs** (maintainer,
  2026-11-14). The maintainer's design canvas "sb list: refined columns"
  (https://claude.ai/artifact/FLU6EQNhqSh2BHD9NkeHv2) is the starting point, one of the designs to weigh, not a spec. It
  proposes, for `sb list`: bold, coloured headings with tag counts; a
  rule under each heading; "install as sandbox-<tag>"; uniform
  column-major columns (the longest tag plus a gap); and a footer hint.
  The maintainer's notes on it:
  - keep a bit of colour;
  - align the columns across the Saltbox and Sandbox sections, so they
    are easier on the eyes;
  - fix what the canvas gets wrong: no blank line at the top, and the
    cache state shown as a right-aligned "cached" or "rebuilt, repo
    changed" is not how it should look.
  The phase explores alternatives for sb's outputs, this one included,
  once the commands exist; until then the approved looks stand.
  `sb list`'s design was taken forward on 2026-11-16, at the
  maintainer's request: the ansible spec's `sb list` and its decision
  19 have the layout and the maintainer's changes to the canvas.
- **Tests on a real machine** (maintainer, 2026-11-13). Every test of
  sb's host-changing commands uses stand-ins (apt-get, ansible-playbook,
  systemctl on `PATH`, a fake GitHub and Engine API), and the by-hand
  containers have no systemd and no Docker daemon. Disposable Ubuntu
  VMs of each supported release (cloud image and cloud-init under
  QEMU/KVM) would run `sb install`, `sb update`, self-update and the
  service and container commands for real, as root. Ideas from
  bootvine's NixOS VM tests: the machine holds at fixed points on a
  marker file, and the check reads the screen as text and waits on an
  event instead of sleeping. Costs: a harness of our own, KVM, heavy CPU
  within the host's cap, slow runs, and CI not yet authorised.

## Open items carried into module specs

- **`install`'s Ansible pass-through** (maintainer, 2026-11-13; for the
  ansible module spec). Both are requirements, with tests in the spec:
  - **Verbosity:** `-v` is a counted flag (`-v`, `-vv`, `-vvv`, `-vvvv`,
    and `-v -v`), passed to every `ansible-playbook` run as one `-v...`
    argument of the same count. sb-go does this with cobra's `CountVarP`
    (cmd/install.go); clap's `ArgAction::Count` is the equivalent. sb-go
    also turns on its own debug lines with it. Whether sb's does too, or
    leaves that to the run log, is for the spec.
  - **`-e` / `--extra-vars` keeps every value whole.** Each `-e` value
    reaches Ansible as one `--extra-vars` argument, exactly as given:
    JSON with commas (`-e '{"a": 1, "b": [2, 3]}'`), `key=value` pairs,
    and `@file`. Values are never split on commas, joined, or passed
    through a shell. Early sb-go split them on commas, which broke JSON
    with more than one key. It used cobra's `StringSlice`, and sb-go
    bc1be03 (issue #11, 2025-05-31) moved it to `StringArray`. In clap:
    `ArgAction::Append` with no `value_delimiter`, plus
    `allow_hyphen_values` so a value may start with `-`. Each value goes
    to the child as one argv element, `--extra-vars=<value>` (exec's
    argument list, no shell). Not the two elements `--extra-vars <value>`
    that sb-go sends: Ansible's argparse refuses a following element
    that starts with `-` (checked by the ansible research, 2026-11-13).
    The spec's tests cover multi-key JSON, nested quotes, unicode, a
    value starting with `-`, and several `-e`s in order.
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
- **Install script:** moves into this repository (ops spec, "The
  install script"), starting from sb-go's `install.sh` plus `-b`. The
  served installer (`sb_install.sh` in saltyorg/sb, behind
  install.saltbox.dev) no longer captures `sb setup`'s output (checked
  2026-11-13); it passes `-v` and `-b`, so `sb setup` accepts `-v`
  hidden and ignored until the served script is replaced. Publishing
  the new script is a separate, explicitly authorised step. *Amended
  2026-11-13 by the ops spec.*
- **Shell completion:** proto-complete's look and feel is not yet reviewed
  by the maintainer; review it before the `completion` module spec. fish
  registration is generated but untested. Completion runs without sudo
  (cli-core spec), so the tag cache stays readable by any user: `ansible`
  writes it 0644, set explicitly.
- **Runner:** nested subtasks (`insert_after`) and forwarding terminal resize
  to the PTY haven't been prototyped. Evaluate a terminal-emulator crate
  (`vt100`, or termwiz's escape parser) for the output tail instead of the
  custom line splitter.
- **Edit picker:** closed by the editors' picker slice (tui spec). It
  reuses the proto-facts and proto-logs patterns (list picker, then
  `$EDITOR`). *Amended 2026-11-13 by the tui spec.*
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
