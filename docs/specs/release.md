# Spec: `release` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task. *Amended 2026-11-18 with the maintainer's release decisions:
the repository, versions, channels and CI ("Release channels").*

Module of the approved capability map (`release`), build order step 2
(`host` → `repo`, `release`). It looks up GitHub releases (through the
SVM proxy, then GitHub), downloads a release asset with its size and
digest verified, activates it atomically, and on top of that updates sb
itself (`sb self-update`, and the first step of `sb update`). python-env
(build step 3) downloads uv and saltbox.fact through it, and ops' bench
the pinned Speedtest. It builds term-ui's prompts, whose first user it is
(term-ui decision 1).

## Assumptions

1. **sb-go's release and selfupdate packages are the reference**
   (release/asset.go, release/retry.go, selfupdate/update.go,
   cmd/selfUpdate.go), with the decisions below.
2. **Releases come from `saltyorg/sb-rust`** (maintainer, 2026-11-18,
   settling D-Q3), not saltyorg/sb-go. sb's binary names it in one
   constant, and install.sh names the same. sb-go later gets a final
   release whose self-update fetches sb-rust's newest release, once
   sb-rust is declared production-ready: that is sb-go's change, not
   this repository's.
3. **GitHub's per-asset `digest`** (`sha256:<hex>`, published since
   2025-06-03:
   https://github.blog/changelog/2025-06-03-releases-now-expose-digests-for-release-assets/)
   is on every asset sb downloads: sb-go's, uv's and ansible-facts'
   releases carry it (checked 2026-11-13).
4. **The SVM proxy** (`https://svm.saltbox.dev/version?url=<GitHub API
   URL>`) returns GitHub's JSON unchanged; it exists to spare GitHub's
   unauthenticated limit of 60 requests per hour per IP
   (https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api).
5. **sb runs as root** for `self-update` (cli-core decision 3), so it can
   replace its own file (`/usr/local/bin/sb` from install.sh).

## Objective

A download sb installs is the published file of the release it asked for,
or nothing changes. Slow links finish instead of timing out, a dead proxy
costs one warning, and an sb update never leaves a broken or foreign
binary in place.

### Release lookup

`Releases` holds one reqwest client and three bases: the proxy, the
GitHub API (`https://api.github.com`) and the download host
(`https://github.com`). Production uses those; tests pass loopback ones.

- **Queries:** `list(repo)` (`/repos/<o>/<r>/releases`, the first page:
  newest first, which is all self-update needs), `latest(repo)`
  (`/releases/latest`) and `tag(repo, tag)` (`/releases/tags/<tag>`).
- **Proxy first:** `GET <proxy>?url=<percent-encoded API URL>`. On any
  failure, or an answer the caller's check calls unusable, sb asks
  GitHub directly, once, with sb-go's lines as notices on the step:
  warning `SVM proxy <reason>; trying GitHub directly`, then info `GitHub
  fallback succeeded` (6189669, eabf6a2); each at most once per run. Both
  failing: `<proxy error>; fallback GitHub API request failed: <error>`.
- **Usable:** the caller names the asset it needs; the answer is usable
  when the release (for `list`, the newest stable one) has that asset and
  it passes the asset checks below (selfupdate/update.go:132-162,
  generalised to uv and the fact).
- **Request:** `User-Agent: sb/<version>` (GitHub refuses requests
  without one:
  https://docs.github.com/en/rest/using-the-rest-api/getting-started-with-the-rest-api#user-agent;
  proto-musl saw reqwest send none and get 403). Nothing host-specific.
- **Response:** 200 only; the body read chunk by chunk up to 4 MiB (more
  is `returned invalid JSON`); an error body up to 64 KiB, shown through
  term-ui's `DisplayText` and cut. JSON through serde; unknown fields
  ignored.
- **Proxy settings:** reqwest's system proxy variables in production, as
  sb-config's client (crates/config/src/http.rs); none in tests.

### Asset checks (before any download)

From release/asset.go:42-69, plus decided.md D-Q2:

- a non-empty name; `size > 0`;
- `digest` exactly `sha256:` and 64 hex digits, else `release asset
  "<n>" is missing a SHA-256 digest` / `has an invalid SHA-256 digest`;
- **`browser_download_url` pinned:** scheme `https`, host `github.com`,
  no port, user info, query or fragment, and the path exactly
  `/<owner>/<repo>/releases/download/<tag>/<name>` for the repository,
  tag and asset asked for (owner and repo compared ASCII
  case-insensitively, as GitHub treats them). Else `release asset "<n>"
  is not served from https://github.com/<owner>/<repo>/releases/download/`.
  For sb and the fact that is `https://github.com/saltyorg/<repo>/...`;
  for uv the same rule names astral-sh/uv (decision 3).

