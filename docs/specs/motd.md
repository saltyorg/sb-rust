# Spec: `motd` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

This spec covers the `motd` module of the approved capability map (step 5 of
the build order, with `completion`). It builds `sb motd`, the command
Saltbox's `/etc/update-motd.d/01-motd` runs at every SSH login. The module
builds on `cli-core`, `term-ui`, `exec`, `host`, `config` and `docker`, which
are all approved and built, except for host's service list (see "Amendments
to other specs"). It is one plan, in the slices listed at the end.

## Assumptions

1. **sb-go's command is the reference** (`cmd/motd.go`, `motd/`, read at
   HEAD with motd history up to 4bb2bfe). sb keeps its flags, section keys,
   order, texts and exit codes. Every deviation is listed under
   "Differences from sb-go".
2. **The login is the main use.** Saltbox's `roles/motd` writes
   `01-motd` as `export FORCE_COLOR=1; export CLICOLOR_FORCE=1;
   /usr/local/bin/sb motd --all --title 'Saltbox' --font ivrit --type
   parchment`
   (https://github.com/saltyorg/Saltbox/blob/master/roles/motd/templates/01-motd.j2,
   defaults/main.yml). pam_motd runs it as root with stdout piped, and the
   user waits for it. Latency at login is therefore part of the contract
   (Success criteria 9 to 11).
3. **Saltbox installs** `lsb-release figlet jq toilet boxes` and
   `update-notifier-common` (roles/motd/tasks/main.yml). sb needs `toilet`,
   `boxes`, `loginctl`, `last`, `systemctl` and update-notifier's scripts.
   It does not need `lspci`, `lsb_release`, `uname`, `uptime`, `df` or
   `who`. It reads `/usr/share/misc/pci.ids` (Ubuntu's `pci.ids`
   package, which pciutils depends on) for GPU names when the file is
   there; without it GPUs show by number.
4. **motd.yml** is `<saltbox root>/motd.yml` (`SB_SALTBOX_DIR` or
   `/srv/git/saltbox`). It is optional. Saltbox writes it with real
   credentials only for the `motd-generate-config` tag (0600, owned by the
   user). Its schema is `<saltbox root>/schema/motd.schema.yml`, identical to
   sb's pinned fixture
   `crates/config/tests/fixtures/saltbox-schemas/motd.schema.yml` (diffed
   2026-11-13).
5. **Root:** motd isn't exempt from cli-core's root rule (cli-core
   decision 3). Under pam_motd it already runs as root, so there is no
   relaunch and no prompt at login.
