# Spec: `host` (approved 2026-11-08)

Module of the approved capability map (`host`): what sb needs to know about,
and change on, the machine it runs on. That covers the setup checks, disk
space, apt, file ownership and the systemd service list. The build order
puts `host` next (step 2, ahead of `repo` and `release`). The crate already
holds the supported-release check every command runs first (cli-core spec,
startup step 1). This spec covers the rest of the module, its shape and
its tests.

The behaviour comes from sb-go 35a7acc (2026-10-28), package `host/`
(apt.go, ownership.go, systemd.go, system.go, ubuntu.go). Two pieces are
left out: benchmark.go and the journal log stream (Scope).

## Assumptions

1. **All children go through exec** (exec spec). host builds
   `sb_exec::Cmd` values:
   - read-only queries use `capture`;
   - apt uses `stream` into the caller's term-ui `Step`.

   host draws nothing and spawns no process of its own. It returns errors
   and never prints. exec's decision 4 (maintainer, 2026-11-04) named
   "apt progress" as `pty`'s first user. No apt call here needs a
   terminal: with `DEBIAN_FRONTEND=noninteractive` and a non-tty stdout,
   apt prints plain lines. So exec's decision 4 is amended (decision 7):
   `pty`'s first user moves to a later module.
2. **No `-v` and no verbose flag.** The maintainer chose no `-v` for docker
   (2026-11-06), and the runner's step already shows the output tail and
   logs every line. sb-go's `verbose` parameters become step output and
   run-log records:
   - apt's streamed output;
   - the "installed successfully" lines;
   - the file dumps in `AddAptRepositories`.

   Inside sb-go's `Task.RunStreaming`, executor.go:572-577 already forced
   apt output into the task pane whatever `verbose` said, so this keeps
   sb-go's visible behaviour.
3. **Library calls replace children where the answer is a syscall:**
   - `rustix::system::uname().machine()` replaces `uname -m`. The
     workspace rustix gains its `system` feature.
   - `rustix::fs::statvfs` replaces `unix.Statfs`. Its `fs` feature is
     already on.
   - `std::os::unix::fs::lchown` replaces `os.Lchown`.
   - `walkdir` 2.5 replaces `filepath.WalkDir`, with
     `follow_links(false)` and `follow_root_links(false)`. walkdir follows
     a symlinked root by default. sb-go's WalkDir lstats the root and
     doesn't descend.
4. **The service list's facts come from systemctl's JSON output**
   (proto-logs, maintainer-reviewed 2026-10-29), not sb-go's parsing of
   human-readable columns. Times are read with `--timestamp=unix`
   (`@secs`), which avoids sb-go's locale and time-zone parsing. The
   texts motd shows stay sb-go's (motd/systemd.go). proto-logs' picker status
   rule belongs to the picker in `tui`, not here.
5. **The supported lists stay in host:**
   - the runtime list (22.04, 24.04, 26.04), already here;
   - setup's list (24.04, 26.04).

   Both are sb-go's `layout.go` values.

## Objective

One crate answers the host questions for every later module:

- **setup** (`ops`): its checks.
- **setup and gha** (`ops`): apt, repositories included, and ownership.
- **update** (`ops`): disk space.
- **install** (`ansible`): the LXC and disk-space checks.
- **python-env:** apt and ownership.
- **repo:** recursive ownership after a reset.
- **motd and tui:** the systemd service list.

Every answer carries sb-go's texts, so a user who knows sb-go's errors sees
the same ones. Every child has a timeout or a stated reason for none, and
stops on Ctrl-C through exec.

### Release checks (ubuntu.go, system.go)

- **`check_support()`:** already built. It uses the runtime list and
  sb-go's three error texts.
- **`check_setup_support()`:** uses setup's list. Its error reads
  `UNSUPPORTED OS - Install cancelled: <cause>. Supported OS versions:
  24.04, 26.04`.
