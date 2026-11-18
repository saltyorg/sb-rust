# Spec: `python-env` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task.

Module of the approved capability map (`python-env`): the uv-managed
Python, the Ansible venv Saltbox runs, the 22.04 deadsnakes cleanup and
`saltbox.fact`. The build order puts it at step 3, after `host` and
`release` (step 2), beside `ansible`. It builds four commands,
`reinstall-python`, `reinstall-venv`, `update-venv` (hidden) and
`reinstall-facts`. It also exports the reconcile that `update`, `branch`,
`setup` and `gha` (`ops`) call. The release download code is the `release`
module's (its own spec). This spec names the interface it needs from
`release` under "Amendments to other specs".

The behaviour comes from sb-go at HEAD (2026-11-13): `python/` (venv.go,
uv.go, toolchain.go, runtime.go, wrappers.go, activation.go, cleanup.go),
`saltbox/fact.go` and `cmd/reinstallPython.go`, `reinstallVenv.go`,
`updateVenv.go`, `reinstallFacts.go` and `update.go:204-249`. The research
is spec-research/C-python-env.md.

## Assumptions

1. **sb-go keeps shipping** (map assumption 1). Either binary may run after
   the other on the same host, so each must read the other's venv,
   manifest, wrappers and fact as its own (decision 7).
2. **Saltbox owns the toolchain files.** `.python-version`, `.uv-version`
   and `requirements/requirements-saltbox.txt` are written by sb-go's
   release action (`update-saltbox-python`). sb only reads them. Moving
   that action into this repository is release preparation's job, not
   this spec's.
3. **`/usr/local/bin/uv` and a Saltbox-user-owned `/srv/python` are a
   Saltbox contract.** Saltbox's `python`, `cloudplow` and `glances` roles
   run `uv python install` with `UV_PYTHON_INSTALL_DIR=/srv/python` as
   that user. So sb's uv is a host CLI whatever sb does inside (decision 4).
4. **Every child goes through exec** (exec spec). python-env builds
   `sb_exec::Cmd` values and draws nothing itself. Output goes into the
   caller's term-ui `Step` and the run log.
5. **The commands run as root** through cli-core's relaunch, as every sb
   command but `version` and completion (cli-core decision 3).
6. **No consumer changes** (map assumption 1). Saltbox's sanity-check text
   ("Run 'sb reinstall-facts'") and saltyorg/docs keep naming these
   commands, which is why all four names stay (decision 1).

## Objective

A Saltbox user's Ansible runs on a venv that is correct for the checked-out
Saltbox: the Python that `.python-version` pins, and exactly the hashed
lock. The fact Ansible runs is the published release. `sb update` brings
all of it in line by itself and repairs damage it can detect, so the
reinstall commands are a rarely needed "rebuild from scratch on request".
A failed or interrupted run never leaves the active venv pointing at
something unvalidated.

### Fixed layout (sb-go's `layout.go:9-35`; decision 7)