### Download

- **Client:** `connect_timeout` 10 s and a per-read `read_timeout` 30 s,
  no total timeout
  (https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.read_timeout:
  "applies to each read operation, and resets after a successful read");
  `https_only(true)`, so a redirect to plain http fails
  (https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.https_only);
  redirects otherwise followed (github.com redirects to its asset host).
  Cancellation: the step's token. The tokio runtime is shut down with
  `shutdown_background` at the end of the run, as validate-config's
  (75d5609), so a hung DNS lookup doesn't hold the exit.
- **Staging:** `tempfile::Builder` with the caller's prefix in the
  caller's directory (self-update: the executable's directory,
  `.sb-update-`), so the final rename stays on one filesystem.
- **Verification while streaming** (`Response::chunk`): SHA-256 with
  `sha2`, a running size; past the asset's size or the caller's cap
  (128 MiB for sb, as sb-go) it stops (`larger than expected`, not
  retried); short at the end is retried; then the digest must equal the
  asset's (`digest mismatch`, not retried); then `sync_all`, and the
  file is closed (`NamedTempFile::into_temp_path`), so it can be run
  (an open write handle gives ETXTBSY).
- **Retries** (release/retry.go, f15b4c9): up to 4 attempts on a network
  error, 403, 429, 5xx or a short body; waits 1, 2, 4 s, or
  `Retry-After` (seconds or an HTTP date), or `X-RateLimit-Reset` when
  `X-RateLimit-Remaining` is `0`; a wait over 60 s gives up with the
  reason. Each attempt stages a fresh file. The same policy covers the
  metadata requests (decision 7).
- **Result:** a `Staged` file: verified, closed, deleted on drop.

### Activation

`activate(staged, target, mode)`: set the mode, `TempPath::persist` over
the target (a rename), then `sync_all` on the target's directory
(release/asset.go:219-238). Dropped before that, the staged file is
removed. Shared with python-env (uv, uvx, the fact).

### ELF check

`check_elf(path, arch)` reads the file's header with `object`'s ELF
reader: 64-bit, little-endian, `e_machine` `EM_X86_64` for amd64 or
`EM_AARCH64` for arm64, type `ET_EXEC` or `ET_DYN`. It returns
`ElfError::NotElf(cause)` or `ElfError::WrongMachine(arch)`, and each
caller words them with its sb-go text: self-update `executable is not a
64-bit <x86|arm64> ELF binary` (asset.go:36), the fact `file is not a
valid ELF binary: <cause>` and `file is not an <x86-64|aarch64> ELF
binary` (saltbox/fact.go:78-82). sb-go checks amd64 only
(asset.go:29-39). Shared with python-env's fact check (decided.md C-3).

### `sb self-update` (hidden)