6. **Host facts in this spec were checked against public sources:**
   pciutils' `lib/names.c` at v3.7.0
   (https://github.com/pciutils/pciutils/blob/v3.7.0/lib/names.c), and the
   crates' sources at the versions named. No host data is quoted here.

## Objective

`sb motd` prints a banner, then one aligned `Key:  value` line per selected
section: system facts, packages, logins, disks, services, containers,
Traefik routers, download queues, downloaders and media servers. It is what
a Saltbox user sees at every SSH login. It must be quick, never hang the
login, never print a credential, and look like sb-go's MOTD except where
listed below.

### Flags (cmd/motd.go:68-100; the contract test checks them)

- **Sections (24):** `--apt --cpu --cpu-info --disk --distro --docker
  --emby --gpu --jellyfin --kernel --login --memory --nzbget --plex
  --processes --qbittorrent --queues --reboot --rtorrent --sabnzbd
  --sessions --systemd --traefik --uptime`, plus `--all`. As in sb-go,
  `--cpu` means the load averages and `--cpu-info` means the CPU model.
- **Banner (5):** `--title` (default `Saltbox`), `--type` (default `peek`;
  `none` means no box), `--font` (default `ivrit`), `--banner-file` and
  `--banner-file-toilet`.
- **Other:** `--share` and `--generate-config`. No `-v/--verbose`
  (decision 9): `-v` is clap's parse error, exit 2, as in every command
  but `install` and `setup`.
- No arguments (`cobra.NoArgs`). The help texts are sb-go's.

### Command flow

1. **`--generate-config`** prints an example motd.yml and exits 0. It
   ignores every other flag, as sb-go does (`cmd/motd.go:107-114`).
   - The schema comes from `<root>/schema/motd.schema.yml` through
     `sb_config::schema::load` (decision 18).
   - The output is byte-identical to sb-go's `GenerateExampleConfig`
     (`motd/config.go:36-73`). The sections are written in sb-go's fixed
     order: colors, emby, jellyfin, lidarr, nzbget, plex, qbittorrent,
     radarr, readarr, rtorrent, sabnzbd, sonarr, systemd.
   - Each section gets its `# description` comment. Its values are the
     schema's `example`s, with one example instance per `instances` list.
     Fields are written in the order name, url, apikey, token, user,
     password, timeout, enabled, and strings are quoted as Go's
     `strconv.Quote` does.
   - `colors` takes sb-go's charmtone defaults (`motd/config.go:157-185`),
     so the printed file looks as it does today.
   - A schema that won't load is an error naming the file and the cause,
     exit 1.
2. **Selection.** `--all` turns every section on. With no section selected,
   sb fails with sb-go's text, `no information selected to display (use
   --all or specific flags)`, exit 1.
3. **Banner validation.** This runs only when a banner will be drawn, that
   is, with no `--banner-file` and a non-empty `--title`.
   - `--type` must be `none` or one of sb-go's 59 boxes designs
     (`motd/banner.go:18-25`). Otherwise sb fails with `invalid banner type
     specified: X` and the list in 4 columns of 16 (sb-go's layout), exit 1.
   - `--font` is checked only when it is given explicitly. It must be
     `term`, or `<font>.flf`/`.tlf` in `/usr/share/figlet` or the current
     directory (`motd/banner.go:28-54`). Otherwise sb fails with `invalid
     font specified: X` and the fonts, **sorted**, in sb-go's columns,
     exit 1.
4. **motd.yml** is read and checked once (decisions 3 and 17), before
   collection starts.
5. **Banner and collection run concurrently** (decision 25). The banner is
   still printed first, before any section line.
   - With `--banner-file`, the file is printed raw, or piped through
     `toilet <args>` when `--banner-file-toilet` is set. The args are
     split with `shlex` and passed as argv, never through a shell.
   - Without one, the banner is `toilet -f <font> <title>`, piped through
     `boxes -d <type> -a hc -p h8` unless the type is `none`
     (`motd/banner.go:88-164`).
   - Each child runs through exec with a 5 s timeout, the input given on
     stdin.
   - A missing `toilet` prints `--- <title> ---` plus sb-go's warning on
     stderr. A missing `boxes` prints toilet's output alone, plus a
     warning. An unreadable banner file is an error, exit 1, with sb-go's
     text `could not read banner file '<path>': <cause>`.
6. **Collection.** Each selected section runs as one tokio task, and each
   instance within a section as its own task (decision 11).
   - Each section is capped at 30 s (sb-go's `defaultProviderTimeout`).
     A section that times out shows `Error: provider timed out after
     30s`.
   - A panic in a section shows `Error: panic occurred (<message>)`, with
     the message redacted (decision 12). The other sections still print.
7. **Layout** (sb-go's `displayMotd`, `cmd/motd.go:202-339`):
   - Results come in sb-go's fixed order: Distribution, Kernel, Uptime,
     Load Averages, Processes, CPU, GPU, Memory Usage, Package Status,
     Reboot Status, User Sessions, Last login, Disk Usage, Services,
     Docker, Traefik, Download Queues, SABnzbd, NZBGet, qBittorrent,
     rTorrent, Plex, Emby, Jellyfin. sb adds a `Config:` line, first, when
     motd.yml has issues (decision 3).
   - A section with an empty value is hidden.
   - Keys are padded to the longest shown key plus 2, in the label colour.
   - Continuation lines are indented to the value column.
   - One blank line ends the output.
   - Exit 0, whatever a section shows (decision 23).
8. **`--share`** runs one filter over every value line before it is printed
   (decision 7).

### Sections

All texts are sb-go's unless the Differences list says otherwise. Any value
that comes from outside sb goes through `DisplayText` (decision 24). That
covers names, statuses, router names, error bodies, display names and
mount points.

| Section | Source in sb | sb-go's source |
|---|---|---|
| Distribution | `/etc/os-release` `PRETTY_NAME` and `VERSION_CODENAME`, formatted as `PRETTY_NAME (codename)` | `lsb_release -ds`, `-cs` |
| Kernel | `rustix::system::uname().release()` | `uname -r` |
| Uptime | `procfs::Uptime`, formatted as procps' `uptime -p` without `up ` (forms pinned per release, below) | `uptime -p` |
| Load Averages | `procfs::LoadAverage`, two decimals, as `/proc/loadavg` prints them | `/proc/loadavg`, then `uptime` |
| Processes | `procfs::process::all_processes()` counted | numeric `/proc` dirs, then `ps ax` |
| CPU | `procfs::CpuInfo`: the model with ` N-Core…` stripped, sockets, cores and threads (`motd/info.go:644-783`) | `/proc/cpuinfo` |
| GPU | sysfs `/sys/bus/pci/devices/*/{class,vendor,device,revision}`, classes 0300, 0302 and 0380, names from the host's `/usr/share/misc/pci.ids` through sb's own parser (decision 2), sb-go's BMC exclusions (`motd/info.go:780-790`) | `lspci` |
| Memory Usage | `procfs::Meminfo`: MemAvailable, or else Free+Cached+Buffers; sb-go's `xG used, free, cached, available, total` | `/proc/meminfo` |
| Package Status | update-notifier's `updates-available` file, then `/usr/lib/update-notifier/apt-check --human-readable --no-esm-messages`, then `apt list --upgradable` (children, 5 s each; `motd/info.go:372-570`) | same |
| Reboot Status | `/var/run/reboot-required` and `.pkgs`, then `/usr/lib/update-notifier/update-motd-reboot-required` | same |
| User Sessions | `loginctl list-sessions --output=json`, counting user sessions (decision 6) | `who` |
| Last login | `last` (decision 6), with sb-go's text `user at Day Mon D HH:MM until HH:MM (dur) from IP` | `last -n 5`, then `lastlog -u root`, then `who -u` |
| Disk Usage | `procfs` mountinfo plus `rustix::fs::statvfs`, with df's rules (below) and the bar of decision 8 | `df -H -x … --output=target,pcent,size` |
| Services | `sb_host::services(filters)` with motd.yml's `systemd` additions and display names (`motd/systemd.go`) | `host.GetFilteredServices` |
| Docker | sb-docker's `connect::list` rows (decision 13) | `systemctl is-active docker`, `docker ps -a` |
| Traefik | the same listing's `traefik` row, then reqwest `GET http://traefik:8080/api/http/routers` with a 3 s connect timeout (decision 15) | `docker ps`, `curl` |
| Download Queues | Sonarr, Radarr, Lidarr and Readarr `/api/v3/queue` (v1 for Lidarr and Readarr), 100 records per page until done, `X-Api-Key` | golift.io/starr |
| SABnzbd | `GET <url>/api?mode=queue&output=json&apikey=…` | same |
| NZBGet | JSON-RPC `status` then `listgroups`, with Basic auth | same |
| qBittorrent | `POST /api/v2/auth/login` (1 s), then `GET /api/v2/sync/maindata?rid=0` (20 s), counting every state | autobrr/go-qbittorrent |
| rTorrent | XML-RPC on quick-xml: `system.hostname` (1 s), then one `d.multicall2` and the two global rates (20 s) | saltydk/go-rtorrent |
| Plex | `GET <url>/status/sessions`, `X-Plex-Token`, JSON | same |
| Emby | `GET <url>/emby/Sessions`, `X-Emby-Token` | same |
| Jellyfin | `GET <url>/Sessions`, `Authorization: MediaBrowser Token="…"` | sj14/jellyfin-go |

**HTTP instances** (sb-go's rules, from the providers' files):
- A section is shown when it exists in motd.yml, `enabled` isn't false
  and it has instances.
- An instance with `enabled: false` is skipped.
- Instances are fetched concurrently and listed by name.
- With one instance, the summary is shown alone. The exception is the Arr
  queues, which always show names. With several, each line starts with
  `Name:` padded, in the app_name colour.
- The default timeout is 1 s per request. A configured `timeout` (seconds,
  fractions allowed) replaces it. For qBittorrent and rTorrent, it replaces
  the 20 s data timeout, and the 1 s fast check stays. Every timeout is
  capped by what is left of the section's 30 s (sb-go's
  `timeoutFromContext`, `motd/queue.go:551-571`).
- An error shows as `Error: <first line, redacted>` in the error colour.
- Every HTTP client is one `sb_config::http::Http` (reqwest with rustls,
  1 MiB body cap) built once per run.

**Disk Usage, df's rules.** The mounts come from `/proc/self/mountinfo`.
- The filesystem types tmpfs, overlay, fuse.mergerfs and fuse.rclone are
  skipped, as are mount points under /dev, /sys, /proc and /run (sb-go's
  filters).
- So are df's dummy filesystems (zero total blocks), and duplicate mounts
  of one device, using coreutils' `filter_mount_list` rule
  (https://github.com/coreutils/coreutils/blob/master/src/df.c).
- Percent is `used / (used + avail)` rounded up, where `used = f_blocks -
  f_bfree` and `avail = f_bavail`, in `f_frsize` units.
- Sizes are df `-H`'s SI form: powers of 1000, rounded up, one decimal
  below 10.
- The line is `%-30s%3d%% used out of %4s`, followed by the bar on the
  next line.
- Levels: low below 80, high below 90, critical from 90.
- The container matrix compares sb's lines with `df`'s on each release
  (Testing).

**GPU names** (decision 2). The device IDs come from sysfs. The names
come from the host's `/usr/share/misc/pci.ids`, the file lspci reads,
through a small parser of sb's own (`gpu.rs`):
- The file is read with cli-core's `read_small`-style cap (8 MiB; the
  file is about 1.4 MB), as UTF-8 with invalid bytes replaced, and only
  the vendor lines (`vvvv  name`) and their device lines (a tab, then
  `dddd  name`) are used. Subsystem lines (two tabs), comments (`#`),
  and the class section (from the first `C ` line on) are skipped. A
  malformed line is skipped and counted in the run log.
- Each display-class device reads `<vendor> <device>`. Where the device
  ID isn't in the file, it reads `<vendor> Device <dddd>`; where the
  vendor isn't either, `Device <vvvv>:<dddd>`. These are pciutils' forms
  for unknown IDs (`names.c`, `format_name_pair`, names.c:108-113).
- A missing or unreadable file is not an error: every device reads in
  those unknown-ID forms, and the run log records why the file wasn't
  read. lspci without the file shows the same numbers.
- ` (rev xx)` is appended when the sysfs `revision` is not zero, as lspci
  does. Devices are ordered by PCI address, as lspci orders them.
- Names go through `DisplayText`, like any text sb didn't write.

So the names match lspci's on that host (difference 21), except where
lspci asks udev's hwdb (difference 22).

**Secrets** (decision 12). motd.yml's `token`, `apikey` and `password`
(typed `string` in the schema), and SABnzbd's `apikey` query parameter,
never reach stdout, stderr or the run log.
- Credentials travel in headers or Basic auth wherever the API allows
  (4bb2bfe). SABnzbd has to take its key in the query.
- Every error text from a provider is built from reqwest's error
  `without_url()`
  (https://docs.rs/reqwest/0.13.5/reqwest/struct.Error.html#method.without_url),
  then each configured credential is removed with
  `sb_config::secret::hide`, raw and percent-encoded, and replaced by
  `[REDACTED]`. Then it is cut to its first line.
- Run-log records name the instance by its name and its configured base
  URL through `sb_config::http::loggable_url`, with the query removed.
  They never carry the request URL.
- The panic message and the `Config:` issues get the same treatment. The
  issue line shows only a count. The run log gets the issues with their
  positions, through config's `secret_paths` with Task 60's credential
  paths.

### Differences from sb-go (the complete list)

1. **No `-v`** (decision 9): it is a parse error, exit 2, and the detail
   goes to the run log. sb-go printed DEBUG lines to stdout, so they
   ended up in the login MOTD itself.
2. **An invalid motd.yml** prints `Config:  motd.yml has N issue(s); run
   sb validate-config` and skips only the config-driven parts: the HTTP
   sections, the colours, and Services' additions (decision 3). sb-go hid
   every config section silently, and treated valid files it couldn't
   decode the same way (`timeout: 1.5`).
3. **The legacy bare-list form** (`sonarr: [ {...} ]`) is an issue under
   difference 2 (decision 4). sb-go accepted it.
4. **Without motd.yml, or with a colour key absent,** sb's palette applies
   (decision 5). sb-go used its charmtone defaults. Saltbox always writes
   the colour block, so most hosts look the same in both.
5. **Redaction reads `[REDACTED]`** (term-ui's form). sb-go wrote
   `<redacted>`. Provider errors no longer carry the request URL.
6. **`--share`** masks IP addresses in every line, and host names in the
   last-login source, in every provider error, and wherever a configured
   instance's host appears (decision 7). sb-go masked only the last-login
   IP.
7. **User Sessions comes from logind** (decision 6). sb-go read `who`,
   which reads utmp, and utmp is gone on newer Ubuntu. Sessions of the
   classes `manager`, `manager-early` and `background` are not counted
   where `loginctl` reports a class.
8. **Last login comes from `last` alone.** The `lastlog` and `who -u`
   fallbacks are dropped (decision 6), and a failure reads `No recent
   logins`.
9. **Distribution, Kernel, Uptime, Load Averages, Processes and Disk
   Usage run no children** (decision 16). They show the same text, which
   the container matrix checks against the tools on each release.
10. **Docker uses sb-docker's classification** (decision 13), not sb-go
    motd's own.
    - Problem lines read `ps`'s status texts: `exited (code 1)`,
      `exited (out of memory)`, `created (failed to start)`, `running
      (unhealthy)`, `running (starting)`, `restarting`, `dead`. sb-go
      wrote `stopped (error: 1)` and `stopped`.
    - A container "needs attention" when its role is Problem or Warn.
      Stopped with exit 0, created and paused are Idle, so they are counted
      but not listed. sb-go listed them.
    - The daemon is reached through bollard, and an unreachable daemon
      reads `Error: <cause>`.
11. **Traefik:** a non-JSON answer reads `Error: unexpected answer from
    Traefik (HTTP <code>)`. sb-go's `curl -s` hid connection errors and
    showed "Failed to parse".
12. **qBittorrent counts every state.** allocating, moving,
    checkingResumeData and unknown are counted under their nearest sb-go
    group (moving and allocating as Downloading, checkingResumeData as
    Stopped, unknown as Error). sb-go counted them nowhere.
13. **rTorrent makes one multicall.** `d.is_active` is already in it.
    sb-go made N+1 calls. An unexpected XML-RPC type is an error line,
    where sb-go panicked and recovered.
14. **Plex doesn't count paused sessions** as active, as Emby already
    doesn't. sb-go counted them.
15. **The font list is sorted.**
16. **The banner is drawn while collection runs** (decision 25). The
    output order is unchanged.
17. **motd.yml is read once per run.** sb-go read it up to 10 times.
18. **Errors** are cli-core's error block. Unknown flags are clap's parse
    error, exit 2 (sb-go: exit 1), as for every sb command.
19. **`timeout` takes fractions** (the schema's `number`). sb-go's `int`
    rejected `1.5`, which hid every config section.
20. **A GPU on a non-zero PCI domain** shows its name. sb-go's colon split
    showed `02.0 VGA compatible controller: <name>` there.
21. **GPU names come from the host's `/usr/share/misc/pci.ids`** through
    sb's own parser, with device IDs from sysfs (decision 2), not from an
    `lspci` child. They are the names lspci shows on that host, from the
    same file, in pciutils' forms for unknown IDs. sb-go parsed lspci's
    output.
22. **No udev hwdb fallback** for GPU names. pciutils asks the hwdb for an
    ID missing from pci.ids (`names.c:28-31`), and sb doesn't. The hwdb is
    generated from pci.ids, so this matters only where the hwdb is newer
    than the host's file; such a device reads `<vendor> Device <dddd>` in
    sb where lspci names it.

### Scope

- **In:** the `sb-motd` crate; `sb motd` with every flag; the 24
  sections; the banner; motd.yml's model, check and colours;
  `--generate-config`; `--share`; the run log; the platform note for the
  container checks (`docs/platform-notes/motd-sources.md`).
- **Out:**
  - Saltbox's `01-motd` and `motd.yml.j2` (consumer repo, read-only).
  - The legacy Python MOTD (`motd_use_python`).
  - A JSON output: no consumer asks for one (capability map open item
    "Plain/JSON output", decided per command; none here).
- **Later:** a figlet-rs banner if boxes ever gets a library (decision 1).

## Tech Stack

New to the binary is `procfs`. Everything else is already in sb or in
Cargo.lock.

| Crate | Version | Why | Docs | In Cargo.lock |
|---|---|---|---|---|
| reqwest (rustls) through `sb_config::http::Http` | 0.13.5 | the thin HTTP clients (map policy 3) | https://docs.rs/reqwest/0.13.5/reqwest/ | yes, in sb |
| serde, serde_json | 1.0.154 | every JSON API, loginctl's JSON | https://docs.rs/serde_json/1.0.154/serde_json/ | yes, in sb |
| quick-xml | 0.42.0 | rTorrent's XML-RPC: write `methodCall`, read `methodResponse` (int, i4, i8, string, array, fault) | https://docs.rs/quick-xml/0.42.0/quick_xml/ | yes |
| procfs (`default-features = false`) | 0.18.0 | meminfo, cpuinfo, loadavg, uptime, the process list, mountinfo; 23.7M downloads in 90 days | https://docs.rs/procfs/0.18.0/procfs/ | **no** |
| rustix (`system`, `fs`) | 1.1.6 | `uname`, `statvfs` | https://docs.rs/rustix/1.1.6/rustix/system/fn.uname.html, https://docs.rs/rustix/1.1.6/rustix/fs/fn.statvfs.html | yes, in sb |
| shlex | 2.0.1 | `--banner-file-toilet` args | https://docs.rs/shlex/2.0.1/shlex/fn.split.html | yes |
| regex | 1.13.1 | `--share`'s candidates for IP addresses, each confirmed by `std::net::IpAddr`'s parser | https://docs.rs/regex/1.13.1/regex/ | yes, in sb |
| jiff | 0.2.38 | last-login times | https://docs.rs/jiff/0.2.38/jiff/ | yes, in sb |
| tokio, tokio-util, futures-util | workspace | tasks, `CancellationToken`, timeouts | https://docs.rs/tokio/latest/tokio/time/fn.timeout.html | yes |
| sb-cli-core, sb-term-ui, sb-exec, sb-host, sb-config, sb-docker | workspace | startup and run log, paint/`DisplayText`/bar, children, services, motd.yml and HTTP, containers | — | yes |
| insta, assert_cmd, `sb_config::testing::Fake`, sb-docker's `testing` | workspace | tests only | — | yes |

**Considered and not used:**
- sysinfo has no `PRETTY_NAME`, codename, `Cached` or `Buffers`
  (sysinfo-0.40.0 `src/unix/linux/system.rs:614-624`). It stays for
  host's release check.
- sysinfo `disk` gives no `f_bfree`, so it can't reproduce df's percent.
- `utmp-rs` is stale, and utmp is going away.
- `dxr`, `qbit-rs`, starr-like crates: map policy 3.
- figlet-rs: decision 1.
- **pci-ids** 0.2.6 vendors the PCI ID database into the binary ("this
  crate vendors the PCI ID database",
  https://docs.rs/pci-ids/0.2.6/pci_ids/), which measured +3,432,280
  bytes in a minimal production-profile build, and its names are those
  of its snapshot, not the host's. `pciids` 0.1.4 parses the file but
  has had no release since 2020-07-26
  (https://docs.rs/crate/pciids/0.1.4). No maintained crate on docs.rs
  parses the host's pci.ids at run time without embedding it (searched
  2026-11-13: https://docs.rs/releases/search?query=pci.ids), so the
  parser is sb's own, as domain logic (decision 2).

**Binary size.** procfs's cost is measured on the production binary when
it lands; the GPU parser is a few hundred lines of std. An ADR follows
only if the growth crosses the ratchet's tolerance.

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-motd -p sb
Matrix:  crates/motd/tests/matrix.sh <22.04|24.04|26.04>   (systemd-booted ubuntu containers; by hand at checkpoints)
Time:    hyperfine --warmup 3 --runs 30 'SB_LOG_DIR=<scratch> SB_SALTBOX_DIR=<fixture> target/production/sb motd --all --title Saltbox --font ivrit --type parchment'
Try:     the production build of sb, run as root with SB_LOG_DIR set (sb motd --all)
```

The matrix and the timing runs are capped with `systemd-run --scope -p
MemoryMax=2G -p CPUQuota=200%`, one at a time.

## Project Structure

```
crates/motd/                 sb-motd
  src/lib.rs                 Options (the flags), run(): selection, check, banner + collection, layout
  src/run.rs                 the parallel runner: tasks, 30 s cap, panics, fixed order
  src/layout.rs              key/value writer, padding, continuation lines
  src/config.rs              motd.yml model, check, colours, --generate-config
  src/redact.rs              credential hiding and --share's filter
  src/banner.rs              toilet/boxes children, type and font lists
  src/system.rs              distro, kernel, uptime, load, processes, cpu, memory
  src/gpu.rs                 sysfs scan, the host's pci.ids parser
  src/disk.rs                mountinfo, statvfs, df's rules, bars
  src/packages.rs            package and reboot status
  src/logins.rs              loginctl sessions, last
  src/services.rs            Services (host's list, display names)
  src/docker.rs              Docker and Traefik
  src/providers/             arr, plex, emby, jellyfin, sabnzbd, nzbget, qbittorrent, rtorrent
  tests/                     fixtures (synthetic), fake servers, stand-in programs, matrix.sh
crates/sb/src/main.rs        the `motd` command, runtime after parsing
crates/sb/tests/motd.rs      end-to-end runs of the built binary
docs/platform-notes/motd-sources.md   the container checks
```

deny.toml: an entry for `sb-motd` that allows only `sb`. `sb-motd` is added
to the wrappers of `sb-cli-core`, `sb-term-ui`, `sb-exec`, `sb-host`,
`sb-config` and `sb-docker`.

## Code Style

As the built modules do: pure functions for the rules, tested first, with
the I/O at the edge. Each section returns its value and its log record.

```rust
/// df's percent: used over used plus available, rounded up
/// (coreutils src/df.c, `get_dev`), in fragment units.
#[must_use]
pub fn percent(stat: &StatVfs) -> Option<u8> {
    let used = stat.f_blocks.checked_sub(stat.f_bfree)?;
    let total = used.checked_add(stat.f_bavail)?;
    if total == 0 {
        return None; // a dummy filesystem: df leaves it out
    }
    u8::try_from(used.saturating_mul(100).div_ceil(total)).ok()
}
```

## Testing Strategy

Nothing touches a real host's state, and every fixture is synthetic.

- **Unit (most):**
  - the selection rules and sb-go's error texts;
  - the layout (insta snapshots, with colour on and off);
  - every section's formatting from fixture input: procfs types read with
    `FromRead` from fixture files
    (https://docs.rs/procfs/0.18.0/procfs/trait.FromRead.html), sysfs
    trees in a temporary directory, mountinfo plus statvfs results, and
    loginctl and `last` outputs captured from the matrix containers
    (synthetic users);
  - df's percent and `-H` sizes (cases taken from coreutils' rounding);
  - procps' uptime forms;
  - GPU names from a synthetic fixture `pci.ids` (written by hand in the
    file's format, with a comment, a subsystem line, a class section, a
    malformed line and a non-UTF-8 byte) and fixture sysfs trees: a
    known vendor and device; an unknown device under a known vendor
    (`<vendor> Device <dddd>`); an unknown vendor (`Device
    <vvvv>:<dddd>`); a device ID that appears only in the class section
    or as a subsystem (not matched); the revision suffix; the BMC
    exclusions; a non-zero PCI domain; ordering by address;
  - the file missing, unreadable (mode 0000 under a non-root test, or a
    directory at the path) and over the cap: no error line, every
    device in the unknown-ID forms (`Device <vvvv>:<dddd>`, and
    `<vendor> Device <dddd>` cannot occur without the file), the run
    log naming the cause;
  - every provider's counting from JSON and XML fixtures (Plex paused,
    every qBittorrent state, an rTorrent fault, i4 and i8);
  - the `--generate-config` golden: sb-go's output on the pinned schema,
    captured once and committed, since it holds only schema examples;
  - the redaction and `--share` filters (IPv4, IPv6 with and without
    brackets and ports, hosts, short and long credentials, encoded
    credentials).
- **The runner, on tokio's paused clock**
  (https://docs.rs/tokio/latest/tokio/time/fn.pause.html):
  - a section that never ends hits the 30 s cap;
  - an instance timeout is capped by what's left;
  - a panic becomes an error line;
  - the order is fixed whatever the finishing order.
- **HTTP providers on `sb_config::testing::Fake`:**
  - each provider's request (path, method, headers via `seen()`; no
    credential in a URL except SABnzbd's);
  - pagination;
  - qBittorrent's cookie;
  - an error body, a 500, a redirect, an instance that accepts the
    connection and never answers, and a refused connection.
  - The Traefik URL comes from the run's settings, set from
    `SB_MOTD_TRAEFIK_URL` in the tests, as config's `CLOUDFLARE_BASE_URL`
    is.
- **Docker** on sb-docker's fake Engine API: problem rows, counts, the
  `traefik` row, and an unreachable daemon.
- **Children** run as stand-in programs first on `PATH`, as exec's and
  host's tests do: `toilet`, `boxes`, `loginctl`, `last`, `systemctl`.
  update-notifier's absolute paths come from a `Paths` value the tests
  point at a temporary directory. Cases: missing, failing, hanging past
  5 s, and odd output.
- **End to end** (`crates/sb/tests/motd.rs`, the built binary, as root
  with `SB_TEST_ROOT`, `SB_LOG_DIR` and `SB_SALTBOX_DIR` in the test's
  directory):
  - Saltbox's 01-motd flags with stdout piped and `CLICOLOR_FORCE=1`, which
    should colour the output;
  - `--color=never` and `NO_COLOR`;
  - no motd.yml, an invalid one, and an unloadable schema;
  - `--share`;
  - `--generate-config`;
  - the error exits.
- **Secrets:** a motd.yml whose every credential is a distinct canary
  (one short, one long, one with characters that percent-encode). The fakes
  echo the request (URL, headers) in error bodies and redirect to URLs
  carrying the canary. SABnzbd's instance points at a refused port. The test
  asserts that no canary, raw or encoded, is in stdout, stderr or the run
  log (CONSTRAINTS.md "Logging" row).
- **Container matrix** (by hand at each checkpoint, as the journal matrix
  is). It runs on `ubuntu:22.04`, `24.04` and `26.04` booted with systemd,
  with a user logged in over SSH twice (once still logged in, once logged
  out) and a tmpfs, a bind mount and a loop filesystem mounted. It
  compares sb's sections with the tools sb-go ran:
  - `lsb_release -ds`/`-cs`, `uname -r`, `uptime -p`;
  - `df -H …` (the same set of mount points, percent and size);
  - `lspci` names, with pciutils installed in the container: sb's GPU
    lines equal lspci's for the same devices, from the container's own
    pci.ids;
  - `who` against logind's count, and the parsing of `last`.
  - The results go to `docs/platform-notes/motd-sources.md` with the
    versions and method, and no host details. A form that differs between
    releases is pinned per release in the unit tests.
- **Timing** (Success criteria 9 to 11): hyperfine on the production build
  against the fixtures, beside sb-go on the same fixtures, both recorded
  in the plan's checkpoint.
- **Gates:** CONSTRAINTS.md as always (changed-line coverage ≥ 90%,
  mutation ≥ 80% on changed code, the ratchets).

## Boundaries

- **Always:**
  - read only: motd changes nothing on the host but the run log;
  - run sb as root only with `SB_LOG_DIR` in `target/` or a
    temporary directory;
  - use synthetic fixtures;
  - send credentials in headers where the API allows;
  - send remote text through `DisplayText`.
- **Ask first:**
  - dependencies beyond the Tech Stack table;
  - any difference from sb-go not listed above;
  - changing a timeout or the 30 s cap (decision 26);
  - a JSON output.
- **Never:**
  - a shell for any child;
  - a credential in argv, a URL other than SABnzbd's, stdout, stderr or
    the log;
  - a write to any service (qBittorrent's login session is the only
    state a request creates, as in sb-go);
  - edits to Saltbox's repository or `01-motd`;
  - host data in a commit.

## Success Criteria

1. **Flags.** The contract test passes against sb-go's motd flags, and
   `-v` is a parse error, exit 2, as in validate-config and docker. Selection, type,
   font and banner-file errors print sb-go's texts (fonts sorted) and
   exit 1.
2. **Layout.** Snapshot tests show sb-go's layout: order, padding,
   continuation lines, hidden empty values, the trailing blank line. A
   manual run of sb-go against the same fakes and fixtures shows only the
   listed differences.
3. **System sections.** Each has unit tests from fixtures. The container
   matrix shows the same text as sb-go's tools on 22.04, 24.04 and 26.04,
   apart from difference 9, recorded in the platform note; GPU names
   equal lspci's from the same pci.ids.
4. **Providers.** Every provider has `Fake` tests for its request shape,
   its counting and its errors. rTorrent makes exactly three calls after
   the check. Every qBittorrent state is counted.
5. **motd.yml.**
   - A valid file drives the sections and colours.
   - An invalid file (a pattern miss, a bare list, an unknown key) prints
     the one `Config:` line with the right count. The system sections
     still show, and the HTTP sections don't.
   - An unloadable schema prints the line in its own words.
   - The file is opened once (a test counts it through the run log).
   - `--generate-config` is byte-identical to sb-go's golden.
6. **Secrets.** No canary credential, raw or percent-encoded, appears in
   stdout, stderr or the run log, in any of the secret test's cases.
7. **`--share`.** No IPv4 or IPv6 address, and no configured or error host
   name, is left in any line of a run whose fixtures put them in last
   login, provider errors and Traefik errors.
8. **Colour.** With stdout piped and `CLICOLOR_FORCE=1`, the output is
   coloured. `--color=never` and `NO_COLOR` give none. motd.yml's colours
   appear as truecolor (converted under `SB_COLOR_PROFILE`). An absent key
   takes sb's palette.
9. **Login budget, fast path.** With every source answering at once
   (fakes, stand-ins, the Saltbox login flags, a motd.yml with one instance
   per provider), `sb motd`'s median wall time is ≤ 500 ms, and not above
   sb-go's median on the same fixtures and machine. Both are recorded.
10. **Login budget, slow sources.** A refused connection adds nothing
    measurable. A source that accepts and never answers adds no more than
    its timeout plus 250 ms (1.25 s with the default). The sections are
    independent: one hung instance delays no other section's result.
11. **Login budget, worst case.** With every source hung (fakes that never
    answer, stand-ins that sleep forever), the run ends within 30.5 s,
    prints every section's line, and leaves no child behind (exec's group
    kill). The banner's children add no time beyond their 5 s cap, because
    they run during collection.
12. **Logging.** The run log has the sections chosen and each section's
    and instance's elapsed time and outcome. Its errors are already
    redacted, and so is the issue list of an invalid motd.yml, with its
    positions.
13. **Size.** The production binary is measured with procfs and the GPU
    parser linked, within the ratchet's tolerance or with an ADR. `sb
    --help` and `sb version` stay within their budgets.
14. **Library first.** The `library-reviewer` has run on the disk bar, the
    children and the HTTP clients, and each REPLACE finding is fixed or
    declined with its reason in the commit.

## Decisions

1. **Banner: toilet and boxes stay as children** (maintainer's answer
   E-Q1, 2026-11-13). The figlet-rs test results are recorded in the
   research: all 21 `.tlf` fonts were byte-identical; 11 of 18 `.flf` were
   identical with the RTL override; three failed and three differed.
   Revisit if boxes gains a library.
2. **GPU names: device IDs from sysfs, names from the host's
   `/usr/share/misc/pci.ids` through a small parser of sb's own,** not
   the pci-ids crate and not an lspci child (the maintainer's answer of
   2026-11-13, given after the first E-Q8 answer's pci-ids measured
   +3,432,280 bytes). Reasons: pure Rust, no child process, almost no
   binary growth, and names that match lspci on that host, because lspci
   reads the same file. The parser is domain logic: no maintained crate
   parses the host's file at run time without embedding it (Tech Stack,
   "Considered and not used"). Differences 21 and 22.
3. **An invalid motd.yml prints one line and skips only the config
   sections** (decided.md E-Q2).
   - An unreadable file or an unloadable schema is treated the same,
     with its own wording.
   - Services keeps its default filters and drops only motd.yml's
     additions, because the section exists without the file.
4. **The legacy bare-list form is dropped** (decided.md E-Q3).
5. **motd.yml's colours are honoured over sb's palette** (decided.md
   E-Q4).
   - Nine slots: text.label/value/app_name, status.warning/success/error,
     and progress_bar.low/high/critical.
   - An absent key takes the role sb's palette gives that slot: Accent,
     Text, Accent2, Warn, Ok, Problem, Ok, Warn, Problem.
   - Each colour is resolved for the colour profile with cli-core's
     `Profile::resolve`. `#abc` means `#aabbcc`.
6. **Sessions come from logind's JSON and last login from `last`, each
   checked in 22.04, 24.04 and 26.04 containers** (decided.md E-Q5).
   - The arguments (for example `-w`, `--time-format iso`) and the
     parsing are fixed per release by that check. Until then the parsing
     is marked UNVERIFIED.
   - The first entry that isn't a reboot, shutdown or wtmp line is shown,
     as in sb-go.
7. **`--share` masks IPs and hosts everywhere** (decided.md E-Q6, with
   the maintainer's brief of 2026-11-13).
   - IPs read `xxx.xxx.xxx.xxx` and `xxxx:xxxx:xxxx:xxxx` (sb-go's
     forms), and host names read `[HOST]`.
   - It is one filter over the rendered values, and it doesn't touch the
     run log.
8. **Disk bars are chosen under Library first** (decided.md E-Q7). The
   choice is a term-ui helper, `bar(theme, role, on, ratio, width)`:
   `paint` over `█` repeated `round(width × ratio)` times and `░` for the
   rest, in the Faint role (bubbles' `ViewAs` rounding). The checks:
   - **indicatif** renders a bar only through a draw target
     (`ProgressStyle::format_state` is `pub(crate)`, indicatif-0.18.6
     `src/style.rs:214`), and its stdout target draws nothing when stdout
     isn't a terminal (`src/draw_target.rs:80`), which is the pam_motd
     case.
   - **ratatui** isn't a term-ui dependency. Its inline viewport asks the
     terminal for the cursor position
     (ratatui-core-0.1.2 `src/terminal/inline.rs:396`), which a pipe can't
     answer. Rendering a `LineGauge` into a `Buffer` and turning cells
     into SGR text would be our own rendering code.
   - A "why not" comment with these URLs goes in the code, and the
     library-reviewer runs on it.
9. **Flags as sb-go's** (capability map decision 1), **without `-v`.**
   term-ui decision 9 drops verbose output, and the run log has the
   detail; `-v` is dropped with a parse error, exit 2, as in
   validate-config (config decision 1), docker (docker decision 6), `sb
   list` and every other command. decided.md G-Q1's hidden `-v` is for a
   documented caller that passes it (the served installer's `sb setup
   -v`); Saltbox's `01-motd`, the only documented caller of `sb motd`,
   passes none (assumption 2). Reconciled at integration and settled
   (2026-11-13): `-v` is a parse error, like every
   command but `install` (counted) and `setup` (hidden).
10. **Thin reqwest clients for every provider** (capability map policy 3).
    rTorrent's XML-RPC uses quick-xml (stack table). Not starr,
    jellyfin-go, qbit-rs or dxr.
11. **Parallel collection on tokio** (capability map decision 8), with
    sb-go's timeouts as the reference: 30 s per section, 1 s per HTTP
    request, 20 s for the qBittorrent and rTorrent data after a 1 s check,
    5 s per child, and 3 s to connect to Traefik.
12. **Secrets never reach output or the log** (term-ui decision 8,
    capability map decision 11, CONSTRAINTS.md "Logging"). This covers
    token, apikey and password, and SABnzbd's query key; the mechanism is
    under "Secrets".
13. **Docker through sb-docker's `connect::list` and `ps::status`.** The
    docker spec's Scope says that "`ps`'s status function is the module's
    one container classification", with exit 0 as Idle (maintainer,
    docker spec). The count texts stay sb-go's (difference 10).
14. **Services through `sb_host::services(filters)`** (host spec
    decisions 1 and 4: the texts motd shows stay sb-go's).
15. **Traefik through reqwest and the bollard listing,** not `curl` and
    the docker CLI. This follows the pure-Rust preference recorded in host
    decision 3. Router names and errors go through `DisplayText`.
16. **System facts from /proc, sysfs and syscalls** (host decision 3;
    CLAUDE.md "Library first"). procfs is the library for /proc, rustix
    for `uname` and `statvfs`. os-release is read by sb (freedesktop's
    format,
    https://www.freedesktop.org/software/systemd/man/latest/os-release.html)
    because sysinfo exposes neither key. Children remain only where no file
    or call answers: update-notifier's two scripts, `apt list`,
    `loginctl`, `last`, `toilet`, `boxes`, `systemctl` (inside host).
17. **motd.yml is read once** through config's `check_file(settings,
    "motd.yml")`, the one per-file check `sb edit` uses too (cli-core's
    `read_small` at 1 MiB, `sb_config::parse` with ADR-022's acceptance,
    the duplicate keys, the schema rules, no network), and the tree it
    returns is mapped to motd's typed model. The schema is loaded
    strictly (config decision 5).
18. **`--generate-config` reads Saltbox's schema** (config assumption 2:
    Saltbox owns the schemas, nothing embedded), keeping sb-go's order and
    quoting.
19. **Colour under pam_motd needs no code** (capability map decision 10).
    anstream honours the `CLICOLOR_FORCE` that 01-motd exports (term-ui
    `lib.rs:30-42`). `FORCE_COLOR` isn't read.
20. **Root as every command** (cli-core decision 3).
21. **Every run is logged** (capability map decision 11), each login's
    included.
22. **`--banner-file-toilet` is split with shlex** (stack table) and passed
    as argv through exec.
23. **Exit codes are sb-go's:** 1 for the selection, type, font, banner
    file and generate-config errors. A failed section is its own error
    line, and the run exits 0.
24. **Remote and config text goes through `DisplayText`** (term-ui,
    "Escape handling").
25. **The banner runs concurrently with collection.** The source is the
    maintainer's brief of 2026-11-13, which makes the login time budget a
    success criterion. Output order is unchanged.
26. **sb-go's 30 s per-section caps stay, with no run-wide cap;** the run
    log records each section's and instance's time (criterion 12). A
    run-wide cap is revisited only on a measured slow login. Reason:
    nothing a slow but healthy instance shows is lost (a large
    qBittorrent `maindata` is why its data timeout is 20 s), and a
    tighter bound has no measured need. Decided from
    recorded rules, 2026-11-13.

## Known limits

- **The first `last` entry is often the current login** ("still logged
  in"), because sshd records it before pam_motd runs. sb-go behaves the
  same. Which entry wins is pinned by the matrix, not changed.
- **GPU names are as current as the host's pci.ids**, as lspci's are;
  `update-pciids` or a newer `pci.ids` package refreshes both.
- **The 30 s cap is sb-go's.** A source that hangs still delays the login
  by up to 30 s (decision 26).
- **Errors inside host's service list** are shown through motd's
  redaction, but systemctl's own texts aren't secret-bearing.

## Open questions

None. The two raised in research were answered on 2026-11-13: the login time cap
(decision 26, from recorded rules) and the GPU names (decision 2, the
maintainer's answer).

## Amendments to other specs

1. **host spec, "systemd services"** (`docs/specs/host.md:266-305`).
   Wording, added after "**Failures.**" (applied 2026-11-13):

   > **Cancellation and time.** `services(filters, token)` takes the
   > caller's `CancellationToken`, which reaches every capture, so motd's
   > section cap and Ctrl-C end it. `list-units`, `list-unit-files` and
   > `list-timers` run concurrently, then `show` runs on the matched
   > units, so the call takes at most two timeouts (20 s), not four
   > (40 s), inside motd's 30 s cap. The `show` call reads both
   > `ActiveEnterTimestamp` and `StateChangeTimestamp` (the tui spec's
   > amendment to step 4). motd and tui are its consumers (decision 1);
   > this part of the host plan is built before whichever of motd's
   > Services slice and tui's first slice comes first.

   And in "**Matching.**", after the `with_additional` sentence:

   > A configured name that matches no loaded unit or unit file is left
   > out, as sb-go leaves out `not-found` units.

2. **config spec**: the one per-file check, `check_file(settings, file)
   -> Result<Checked, FileError>` (no network; the `Report` and the
   parsed tree), with its step form `run::validate_file(step, settings,
   file, checks)` for `sb edit` and validate-config. The wording is in
   the tui spec's config amendment; motd uses `check_file` for motd.yml
   (decision 17).

3. **term-ui spec**: the bar helper. Wording, after `paint` (applied
   2026-11-13 beside the adapters, where `paint` is described):

   > **`bar(theme, role, on, ratio, width)`:** a one-line usage bar,
   > `█` for the filled part (`round(width × ratio)` cells) in `role` and
   > `░` for the rest in Faint, through `paint`. Its first user is motd's
   > Disk Usage (motd decision 8, with the "why not indicatif or ratatui"
   > evidence).

4. **Capability map**:
   - The `motd` row: "System facts (/proc, sysfs, syscalls), packages,
     logins, disks, services, containers and Traefik, plus media-server and
     downloader providers (thin reqwest clients), collected in parallel
     under the login's time budget (motd spec)". Depends on `cli-core`,
     `term-ui`, `exec`, `host`, `config`, `docker`.
   - The stack table gains a row for `procfs` 0.18.0 (backed by the motd
     spec). The quick-xml row's "Backed by" becomes
     the motd spec.
   - The "Commands" row for `motd`: the planning note becomes "spec
     approved; `-v` dropped (motd decision 9)".
   - deny.toml as in Project Structure.

## Suggested slices for the plan

1. **Vertical slice (M):** `sb motd --distro --kernel --uptime --cpu
   --memory --processes --cpu-info --reboot`.
   - clap flags (all 32), the selection and `--all` rules and their
     texts;
   - the runner (cap, panics, order), the layout writer, the run log;
   - the procfs, rustix and os-release sections with fixture tests;
   - colour with `CLICOLOR_FORCE` piped;
   - the fast-path timing baseline beside sb-go.
2. **motd.yml (M):** config's `check_file` amendment, the typed model, the
   `Config:` line, colours, `--generate-config` and its golden.
3. **Disk (S):** mountinfo, statvfs, df's rules, term-ui's `bar` (with the
   library-reviewer).
4. **Packages and GPU (S):** update-notifier sources and children; sysfs
   plus the host-pci.ids parser with its fixture tests.
5. **Logins (M):** the container matrix first (logind JSON and `last` on
   three releases, the platform note), then sessions and last login.
6. **Docker and Traefik (S):** sb-docker's listing, the Traefik client.
7. **Services (S):** after host's `services` slice (amendment 1) lands.
8. **HTTP providers (M, then M):**
   - first, the shared client, the redaction helper and the secret canary
     test, with Plex, Emby, Jellyfin, SABnzbd and NZBGet;
   - then the Arr queues, qBittorrent and rTorrent.
   - The security-auditor runs on this slice (untrusted input, secrets).
9. **Banner (S):** toilet and boxes children, fallbacks, type and font
   checks.
10. **`--share` and the budgets (S):** the filter, then criteria 9 to 11
    measured on the production build. The checkpoint records the numbers.

Each slice is visible through `sb motd` flags, so the maintainer can try
it with the sb binary.