- **`ubuntu_release()`:** os-release's `VERSION_ID` through sysinfo, as
  sb-journal's `Release` (journal spec, assumption 2). It serves
  python-env's deadsnakes rule (`22.04` only) and replaces sb-go's
  `ParseOSRelease` caller. A missing `VERSION_ID` gives
  `UbuntuReleaseError::NoVersion`, with `check`'s text; one that names no
  supported release gives `UbuntuReleaseError::Unsupported`, carrying
  sb-journal's `ReleaseError`.

### Setup checks (system.go)

| Check | How | Passes | Errors (sb-go's texts) |
|---|---|---|---|
| `check_architecture` | `uname(2)` machine | `x86_64` | `UNSUPPORTED CPU Architecture - Install cancelled: <arch> is not supported. Supported: x86_64` |
| `check_lxc` | `systemd-detect-virt -c`, capture, 5 s | a non-zero exit with output `none`; or success with any container other than `lxc` (compared lowercased) | `UNSUPPORTED VIRTUALIZATION - Install cancelled: Running in an LXC container is not supported`; otherwise `could not detect virtualization using systemd-detect-virt: <cause>, output: <out>` |
| `check_desktop` | `package_installed("ubuntu-desktop")` | not installed | `UNSUPPORTED DESKTOP INSTALL - Install cancelled: Only Ubuntu Server is supported`; a failed query gives `unexpected error checking for desktop environment: <cause>` |

The output that `check_lxc` matches is stdout and stderr together, as in
sb-go.

sb-go's desktop check used `dpkg -l`. That command also exits 0 for a
package dpkg only remembers (`un`, `rc`). So an Ubuntu Server that once had
the desktop removed is refused. sb-rust asks for the installed state
(difference 2). sb-go's `dpkg command failed with unexpected exit code`
text has no counterpart: dpkg-query's failures arrive as the query error.

### Disk space (system.go)

`check_disk_space(paths)` resolves each path to its nearest existing
ancestor, then checks each filesystem once, de-duplicated by `f_fsid`.

- **Free space:** `f_bavail × f_frsize`. sb-go multiplied by `f_bsize`.
  The two are equal on ext4 and xfs, and `f_frsize` is the POSIX unit
  (difference 10).
- **Used:** sb-go's formula, `(f_blocks − f_bavail) / f_blocks × 100`.
  This is not df's formula, and the message depends on it.
- **Fails when** free space is under 2 GiB, with `INSUFFICIENT DISK SPACE
  - Install cancelled: <path> is <n.n>% full (<size> free). Free up space
  on <path> before continuing`.
- **Sizes** use sb-go's `formatBytes`: `N B`, or `%.1f` in KiB up to EiB.
- **Other errors** nest as sb-go's do: `unable to check disk usage for
  <path>: <cause>`. Zero blocks gives `unable to check disk usage for
  <path>: filesystem stats for <path> reported zero blocks`.
- **The run log** gets each filesystem's total, free and used space, and
  each skipped duplicate. sb-go printed these as debug lines.
- **`disk_space_error(path)`** builds the failure text with 100% and 0
  free, for install's hidden `--force-disk-full`.

### apt (apt.go)

**The policy.** Every apt-get call is `apt-get -o
Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold <args>`.

- **Environment:** `DEBIAN_FRONTEND=noninteractive` and
  `NEEDRESTART_MODE=l` are added to the inherited environment.
- **Output:** streamed into the caller's step.
- **Errors:** exec's, carrying apt's own messages.
- **Lock wait:** `install`, `remove` and `autoremove` also get
  `-o DPkg::Lock::Timeout=240` (decision 3).

**`package_installed(name)`.** Runs `dpkg-query -W -f=${db:Status-Abbrev}
<name>` (capture, 10 s; 5 s for `check_desktop`, sb-go's timeout there).
- True only for `ii`.
- Exit 1 means unknown or not installed: false.
- Any other failure gives `query package <name>: <cause>`.

**`install(step, packages)`.** Runs `install -y <packages>`.
- Error: `failed to install packages '<a, b>': <cause>`.
- No timeout. An install's length depends on the packages and the mirror,
  and Ctrl-C still stops it.

**`remove(step, packages)` and `autoremove(step)`.** Run `remove -y
<packages>` and `autoremove -y`, python-env's deadsnakes cleanup. No
timeout, for the same reason as `install`.

**`update_lists(step)`.** Runs `update` with retries.
- **Attempts:** up to 3, each with a 2 min timeout.
- **Retry when** the attempt timed out, or its stderr (`Streamed`'s
  stderr tail, as sb-go matched `RunVerbose`'s stderr) names one of:
  - `Mirror sync in progress`
  - `File has unexpected size`
  - `Hashes of expected file`
- **Delays:** 5 s, then 10 s.
- **Ctrl-C:** an interrupted attempt is never retried.
- **Texts:** sb-go's.
  - Errors: `failed to update package lists: <cause>` and `failed to
    update package lists: context cancelled during retry: <cause>`.
  - Retry lines: `apt-get update timed out after 2m0s (attempt n/3),
    retrying in 5s...` and `apt-get update failed (attempt n/3), retrying
    in 5s due to transient mirror sync error...`, in Go's duration
    format.
- **Lock:** `update` locks `/var/lib/apt/lists/lock`, which
  `DPkg::Lock::Timeout` doesn't cover. An attempt that fails on apt's
  `Could not get lock /var/lib/apt/lists/lock` text waits 5 s and tries
  again, within the same 240 s budget as the dpkg lock, outside the 3
  mirror attempts (decision 5).

**`add_repositories(step)`.** Used by setup and gha. It reads the codename
from `lsb_release -sc`'s stdout only (capture, 5 s; error `error getting
Ubuntu release codename: <cause>`). Then it applies sb-go's release rules
on a given root:

- **Accepted codenames:** `jammy` (legacy `sources.list`), and `noble` or
  `resolute` (DEB822). Any other codename fails before anything changes:
  `unsupported Ubuntu release: <x>`.
- **Cleanup:** removes every entry in `sources.list.d`. On DEB822
  releases it keeps `ubuntu.sources`. A subdirectory fails to remove, as
  in sb-go (`error removing <p>: <cause>`).
- **jammy:** appends the four archive lines to `sources.list` and rewrites
  it without blank or duplicate lines.
- **DEB822:** writes `ubuntu-archive.sources` (sb-go's exact content), but
  only when `ubuntu.sources` doesn't already name `archive.ubuntu.com` or
  `security.ubuntu.com`.
- **Saving:** files are written through `tempfile` (stack table: atomic
  saves, keeping owner and mode).
  - New files get 0644.
  - A symlinked `sources.list` is written at its target.
  - The temporary file is created in the same directory under a name apt
    ignores silently (`Dir::Ignore-Files-Silently`; the plan checks the
    pattern against apt's source).
  - sb-go truncated in place (difference 8).

**Waiting for the dpkg lock (decision 3).** sb-go polls
`/var/lib/dpkg/lock-frontend` with `flock`. apt and dpkg hold it with an
`fcntl` record lock, and on Linux the two kinds don't see each other. So
sb-go's wait goes straight on, and apt-get then fails if
unattended-upgrades holds the lock.

`DPkg::Lock::Timeout=<seconds>` makes apt retry the frontend lock once a
second, printing `Waiting for cache lock: <holder>`. Source, apt 2.4.13
and main:
- `GetLockMaybeWait` in apt-pkg/deb/debsystem.cc waits only when the
  timeout is non-zero and a progress object is given.
- `install` and `remove` pass one: `CacheFile::OpenForInstall()` opens
  with an `OpTextProgress` (apt-private/private-cachefile.h).

The 240 s is about sb-go's intended total for its 24 attempts. sb-go's
texts `failed waiting for apt lock`, `context cancelled while waiting for
apt lock` and `timed out waiting for apt lock after 24 attempts` go away.
apt's own `Could not get lock …` / `Unable to acquire the dpkg frontend
lock …` replace them (difference 1).

The plan's first apt task verifies this on a real host and records it in
`docs/platform-notes/apt.md`:
- **Method:** `install -y` of an installed package, as root, under the
  resource cap, against a lock the test holds itself, with `-o
  Debug::Locking=1`.
- **Why not `apt-get -s`:** it sets `Debug::NoLocking` and never waits.
- **Also checked:** whether the waiting line arrives with `\r` updates on
  a non-tty, which `Step` shows at the next line (exec spec).

Until it is verified, the code marks difference 1 UNVERIFIED.

### Ownership (ownership.go)

- **`ensure_owner(user, paths)`:** for each path, `lstat`, then `lchown`
  to the user's uid and primary gid when either differs.
  - A missing path is skipped.
  - An empty or unknown user skips everything, as in sb-go.
  - A symlink itself is changed, never its target.
- **`ensure_owner_recursive(user, paths)`:** the same over a walk
  (assumption 3).
  - A missing root is skipped.
  - A symlinked root changes only the link.
  - An entry that disappears mid-walk is skipped, and the walk goes on.
    sb-go ended the whole walk silently there (difference 11).
- **User lookup:** `nix::unistd::User::from_name` (decision 2), at the
  nix version portable-pty already links (0.28), with its `user` feature.
  This adds no second nix to the binary. glibc's `getpwnam_r` goes
  through NSS (`/etc/nsswitch.conf`, `files` first on Ubuntu), as every
  other program on the host does (ADR-020; musl's read `/etc/passwd` and
  asked nscd only if one ran). sb-go's pure-Go lookup reads `/etc/passwd`
  alone. With `files` first they agree on every user in `/etc/passwd`;
  glibc can also find a user that only systemd-userdb knows, which sb-go
  can't.
- **Errors** nest as sb-go's do:
  - `look up user <u>: <cause>`
  - `inspect ownership of <p>: <cause>`
  - `set ownership of <p>: <cause>`
  - `set recursive ownership of <root>: set ownership of <p>: <cause>`
- **The run log** gets the count of entries changed per root, not each
  path: `/srv/git/saltbox` holds thousands.

### systemd services (systemd.go)

`services(filters)` returns one record per matching service unit. Each
record holds:
- the name;
- the active and sub state;
- the unit-file state;
- when the unit entered its active state;
- its timer, when one exists: the timer's active and sub state, and its
  next run.

**Matching.** sb-go's filters: the prefix `saltbox_managed_` and the exact
name `docker`. `with_additional(names)` adds motd's configured names, with
`.service` stripped. The result is sorted by name, without duplicates, and
`not-found` units are dropped.

**How it is read.** Each call below is a capture with a 10 s timeout:
1. `systemctl list-units --type=service,timer --all --output=json`.
   Timers and their states come from the same call.
2. `systemctl list-unit-files --type=service --output=json`, so unloaded,
   disabled leftovers still appear (proto-logs).
3. `systemctl list-timers --all --output=json`, for next runs.
4. One `systemctl show --timestamp=unix` over the matched units, for
   `ActiveEnterTimestamp`.

**Failures.**
- A failed `list-units` or `list-unit-files` is an error: `failed to list
  systemd services: <cause>`. sb-go named the pattern, one call per
  filter (difference 6).
- A failed `list-timers` or `show` leaves those fields empty, as sb-go
  ignores those errors.

**sb-go's formatting** comes with the facts, for motd:
- **`format_duration`:** `FormatDuration`'s forms, `Ns`, `Nm Ns`, `Nh Nm`
  and `Nd Nh`, with a zero minor unit left out.
- **`format_next_in`:** a run already due reads `soon`; an unknown time
  reads empty.
- **The runtime:** now minus the active-enter time. sb-go computed it from
  the monotonic timestamp and `/proc/uptime`, falling back to the wall
  clock. The unix timestamp gives the same value in one step.

### Differences from sb-go (the complete list)

Pinned tests are named in Success criteria.

1. **apt waits for the dpkg lock itself** (`DPkg::Lock::Timeout`) on
   install, remove and autoremove. sb-go's `flock` poll can't see apt's
   lock, and its three wait texts are gone.
2. **The desktop check asks for the installed state** (`ii`). A removed
   `ubuntu-desktop` no longer blocks setup.
3. **The codename is `lsb_release -sc`'s stdout only.** sb-go mixed in
   stderr.
4. **`uname -m` becomes `uname(2)`:** same answer, no child.
5. **No verbose output.** apt output always streams into the step.
   sb-go's verbose and debug messages go to the run log (assumption 2).
6. **The service list reads systemctl's JSON.** It also shows unloaded,
   disabled leftovers. One listing error replaces sb-go's per-pattern
   text.
7. **Timeouts sb-go doesn't have:** 10 s on `package_installed`'s
   `dpkg-query` for python-env (sb-go's desktop check already had 5 s,
   which stays), 10 s on each `systemctl` query, and 5 s on
   `lsb_release`. A hung call would hang sb-go.
8. **`add_repositories` writes atomically,** keeping owner and mode,
   instead of truncating in place.
9. **The runtime comes from the unix active-enter timestamp,** not the
   monotonic one plus `/proc/uptime`.
10. **Free space uses `f_frsize`,** not `f_bsize`.
11. **An entry that vanishes during a recursive chown is skipped,** and
    the walk continues.
12. **`update_lists` waits for the lists lock** by retrying on apt's lock
    error (decision 5); sb-go failed at once.

Note: sb-go's `UpdatePackageLists` comment says the delays are 5/10/20 s,
but the third delay never runs. sb-rust's docs say 5 s and 10 s, which is
what both do.

### Scope

**In:**
- the release, setup, disk-space, apt, ownership and service-list parts
  above;
- their tests;
- the platform note on apt's lock wait.

**Out, with their owners:**
- **`RunBenchmark`:** `ops` (`sb bench`), its only caller. It needs
  `release`'s verified download, which host can't depend on.
- **`GetSaltboxUser`:** `config`. It reads `accounts.yml`, and Saltbox
  YAML is config's. host takes the user name as an argument.
- **`RelaunchAsRoot`:** `cli-core`. Already built.
- **The journal log stream:** decisions 4 and 6 (the `sb-journal` crate,
  with its own spec).

### Capability map correction

- **`host` row:** "Ubuntu/arch/LXC/desktop checks, disk space, apt,
  systemd service list, file ownership". The journal moves to the
  `sb-journal` crate, which host wraps (decision 6).
- **`ansible` row:** gains `host`, for install's checks.
- **`config` row:** reads the Saltbox user for `ops` and `python-env`.
- **`ops` row:** gains the benchmark.
- **Stack table:**
  - walkdir and nix (`user`) get rows;
  - rustix's row gains `system`.
- **deny.toml:**
  - add `sb-host` to the wrappers of `sb-exec`, `sb-term-ui` and
    `sb-cli-core` (the run log);
  - `sb-host`'s own entry allows `sb` and the later modules as each
    arrives.
- **proto-logs FINDINGS "Map impact":** put the `journalctl -f` stream in
  host. Decision 4 overrides that.
- **exec spec, decision 4:** "apt progress" is no longer `pty`'s first
  user (assumption 1); the first module that runs a child needing a
  terminal takes that role. Confirmed (decision 7); exec.md decision 4
  carries the amendment.
- **`sb-journal` (decision 6), with the crate's spec:**
  - a module row or a note under `host`;
  - a stack-table row for the reader;
  - the rejected list's "replaced by `journalctl -f`" corrected;
  - a deny.toml entry allowing `sb-host`, plus sb-journal's own name
    for its self dev-dependency, which cargo-deny counts as a
    parent (the sb-journal plan, "Code with no user yet");
  - the ADR for the library-first divergence.

## Tech Stack

- **exec** (`Cmd`, `capture`, `stream`) and **term-ui's `Step`**
- **sysinfo 0.40** (`system`): os-release, already here
- **rustix 1.1:**
  - `system::uname`, through the new `system` feature;
  - `fs::statvfs`;
  - `event::poll` and `event::eventfd`, through the `event` feature
    (sb-host's own; the workspace entry is unchanged): the follow's wake
    wait, `journal::FollowWait` (journal spec, decision 12).
- **walkdir 2.5:** new to the binary, already in the lockfile
- **nix 0.28,** `user`: already linked through portable-pty; gains the
  feature. Also the systemd-coredump user's uid for the journal's unit
  query (`journal::coredump_uid`, `look up user <u>: <cause>`)
- **tokio 1.54** (the workspace entry; host uses `rt`): `spawn_blocking`
  for the journal's blocking reads, `journal::page` and `journal::drain`
  (journal spec, assumption 3 and API: a correction to this list), and
  for the wake wait's `poll`, `journal::FollowWait`. Not `AsyncFd`, whose
  constructors 1.54.1 deprecates as unsound (journal spec, decision 12)
- **serde and serde_json:** systemctl's JSON
- **jiff 0.2:** times
- **tempfile 3:** atomic saves
- **thiserror:** errors, one enum per part, with sb-go's texts
- **tokio-util's `CancellationToken`:** through exec

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-host
Live:    SB_LIVE_HOST=1 SB_LOG_DIR=<scratch> systemd-run --scope -p MemoryMax=1G -p CPUQuota=100% cargo test -p sb-host --test live -- --test-threads=1
```

The live test is opt-in. It is read-only, except the lock check in Testing.

## Project Structure

```
crates/host/src/lib.rs          re-exports; the release checks (existing)
crates/host/src/setup.rs        architecture, LXC, desktop
crates/host/src/disk.rs         disk space
crates/host/src/apt.rs          policy, installed, install/remove, update
crates/host/src/apt/sources.rs  the repository rules on a given root
crates/host/src/owner.rs        ownership
crates/host/src/systemd.rs      service list and sb-go's formatting
crates/host/src/journal.rs      sb-journal re-exported; coredump uid; helpers
crates/host/src/testing.rs      stand-in programs (`testing` feature)
crates/host/tests/journal.rs    the journal helpers on crafted files
crates/host/tests/live.rs       opt-in live test
docs/platform-notes/apt.md      the lock-wait check
```

**Paths.** Program paths (`apt-get`, `dpkg-query`, `systemctl`,
`systemd-detect-virt`, `lsb_release`) and root paths (`/etc/apt`) are
fields with sb-go's defaults. Their setters:
- live behind the `testing` feature (M-TEST-UTIL);
- except where an environment override is specified later, as docker's
  `SB_DOCKER_CONTROLLER_UNIT` was.

**Injected functions.** `statvfs` and `lchown` are injected functions, as
sb-go stubs them (M-MOCKABLE-SYSCALLS).

## Code Style

As docker's control module:
- the rules are pure functions over parsed values, tested first;
- the query functions build a `Cmd` and parse its capture;
- no `#[allow]`;
- output only through steps and the run log.

```rust
/// sb-go's desktop rule on dpkg's abbreviated status: only an installed
/// `ubuntu-desktop` (`ii`) refuses setup.
fn installed(status: &str) -> bool {
    status.trim() == "ii"
}
```

## Testing Strategy

- **Unit (most):**
  - **Checks:** the LXC answers (including uppercase and a non-1 exit),
    the desktop states (`ii`, `rc`, unknown, a query failure).
  - **Disk:** thresholds, the used formula, fsid de-duplication over a
    stubbed `statvfs`, and the `formatBytes` boundaries.
  - **apt:** the policy's argv and environment for each operation, and
    the retry decision for each mirror text, a timeout and an
    interruption.
  - **Repositories, on temp trees:** sb-go's four `apt_test.go` cases;
    unchanged on refusal; a symlinked `sources.list`; mode and owner
    kept.
  - **systemctl parsing:** JSON fixtures from systemd 249 (22.04), 255
    (24.04) and 26.04's systemd. Also `format_duration`,
    `format_next_in` and the matching rules.
  - **Ownership on a temp tree** through an injected lchown: an unknown
    user skips; a symlink is changed and its target isn't; a symlinked
    root changes only the link; a vanished entry is skipped.
- **Stand-in programs:** shell scripts written to a temp directory, under
  docker's ETXTBSY guard.
  - an apt-get exiting 23 with stderr;
  - one printing a mirror error twice, then succeeding;
  - one that never ends, under a shortened attempt timeout;
  - a dpkg-query exiting 1 and 2;
  - Ctrl-C during apt, ending the child within exec's grace.

  No test runs the real apt-get against the developer's host. sb-go's
  `TestInstallPackage_*` tests do.
- **Live (opt-in, as root, capped):**
  - the setup checks;
  - disk space on `/`;
  - the service list;
  - the lock wait, by the method under apt.
- **Bars** (CONSTRAINTS.md):
  - changed lines ≥90%;
  - mutation ≥80%;
  - project coverage within the ratchet.

## Boundaries

- **Always:** sb-go's texts; exec for every child; a timeout or a stated
  reason for none; the step for output.
- **Ask first:**
  - any new dependency beyond decision 2;
  - any change to apt's policy beyond the lock timeout;
  - anything touching `/etc/apt` outside `add_repositories`.
- **Never:**
  - change packages or ownership on the developer's host in a test;
  - read or touch `/srv/git/saltbox` in a test;
  - put host details in the platform note (method and versions only).

## Success Criteria

1. Every function returns sb-go's result and text on the inputs from
   sb-go's tests, and on the cases under Testing.
2. Differences 2, 3, 5–8 and 10–12 are pinned by unit or stand-in tests.
   Difference 1 is pinned by the argv test and the platform note.
   Difference 4 by a unit test on the parsed machine. Difference 9 by the
   runtime test.
3. apt's lock wait is verified on a real host and recorded in
   `docs/platform-notes/apt.md`.
4. No child outlives Ctrl-C by more than exec's grace plus 1 s.
5. `scripts/check.sh full` passes. Binary size and startup stay within
   the ratchet: host's new code reaches the binary only when a command
   uses it.
6. The capability map, stack table and deny.toml carry the corrections
   above.

## Decisions (maintainer, 2026-11-08)

1. **First users:** every part is built now, as a library with
   stand-in-program tests, as exec was. Each part is wired by its first
   consumer:
   - ownership: `repo`;
   - apt: `python-env`;
   - the checks: setup and install;
   - the service list: `motd` and `tui`.
2. **Dependencies:** nix (`user`) for the uid/gid lookup, and walkdir.
3. **apt's lock wait:** apt's own `DPkg::Lock::Timeout`, not a poll in sb.
4. **The journal log stream:** sb reads the journal files itself with a
   small pure-Rust reader, not a `journalctl` child and not sdjournal
   (bus factor 1, RustSec advisory through ruzstd, looser unit match,
   mmap without a SIGBUS handler). sb refuses unsupported releases, so the
   reader supports exactly the formats and `-u` rules of the supported
   ones, extended with each new LTS. It is proven first in
   `prototypes/proto-journal` (tasks/prototypes Task 11; removed in the
   sb-journal plan's Task 12, its findings in
   [docs/prototypes/proto-journal.md](../prototypes/proto-journal.md)) on
   22.04, 24.04 and 26.04 containers; its spec section follows the
   prototype's review.
   *Amended by decision 6:* the reader's spec comes with the
   `sb-journal` crate.
5. **The lists lock:** `DPkg::Lock::Timeout` doesn't reach `apt-get
   update`, which takes `/var/lib/apt/lists/lock` with no wait
   (apt-pkg/update.cc, apt-pkg/acquire.cc at apt 2.4.13). `update_lists`
   retries on apt's own lock error, every 5 s up to 240 s; no lock code of
   sb's own. sb-go had the same gap (difference 12).
6. **The journal reader's verdict and home:** proto-journal is adopted
   (FINDINGS.md, "adopt with changes"). The reader becomes its own crate,
   `sb-journal`, which sb-host wraps for the service logs; its spec comes
   with that crate. Before the crate, several agents in parallel look for
   performance, reliability and feature improvements relevant to Saltbox,
   and their results go back into the prototype's findings.
7. **The exec amendment is confirmed:** "apt progress" is no longer
   `pty`'s first user (assumption 1). The first module that runs a child
   needing a terminal takes that role.
8. **The spec is approved** as written above.