Flags: `-y/--yes`; `--channel beta|release` (2026-11-18, "Release
channels"); `--force-update` only in builds where self-update is off
(cmd/selfUpdate.go:51-53). No `-v` (decision 13).

1. **Off:** a build whose channel is `none` (`SB_CHANNEL` unset or
   `none`), or a raw build (no `SB_VERSION` at build time; decision 11).
   *Amended 2026-11-18: `SB_CHANNEL` replaces `SB_DISABLE_SELF_UPDATE`.* Without
   `--force-update`: warning `Self-update is disabled in this build`,
   info `Use --force-update to override this restriction`, exit 0 (the
   text sb-go's CI greps, build.yml:108-113). With it: info `Forcing
   self-update despite build configuration`, then on.
2. **Look up:** step "Checking for sb CLI updates": `list` for the
   configured repository; drafts skipped, and prereleases for the
   `release` channel ("Release channels"); tags with an optional `v`
   parsed with `semver`, others skipped; the newest kept.
   Not newer than the running version: info `Current binary is the latest
   version: <v>`, exit 0. The asset is `sb_linux_amd64` or
   `sb_linux_arm64` by `std::env::consts::ARCH` (decision 9); missing:
   `release <tag> is missing <asset>`.
3. **Ask:** info `New sb CLI version available: <new> (current: <cur>)`,
   then term-ui's confirm `Do you want to update` (default no).
   `--yes` answers it. No terminal and no `--yes`: an error naming
   `--yes`, exit 1. Declined: warning `Update of sb CLI cancelled`,
   exit 0.
4. **Update:** step "Updating sb CLI to <new>", sub-steps:
   "Downloading <asset>" (the download above, into the directory of
   `std::env::current_exe()`, which reads `/proc/self/exe`); "Checking
   the new binary": `check_elf` for this arch, then the staged file
   (mode 0755) runs `version --json` through exec `capture` with a 10 s
   timeout and an empty environment, and its `version` must equal the
   tag without `v` (decided.md D-Q5), else `binary update failed
   validation: <reason>` (the reason names what it printed, cut);
   "Installing": `activate` over the executable. Errors keep sb-go's
   wrapping: `check for updates: ...`, `binary update failed: ...`.
5. **Done:** info `Successfully updated sb CLI to version: <new>`, then
   the caller's optional message as a warning. No completion refresh
   (sb-go removed it, 32a2a3a: the old process can't describe the new
   tree).

**Inside `sb update`** the same code runs with the answer given (sb-go's
`autoUpdate=true`), and the off case only warns and goes on; ops' update
section says what follows.

### Release channels

The maintainer's decisions of 2026-11-18.

- **Versions** start at `1.0.0`, with `1.0.0-rc.N` prereleases. Tags
  are bare (`1.0.0`, `1.0.0-rc.1`), as sb-go's; `v` stays optional when
  read. The version comes from `SB_VERSION` at build time. A raw build
  reports its package version and is self-update disabled. No code
  compares the version with `0.x`.
- **The channel is baked in at build time,** as sb-go's
  `disableSelfUpdate` is, through `SB_CHANNEL`. It replaces
  `SB_DISABLE_SELF_UPDATE`, which is gone.
  - Its values are `release`, `beta` and `none`. Unset means `none`, so
    local and raw builds have self-update off, as they do today. Any
    other value fails the build, so a typo can't ship a binary that
    quietly never updates.
  - `BuildInfo::channel` holds it. Self-update is off when the channel
    is `none` or the build isn't released (decision 11).
- **What each channel follows:**
  - **`release`:** the newest **stable** release, and its plain asset
    `sb_linux_<arch>`. Stable means GitHub's `prerelease` flag is off
    *and* the tag has no semver prerelease part, so a mislabelled rc is
    invisible too.
  - **`beta`:** the newest release **including prereleases** (drafts
    still skipped), by semver precedence, and its `sb_linux_<arch>_beta`
    asset. Every stable release publishes both assets, built from the
    same code; a prerelease publishes only `_beta` assets. So a beta
    host moves from `1.0.0-rc.2` to `1.0.0`'s `_beta` asset and stays
    beta.
  - **`none`:** self-update off, with today's two lines
    (`Self-update is disabled in this build`, `Use --force-update to
    override this restriction`).
- **Switching:** `sb self-update --channel beta|release`.
  - When the asked channel isn't the baked one, it looks up that
    channel's newest release and installs its asset once, whether or
    not its version is higher. It says `Switching sb CLI to the
    <channel> channel: <new> (current: <cur>, <channel>)`.
  - A lower version is said before the question (warning `<new> is
    older than the running <cur>`), and the question becomes `Do you
    want to switch`.
  - From then on the new binary's own baked channel decides; nothing is
    stored on the host.
  - `--channel` naming the baked channel is a normal update. In a
    `none` or raw build, `--channel` takes effect only with
    `--force-update`; forced without `--channel`, it follows `release`.
- **The staged binary** must report, in its own `version --json`, the
  tag's version (decision 12) and the channel that was asked for.
  Otherwise the swap is refused: `the new binary reports channel <c>,
  not <want>`. A mislabelled asset can't switch a host's channel.
- **`sb update`'s self-update step** follows the baked channel, with no
  switching.
- **Reporting:**
  - `sb version` prints sb-go's line, then `Release channel: <channel>`
    (with ` (self-update disabled)` for `none`).
  - `--json` adds a `channel` field after sb-go's three. `version`,
    `git_commit` and `uv_version` are kept for sb-go's
    update-saltbox-python action.
- **arm64 ships** (maintainer, 2026-11-18): `sb_linux_arm64` and
  `sb_linux_arm64_beta` beside the amd64 assets, chosen by the build's
  architecture (decision 9) with `_beta` for the beta channel.
  - **Locally,** `check.sh full` cross-builds the aarch64 production
    binary and runs its `version --json` under QEMU user-mode
    (`qemu-aarch64-static -L` on 22.04's arm64 glibc, in a throwaway
    amd64 container). That needs no binfmt handler on the host.
  - **The test suite stays CI-only for arm64,** on GitHub's native arm
    runners (`ubuntu-22.04-arm`, `ubuntu-24.04-arm`). Under emulation it
    would run several times slower within the CPU cap, and the
    timing-sensitive PTY and process tests would turn flaky. Native
    runners also run the root and namespace tests as they run on amd64.
- **The install script** installs `release` by default:
  `/releases/latest` and the plain asset. `--beta` installs the `_beta`
  asset of the highest version in the release list, prereleases
  included. It ranks with `sort -V`, after mapping a version's `-` to
  `~`, which GNU sort orders before the release it precedes. Either way
  the installed binary then follows its own baked channel. It accepts
  x86_64 and aarch64, and installs that architecture's asset after the
  ELF check names the matching machine.
- **On an unsupported Ubuntu release** (not another distribution),
  `sb version` and the standalone `sb self-update` run with a warning;
  every other command, `--help` and `sb update` included, still refuses.
  The warning is `Ubuntu <found> isn't a supported release (supported
  versions: <list>); sb self-update may bring a version that supports
  it`. Self-update is how such a host reaches a binary that supports
  its release (cli-core, startup step 1).
- **CI** in saltyorg/sb-rust, written locally only. Creating the
  repository, pushing and running workflows are remote actions the
  maintainer authorises separately.
  - `.github/workflows/ci.yml` runs `check.sh task` on every push and
    pull request on 22.04, 24.04 and 26.04.
  - `.github/workflows/release.yml`, on a version tag:
    - runs `check.sh full` and the by-hand container suites (completion
      shells, the install-script containers, the journal matrix);
    - runs `check.sh task` on the arm runners;
    - builds each asset with cross's production profile, setting
      `SB_VERSION` (the tag), `SB_GIT_COMMIT` and `SB_CHANNEL` per asset
      (stable tags: plain `release` and `_beta` assets for both
      architectures; prerelease tags: `_beta` only);
    - checks each binary's `version --json` (the tag's version, the
      asset's channel, a valid `uv_version`);
    - checks that a build without `SB_VERSION` has self-update disabled,
      mirroring sb-go's build.yml;
    - publishes the release, as a prerelease for rc tags.
  - **Closing findings:**
    - **sb-M4:** `Cross.toml`'s `[build.env] passthrough` names
      `SB_VERSION`, `SB_GIT_COMMIT` and `SB_CHANNEL`, so cross's
      container sees them. A test reads Cross.toml against the variables
      build_info reads.
    - **sb-L8:** `build/Dockerfile`'s base is pinned by digest.

### Differences from sb-go (the complete list)

1. A `User-Agent` on every request (decision 4).
2. Downloads must come from
   `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`
   of the release asked for; sb-go took any https URL the metadata named
   (decided.md D-Q2).
3. Connect and per-read timeouts, no total; sb-go's 30 s total included
   the body, so a 12 MB binary failed below about 400 KB/s (decision 5).
4. Metadata requests are retried like downloads; sb-go's self-update
   metadata wasn't (decision 7).
5. The staged binary must run `version --json` and report the tag before
   it replaces sb (decided.md D-Q5).
6. arm64: `sb_linux_arm64`, the ELF machine checked per architecture;
   sb-go was amd64 only (decision 9).
7. Raw builds count as self-update disabled, and show `--force-update`
   (decision 11). sb-go's raw builds failed at `invalid current version
   ""`.
8. The confirm reads the terminal, not stdin; no terminal without `--yes`
   is an error naming the flag; only the whole line `yes` or `y`
   accepts, in any case with spaces around trimmed, and anything else
   declines (term-ui "Prompts"). *Amended 2026-11-13 with M26.* sb-go read stdin, and EOF was an error.
9. The lookup and the update are runner steps; sb-go printed lines only.
10. No `-v` (decision 13). *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*
11. The proxy's usability check covers uv's and the fact's assets too,
    not only sb's (assumption 4; sb-go checked only sb's list).
12. Release channels (`release`, `beta`, `none`) baked in at build time,
    `--channel` to switch, a `_beta` asset per release; sb-go had a
    stable channel and a disable flag ("Release channels").
13. `sb version` adds a `Release channel:` line and `--json` a `channel`
    field after sb-go's three.
14. The staged binary's `version --json` must also report the channel
    asked for.
15. `version` and the standalone `self-update` run on an unsupported
    Ubuntu release, with a warning; sb-go refused every command there
    (maintainer, 2026-11-18).

### Scope

- **Here:** the lookup, the checks, the download, activation, the ELF
  check, `sb self-update`, term-ui's confirm prompt (term-ui decision 1),
  the API python-env needs (below).
- **Here since 2026-11-18:** the channels, the repository, the local
  CI and release workflows, sb-M4 and sb-L8 ("Release channels").
- **Later:** signed checksums (minisign or sigstore; decided.md D-Q2)
  and sb-M1 (triage DEFER); creating saltyorg/sb-rust, pushing and
  publishing, each authorised separately. The `update` integration is
  ops'.

### The interface python-env needs

```rust
pub struct GithubRepo { pub owner: &'static str, pub name: &'static str }
pub enum Arch { Amd64, Arm64 }
impl Arch {
    /// From `std::env::consts::ARCH`; else `unsupported architecture <a>`.
    pub fn current() -> Result<Self, Error>;
    pub fn go_name(self) -> &'static str;    // "amd64", "arm64" (sb's and the fact's assets)
    pub fn rust_name(self) -> &'static str;  // "x86_64", "aarch64" (uv's asset)
}