| What | Path |
|---|---|
| Saltbox checkout | `/srv/git/saltbox` |
| Python pin, uv minimum | `<checkout>/.python-version`, `<checkout>/.uv-version` |
| Hashed lock | `<checkout>/requirements/requirements-saltbox.txt` |
| Active venv | `/srv/ansible/venv`, a symlink to `/srv/ansible/releases/<gen>/venv` |
| Generation | `/srv/ansible/releases/<YYYYmmddTHHMMSS.nnnnnnnnn>-<lock sha256[:12]>/`, holding `venv/` and `manifest.json` |
| Python release | `/srv/python/releases/python-<v with . as ->-<YYYYmmddTHHMMSS.nnnnnnnnn>/` |
| Legacy venv, adopted | `/srv/ansible/releases/legacy-<ts>/venv` |
| uv, uvx | `/usr/local/bin/uv`, `/usr/local/bin/uvx` |
| uv cache | `/var/cache/sb-go/uv` |
| Wrappers | `/usr/local/bin/<cmd>` for each exported command |
| Fact | `<checkout>/ansible_facts.d/saltbox.fact` |
| Metadata proxy | `https://svm.saltbox.dev/version?url=<GitHub API URL>` (release's) |

Timestamps are UTC, with Go's `20060102T150405.000000000` layout.

Every path is a field of one `Layout` value with these defaults. Tests
build it on a temporary root (sb-go's `releaseLayout` injection).

### The toolchain (toolchain.go, venv.go:76-92)

- `.python-version` and `.uv-version` each hold one `^\d+\.\d+\.\d+$` after
  trimming. Otherwise: `<path> must contain one exact major.minor.patch
  version, got "<v>"`. A read failure: `read <Python|uv> version from
  <path>: <cause>`.
- `.uv-version` is a minimum, compared with `semver`. If sb's pinned uv
  (`cli-core::UV_VERSION`, passed in by `sb`) is older: `saltbox requires
  uv <min> or newer, but this sb release provides <v>; run sb self-update`.
- The lock's sha256 (sha2) and `git rev-parse HEAD` in the checkout
  (capture, 10 s; sb-go has none) go into the manifest.

### The venv reconcile, step by step (venv.go:76-236)

`reconcile_venv(step, env, user, force)` with `force` one of `None`,
`Venv` or `Python`:

1. **Toolchain** as above.
2. **Step "Ensuring uv <v> is installed"** (uv.go:38-162):
   - `uv --version` and `uvx --version` (capture, 10 s each). If both print
     `<name> <v>`, the step is done.
   - Otherwise release metadata for astral-sh/uv tag `<v>`, the asset
     `uv-<arch>-unknown-linux-gnu.tar.gz` for the build's architecture
     (`x86_64` or `aarch64`; decision 8). Its digest comes from the API
     (decision 5).
   - A verified download (release's, 128 MiB cap) into a scratch directory.
   - Extraction with `tar` and `flate2` (decision 13): only a regular file
     named `uv` or ending in `/uv` (likewise `uvx`), each at most 128 MiB,
     written to a fixed staged name inside a `.uv-install-*` directory
     next to the targets, never to an archive path. The written size must
     equal the entry's size.
   - Each staged binary runs `--version` (10 s) and must report `<v>`.
     Text: `downloaded <name> reports version <got>, expected <v>`.
   - Both are activated by rename with a directory fsync (release's
     `activate`), uv first, as sb-go does.
3. **Native build prerequisites.** If `host::package_installed("libpq-dev")`
   is false: steps **"Updating apt package cache"** (`host::update_lists`)
   and **"Installing native Python build prerequisites"**
   (`host::install(["libpq-dev"])`). Errors: `check native Python build
   prerequisites: <cause>`, `update apt cache for native Python build
   prerequisites: <cause>`, `install native Python build prerequisites:
   <cause>`.
4. **Inspect the active venv** (venv.go:281-317):
   - missing: unhealthy;
   - a real directory: legacy, unhealthy, adopted at activation;
   - a symlink: read its generation's `manifest.json`. Healthy only when
     the schema is 1, the Python version and lock digest match, and the
     **environment check** passes:
     - `python3 -I --version` is `Python <v>`;
     - the **Python check**: `python3 -I -c "import encodings, ssl,
       sqlite3, ctypes"` (sb-go imports `encodings` only; decision 3);
     - `uv pip check --python <venv>/bin/python3`;
     - `import ansible, apprise, certbot`;
     - `ansible`, `certbot` and `apprise` each run `--version`. ansible runs
       with `ANSIBLE_CONFIG` empty and every other `ANSIBLE_*` removed.
     Each probe: capture, 30 s.
5. **Step "Checking Ansible virtual environment for updates"**, done label
   one of sb-go's four (venv.go:238-251): `Ansible virtual environment was
   already up to date`, `Ansible virtual environment update required`,
   `Ansible virtual environment recreation requested`, `Python and Ansible
   virtual environment recreation requested`.
6. **Healthy and `force` is `None`:** rewrite the wrappers, clean old
   generations, fix ownership, return.
7. **Python.** Reuse the manifest's Python when `force` isn't `Python`, its
   version matches and it passes the Python check. Otherwise step
   **"Installing Python <v>"**: `uv python install --managed-python --no-bin
   --install-dir <release dir> [--reinstall] [--no-cache] <v>`, then `uv
   python find --managed-python --no-project --no-python-downloads <v>`
   with `UV_PYTHON_INSTALL_DIR=<release dir>`. The result must be an
   absolute path and pass the Python check (uv.go:291-335).
8. **Step "Creating Ansible virtual environment":** `uv venv --python <py>
   --no-project --no-python-downloads <gen>/venv`.
9. **Step "Syncing hashed Saltbox requirements":** `uv pip sync
   --no-progress --python <gen>/venv/bin/python3 --require-hashes
   [--no-cache] <lock>`, streamed into the step. `--no-cache` whenever
   `force` isn't `None` (sb-go 7d1ee0e) and on the retry (decision 3).
10. **Command discovery:** regular executables in `venv/bin` named
    `ansible*`, `certbot` or `apprise`. ansible, certbot and apprise are
    required. A name with `/`, `\n` or `\r` fails: `unsafe venv command name
    "<n>"`.
11. **Staged check:** the environment check on the new venv, then
    `manifest.json` written through `tempfile`, mode 0644, with sb-go's
    fields in sb-go's order (venv.go:42-53): `schema_version` (1),
    `generation_id`, `python_version`, `python_path`, `python_install`
    (omitted when empty), `uv_version`, `lock_sha256`, `saltbox_commit`,
    `created_at` (RFC 3339 with Go's trimmed nanoseconds),
    `exported_commands`.
12. **Wrappers,** written before activation, each through `tempfile` in
    `/usr/local/bin` (prefix `.sb-wrapper-`), mode 0755, byte for byte
    sb-go's (wrappers.go:75-86):
    ```
    #!/bin/sh
    # Managed by sb-go Saltbox venv
    venv_bin=$(/usr/bin/readlink -f /srv/ansible/venv/bin) || exit 1
    PATH="${venv_bin}${PATH:+:${PATH}}"
    export PATH
    exec /srv/ansible/venv/bin/<cmd> "$@"
    ```
13. **Activation** (activation.go:32-92): a legacy directory moves to
    `releases/legacy-<ts>/venv`; `/srv/ansible/venv` is replaced by a
    temporary symlink plus `rename`; on failure a moved legacy directory
    is put back.
14. **Stale wrappers:** a wrapper the previous manifest listed and the new
    one doesn't is removed, only when its first two lines are exactly the
    marker header (wrappers.go:225-249).
15. **Generation cleanup** (activation.go:98-126): an obsolete
    `/srv/ansible/previous` symlink goes; only the active generation and
    its Python release are kept; a path that isn't a direct child of
    `releases/` is an error. Nothing outside `/srv/python/releases/` is
    touched (Saltbox's `python` role installs there too).
16. **Ownership** (venv.go:486-496): `host::ensure_owner(user, ["/srv/git"])`,
    then `host::ensure_owner_recursive(user, [checkout, "/srv/ansible",
    "/srv/python"])`. Skipped when the user is `None` (gha).
17. **Failure or cancel** (venv.go:163-171, 259-264): the new generation
    directory is removed, and the new Python release unless an active
    manifest names it. The active link is never changed before step 13.

### Child isolation (runtime.go; sb-go bb69839; decision 11)

Every uv, Python and entrypoint child:
- **cwd** `/`, except entrypoints, which run in the venv directory;
- **`PATH`** `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`
  (entrypoints: the venv's `bin` first);
- **`TMPDIR`, `TMP`, `TEMP`** a fresh `tempfile::TempDir` (prefix
  `sb-python-`), removed after the call;
- **removed:** `UV` and every `UV_*`, `PIP_*`, `PYTHON*`, `VIRTUAL_ENV*`,
  `CONDA_*` and `_CONDA_*` variable, and `__PYVENV_LAUNCHER__`, except
  `UV_SYSTEM_CERTS` and `UV_NATIVE_TLS` (sb-go's
  `filteredManagedEnvironmentName`; a name like `UVICORN_HOST` stays);
  relative `SSL_CERT_FILE`, `SSL_CLIENT_CERT`, `REQUESTS_CA_BUNDLE` and
  `CURL_CA_BUNDLE`, and each relative entry of `SSL_CERT_DIR`, are made
  absolute against the caller's cwd;
- **uv** gets `--no-config` first and `UV_CACHE_DIR`; **python** gets `-I`;
  entrypoints get `PYTHONNOUSERSITE=1` and `PYTHONSAFEPATH=1`.

Timeouts: the version and health probes as listed; `uv python install`,
`uv venv` and `uv pip sync` have none, because their length depends on the
mirror and the package set, and Ctrl-C still stops them (host's apt rule).
On failure the error carries exec's stderr tail, as sb-go's
(runtime.go:81-96).

### Deadsnakes cleanup (cleanup.go; decisions 10 and 22)

`cleanup_deadsnakes(step, env)` returns whether it removed anything.
- Runs only when `host::ubuntu_release()` is 22.04. On any other release it
  returns `false` at once.
- **Candidates** from `host::package_list()` (amendment H2): `ii` rows
  whose source is `python3.12`, or whose binary is `python3.12`,
  `python3.12-*`, `libpython3.12*` or `idle-python3.12[-*]`.
- **Origin** from `host::package_policy(pkg)` (amendment H2), parsed here:
  the installed `***` version must come only from
  `ppa.launchpadcontent.net/deadsnakes/ppa` or
  `ppa.launchpad.net/deadsnakes/ppa`, or, with no source left, carry a
  `+focal<N>` or `+jammy<N>` suffix. Any other origin keeps the package.
- Then `host::remove(pkgs)` and `host::autoremove()`, and the query again.
  Leftovers: `deadsnakes Python packages remain after cleanup: <pkgs>`.
- Every file in `/etc/apt/sources.list.d` that mentions `/deadsnakes/ppa`
  (the path both PPA hostnames share; *amended 2026-11-15 from
  `deadsnakes`*) is moved aside into a root-only `/var/backups/sb/deadsnakes/<run id>/`, the
  run id being the run log's (`host::remove_sources_mentioning`, amendment
  H3; decision 22), then `host::update_lists()`, then the directory is
  checked again with the read-only `host::sources_mentioning` (host spec,
  added 2026-11-15), by the same rule. Leftovers: `deadsnakes repository
  files remain after cleanup: <files>`, each path escaped as sb-host logs
  a moved name. "Mentions" ignores ASCII case, as sb-go lowercases the
  text; a symlink whose target mentions it moves as a link (host spec,
  H3 as amended 2026-11-15). The needle is the PPA's path, not
  `deadsnakes` alone, so the file rule is the package rule (the origins
  above): a `deadsnakes-snapshot` source, whose package is kept, stays.
  Moving it would leave that package with no source left on the next
  run, where its `+jammy<N>` suffix would read as deadsnakes' and it
  would be removed, which sb-go never does. *Amended 2026-11-15 (module
  build M20, from its review).*
  The move is fenced as ops' setup moves its apt
  sources (ops spec decisions 5 and 28): fixed paths, the directories
  opened `O_DIRECTORY | O_NOFOLLOW` and refused when a symlink, not
  root's, or group- or world-writable (`/var/backups/sb` and `deadsnakes`
  are created 0700 when missing), the run's directory created 0700 with
  `mkdirat`, exclusively, only when there is something to move, each entry
  `lstat`ed through the handle and moved with `renameat` between the two
  handles (across filesystems, `EXDEV`, copied with its bytes and mode,
  synced, then unlinked through the handle), and every name logged before
  it moves. The step adds the info `Moved <n> deadsnakes repository files
  to /var/backups/sb/deadsnakes/<run id>`. Old copies stay until the user
  removes them.
- **The run id** is the run log's, which `sb` puts in `Env::run_id`. A
  run without a log (cli-core's "no run log this time" warning) names
  the directory by the run's start in UTC, `YYYYmmddTHHMMSSnnnnnnnnn`,
  letters and digits as a run id must be. *Added 2026-11-15 (module
  build M20): the spec named only the log's id.*
- **sb-go's texts** for the rest (cleanup.go): `parse installed packages:
  line <n>: package name is empty` and `... installed package version is
  empty`, `apt policy for <pkg> does not describe installed version <v>`,
  `remove deadsnakes Python packages: <cause>`, `autoremove deadsnakes
  Python dependencies: <cause>`, `verify deadsnakes package removal:
  <cause>`, and, under `remove deadsnakes repositories: <cause>`, `update
  apt package lists after removing deadsnakes repositories: <cause>` and
  `verify deadsnakes repository removal: <cause>`. The package list and
  policy errors are host's (`list packages: <cause>`, `query package
  policy <name>: <cause>`). The release is read through `Env`'s
  `ubuntu_release` (`None`: `host::ubuntu_release()`); one that can't be
  read is not 22.04. Packages are queried again only after a removal.

### saltbox.fact (saltbox/fact.go; decisions 2, 6, 8)

`install_fact(step, env, mode)` with `mode` `Update` or `Reinstall`:

1. **Metadata:** saltyorg/ansible-facts `releases/latest` through release
   (proxy first, then GitHub, up to 4 attempts with backoff), with sb-go's
   warning `SVM proxy <reason>; trying GitHub directly`. The asset is
   `saltbox-facts-<amd64|arm64>` for the build's architecture, with its
   API digest required. A missing asset is release's `release <tag> is
   missing <name>` (sb-go's wording for a missing asset).
2. **Decide** (fact.go:150-187, plus the digest rule):
   - `Reinstall`: info `Reinstall forced.`, install.
   - file missing: info `saltbox.fact not found. Proceeding with update.`,
     install.
   - `saltbox.fact --version` (capture, 10 s, cwd `/`) fails or prints
     nothing parseable: warn `<cause>. Proceeding with update.`, install.
     A plain one-line semver (optional `v`) is read, and legacy JSON's
     `saltbox_facts_version` too (sb-go e82c34d). The fact is never run
     bare: that run makes network calls and writes the IP cache.
   - either version unparseable: sb-go's two warnings, install.
   - installed older than latest: install.
   - installed newer: keep, info `saltbox.fact is up to date (version <v>)`.
   - **equal:** the installed file's sha256 against the asset's digest.
     Equal: info `saltbox.fact is up to date (version <v>)`. Different:
     warn `saltbox.fact <v> does not match its release (sha256 <got>,
     expected <want>). Reinstalling.`, install. No network call beyond the
     metadata already fetched.
3. **Install** in a step `Updating saltbox.fact to version <v>` (Reinstall
   and a digest mismatch: `Reinstalling saltbox.fact with version <v>`):
   - verified download (release's, 32 MiB cap) into the fact's own
     directory, prefix `.saltbox.fact-`; the directory is made first,
     0755, when missing, as sb-go's `DownloadVerified` makes it (Saltbox
     tracks no `ansible_facts.d`, so a fresh checkout has none; error
     `create release asset staging directory: <cause>`). *Amended
     2026-11-15 (module build M21, found by the live acceptance).*
   - nested step **"Validating downloaded saltbox.fact"**: size equals the
     API size, then release's `check_elf` for the build's architecture
     (64-bit, `EM_X86_64` or `EM_AARCH64`; sb-go's texts `file is not a
     valid ELF binary: <cause>` and `file is not an <x86-64|aarch64> ELF
     binary`, fact.go:78-82), then the staged file's
     `--version` (10 s) must equal the tag (decision 20). Failure: `downloaded
     binary validation failed: <cause>`;
   - activation, mode 0755, with a directory fsync. The staged file is
     removed on any failure.
4. **Info lines** as sb-go: `saltbox.fact reinstalled successfully
   (version <v>)`, `saltbox.fact updated successfully: <old> → <v>` or
   `saltbox.fact installed successfully (version <v>)`. A digest-mismatch
   repair uses the first.

### The commands

All four take no arguments (a positional is clap's parse error, exit 2) and
no flags (decision 9). Help texts are sb-go's but one short help
(difference 15). Exit codes are cli-core's: 0, 1 on any error (cli-core's
error block), 130 on Ctrl-C.

| Command | Help (sb-go) | Saltbox user | Steps (outer, then nested) |
|---|---|---|---|
| `reinstall-python` | Reinstall the Python version used by Saltbox and related Ansible virtual environment using uv (the short help reworded, difference 15) | read first; `error getting saltbox user: <cause>` | "Reinstalling Saltbox Python and Ansible environment" (done "Saltbox Python and Ansible environment reinstalled", failure "Python and Ansible environment reinstall") > "Recreating Ansible virtual environment" (done "Ansible virtual environment recreated", failure "Ansible virtual environment") > `reconcile_venv(Python)` |
| `reinstall-venv` | Reinstall the Ansible virtual environment | read first | "Reinstalling Ansible virtual environment" (done "Ansible virtual environment reinstalled") > `reconcile_venv(Venv)`; error `error managing Ansible venv: <cause>` |
| `update-venv` (hidden) | Update the Ansible virtual environment and saltbox.fact | read first | "Checking Ansible virtual environment and saltbox.fact for updates" (done "Ansible virtual environment and saltbox.fact are ready", failure "Saltbox runtime dependency update check") > `reconcile_runtime` |
| `reinstall-facts` | Reinstall the Rust saltbox.fact file | not read: works with a broken accounts.yml | "Reinstalling saltbox.fact" > `install_fact(Reinstall)`; error `error reinstalling saltbox.fact: <cause>` |

**`reconcile_runtime(step, env, user)`** (cmd/update.go:204-249), shared by
`update` and `update-venv`, runs three nested steps in this order:
1. "Checking for old deadsnakes Python packages"; error `error cleaning up
   deadsnakes packages: <cause>`. A removal or a move of source files adds
   the info `Removed old deadsnakes Python packages` (sb-go's
   `cleanedUp || repositoriesRemoved`).
2. "Preparing Ansible virtual environment" (done "Ansible virtual
   environment ready", failure "Ansible virtual environment"):
   `reconcile_venv(None)`; error `error managing Ansible venv: <cause>`.
3. "Checking saltbox.fact" (done "saltbox.fact is ready", failure
   "saltbox.fact update"): `install_fact(Update)`; error `error downloading
   and installing saltbox fact: <cause>`.

`setup` and `gha` call `reconcile_venv` and `install_fact` directly, in
their own order, as sb-go does. `branch` calls `reconcile_runtime`, as
`update` does (ops spec, decision 21 and difference 28).

### Self-healing on the normal path (decision 3)

`reconcile_venv(None)`, which `update`, `update-venv`, `branch`, `setup` and
`gha` all use, heals what it can detect:
- a missing, legacy or unhealthy venv, a changed lock or `.python-version`,
  missing or edited wrappers, a wrong uv or uvx, wrong ownership (sb-go);
- a reused Python that fails the wider Python check (new);
- **one clean retry (new):** when a non-forced build fails in steps 7-11
  (Python install or find, `uv venv`, `pip sync`, command discovery, the
  staged check), the partial generation and Python release are removed and
  the build runs once more as `Python` force (a fresh Python release, both
  uv calls with `--no-cache`). The info line before it reads `Retrying
  with a fresh Python and no uv cache after: <cause>`. A cancel, a
  toolchain or uv-minimum error, a uv install or apt failure, and a
  failure in activation or later steps are not retried. A second failure
  fails the step with the second cause; the run log keeps both.

`install_fact(Update)` heals a fact that is missing, fails `--version`, is
older, or differs from its release's bytes at the same version (the digest
rule).

What remains for the reinstall commands: damage no probe detects (a venv
that passes every check but misbehaves; a fact that matches its release
but the release itself is broken, fixed by a new release; a deliberately
newer local fact).

### Library surface (decision 17)

```rust
pub struct Layout { /* the paths above; Layout::saltbox() gives defaults */ }
pub struct Env<'a> { pub layout: &'a Layout, pub release: &'a sb_release::Releases,
                     pub uv_version: &'a str, pub arch: sb_release::Arch, /* program paths */
                     pub ubuntu_release: Option<String>, pub run_id: String }
pub enum Force { None, Venv, Python }
pub enum FactMode { Update, Reinstall }

pub async fn reconcile_venv(step: &mut Step, env: &Env<'_>, user: Option<&str>, force: Force) -> Result<VenvOutcome, VenvError>;
pub async fn install_fact(step: &mut Step, env: &Env<'_>, mode: FactMode) -> Result<FactOutcome, FactError>;
pub async fn cleanup_deadsnakes(step: &mut Step, env: &Env<'_>) -> Result<bool, DeadsnakesError>;
pub async fn reconcile_runtime(step: &mut Step, env: &Env<'_>, user: Option<&str>) -> Result<(), RuntimeError>;
```

Errors are thiserror enums, one per part, with sb-go's texts. Outcomes
carry what the step reported (up to date, rebuilt, retried, version
moved), so `ops` callers and the tests can read them.

### Differences from sb-go (the complete list)

1. **No `-v/--verbose`** on any of the four (decision 9). uv and apt output
   always streams into the step and the run log keeps every line; sb-go
   discarded uv output (but `pip sync`'s) unless verbose. *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*
2. **The fact's digest rule:** at an equal version, a fact whose sha256
   differs from its release's digest is reinstalled. sb-go keeps it.
3. **The wider Python check** (`import encodings, ssl, sqlite3, ctypes`).
   sb-go imports `encodings` only. A Python that fails it is replaced, and
   a venv built on it is unhealthy.
4. **One clean retry** of a failed non-forced venv build.
5. **Assets by architecture:** `uv-aarch64-unknown-linux-gnu.tar.gz` and
   `saltbox-facts-arm64` on arm64, the ELF check for `EM_AARCH64`. On amd64
   the fact asset is `saltbox-facts-amd64`, not the legacy `saltbox-facts`
   (the same digest today, checked 2026-11-13). sb-go hard-codes x86_64.
6. **The staged fact runs `--version`** before activation and must equal
   the tag. sb-go checks size, digest and ELF only.
7. **Download URLs are pinned** to
   `https://github.com/<owner>/<repo>/releases/download/...` (release's
   rule, decided D-Q2). sb-go accepts any https URL.
8. **Timeouts sb-go lacks:** 10 s on `git rev-parse` and the uv version
   probes, 30 s on each health probe and on `uv python find` (a local
   lookup; added with the build, module build M15). sb-go's health probes
   and its find had none.
9. **Scratch TMPDIR prefix** `sb-python-` instead of `sb-go-python-`
   (temporary, not part of the layout).
10. **Errors** are cli-core's error block; a positional argument is a parse
    error, exit 2 (sb-go: exit 1); Ctrl-C exits 130.
11. **apt's lock wait** is host's `DPkg::Lock::Timeout` (host difference 1),
    replacing sb-go's `WaitForAptLock` around the deadsnakes and libpq-dev
    calls.
12. **The deadsnakes source files are moved aside** into a root-only
    `/var/backups/sb/deadsnakes/<run id>/` through fenced directory
    handles, not deleted (decision 22). sb-go deleted them by path. A
    file is moved when it mentions `/deadsnakes/ppa` in any ASCII case;
    sb-go looks for the two hostnames before it
    (`ppa.launchpadcontent.net/deadsnakes/ppa`,
    `ppa.launchpad.net/deadsnakes/ppa`), so only a source naming that
    path on another host would differ. A link sb-host leaves alone
    (dangling, chained, to a directory or a FIFO; host spec difference
    14) is neither moved nor reported by the re-check, where sb-go fails
    or waits on it. *Amended 2026-11-15 (module build M20).*
13. **`reinstall-python` reads the Saltbox user before its runner
    starts** (M16), as this spec's table says. sb-go read it inside the
    outer step, so there a missing user showed as a failed "Python and
    Ansible environment reinstall" step.
14. **"error managing Ansible venv" sits in the failing step's error
    block** (M16), as `update-venv` already has it: when a nested step
    such as the sync fails, that step shows its own cause and the steps
    above it end quietly.
15. **`reinstall-python`'s short help is "Reinstall Saltbox's Python and
    its Ansible virtual environment using uv"** (maintainer, 2026-11-14):
    sb-go's 92 columns wrapped in `sb --help` at fang's 120, where the
    help column leaves 90. The command's own help (`sb reinstall-python
    --help`) keeps sb-go's sentence as its about, which fits there. The
    contract test's `REWORDED` list pairs the two lines.

### Scope

**In:**
- the `sb-python-env` crate: toolchain, uv install, venv reconcile,
  wrappers, activation, cleanup, ownership calls, deadsnakes, the fact;
- the four commands in `sb`;
- `reconcile_runtime` and the parts, exported for `ops`;
- host's apt and ownership parts this module first uses (amendments H1-H3);
- an opt-in container acceptance test (Testing Strategy).

**Later, elsewhere:**
- `update`, `branch`, `setup`, `gha` wiring: `ops` and `repo`.
- Moving sb-go's `update-saltbox-python` release action: release
  preparation.
- A shared run lock between sb runs: sb-go has none; not part of parity.

## Tech Stack

| Crate | Version | Why | Docs | In Cargo.lock / linked by `sb` |
|---|---|---|---|---|
| uv (host CLI) | `cli-core::UV_VERSION` (0.13.4) | Python, venv and hashed install; what Saltbox's lock is compiled and tested with | https://docs.astral.sh/uv/reference/cli/ | n/a |
| `sb-release` | workspace | metadata, verified download, activation, ELF check | release spec | new crate |
| `sb-exec`, `sb-term-ui`, `sb-host` | workspace | children, steps, apt, ownership, release check | their specs | yes / yes |
| `tar` | 0.4.46 | reads uv's archive, pure Rust | https://docs.rs/tar/0.4.46/tar/struct.Archive.html | no / no |
| `flate2` | 1.1.10 (lockfile), `rust_backend` (miniz_oxide) | gzip, pure Rust | https://docs.rs/flate2/1.1.10/flate2/read/struct.GzDecoder.html | yes / no |
| `sha2` | 0.11.1 | lock digest, the fact's digest rule | https://docs.rs/sha2/0.11.1/sha2/ | yes / yes |
| `semver` | 1.0.28 | uv minimum, fact versions | https://docs.rs/semver/1.0.28/semver/ | yes (build dep) / no |
| `tempfile` | 3.27.0 | manifest, wrappers, staging dirs, scratch TMPDIR | https://docs.rs/tempfile/3.27.0/tempfile/ | yes / yes |
| `serde`, `serde_json` | workspace | manifest | https://docs.rs/serde_json/ | yes / yes |
| `jiff` | 0.2 | generation timestamps, `created_at` | https://docs.rs/jiff/latest/jiff/fmt/strtime/index.html | yes / yes |
| `thiserror` | workspace | errors | https://docs.rs/thiserror/ | yes / yes |
| `object` | 0.41.0, `read_core` + `elf` | the ELF check, through release (decision 6) | https://docs.rs/object/0.41.0/object/read/elf/index.html | no / no |

Rejected (research §5): uv's own crates ("internal and unstable",
https://docs.astral.sh/uv/reference/policies/versioning/), rattler and rip
(a different ecosystem, or no sdist builds and no release since 2024), and
a venv or installer of our own (re-implements uv and drifts from what
Saltbox CI tests). A `tar` child is rejected for the pure-Rust preference.

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-python-env -p sb
Live:    SB_LIVE_PYTHON=1 SBP_SB=<sb, no testing feature> SBP_SBGO=<sb-go> SBP_SALTBOX=<toolchain files> \
           SB_LOG_DIR=<scratch> systemd-run --scope -p MemoryMax=4G -p CPUQuota=200% \
           cargo test -p sb-python-env --test live -- --test-threads=1
         (or crates/python-env/tests/live.sh [RELEASE]...). The scope caps the build;
         Docker caps each container itself (--cpus 4 --memory 4g; pin its cores with
         SBP_DOCKER_OPTS=--cpuset-cpus=...), as a systemd scope can't reach it
Try:     the production build of sb, as root, with SB_LOG_DIR set, inside a container only
```

The live test needs Docker and network. It runs each release's container in
turn, never on the developer's host.

## Project Structure

```
crates/python-env/src/lib.rs        re-exports, Layout, Env, reconcile_runtime
crates/python-env/src/toolchain.rs  pin files, uv minimum, lock digest
crates/python-env/src/isolate.rs    the isolated Cmd builder (uv, python, entrypoints)
crates/python-env/src/uv.rs         probe, download, extraction, activation
crates/python-env/src/venv.rs       inspect, health check, build, retry
crates/python-env/src/manifest.rs   schema 1 read and write
crates/python-env/src/wrappers.rs   wrapper text, write, stale removal
crates/python-env/src/activate.rs   symlink swap, legacy adoption, cleanup
crates/python-env/src/deadsnakes.rs dpkg/apt-cache parsing and the cleanup
crates/python-env/src/fact.rs       version probe, decision, install
crates/python-env/src/testing.rs    stand-in uv/python/fact programs, fixtures (`testing` feature)
crates/python-env/tests/            stand-in end-to-end tests; live.rs (opt-in) and live.sh
crates/python-env/tests/fixtures/   sb-go manifests, apt-cache policy and dpkg-query outputs
crates/sb/src/main.rs               the four commands
```

deny.toml: `sb-python-env` added to the wrappers of `sb-exec`,
`sb-term-ui`, `sb-host` and `sb-release`; its own entry allows `sb` and,
when they arrive, `ops`'s crate.

## Code Style

As host and docker: rules are pure functions over parsed values, tested
first; I/O functions build a `Cmd` and parse its capture.

```rust
/// sb-go's fact rule plus the digest check (decision 2): an equal
/// version is kept only when its bytes are the release's.
fn fact_action(installed: &Version, latest: &Version, digest_matches: impl FnOnce() -> bool) -> FactAction {
    match installed.cmp(latest) {
        Ordering::Less => FactAction::Update,
        Ordering::Greater => FactAction::Keep,
        Ordering::Equal if digest_matches() => FactAction::Keep,
        Ordering::Equal => FactAction::Repair,
    }
}
```

## Testing Strategy

No test touches `/srv`, `/usr/local/bin`, `/etc/apt` or the network on the
developer's host. Every path is a `Layout` field on a `tempfile` root.

- **Unit (most):**
  - toolchain parsing: each malformed pin, the uv minimum by semver
    (`0.13.4` vs `0.13.10`), sb-go's texts;
  - isolation: the built `Cmd`'s environment and argv from a seeded
    environment (every prefix dropped, the two kept, relative CA paths
    made absolute, `--no-config` first, `-I`);
  - generation and Python release names, and `created_at`, against strings
    Go's layouts produce (fixtures taken from sb-go's tests);
  - manifest: reads sb-go's JSON fixtures; writes bytes equal to sb-go's for
    the same values; schema 2 and a missing field are unhealthy;
  - wrappers: bytes equal to sb-go's template; the unsafe-name rule; stale
    removal only with the exact header (a user's own `ansible-lint` script
    survives);
  - extraction on tar.gz fixtures built in the test with `tar::Builder`:
    nested and top-level names, a symlink or directory named `uv`, a
    `../uv` entry, an oversized entry, a truncated archive, a missing
    `uvx`;
  - the fact: `--version` parsing (plain, `v`-prefixed, legacy JSON, two
    lines, empty), `fact_action` for each order and digest result;
  - deadsnakes: dpkg-query rows and apt-cache policy fixtures from 22.04
    (the PPA, the old hostname, a vanished source with `+jammy1`, an
    Ubuntu-origin package kept);
  - the deadsnakes source move on a temp root: a symlinked, foreign-owned
    or group-writable `sources.list.d` or backup directory refused with
    nothing moved, a symlinked entry moved as a link and never followed, a
    subdirectory left alone, each matching file in a new 0700
    `deadsnakes/<run id>/` with its bytes and mode intact (the `EXDEV` copy
    through a seam too), no backup directory when nothing matches, and
    every moved name logged;
  - the retry rule: which failures retry and which don't.
- **Stand-in programs** (exec's pattern, docker's ETXTBSY guard), shell
  scripts in a temp directory:
  - a uv that records argv and env, and creates a fake venv with
    `bin/python3`, `ansible`, `certbot`, `apprise`;
  - a uv whose `pip sync` fails once, then succeeds (the clean retry, with
    `--no-cache` on the second call only);
  - a python failing the `ssl` import (reused Python replaced);
  - a `pip sync` that never ends, cancelled through the token: the
    generation is removed, the link and wrappers are unchanged, the child
    ends within exec's grace;
  - a fact printing a version, legacy JSON, nothing, or hanging past 10 s.
- **Fake release server:** release's `testing` feature (axum on loopback,
  the release spec's): uv and fact metadata, a digest mismatch, a short
  body, an x86_64 fact served to an arm64 `Env`, a non-github URL refused.
- **Activation on a temp tree:** legacy adoption and its rollback when the
  rename fails (an injected rename), cleanup refusing a path outside
  `releases/`, ownership through host's injected `lchown`.
- **Compatibility, both directions:** a tree laid out by sb-go's own test
  fixtures is read as healthy (stand-ins answering), and a tree sb-rust
  writes is read by sb-go's `inspectActive` in the live test.
- **`sb` end to end** (assert_cmd): the four commands' help, hidden state,
  no-argument rule, and `reinstall-facts` succeeding with a broken
  accounts.yml, all on a temp `Layout` through the `testing` feature.
- **Live (opt-in, capped, containers only):** sb-go's acceptance-script
  pattern (`scripts/test-python-isolation-container.sh`,
  `test-deadsnakes-cleanup-container.sh`) in throwaway `ubuntu:22.04`,
  `24.04` and `26.04` containers: a fresh reconcile, a second run up to
  date, a `.python-version` bump, `reinstall-python`, a tampered fact
  repaired by `update-venv`, sb-go then sb-rust then sb-go on the same tree,
  and on 22.04 a planted deadsnakes source file (found moved aside
  afterwards) and packages.
- **Bars** (CONSTRAINTS.md): changed lines ≥90%, mutation ≥80%, the
  coverage ratchet, and the logging row (no secret reaches the log; this
  module handles none, but the run log test covers its output lines).

## Boundaries

- **Always:**
  - sb-go's paths, file formats, wrapper bytes and texts, except the listed
    differences;
  - exec for every child, with the isolation above;
  - a timeout or a stated reason for none;
  - validate before activating; never leave `releases/` when cleaning.
- **Ask first:**
  - a new dependency beyond the Tech Stack;
  - any change to the on-disk layout, manifest schema or wrapper text;
  - removing or renaming any of the four commands;
  - a size cost of `object` beyond the threshold in decision 6.
- **Never:**
  - run a test against the developer's `/srv`, `/usr/local/bin`, apt or
    Saltbox checkout;
  - run the fact bare (without `--version`);
  - touch `/srv/python` outside `releases/`;
  - commit host details from the live runs; platform notes carry method and
    versions only.

## Success Criteria

1. On a tree written by sb-go, `sb update-venv` reports "Ansible virtual
   environment was already up to date" and changes nothing but wrappers it
   rewrites with identical bytes (stand-in and live tests).
2. sb-go reads a tree written by sb-rust as healthy, and the reverse (live
   test, all three releases).
3. Every rule in "The venv reconcile", "Deadsnakes cleanup" and
   "saltbox.fact" gives sb-go's result and text on sb-go's test cases and on
   the cases under Testing.
4. Each difference 1-12 is pinned by a named unit, stand-in or end-to-end
   test.
5. A cancel or failure at any step from 7 to 12 leaves the active link,
   the wrappers and the previous generation unchanged, and removes the new
   generation (stand-in tests at each step).
6. A fact edited by one byte at the released version is replaced by
   `sb update-venv` with no extra network request (fake server request
   count).
7. An arm64 `Env` downloads only arm64 assets and refuses an x86_64 fact
   (fake server).
8. `reinstall-facts` works with accounts.yml missing or invalid.
9. No child outlives Ctrl-C by more than exec's grace plus 1 s.
10. `object`'s, `tar`'s and `flate2`'s cost on the production binary is
    measured and recorded in the commit that links each; `scripts/check.sh
    full` passes within the ratchet and the size ceiling.
11. The command-tree contract shows the four commands with sb-go's names,
    help (but difference 15, declared in its `REWORDED` list) and hidden
    state, and `-v` in the contract's dropped list.

## Decisions

1. **All four names stay, as forced variants; `update-venv` stays hidden.**
   `reinstall-python` is `Force::Python`, `reinstall-venv` is
   `Force::Venv`, `reinstall-facts` is `FactMode::Reinstall`. Maintainer,
   2026-11-13 (decided.md C-Q1).
2. **The normal fact path checks the installed file's sha256 against its
   release's digest** at an equal version and repairs a mismatch. A newer
   local version is kept. Maintainer, 2026-11-13 (C-Q1); research 8.4.
3. **The normal venv path heals itself:** the wider Python check and one
   clean retry as `Force::Python`. The retry is a full rebuild because a
   failure's cause (a corrupt cached wheel, a broken Python, a transient
   index error) can't be told apart from uv's exit, and the extra cost is
   paid only on the failure path. Maintainer, 2026-11-13 (C-Q1, "the
   normal path heals itself"); research 8.4's two measures.
4. **uv stays a host CLI** at `/usr/local/bin/uv` and `uvx`, pinned by
   `cli-core::UV_VERSION`. Maintainer, 2026-11-13; research D1 (assumption
   3, uv's crates unstable).
5. **uv's digest comes from the release API,** through the proxy or GitHub,
   as sb-go. Revisited with D-Q2's signed checksums at release preparation.
   Maintainer, 2026-11-13; decided.md C-2.
6. **The fact's ELF check stays,** as release's shared check (self-update
   needs it too, decided.md D-Q5), with `object` for the build's
   architecture. The commit that first links `object` records its size
   cost; more than 100 KB on the production binary goes back to the
   maintainer before it lands. Maintainer, 2026-11-13; decided.md C-3. The
   100 KB threshold is this spec's proposal.
7. **The on-disk layout stays byte-compatible with sb-go's:** paths,
   generation and release names, manifest schema 1, the venv symlink and
   legacy adoption, the wrapper bytes and marker, the uv cache path, the
   fact path and mode. Maintainer, 2026-11-13; map decision 1; research
   D2.
8. **Downloads are chosen by architecture** (`std::env::consts::ARCH`:
   `x86_64`/`aarch64` for uv, `amd64`/`arm64` for the fact). Any other
   architecture fails before a download. Maintainer, 2026-11-13; map
   decision 6; research D9.
9. **No `-v`.** Map decision 11 (verbose by default, no flag), config
   decision 1, docker decision 6; research D3.
10. **Deadsnakes runs on host's pieces**, with the parsing in python-env.
    Host spec (apt, release checks, decision 1); research D4.
11. **Every uv, Python and entrypoint child runs through exec with sb-go's
    isolation rebuilt.** Exec spec; host assumption 1; sb-go bb69839;
    research D5.
12. **Downloads, metadata, retries and activation are release's**, with its
    URL pin. Map row (python-env depends on release); decided.md D-Q2;
    research D6.
13. **Extraction uses `tar` and `flate2`** with sb-go's rules. Library
    first; the pure-Rust preference; research D7.
14. **Versions are compared with `semver`.** Stack table, Versions row;
    research D8.
15. **The Saltbox user comes from `config`**, read by `sb` and passed in;
    `reinstall-facts` reads none. Host spec Scope (`GetSaltboxUser` is
    config's); research D10.
16. **sb-go's step labels and texts are kept,** new texts only for new
    behaviour. Host and docker practice; map decision 1; research D11.
17. **The reconcile and its parts are a library for `ops`.** Map rows for
    `update-venv` and `reinstall-facts`; research D12.
18. **Failure and cancel cleanup is kept** (sb-go venv.go:163-171,
    259-264). Research D13; cli-core's cancellation token.
19. **Each command's step sequence stays sb-go's**; the healing lives
    inside `reconcile_venv` and `install_fact`, not in new steps. Research
    D14, with decision 3.
20. **The staged fact's `--version` must equal the tag** before activation,
    as decided for the staged sb binary (decided.md D-Q5): side-effect free
    (sb-go e82c34d), code the digest vouches for, and it catches a binary
    this host can't run.
21. **The amd64 fact asset is `saltbox-facts-amd64`**, the arch-named asset
    with the same bytes as the legacy name, so one naming rule covers both
    architectures. From decision 8.
22. **The deadsnakes cleanup moves the source files aside** into a
    root-only `/var/backups/sb/deadsnakes/<run id>/`, fenced the way ops'
    setup moves its apt sources (ops spec decisions 5 and 28, the same
    folder scheme and handle rules), instead of deleting them (difference
    12). Reason: the hardening rules for destructive operations; a removal
    that can't be undone has no reason to stay that way. Decided
    from recorded rules, 2026-11-13.

## Known limits

- **uv's digest covers the archive, not the installed binary.** A
  `/usr/local/bin/uv` that reports the right version but was altered isn't
  detected; `reinstall-python` doesn't reinstall uv either (sb-go's
  behaviour). A forced uv reinstall would be a new behaviour, not asked
  for.
- **The fact stays root-owned, mode 0755,** inside the Saltbox-owned
  checkout until the next venv reconcile chowns it (sb-go's behaviour).
- **The fact's directory is the Saltbox user's.** The staged fact is
  checked, run (`--version`) and activated by its path in
  `<checkout>/ansible_facts.d`, and the installed fact is run to read its
  version, as sb-go does: both are protected against the network, not
  against that user, who is the host's admin and whose checkout Ansible
  runs as root anyway. A local writer can also keep a fact that reports
  a newer version (decision 2 keeps a newer one). Added with module
  build M19, from its security review.
- **No lock between concurrent sb runs** (sb-go has none).
- **A broken published fact release** is installed and kept; the fix is a
  new release.

## Verification notes

- **Live acceptance (M21, 2026-11-15).** `crates/python-env/tests/live.sh`
  in throwaway containers of each supported release, one at a time: sb
  from this repository (a release build, without the `testing`
  feature), sb-go at 35a7acc, Saltbox's master toolchain files of
  2026-11-10 (`.uv-version` 0.13.4, `.python-version` 3.12.15, the
  hashed lock), saltbox.fact 1.1.0 from its latest release, ansible-core
  2.21.2 from the lock. The release metadata came through the proxy.
  Passed on 22.04.5, 24.04.5 and 26.04.1, each with the same results:
  - `sb update-venv` from nothing, from sb-go's hostile caller (uv, pip
    and Python variables and config files pointing elsewhere): uv and
    uvx 0.13.4 installed, libpq-dev installed through apt, Python 3.12.13
    (the start pin), the venv synced from the lock, saltbox.fact installed
    ("saltbox.fact not found. Proceeding with update."), one generation
    and one Python release kept, the trees the Saltbox user's, the
    `ansible` wrapper running, no scratch directory left;
  - run again: "Ansible virtual environment was already up to date" and
    "saltbox.fact is up to date", nothing changed (the link, the entries
    of both release trees, the manifest, the wrappers' bytes, the fact's
    bytes; owners and modes aren't compared);
  - sb-go's `update-venv` on that tree: up to date, nothing changed;
  - the pin bumped to 3.12.15: "Ansible virtual environment update
    required", a new generation on the new Python;
  - the fact changed by one byte: the digest warning, reinstalled with
    the release's bytes, nothing else changed;
  - `reinstall-venv` (a new generation on the same Python),
    `reinstall-python` (a new Python release) and `reinstall-facts`
    (the same bytes);
  - sb-go's `reinstall-venv`, then `sb update-venv`: sb-go's generation
    kept unchanged and up to date, and sb-go's `update-venv` after it
    kept it too.

  On 22.04 the first run also found what the old sb and pre-uv sb-go left
  (sb-go's deadsnakes script): deadsnakes' PPA added, its python3.12,
  -dev and -venv (3.12.16-1+jammy1) installed from it, and a venv on that
  Python as the active venv directory. The packages were removed, the
  PPA's source file moved into a 0700
  `/var/backups/sb/deadsnakes/<run id>/`, "Removed old deadsnakes Python
  packages" shown, and the legacy directory replaced by a generation
  link.

  The first run found that sb didn't make a missing `ansible_facts.d`, as
  sb-go does; fixed (fact install, step 3) before these results. The container's root git has the checkout in its
  `safe.directory`, as Saltbox's pre_tasks set it (repo spec, assumption
  2): without it, root's git refuses the checkout once the Saltbox user
  owns it, and the second run failed reading the commit (sb-go reads it
  with the same `git rev-parse HEAD`).

## Open questions

None. Decision 6's 100 KB threshold and decision 3's retry shape follow
from the maintainer's answer; they are named here so the review sees them.

## Amendments to other specs

**`release` (its own spec).** python-env needs eight things from
it. The release spec provides them in "The interface python-env needs",
under its names, which this spec uses:

> - **R1. Client:** `Releases::production(user_agent)` (reqwest with
>   rustls and aws-lc-rs, `https_only`, connect and per-read timeouts),
>   and `Releases::testing(..)` behind the `testing` feature.
> - **R2. Metadata:** `Releases::release(GithubRepo, Query::Tag(t) |
>   Query::Latest, asset, &mut Step)`: the SVM proxy first, then
>   `api.github.com`, with sb-go's warning; up to 4 attempts with
>   sb-go's backoff and `Retry-After` rules. `GithubRepo`'s owner is a
>   parameter (uv's is `astral-sh`).
> - **R3. Assets:** `Release { tag, version, asset }`, the asked-for
>   asset checked (name, size > 0, `sha256:<64 hex>` digest, the URL
>   pinned to `https://github.com/<owner>/<repo>/releases/download/`);
>   `Asset::sha256()` and `Asset::size()`; a missing asset is `release
>   <tag> is missing <name>`.
> - **R4. Architecture:** `Arch::current()` (`unsupported architecture
>   <a>` otherwise), `Arch::rust_name()` (`x86_64`, `aarch64`, for uv)
>   and `Arch::go_name()` (`amd64`, `arm64`, for the fact).
> - **R5. Verified download:** `Releases::download(&Asset, Stage { dir,
>   prefix, max_size }, &mut Step) -> Staged`, removed on drop.
> - **R6. Activation:** `activate(staged, target, mode)`, taking a
>   `Staged` or a caller's own staged file in the target's directory.
> - **R7. ELF check:** `check_elf(path, Arch) -> Result<(), ElfError>`
>   with `object`; python-env words its two variants with sb-go's fact
>   texts.
> - **R8. `sha256_file(path)`,** streaming.

**`host` (approved 2026-11-08).** Amendments (applied 2026-11-13):

> **H1. First users (decision 1).** "Ownership: `repo`" becomes
> "ownership: `python-env` or `repo`, whichever is built first". apt stays
> python-env's. The deny.toml entry for `sb-host` allows `sb-python-env`.
>
> **H2. Two read-only apt queries for python-env's deadsnakes cleanup,**
> under "apt":
> - `package_list()`: `dpkg-query -W
>   -f=${binary:Package}\t${source:Package}\t${db:Status-Abbrev}\t${Version}\n`
>   (capture, 10 s), returned as rows of four fields. Error: `list
>   packages: <cause>`.
> - `package_policy(name)`: `apt-cache policy <name>` (capture, 10 s, with
>   `LC_ALL=C` so the `***` and origin lines parse whatever the locale),
>   returned as text; python-env parses it. Error: `query package policy
>   <name>: <cause>`.
>
> **H3. `remove_sources_mentioning(needle, backup) -> Result<Vec<PathBuf>, _>`,**
> under "apt": moves each regular file directly in `sources.list.d`
> (on host's `/etc/apt` root field) whose content contains `needle` out
> of it, into a root-only `/var/backups/sb/<backup>/<run id>/`, and
> returns what it moved. `backup` is a `&'static str` folder name, never
> input (python-env passes `deadsnakes`); the run id is the run log's. A subdirectory or
> symlink is left alone. It uses the same fenced directory handles and
> folder scheme as `add_repositories`' cleanup (ops spec decisions 5 and
> 28): both directories are fixed paths (the root arguments exist for
> tests only); `sources.list.d` is opened `O_DIRECTORY | O_NOFOLLOW` and
> refused when it is a symlink, not root's, or group- or world-writable,
> and so are `/var/backups/sb` and `<backup>` (created 0700 when
> missing); the run's directory is created 0700 with `mkdirat`,
> exclusively, only when there is something to move; each entry is
> `lstat`ed and read through the handle and moved with `renameat` between
> the two handles, and across filesystems (`EXDEV`) copied with its bytes
> and mode, synced, then unlinked through the handle. Each name is logged
> before it moves. The Boundaries' "anything touching `/etc/apt` outside
> `add_repositories`" gains this function as the second allowed writer,
> with `/var/backups/sb/<backup>` as its backup target. Host's differences
> gain: the deadsnakes source files are moved into
> `/var/backups/sb/deadsnakes/<run id>/` through fenced handles instead of
> deleted by path (python-env decision 22).

*H3's symlink rule was amended in host.md on 2026-11-15 (maintainer): a
symlink whose target mentions the needle is moved, as a link, its target
never followed past that one link nor written; this spec's Testing case
"a symlinked entry moved as a link and never followed" is that rule.*

**`config` (approved).** The one `saltbox_user()` that the repo spec
also proposes (its wording is the repo spec's): `user.name` from
accounts.yml, read as sb-go's `GetSaltboxUser` reads it (no schema
validation), for `python-env`'s commands and `ops`. python-env's commands
add sb-go's wrapping, `error getting saltbox user: <cause>`.

**Capability map.**

> **M1.** `python-env` row: "uv-managed Python, Ansible venv build/update,
> deadsnakes cleanup, saltbox.fact install", depends on exec, term-ui,
> host, release. Command rows: `reinstall-facts` module python-env (was
> "ops, python-env"); the planning notes of `reinstall-facts`,
> `reinstall-python`, `reinstall-venv` and `update-venv` read "forced
> variants; the normal path heals itself (python-env decisions 1-3)".
> Stack table: one archives row shared with the ops spec, `Archives (uv,
> Speedtest) | tar + flate2 (rust_backend) | 0.4.46 / 1.1.10`, and an
> `object` 0.41.0 row (the ELF check, release and python-env; proposed
> by the release spec).

## Suggested slices for the plan

1. **First vertical slice (M): `sb update-venv` on a healthy tree.**
   Layout, toolchain files, the uv minimum, isolation, the uv `--version`
   probe (uv present), manifest reading, the environment check, wrapper
   rewrite, generation cleanup, ownership (host H1 built here: `ensure_owner`
   and `ensure_owner_recursive`), libpq-dev present (`package_installed`).
   Deadsnakes and fact are stubs returning "nothing to do" behind the same
   steps. Proves exec isolation and the step texts on the path every
   `sb update` hits.
2. **The build path (M):** Python install and find, `uv venv`, `pip sync`
   streamed, discovery, staged check, manifest write, wrappers,
   activation with legacy adoption, stale wrappers, failure and cancel
   cleanup. Adds `reinstall-venv` and `reinstall-python`.
3. **Self-healing (S):** the wider Python check and the clean retry.
4. **uv install (M):** needs release's `Releases`, `release`, `download`, `activate` and `Arch` (R1-R6). Metadata, download,
   extraction, staged `--version`, activation; the libpq-dev apt steps
   (host's `install` and `update_lists`, first use).
5. **The fact (M):** needs release's `check_elf` and `sha256_file` (R7-R8). Version probe, decision with
   the digest rule, download, ELF and staged `--version`, activation;
   `reinstall-facts` and update-venv's real fact step. Measures `object`.
6. **Deadsnakes (S):** host H2 and H3, the parsing from fixtures, the
   cleanup with its fenced move aside; update-venv complete.
7. **Live acceptance (S):** the container test on 22.04, 24.04 and 26.04,
   both directions with sb-go.