impl Releases {
    pub fn production(user_agent: &str) -> Result<Self, Error>;
    /// `testing` feature: the proxy, API and download bases on a fake server.
    pub fn testing(bases: TestBases, user_agent: &str) -> Result<Self, Error>;
    /// The release `query` names, with `asset` checked (proxy, then GitHub,
    /// sb-go's notices on `step`, the retry policy). A release without
    /// the asset: `release <tag> is missing <asset>` (sb-go's text).
    pub async fn release(&self, repo: GithubRepo, query: Query, asset: &str,
                         step: &mut Step) -> Result<Release, Error>;
    /// Newest stable release above `current` with `asset`, or None.
    pub async fn newest(&self, repo: GithubRepo, current: &Version, asset: &str,
                        step: &mut Step) -> Result<Option<Release>, Error>;
    /// Streamed size and SHA-256 checks, the cap, `sync_all`, the retries.
    pub async fn download(&self, asset: &Asset, stage: Stage<'_>,
                          step: &mut Step) -> Result<Staged, Error>;
}
pub enum Query { Latest, Tag(String) }
pub struct Release { pub tag: String, pub version: Option<Version>, pub asset: Asset }
pub struct Asset { /* name, pinned url, size, sha256: exists only when valid */ }
impl Asset {
    pub fn name(&self) -> &str;
    pub fn size(&self) -> u64;
    pub fn sha256(&self) -> [u8; 32];
    /// bench's Speedtest: a URL, size and digest from source, the URL
    /// checked against the same host rule.
    pub fn pinned(url: Url, size: u64, sha256: [u8; 32]) -> Result<Self, Error>;
}
pub struct Stage<'a> { pub dir: &'a Path, pub prefix: &'a str, pub max_size: u64 }
pub struct Staged { /* a closed TempPath: removed on drop */ }
impl From<Staged> for TempPath { /* ... */ }
/// chmod, rename over `target`, fsync of its directory. `staged` must be
/// in `target`'s directory (else an error, before anything changes): a
/// `Staged`, or a file the caller staged itself (uv's extracted binaries).
pub fn activate(staged: impl Into<TempPath>, target: &Path, mode: u32) -> Result<(), Error>;
pub fn check_elf(path: &Path, arch: Arch) -> Result<(), ElfError>;
pub enum ElfError { NotElf(String), WrongMachine(Arch) }
/// Streamed SHA-256 of an installed file (python-env's digest rule).
pub fn sha256_file(path: &Path) -> Result<[u8; 32], std::io::Error>;
```

This is the interface python-env's spec asks for as R1-R8, under
these names: R1 `Releases::production` and `Releases::testing`; R2
`release` with `GithubRepo` (owner a parameter, uv's being astral-sh)
and `Query`; R3 `Release`, `Asset::sha256` and `Asset::size`, the
missing-asset error in `release`; R4 `Arch::current`, `go_name`,
`rust_name`; R5 `download` with `Stage`; R6 `activate`; R7 `check_elf`
and `ElfError`; R8 `sha256_file`.

- **uv:** `release(astral-sh/uv, Tag(v), "uv-<rust_name>-unknown-linux-gnu.tar.gz")`,
  `download` to a scratch directory (cap 128 MiB); python-env extracts
  `uv` and `uvx` into its own `tempfile` files in the target directory
  and calls `activate` on each after their `--version` check.
- **saltbox.fact:** `release(saltyorg/ansible-facts, Latest,
  "saltbox-facts-<go_name>")`, `download` into the fact's directory
  (prefix `.saltbox.fact-`, cap 32 MiB), `check_elf`, `activate` 0755.
  `Asset::sha256` is the digest python-env compares with
  `sha256_file` of the installed file (the maintainer's answer C-Q1).
- **bench:** the pinned Speedtest has its own sha256 in source; it uses
  `download` with a `Asset` built from that pin (`Asset::pinned(url,
  size, sha256)`, the URL checked against the same host rule).

## Tech Stack

| Crate | Version | Why | In Cargo.lock |
|---|---|---|---|
| reqwest | 0.13.5 (rustls, json) | the stack's HTTP client; `user_agent`, `connect_timeout`, `read_timeout`, `https_only`, `Response::chunk` (https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html) | yes |
| sha2 | 0.11.1 | streamed SHA-256 (https://docs.rs/sha2/0.11.1/sha2/) | yes (lzma-rust2) |
| hex | 0.4.3 | the digest's hex | yes |
| semver | 1.0.28 | tags and the running version (stack table "Versions"; https://docs.rs/semver/1.0.28/semver/) | yes (build dependency only today) |
| tempfile | 3.27.0 | staging and `TempPath::persist` (stack table "Atomic file saves"; https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.into_temp_path) | yes |
| object | 0.41.0, `default-features = false`, `features = ["read_core", "elf"]` | the ELF header (https://docs.rs/object/0.41.0/object/read/elf/index.html); goblin 0.10.7 is the alternative, with less adoption | no: new |
| serde, serde_json, thiserror, url | workspace | JSON, errors, URL building | yes |
| sb-exec, sb-term-ui, sb-cli-core | workspace | the staged `version --json`, steps and the prompt, `BuildInfo` | yes |
| dialoguer | 0.12.0 | term-ui's confirm, on its text input (stack table "Prompts"; https://docs.rs/dialoguer/0.12.0/dialoguer/struct.Input.html) | no: new, in term-ui |

Test only (`testing` feature): axum 0.8.9 (workspace) on a tokio-rustls
listener (axum's low-level-rustls example,
https://github.com/tokio-rs/axum/tree/main/examples/low-level-rustls),
`rcgen` 0.14.10 (new to Cargo.lock, test only;
https://docs.rs/rcgen/0.14.10/rcgen/) for a loopback certificate made at
test time (no key in the repository), and reqwest's `tls_certs_only` for the test client
(https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.tls_certs_only).

**Why not** `self_update` (stack table: archive stacks), `self-replace`
1.5.1 (an extra copy and no directory fsync; tempfile does the same),
`reqwest::retry` (classifies and budgets only: no `Retry-After`, no
backoff, no retry of a short body;
https://docs.rs/reqwest/0.13.5/reqwest/retry/index.html). The retry loop
is ours, with that comment in the code.

The new crates' size (object, dialoguer) is measured against the size
ratchet; python-env's fact check reuses the same `object`.

## Commands

```
Check: scripts/check.sh fast | task | full
Test:  systemd-run --scope -p MemoryMax=4G -p CPUQuota=200% cargo test -p sb-release -p sb
Try:   the production build, as root, SB_LOG_DIR=<scratch>, with
       --skip-self-update elsewhere; `sb self-update` itself only in a
       container or against the fake server (never on this host's sb)
```

## Project Structure

```
crates/release/           sb-release
  src/lib.rs              Releases, GithubRepo, Arch, Error
  src/lookup.rs           list/latest/tag, proxy then GitHub, usability
  src/asset.rs            Asset checks, the pinned URL rule
  src/download.rs         streamed verification, Stage, Staged
  src/retry.rs            the retry policy and waits
  src/activate.rs         activate, check_elf
  src/self_update.rs      sb self-update's flow
  src/testing.rs          the fake proxy/GitHub/download server (`testing`)
crates/term-ui/src/prompt.rs   confirm (term-ui spec, "Prompts")
crates/sb/src/main.rs     the `self-update` command, the repository constant
crates/sb/tests/self_update.rs end to end on the fake server
```

deny.toml: `sb-release` added to the wrappers of `sb-cli-core`,
`sb-term-ui` and `sb-exec`; its own entry allows `sb` (and later
`sb-python-env` and the ops crate).

## Code Style

```rust
/// The asset's URL must be the release's own download URL on GitHub
/// (decided.md D-Q2); the digest only proves the bytes match the metadata.
fn pinned(url: &Url, base: &Url, repo: GithubRepo, tag: &str, name: &str) -> bool {
    let want = [repo.owner, repo.name, "releases", "download", tag, name];
    url.scheme() == "https"
        && url.host_str() == base.host_str()
        && url.port() == base.port()
        && url.username().is_empty()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path_segments().is_some_and(|got| {
            let got: Vec<_> = got.collect();
            got.len() == want.len()
                && got[..2].iter().zip(&want[..2]).all(|(g, w)| g.eq_ignore_ascii_case(w))
                && got[2..] == want[2..]
        })
}
```

## Testing Strategy

No test reaches the network or replaces an installed sb.

- **Unit:** asset checks (each refusal, the pinned URL with wrong owner,
  repo, tag, name, host, port, `http`, user info, query, an encoded
  `%2F`); selection (drafts, prereleases, `v` prefixes, non-semver tags,
  equal and older versions); the retry waits (`Retry-After` seconds and
  date, `X-RateLimit-Reset`, over 60 s); `check_elf` on crafted headers
  (x86_64, aarch64, 32-bit, big-endian, a script, an empty file);
  `version --json` matching (equal, `v` prefix, different, not JSON).
- **Fake server** (`testing` feature): axum over TLS on loopback with an
  `rcgen` certificate the client alone trusts, serving the proxy
  endpoint, the API and the downloads from fixtures, recording each
  request. Cases: proxy ok; proxy 500, then GitHub (both notices, once);
  proxy unusable (asset missing, bad digest), then GitHub; both failing
  (the joined error); 429 with `Retry-After: 1`; a 403 rate limit
  resetting in 2 hours (gives up at once with the reason); a short body
  (retried), a long body (refused), a digest mismatch (refused, staged
  file gone); a redirect to another https path on the fake (followed),
  to `http://` (refused); a stalled body (the read timeout, set short
  through the test constructor); the `User-Agent` on every request.
- **Self-update end to end** (`crates/sb/tests`): the target is a temp
  directory copy, never `current_exe()` (the flow takes the target as a
  parameter; `sb` passes `current_exe()`). The asset is a small stand-in
  ELF built as a test binary that prints `{"version":"<v>"}`, its
  version a fixed-width field the test patches before computing the
  digest. Cases: update (file replaced, mode 0755, directory fsync
  observed as the rename having happened); declined; `--yes`; no
  terminal without `--yes` (the error, exit 1); already latest; the
  staged binary printing another version (refused, target unchanged);
  a non-ELF asset (refused); SIGINT mid-download (target unchanged, no
  `.sb-update-*` left); disabled and raw builds (the texts, exit 0, and
  `--force-update` in the tree only there: the command-tree contract).
- **Prompt:** term-ui's confirm on a PTY (util-linux `script`, as
  term-ui's PTY tests, through `sb self-update`), answered (`y`, `Y`,
  ` yes `), declined (`nay`, `no thank you`, Enter alone),
  interrupted with Ctrl-C, with a background process group and with no
  terminal. *Amended 2026-11-13 with M26: rexpect was dropped for
  `script` with term-ui's tests.*
- **Channels** (2026-11-18), on the fake GitHub:
  - `release` takes the newest stable release and its plain asset, with
    prereleases invisible, both flagged ones and stable-flagged rc tags;
  - `beta` takes the newest release, rc included, and its `_beta` asset;
  - a stable release published after an rc gives a beta host at that rc
    `1.0.0`'s `_beta` asset;
  - `--channel` switches both ways (a lower version said before the
    question; the staged binary's channel checked);
  - `none` and raw builds are off.
  - Unit tests cover `SB_CHANNEL`'s values, the version line and the
    `--json` field. sb's own tests cover the unsupported-release
    warning for `version` and `self-update`, and the refusal of
    another command. install/tests/policy.sh covers `--beta`'s choice
    and its asset URL.
- **Gates:** CONSTRAINTS.md as always; the binary-size row records
  object's and dialoguer's cost.

## Boundaries

- **Always:** verify size and digest before a file is closed for use;
  stage next to the target; send the User-Agent; take the target path as
  a parameter in tests.
- **Ask first:** a release repository other than the constant's value;
  accepting an asset without a digest; any new crate beyond those listed;
  changing the asset names.
- **Never:** run `sb self-update` on this host's installed sb; follow a
  redirect to http; replace a file before its checks pass; publish a
  release, tag or asset (remote mutation, user rules).

## Success Criteria

1. Every asset refusal and the pinned URL rule have a unit test that
   fails against a stub first.
2. On the fake server: proxy failures fall back once with sb-go's two
   notices; the retry waits follow `Retry-After` and the rate-limit
   headers and give up over 60 s; every request carries `sb/<version>`.
3. A body of 12 MiB served at 100 KB/s downloads (no total timeout); a
   body stalled for longer than the read timeout fails.
4. A digest mismatch, a short or long body, an http redirect, a non-ELF
   file or a wrong `version --json` leave the target byte-identical and
   no staged file behind.
5. A successful update replaces the target atomically with mode 0755,
   prints sb-go's texts, and the new file runs.
6. Disabled and raw builds print sb-go's two lines and exit 0;
   `--force-update` appears only in those builds.
7. No terminal and no `--yes`: an error naming `--yes`, exit 1, nothing
   downloaded.
8. The interface above compiles against a python-env stub calling it for
   uv (by tag) and the fact (latest).
9. Binary size within the ratchet, or an ADR for the new crates.

## Decisions

1. **Lookup through the SVM proxy, then GitHub,** with sb-go's notices
   (research D §6.8: sb-go is the reference).
2. **The digest is required** on every asset (stack table "Self-update":
   keeps the digest check; research D §6.6).
3. **Download URLs pinned** to the asked-for repository's
   `https://github.com/<owner>/<repo>/releases/download/<tag>/<asset>`;
   signed checksums at "When the binary is complete" (decided.md D-Q2).
   The rule's reason (hardening that costs no feature) applies to uv's
   own repository the same way.
4. **A User-Agent, `sb/<version>`** (stack table "Self-update";
   proto-musl; research D §6.4).
5. **Connect and per-read timeouts, no total** (research D §6.5: works on
   Saltbox's real links).
6. **reqwest on tokio, rustls/aws-lc-rs, `https_only`** (capability map
   decisions 6 and 8; research D §6.4).
7. **sb-go's retry policy, hand-rolled, for metadata and downloads**
   (research D §6.7).
8. **Atomic replacement with tempfile and a directory fsync**; not
   `self_update` or `self-replace` (stack table; research D §6.9).
9. **Asset names:** `sb_linux_amd64` kept, `sb_linux_arm64` added, chosen
   by the build's architecture, the ELF machine checked to match
   (capability map decisions 1 and 6; research D §6.10).
10. **Versions with semver**, `v` optional, drafts and prereleases
    skipped (stack table; research D §6.11).
11. **Raw builds are self-update disabled**: no `SB_VERSION` means off,
    with `--force-update` shown (research D §6.12): a developer build
    never replaces itself with a release.
12. **The staged binary passes an ELF check and its own `version --json`
    equal to the tag** before activation (decided.md D-Q5).
13. **No `-v`** (config decision 1, docker decision 6; research D §6.1).
14. **saltyorg/sb-rust, from 1.0.0, with channels baked in** (the
    maintainer, 2026-11-18, settling D-Q3): `SB_CHANNEL`
    `release|beta|none`, unset `none`; `release` follows stable releases
    and the plain asset, `beta` every release and the `_beta` asset;
    `--channel` switches once ("Release channels").
15. **term-ui's confirm is built here** (term-ui decision 1).

## Known limits

- **The cutover from sb-go (D-Q3):** sb-rust publishes to its own
  repository, so sb-go hosts reach it only through sb-go's final
  release, which fetches sb-rust's newest release (sb-go's change).
  sb-rust keeps `version --json`'s `uv_version` and bare tags, as the
  update-saltbox-python action reads sb's version that way.
- **The workflows are unrun:** written locally, never pushed. The
  26.04 job runs in an `ubuntu:26.04` container on GitHub's 24.04
  runner (`UNVERIFIED`: GitHub's image list had no 26.04 runner when
  this was written).
- The proxy and GitHub are trusted for the digest; a compromised GitHub
  release isn't caught until signed checksums.
- If sb's file was replaced while it ran, `/proc/self/exe` ends in
  ` (deleted)`; self-update then stops with `sb was replaced while
  running; run it again` instead of writing next to a stale path.

## Amendments to other specs

- **Capability map, module table, `release` row:** "Depends on" becomes
  `cli-core, term-ui, exec` (steps and the prompt; the staged `version
  --json` run).
- **Capability map, Commands table, `self-update` row:** "`--force-update`
  in builds with self-update disabled, raw builds included".
- **cli-core spec, "Build info":** add "`BuildInfo::released`: true when
  `SB_VERSION` was set at build time. Self-update treats a build that
  isn't released as disabled (release spec, decision 11); `sb version`
  is unchanged."
- **term-ui spec, "Prompts":** "Built with `release` (self-update's
  confirm); the label is the caller's text, `?` and the `[y/N]` hint are
  dialoguer's."

## Suggested slices for the plan

1. **First vertical slice (M):** `sb self-update` end to end on the fake
   server: lookup with fallback, asset checks with the pinned URL,
   verified download, activation, the disabled and raw-build paths,
   term-ui's confirm with `--yes`.
2. **Retries and timeouts (S):** the retry policy for metadata and
   downloads, the per-read timeout test.
3. **Staged binary checks (S):** `object`'s ELF check per architecture
   and the `version --json` run; size measured.
4. **The python-env interface (S):** `release(Query)`, `Asset::sha256`,
   `Asset::pinned`, `activate` on a caller's file, `sha256_file`,
   `Arch::current`, with a stub caller.
