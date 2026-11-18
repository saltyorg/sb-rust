# Spec: `ops` (approved 2026-11-13)

Approved by the maintainer as drafted (2026-11-13); a change after the
maintainer's read-through becomes a fix
task. *Amended 2026-11-15: `sb announcements` is
shown and only reads (decision 27, differences 31 and 35 to 37); both
repositories by default (decision 30, difference 38); lenient without
`-r`, its step line a count (decision 31, difference 39; 2026-11-16).
Amended 2026-11-17: no flags of its own, both repositories always, each
entry labelled with its repository, nothing called new (decisions 27, 30
and 31, difference 45).
Amended 2026-11-16: bench's System rows and disk test
follow sb-go's bench.sh row by row (decision 33, differences 19 and 46
to 53); the help contract is a rewording. Amended 2026-11-16 at M78:
the network rows and Speedtest follow bench.sh the same way (decision
33, differences 54 to 58), the aarch64 pin recorded (decision 13).
Amended 2026-11-18 by the maintainer's decisions: `update` ends with
what moved and a closing hint (step 13, "What moved, and the closing
hint"; decisions 36 to 38, differences 62 and 63).*

Module of the approved capability map (`ops`): the workflow commands
that tie the other modules together. The build order puts `ops` last
(step 7), so `host`, `repo`, `release`, `python-env`, `config` and
`completion` exist when it starts. It covers `setup`, `restore`, `bench`
and `gha`, the install script that moves into this repository (the map's
open item), and `update`, `branch`, `branch-sandbox` and `announcements`,
which orchestrate `repo`, `release`, `host`, `python-env`, `ansible`,
`config`, `tui` and `completion` and own only the order, the prompts and
the announcement rules. The behaviour comes from sb-go (`cmd/setup.go`,
`cmd/restore.go`, `cmd/bench.go`, `cmd/gha.go`, `cmd/update.go`,
`cmd/branch.go`, `cmd/branchSandbox.go`, `cmd/announcements.go`,
`saltbox/setup.go`, `saltbox/announcements.go`, `host/benchmark.go`,
`install.sh`), read 2026-11-13.

## Assumptions

1. **sb-go is the reference** for `setup`, `restore`, `gha`, `update`,
   the branch commands and `announcements`: their order, texts and file
   modes, with the repo and release specs' differences. Every difference
   is listed below.
   `bench` is rewritten in Rust (maintainer, 2026-11-13), so its output
   is new and listed as one difference.
2. **All of them need root** (cli-core decision 3). Not root, sb relaunches
   through cli-core's `sudo -u root --preserve-env=...` exec. sudo sets
   `SUDO_USER`, which `gha` reads.
3. **cli-core's runtime release check runs first** (22.04, 24.04,
   26.04). `setup` then applies its own list (24.04, 26.04). `gha` has
   no check of its own, so it runs on 22.04, as Saltbox's CI needs.
4. **The other modules give ops what it calls.** ops adds no apt, git,
   uv, fact or completion code of its own. The calls it needs:
   - `host`: `check_setup_support`, `check_architecture`, `check_lxc`,
     `check_desktop`, `apt::update_lists`, `apt::install`,
     `add_repositories`, `ensure_owner` (host spec), and `free_space`
     (an amendment, below), `check_disk_space` (update);
   - `repo` (approved 2026-11-13): `Repo::open`, `clone(url, branch, step)` and
     `init_from_remote(url, branch, step)` for setup, `current_branch`,
     `head`, `check_branch_name`, `fetch_and_reset(branch, owner, step)`,
     and `branch_choice(current, wanted, answer)`, which answers `Ask`
     when the branches differ and no answer was given. The prompt itself
     is the command's, so ops asks it (repo spec, assumption 5);
   - `release` (approved 2026-11-13): `Releases::download` with an `Asset::pinned`
     (bench's Speedtest), and self-update's flow with the answer given
     (`update`);
   - `ansible` (approved 2026-11-13): `TagCache::refresh(repo, step)` after each pull,
     and its playbook run by tag for the migrations;
   - `tui` (approved 2026-11-13): the announcement viewer and its inline Markdown form;
   - `python-env` (approved 2026-11-13): `reconcile_venv(step, env, user, Force::None)`
     with `user` `None` for gha, and `install_fact(step, env,
     FactMode::Update)`, called directly in setup's and gha's own order;
     `reconcile_runtime(step, env, user)` for `update` and `branch`
     (decision 21);
   - `config`: `saltbox_user()` (an amendment, below), and
     validate-config's checks for `update`;
   - `completion` (approved 2026-11-13): `refresh(step, paths, executable)`.
5. **The Saltbox root** is `/srv/git/saltbox`, or `SB_SALTBOX_DIR` when
   set (config spec). Tests use it; sudo's relaunch drops it.
6. **No consumer changes.** Saltbox's CI keeps calling `sb gha`, and
   install.saltbox.dev keeps serving today's installer until the
   maintainer publishes the new one (map assumption 1).
7. **The repositories for `update` and the branch commands:** Saltbox
   at the Saltbox root (`SB_SALTBOX_DIR` in tests, as validate-config),
   Sandbox at `<server_appdata_path>/sandbox` (cli-core's
   `appdata_path`), default branch `master` for both.
8. **No Saltbox role or script runs `update` or the branch commands;**
   users run them (https://docs.saltbox.dev/saltbox/basics/update).
   Scripted runs are still supported (decided.md D-Q4).

## Objective

A Saltbox user installs Saltbox on a fresh Ubuntu server with one
documented command, gets their backed-up config back with `sb restore`,
and can measure the server with `sb bench`. Saltbox's CI prepares a
runner with `sb gha`. Each does what sb-go does, with sb's runner, its
run log, and secrets kept out of output and logs.

`sb update` brings Saltbox, Sandbox, their Python runtime and sb itself up
to date in one run, shows what changed, and runs the migrations the user
agrees to, whether a person runs it at a terminal or a script runs it
with flags. `sb branch <name>` and `sb branch-sandbox <name>` switch a
checkout to the named branch, as asked, every time.

### `sb setup` (hidden)

**Flags:** `--branch/-b <name>` (default `master`), `--force`,
`--keep-branch` / `--reset-branch` (mutually exclusive, as on `update`;
decision 29), and `--verbose/-v`, hidden and ignored (decision 1). The flags are clap
`global`, as sb-go's persistent flags, so the contract test's lines
match. Help texts are sb-go's.

**1. Guard** (sb-go `checkSetupAllowed`, setup.go:68-88). `lstat` of the
Saltbox root:
- missing: go on;
- a symlink: `<root> exists but is a symlink`, even with `--force`;
- not a directory: `<root> exists but is not a directory`, even with
  `--force`;
- a directory without `--force`: `setup already exists at <root>; setup
  is intended for fresh installations only (use --force to run it
  anyway)`;
- any other lstat error: `inspect existing Saltbox installation:
  <cause>`.

So `--force` allows exactly one thing: running over a real directory.

**2. Branch.** When `<root>/.git` exists, repo's branch resolution
compares the checkout's branch with `-b`. If they differ, it asks
`Saltbox: Do you want to reset to the '<b>' branch?` through term-ui's
confirm prompt, on the terminal, not stdin (difference 2), or takes the
answer from `--keep-branch` or `--reset-branch` without asking. With no
terminal and neither flag, setup fails before any step with an error
naming both flags, exit 1 (decision 29; difference 6). It only arises
with `--force` over an existing checkout on another branch.

**3. The runner tree.** Root step `Installing Saltbox` (success
`Saltbox installation completed`), then these phases in order, each
named as in sb-go (`<name>`, success `<name> completed`):

1. **Checking system compatibility:** `Checking Ubuntu version`
   (`check_setup_support`), `Checking CPU architecture`, `Checking for
   LXC container`, `Checking for desktop environment`. The texts are the
   host spec's.
2. **Installing system prerequisites** (`prerequisites`, shared with
   gha; error `error during initial setup: <cause>`):
   1. `apt::update_lists`;
   2. install `git curl`;
   3. create `/srv/git` and `/srv/ansible`, 0755 when created;
   4. install `software-properties-common apt-transport-https`;
   5. `add_repositories` (**destructive**, see below);
   6. `apt::update_lists` again;
   7. install `locales nano wget jq file gpg-agent libpq-dev
      build-essential libssl-dev libffi-dev python3-dev
      python3-testresources python3-apt python3-venv python3-pip`.
3. **Configuring system locale** (`locale`, shared with gha; error
   `error configuring locale: <cause>`), sb-go's `ConfigureLocale`
   (saltbox/setup.go:92-185):
   - `locale -a` (capture). Without `en_US.UTF-8` in it, a step
     `Generating locale en_US.UTF-8` runs `locale-gen en_US.UTF-8`; its
     failure ends the phase (`error generating locale: <cause>`).
   - A step `Setting system-wide locale (LC_ALL and LANG) to
     en_US.UTF-8` runs `update-locale LC_ALL=en_US.UTF-8
     LANG=en_US.UTF-8`. A failure is an info line (`update-locale
     failed, attempting fallback: <cause>`), not an error.
   - Read `/etc/default/locale`. Missing is empty; another read error is
     a warning line. If either `LC_ALL=en_US.UTF-8` or
     `LANG=en_US.UTF-8` is missing, a step `Locale not set correctly,
     reconfiguring locales...` runs `dpkg-reconfigure locales` with
     `DEBIAN_FRONTEND=noninteractive` (failure ends the phase:
     `error reconfiguring locales: <cause>`), then the file is read
     again.
   - A variable still missing gives sb-go's warning line for it. Never
     an error.
4. **Preparing Saltbox repository** (error `error setting up Saltbox
   repository: <cause>`):
   - no root: repo's clone of `https://github.com/saltyorg/saltbox.git`
     at `-b` (sb-go: `--depth 1`), then the submodule update;
   - a root without `.git` (only with `--force`): repo's init, remote,
     fetch, branch and hard reset, then the submodule update;
   - a checkout: repo's fetch and reset to the resolved branch;
   - then `install_fact(Update)` (python-env);
   - then `copy_default_configs` (below);
   - then `git_hooks` (below; error `error initializing Git hooks:
     <cause>`).
5. **Installing Python runtime:** the Saltbox user from accounts.yml
   (`get Saltbox user: <cause>` on failure; on a fresh host this is the
   default just copied), then `reconcile_venv` with that user.

**4. Completions.** After success, completion's refresh of installed
registrations, as `update`'s step 12 runs it (a step "Checking shell
completions" after `Installing Saltbox`; M87). A failure is a warning
line, never the exit code.

**Exit:** 0; 1 with cli-core's error block; an interrupt dies of its
signal (`$?` 130 or 143).

**The destructive step.** `add_repositories` clears every entry of
`/etc/apt/sources.list.d` except `ubuntu.sources` on DEB822 releases
(host spec, "apt"; sb-go apt.go:274-425, which deletes them). On a server
that already had third-party repositories (Docker's, a GPU vendor's),
they no longer apply after setup. It runs on every `setup` and every
`gha`, including `setup --force` over an existing install. Under the
security rules for destructive operations, the target is never derived
from input, the operation is fenced, and what it takes away can be put
back (decisions 5 and 28, host amendment below):
- **Fixed root:** the path is a constant, `/etc/apt/sources.list.d`,
  never taken from config, environment or arguments (tests pass a root
  to the library function, never to the command).
- **The root is what it claims:** opened with `O_DIRECTORY |
  O_NOFOLLOW`; a symlink, a non-directory, or a directory not owned by
  uid 0 or writable by group or others is refused before anything is
  moved (`refusing to clean <dir>: <reason>`), the step fails, and setup
  stops.
- **Moved aside, not deleted** (decision 28): the entries go into a
  root-only `/var/backups/sb/apt-sources/<run id>/`, the run id being
  the run log's. `/var/backups/sb` and `apt-sources` are created 0700
  when missing, and refused like the source directory when they are a
  symlink, not root's, or group- or world-writable; the run's own
  directory is created 0700 with `mkdirat`, exclusively.
- **One level below, no following:** each entry is `lstat`ed through
  the open directory (`statat` with `AT_SYMLINK_NOFOLLOW`) and moved
  with `renameat` from that directory handle to the backup directory's
  handle, so a swapped path can't redirect it. A regular file or a
  symlink is moved (the link itself, never its target). A subdirectory
  fails, as sb-go's removal does. Where `/var/backups` is on another
  filesystem than `/etc` (`EXDEV`), the entry is copied (a regular file's
  bytes and mode; a symlink as a symlink), synced, then unlinked through
  the handle.
- **Evidence before acting:** the names to move are listed first and
  written to the run log, then moved; the step shows `Moved <n> entries
  from /etc/apt/sources.list.d to /var/backups/sb/apt-sources/<run id>`
  and names each in the log. With nothing to move, no backup directory
  is created.
- **Old copies** stay until the user removes them; they are a few small
  text files per run.

**`copy_default_configs`** (sb-go saltbox/setup.go:436-469), shared with
gha. For each `<root>/defaults/*.default`, in name order:
- the destination is `<root>/<name without .default>`;
- missing: a step `Copying <file>.default` copies it, created
  exclusively (`O_CREAT | O_EXCL`, 0600, no following);
- present: `lstat`; a symlink is refused (`configuration file <path>
  must not be a symlink`), another non-regular file too (`configuration
  file <path> is not a regular file`), in sb-go's words; a regular file
  gets 0600 as `secure_existing_configs` gives it; its content is
  untouched. *Amended 2026-11-16 (module build M80): the texts are
  sb-go's (setup.go `secureExistingConfigFile`), where this line had
  paraphrased them.*

`secure_existing_configs` is the same without copying (sb-go
setup.go:380-433): for each `<root>/defaults/*.default`, the file without
`.default` in the root, if present, is checked with `lstat` and refused
the same way when it isn't a regular file, then gets 0600 through an
`O_NOFOLLOW` handle checked to be the same file (`fchmod`). `update`
calls it (below). It lives here, in ops, beside `copy_default_configs`,
not in config.

**`git_hooks`** (sb-go saltbox/setup.go:314-341), shared with gha: when
`<root>/bin/git/init-hooks` exists, `bash bin/git/init-hooks` in the
root through exec's `stream`; missing is skipped silently.

### `sb gha` (hidden)

Saltbox's CI calls it in its role jobs on 22.04, 24.04 and 26.04
(Saltbox `.github/workflows/saltbox.yml` and `saltbox-os.yml`, read
2026-11-13). The job:
1. installs sb-go's `latest` release asset (`linux`, `amd64`) as
   `/usr/local/bin/sb` and runs `sb version`;
2. `sudo mkdir -p /srv/git`, `sudo chown -R runner:runner /srv/git`,
   then `ln -s $GITHUB_WORKSPACE /srv/git/saltbox`: **the Saltbox root
   is a symlink** to the checkout;
3. `/usr/local/bin/sb gha && /srv/ansible/venv/bin/ansible --version`,
   **as `runner`, not with sudo**, so sb relaunches and `SUDO_USER` is
   `runner`;
4. `/usr/local/bin/uv pip freeze --python /srv/ansible/venv/bin/python`;
5. `sed -i 's/seed/runner/g' accounts.yml` as `runner`, so the copied
   configs must be the runner's;
6. `sudo /srv/ansible/venv/bin/ansible-playbook saltbox.yml ...` with
   `--extra-vars @vars.yml`.

So gha's contract with Saltbox is: it works with a symlinked root, as a
relaunched `runner`, and leaves `/srv/ansible/venv/bin/ansible`,
`/usr/local/bin/uv`, `saltbox.fact`, and the six config files owned by
`runner` with mode 0600. sb-go's test-gha.yml asserts the same
(`runner:runner:600` on accounts.yml and settings.yml).

**Behaviour** (sb-go gha.go:35-75), in this order, one root step
`Installing GHA dependencies` (success `GHA dependencies installed`):
1. `prerequisites` (as setup's phase 2);
2. `locale`;
3. `reconcile_venv` with **no** Saltbox user (`error reconciling
   Python environment: <cause>`);
4. a step `Checking saltbox.fact` (success `saltbox.fact is ready`):
   `install_fact(Update)`;
5. `copy_default_configs`;
6. `ensure_owner($SUDO_USER, ...)` on `accounts.yml`, `adv_settings.yml`,
   `backup_config.yml`, `hetzner_vlan.yml`, `providers.yml`,
   `settings.yml` in the root (`SUDO_USER` trimmed; empty or unknown
   skips, host spec), error `error assigning GHA configuration
   ownership: <cause>`;
7. `git_hooks`.

No release, architecture, LXC or desktop checks, no clone, no guard, no
completion refresh. Python comes before the fact and the configs, unlike
setup. The guard's symlink refusal is setup's alone and never reaches
gha's path (a test pins it). No flags.

### `sb restore` (hidden)

Fetches a user's backed-up config from Saltbox's restore service and
places it in the Saltbox root. Saltbox's backup role uploads it
(`roles/backup2/tasks/restore_service.yml`); the docs order is install
script, restore, `sb install preinstall`, `sb install restore`.

**Flags:** `--user <name>` and `--password-file <path|->` (decision 8).
No password flag and no `-v` (*amended 2026-11-18: `-v` accepted hidden,
with a notice: the maintainer's decision of 2026-11-18*). Help texts are sb-go's for the command;
the two flags' help: `Restore service user name (asked on a terminal
when not given)` and `Read the password from this file, or stdin with
'-' (asked on a terminal when not given)`.

**On a terminal** (stdout one) and an answer missing (the maintainer,
2026-11-16 and 2026-11-17, decision 32), `sb restore` is tui's restore
screen. With both `--user` and `--password-file` given, there is no
screen: the line form, as without a terminal.

- **The form:** `Username`, `Password` and `Repeat password`, each a
  ratatui-textarea line; the two passwords masked (`•`), the user name
  shown as typed. Tab, Shift-Tab, ↑ and ↓ move between the fields,
  Enter moves to the next and submits from the last. An empty user name
  shows `Enter the user name`, an empty password or two that differ
  sb-go's `Error: passwords do not match or are empty`, in the form, and
  nothing starts. Esc closes it without restoring, sb-go's `Restore
  cancelled.`, exit 0. `--user` or `--password-file` fills its fields
  (the file's bytes are sent as read unless a password field is edited).
- **The live list,** once submitted: the nine files as rows, each
  waiting, then fetching, then `DONE`, `IGNORED` or `FAIL`, a failure
  with its reason (the line form's note: the wrong-password line, a
  technical error, or the transport error). The hints name Ctrl-C to
  stop the run.
- **The viewer,** once the run ends: a line asks the user to check that
  each restored file is decrypted, correct, and the right configuration
  file; under it the menu of the nine files, each with its mark, beside
  the selected file's content (above it below 70 columns), and a
  Continue button.
  - The menu: ↑, ↓, `j`, `k`, Home, End, a click or the wheel select a
    file; Enter moves into its content. A failed or ignored file can't
    be opened: selected, the pane shows why (the failure's reason, or
    that the service has no such file).
  - The content: the file as placed, read-only, each line made safe,
    wrapped, scrolled with a line cursor (↑, ↓, PgUp, PgDn, Home, End,
    the wheel, a click on a line); Esc goes back to the menu.
  - Secrets are masked (`key: ••••••••`) by the shared rule (below); `s`
    shows the cursor line's value until the cursor moves, `a` shows all
    of them until pressed again; the pane's title says when all show.
  - Tab and Shift-Tab move between the menu, the content and Continue.
    Continue (Enter on it, `c`, a click on it, or `q` and Esc in the
    menu) closes the screen. Restore has no step after it, so Continue
    ends sb restore; a later step of a flow would follow it.
- **The masking rule** is sb-config's, shared with the fact editor
  (`facts::secret::is_secret`): a key whose name, in any case, contains
  `pass`, `secret`, `token`, `key`, `api`, `apprise`, `auth`,
  `credential`, `hook`, `hmac` or `claim`, or has `kid` or `eab` as a
  whole word (amended 2026-11-18, tui spec's Secrets), or a value whose
  first word (a comment after it ignored) is a URL with a user or
  password in it, or with a query parameter named by such a word or
  `sig` (an Azure SAS signature). A bare URL, a list item's or a whole
  line's, is judged as a value too.
  When the screen closes, every shown value is hidden again and one more
  frame drawn, so a terminal without an alternate screen (GNU screen's
  default) keeps a masked frame. The run
  log never holds a restored file's content; a URL it logs drops its user
  and password and masks such query values (`http::loggable_url`).
- **Leaving:** Ctrl-C ends sb with 130 at any time, quietly, as the
  viewers do, the run stopped and the files so far listed; a first
  SIGTERM or SIGHUP stops the run and closes the screen. Once the screen
  is gone, the final list prints on stdout in the line form (below),
  each failure's reason indented under its line (a transport failure's
  too, which the line form leaves bare), then the count line or the
  error, so the scrollback keeps it. Rows not yet ended are left out.

**Without a terminal** (stdout not one), or with both flags given, the
line form: the user from `--user`, the password from `--password-file`,
each required (an error naming the flag, exit 1, never a hang); an empty
name is refused (difference 11).

**The password file:** `--password-file <path>` reads at most 4 KiB from
the path, `-` from stdin; a longer file is refused (`the password file
is over 4 KiB`), not cut; one trailing `\n` (or `\r\n`) is dropped; an
empty result is refused (`the password file is empty`); its content never
appears in an error. On a terminal with `-` and stdin a terminal, the
form asks instead (reading the terminal would echo it).

**Protocol** (Saltbox's backup role and sb-go restore.go:236, 368-470;
decision 7):
- the user key is the lowercase hex SHA-1 of the user name's bytes;
- `GET https://crs.saltbox.dev/load/<key>/<file>` for each of, in this
  order: `accounts.yml adv_settings.yml backup_config.yml
  hetzner_vlan.yml localhost.yml motd.yml providers.yml rclone.conf
  settings.yml`;
- one reqwest client: 30 s per request, a `User-Agent` of
  `sb/<version>`, **no redirects** (difference 13);
- a non-200 answer is `[IGNORED]` (how a file that was never uploaded
  shows); a transport error, a body over 16 MiB, a missing `Salted__`
  header, or a decryption failure is `[FAIL]`;
- the base URL follows `SB_RESTORE_URL` when set (tests and debugging;
  `http://` allowed; sudo's relaunch drops it), as config's
  `SB_DOCKERHUB_URL`.

**Decryption:** OpenSSL's `enc -aes-256-cbc -pbkdf2` format, which the
backup role writes with OpenSSL 3's defaults:
- `Salted__`, an 8-byte salt, then the ciphertext;
- PBKDF2-HMAC-SHA256, 10000 iterations, 48 bytes: the key (32), then
  the IV (16);
- AES-256-CBC, PKCS#7 padding;
- a plaintext that isn't UTF-8 counts as a wrong password: nothing is
  placed. Every file the service keeps is text, and a wrong key passes
  the padding check about once in 256 files (the maintainer, 2026-11-16;
  difference 44);
- a padding failure or a plaintext that isn't text prints `Decryption
  failed. This likely means the password was incorrect`; any other
  failure (the format, the length) prints `  (Technical error:
  <cause>)`.

The whole body is held in memory (at most 16 MiB) and decrypted there.
The password, the derived key and IV, and each plaintext are held in
`Zeroizing` buffers and cleared on drop.

**Placement** (sb-go's result, sb's means; decision 9):
- the Saltbox root is created 0755 if missing (sb-go; restore normally
  runs after setup);
- each plaintext is written to a `tempfile::NamedTempFile` in the root
  (0600, exclusive, unpredictable name) and persisted over
  `<root>/<file>` with `rename`, so a destination symlink is replaced,
  never followed, and a reader sees the old file or the new one;
- root-owned, mode 0600, no chown, every file at the root level
  (Saltbox's restore and rclone roles move `localhost.yml` and
  `rclone.conf` from there);
- an existing file is overwritten without a backup, as in sb-go;
- a failure or a first signal (cli-core's cancel) leaves no temporary
  file (dropped on every path); the root is synced after each rename;
- the temporary file is set to 0600 whatever the umask.

**Output** (the line form, sb-go's lines, on stdout; on a terminal after
the screen closes):
`Fetching files from https://crs.saltbox.dev...`, then one line per file,
the name padded to 20 characters, then ` [DONE]`, ` [IGNORED]` or
` [FAIL]`, then `Restore process completed: <n> files successfully
restored.`

**Exit:** at least one file restored, 0 (partial success too); none,
the error `restore process failed: no files were downloaded or
decrypted`, exit 1.

**Secrets** (decision 10):
- the user key is a credential: Saltbox's
  `wipe_restore_service.yml` deletes a backup with `POST
  /delete/<key>` and nothing else. The user name gives the key, so both
  are secrets;
- neither the user name, the key, the password nor any URL with the key
  reaches output, errors or the run log. URLs are logged as
  `<base>/load/[REDACTED]/<file>`; reqwest errors are logged and shown
  through `reqwest::Error::without_url`
  (https://docs.rs/reqwest/latest/reqwest/struct.Error.html#method.without_url);
- cli-core's run record never logs argument values (cli-core spec,
  startup step 4), so `--user` stays out of it;
- prompt answers are never logged (term-ui);
- no child process is started; `--user`'s value is in sb's own argv,
  though, so the process list and, when sb relaunches through sudo or is
  run with it, sudo's command log show the user name. The long help says
  so: leaving `--user` out asks instead (decision 10's limit, below).

### `sb bench`

Rewritten in Rust (maintainer, 2026-11-13; decision 11). It measures the
same things as sb-go's bench.sh, in sb's own output. Nothing is fetched
and run as code except the digest-pinned Ookla Speedtest.

**Help:** `Benchmarks this server's system, disk and network` (sb-go:
`Runs bench.sh benchmark`; difference 15). Not hidden. No flags.

**Progress:** term-ui's runner on stderr, a step per measurement
(`Reading system information`, `Disk write test (n/3)`, `Preparing
Speedtest`, `Speedtest: <node>`), each shown only while it runs (term-ui's
transient steps), so a run that succeeds leaves only the results; a
section without steps starts no runner. **Results:** three term-ui tables on stdout, each printed when
its section ends, then the total time and a timestamp. No `clear`. On a
terminal the tables fit its width; when stdout isn't a terminal they
lay out as at 80 columns, their cells wrapped as on a terminal that
wide, so a long cell (a Speedtest that couldn't be prepared, its
reason) doesn't draw the table wider (the maintainer, 2026-11-18).
`COLUMNS` doesn't change it, as sb-go's piped wrap. This is bench's own
choice; term-ui's piped tables otherwise keep their natural width
(term-ui spec, "Piped").

**1. System** (a two-column table; each row found as sb-go's
bench.sh finds it, `cmd/bench_script.sh`'s `get_system_info`,
`check_virt` and `print_system_info`, in process where the script ran a
program; decision 33):

| Row | Source |
|---|---|
| CPU Model | `/proc/cpuinfo`'s last `model name` (procfs's `CpuInfo`); on ARM lscpu's `Model name`, in process sysinfo's `Cpu::brand` (the same implementer and part names); `CPU model not detected` when empty |
| CPU Cores | the count of `processor` lines (blank for none), `@ <MHz> MHz` with the first `cpu MHz` as written (on ARM lscpu's `CPU max MHz`: the largest `cpufreq/cpuinfo_max_freq`, four decimals, else `cpu MHz`), omitted when none |
| CPU Cache | `/proc/cpuinfo`'s last `cache size`, the row left out when absent |
| AES-NI | `aes` anywhere in `/proc/cpuinfo`, any case (`grep -i aes`): `✓ Enabled` / `✗ Disabled` |
| VM-x/AMD-V | `vmx` or `svm` anywhere, any case (`grep -Ei`), on every architecture |
| Total Disk | df's total of `simfs ext2 ext3 ext4 btrfs xfs vfat ntfs` (`/proc/self/mounts` through procfs, only those types looked at; each mount point's device, rustix's `stat`, once, as df's `filter_mount_list`; rustix's `statvfs`: size `f_blocks × f_frsize`, used `(f_blocks - f_bfree) × f_frsize`), plus the swap's total and used, plus `zpool list -o size,allocated -Hp`'s sums (exec, 5 s; none without zpool); `(<used> Used)`; the filesystems are read within 5 s, else the value is blank |
| Total RAM | `free`'s total and used from `/proc/meminfo` (procfs's `Meminfo`): used is total − available as procps 4.0.4 (24.04, 26.04; available is `MemFree` when `MemAvailable` is 0 or above the total), total − free − buffers − (cached + `SReclaimable`) as procps 3.3.17 (22.04, from os-release's `VERSION_ID`), either total − free when negative; `(<used> Used)` |
| Total Swap | `free`'s swap: total, used total − free; left out when 0 |
| System Uptime | `System::uptime()` split into whole days, hours and minutes, each singular or plural as its number needs: `117 days, 20 hours, 20 mins`, `1 day, 1 hour, 1 min` (difference 46) |
| Load Average | `System::load_average()`, `<1>, <5>, <15>` with two decimals, as `w` |
| OS | os-release's `PRETTY_NAME` (bench.sh's `awk -F=`, a double quote at either end removed), else lsb-release's `DISTRIB_DESCRIPTION`, else redhat-release's text |
| Arch | `System::cpu_arch()` (`uname -m`) and `(<n> Bit)`, `n` the C `long`'s bits (`getconf LONG_BIT`) |
| Kernel | `System::kernel_version()` (`uname -r`) |
| TCP Congestion Ctrl | `/proc/sys/net/ipv4/tcp_congestion_control` (`sysctl -n`) |
| Virtualization | `systemd-detect-virt`'s stdout (exec capture, 5 s, any exit code) upper-cased; `none`, nothing or a failure is `Dedicated` (difference 16) |
| IPv4/IPv6 | `GET https://icanhazip.com` bound to `0.0.0.0` and to `::` at once (reqwest `local_address`), 4 s each, no redirects, an answer over 64 KiB refused (difference 58): a family is `✓ Online` when its answer has any text but newlines (bench.sh's `$(curl -s ...)`), else `✗ Offline`; `<v4> / <v6>` in one cell, the Ok role with both online, Problem with neither, Warn otherwise (difference 54) |
| Organization, Location, Region | `GET https://ipinfo.io/json`, 4 s (difference 17): `Organization` when `org` is there, `Location` (`city / country`) with both, `Region` when `region` is there, and a `Region` row `No ISP detected` without `org`, as bench.sh's `ipv4_info`; a field of nothing but newlines counts as none, as `-n "$(...)"` |

Sizes are bench.sh's `calc_size` with binary unit names (`KiB` to
`TiB`, one decimal; nothing is `0 KiB`). A value the host doesn't give
is blank, as the script printed an empty variable. The address fields
in either answer are never printed or logged.

**2. Disk write** (in-process; decision 12):
- needs at least 2 GiB free on the working directory's filesystem
  (host's `free_space`), else the row `Not enough space for the disk
  test` and the section ends; free space that can't be read gives the
  row `Unable to determine available space for the disk test`
  (bench.sh's `Unable to determine available space for I/O test!`,
  reworded as difference 15) and the section ends;
- three runs, each writing 1 GiB in 512 KiB blocks of zeros to a
  `tempfile::NamedTempFile` in the working directory (0600,
  exclusive), then `File::sync_data` (fdatasync), timed from the first
  write to the end of the sync, the file removed after each run and on
  every interrupted path;
- the blocking writes run under `spawn_blocking`; the shutdown token is
  checked between blocks, so Ctrl-C stops within one block;
- rows `1st run`, `2nd run`, `3rd run`, `Average`, in MB/s (10^6 bytes,
  as dd reported), one decimal; the average is of the runs that
  measured (difference 47); a run whose writes fail, or a later run
  that can't create its file, is `Test failed`;
- an unwritable working directory gives the row `No write permission
  in the working directory` and the section ends (difference 48).

**3. Speedtest:**
- the pinned asset (decision 13) through release's verified download
  into a private 0700 temporary directory in `/tmp` (not `$TMPDIR`;
  difference 58), then the single regular
  entry `speedtest` extracted with `tar` and `flate2` (at most 64 MiB,
  created exclusively, 0700; a duplicate, a link, an empty or oversized
  entry, or none, refused with sb-go's texts), then an ELF check for the
  running architecture (release's `check_elf`: 64-bit, `e_machine` 62 on
  x86_64, 183 on aarch64); the archive is removed once read;
- each node runs `speedtest --format=json --progress=no
  --accept-license --accept-gdpr [--server-id=<id>]` through exec's
  `capture`, 120 s each, with `HOME` set to the temporary directory and
  `XDG_CONFIG_HOME` removed so the licence file
  (`$HOME/.config/ookla/speedtest-cli.json`) goes with it
  (docs/platform-notes/speedtest.md), the rest of sb's environment kept
  as sb-go's;
- nodes, sb-go's (bench_script.sh:115-128): Speedtest.net (no id), Los
  Angeles, US 7190, Dallas, US 22288, Montreal, CA 64420, Paris, FR
  61933, Amsterdam, NL 41423, Suzhou, CN 5396, Ningbo, CN 59387, Hong
  Kong, CN 32155, Singapore, SG 13623, Taipei, CN 65092, Tokyo, JP
  48463;
- a `Speedtest` title row across the table, as System's and Disk
  write's, over bench.sh's columns as headings (term-ui's
  `Table::titled`; the maintainer and 2026-11-17),
  `Node Name | Upload Speed | Download Speed | Latency`: the JSON's
  `upload.bandwidth` and `download.bandwidth` (bytes per second) as
  `<n> Mbps` (× 8 / 10^6, two decimals) and
  `ping.latency` as `<n> ms` (two decimals), the values the CLI's text
  form prints (difference 55); a run that doesn't exit 0 is the row's
  `Test failed`, an answer without the three values `Failed to parse
  results`, as bench.sh's, and the next node runs;
- the JSON's interface, server and result fields are never read, and
  the CLI's output reaches neither the screen nor the log;
- a Speedtest that can't be prepared (the architecture, the client, the
  download or its digest, the archive, the ELF check) is the section's
  one row, `Speedtest unavailable: <cause>`, and the others still print
  (difference 56);
- a `testing` build (sb's own tests) reads the network endpoints and the
  pin from the file `SB_BENCH_TEST_NETWORK` names and the download's
  fake from `SB_RELEASE_TEST_BASES`, and asks nothing without them.

**Exit:** 0 when every section ran, even with failed rows; 1 only when
sb itself fails (the runtime, the terminal); an interrupt dies of its
signal, the disk test's file and the temporary directory removed.

### `update`, `branch`, `branch-sandbox` and `announcements`: commands and help

| Command | Arguments and flags | Hidden |
|---|---|---|
| `update` | `--keep-branch` / `--reset-branch` (conflict: clap's error, exit 2), `--skip-self-update`, `--migrations <run\|skip>` (new) | no |
| `branch <name>` | the branch, required | no |
| `branch-sandbox <name>` | the branch, required | no |
| `announcements` | none (decision 27, amended 2026-11-17: sb-go's `--before`, `--after` and `--repo` removed, difference 45) | no |
| `announcements preview` | `--since <ref>` (default `HEAD`), `--repo <saltbox\|sandbox>`, `--run-migrations` (decision 34, 2026-11-18) | yes |
| `announcements check` | `[file]`, `--repo <saltbox\|sandbox>` (decision 35, 2026-11-18) | yes |

Help texts are sb-go's (`Change the branch used by Saltbox`, and the
others from cmd/). No `-v` anywhere (decision 18). sb-go's usage line
`branch [branch_name]` becomes `branch <name>`: the argument was always
required (`cobra.ExactArgs(1)`, cmd/branch.go:24).

### `sb update`

#### Before anything changes

1. **Terminal check, replaced** (decided.md D-Q4): sb-go refused to run
   without a TTY (fb512a8). sb runs without one when flags answer every
   question that can come up. The questions are known up front except
   the migrations one, so without a terminal sb requires:
   - `--keep-branch` or `--reset-branch` when either checkout's current
     branch isn't `master` (repo's `current_branch`);
   - `--migrations` always (its question comes after the checkouts have
     moved, and the next run can't ask it again: the announcement diff
     is then empty).

   Missing ones: one error naming each flag and why, exit 1, before any
   step. "A terminal" is term-ui's prompt rule: `/dev/tty` opens and sb
   is in its foreground process group.
2. **Disk space:** host's `check_disk_space(["/", <appdata>, "/srv"])`
   (cmd/update.go:65-73), with host's texts.

#### Steps

3. **Self-update** unless `--skip-self-update`: release's flow with the
   answer given (no prompt, sb-go's `autoUpdate=true`).
   - Off (disabled or raw build): warning `Self-update is disabled in
     this build`, and on.
   - Replaced: warning `Re-run the update command to update Saltbox`,
     then exit 0 (cli-core's requested exit code; sb-go 6108017). The new
     binary isn't run in this process (sb-go tried a relaunch, eda3d73,
     and removed it, 1123432).
4. **Branch questions**, now, before the runner draws (repo's
   `branch_choice` for Saltbox, then Sandbox; sb-go asked Sandbox's after
   Saltbox's update had finished): `<Repo>: Currently on branch '<b>'`,
   then the confirm `<Repo>: Do you want to reset to the 'master'
   branch?` (default no), or the flag's answer. Asked after self-update
   so a run that replaces sb doesn't ask twice.
5. **"Updating apt package cache"** with the sub-step "Refreshing apt
   package lists": host's `update_lists`.
6. **Announcements before:** both files read (below).
7. **"Updating Saltbox"**, sub-steps as sb-go (cmd/update.go:173-312):
   the Saltbox user (config's `saltbox_user()`), `head()` before,
   "Updating Saltbox repository" (repo `fetch_and_reset` to the chosen
   branch, owner the Saltbox user), the runtime (python-env's
   `reconcile_runtime`: deadsnakes, venv, fact, as `update-venv`),
   `head()` after, "Updating Saltbox tags cache" (ansible's
   `TagCache::refresh`) when the hash changed or the cache entry or its
   tags are missing. `head()` after is read right after the fetch and
   reset, before the runtime (HEAD doesn't move in between; what moved
   is then known even if the runtime fails, decision 38), and the old
   cache entry is read before the rebuild writes over it (step 13).
8. **"Securing Saltbox configuration permissions"** / "Saltbox
   configuration permissions secured": `secure_existing_configs`
   (`sb setup`, above).
9. **"Updating Sandbox"**: the same without the runtime, its own tags
   cache.
10. **Announcements after**, the diff, then the announcements shown and
    the migrations (below).
11. **"Validating Saltbox configuration"**: validate-config's checks, as
    one step group, each file through config's
    `validate_file_migrating(step, settings, file, Checks::Run, backups)`
    (the config amendment the tui spec proposes, with motd.yml's
    migration from its older format first, its backup in
    `/opt/saltbox/backups`: motd spec, decision 4).
12. **Completion refresh** (completion's `refresh`): a failure is a warning
    (cmd/completion.go:160-179). After validation passed, in the
    validation's runner, as a step of its own, "Checking shell
    completions" (success "Shell completions checked"), holding
    completion's "Refreshing shell completions" and a step per shell
    (M87).
13. **What moved, then the closing hint** ("What moved, and the closing
    hint", below), once the last runner is done.

Errors keep sb-go's wrapping: `error updating Saltbox: ...`, `error
updating Sandbox: ...`, `error securing Saltbox configuration
permissions: ...`, `error validating Saltbox configuration: ...`; exit 1.

#### Announcements

- **File:** `<repo>/announcements.yml`, read through sb-config's
  Ansible-equivalent YAML entry (ADR-022) into a typed list, unknown keys
  ignored (yaml.v3 non-strict, research D §6.15). Fields: `date`
  (`YYYY-MM-DD`), `title`, `message` (Markdown), `migration.required`,
  `migration.tag`, `required_folder`, `required_file`.
- **Missing file:** none. **Unreadable or invalid:** a warning naming the
  file and the issue (positions from sb-config), and that repository's
  announcements and migrations are skipped for this run (decision 23).
- **New** (saltbox/announcements.go:166-231): no after file, nothing; no
  before file or an empty one, every after entry; otherwise entries
  whose `date|title` the before file lacks. Then kept only if
  `required_folder` is an existing directory and `required_file` an
  existing non-directory (both when both are set); sorted by the date
  text within each repository (a stable sort; sb-go's `sort.Slice` left
  equal dates in any order), Saltbox's shown before Sandbox's.
- **Shown:** info `Displaying new announcements`, then
  - stdout a terminal: tui's announcement viewer (capability map module
    table; its design is the tui spec's), every announcement that
    applies listed, the new ones marked (M87). `q` or Esc goes on to the
    migrations; Ctrl-C (130) or a signal ends the run there, after the
    warning `Migrations interrupted; to run by hand: ...` naming any it
    had to offer, since the next update won't (decision 22, difference
    43; decided 2026-11-17);
  - otherwise: tui's inline Markdown output on stdout, each announcement
    under a heading `<Repo> Announcement - <Month D, YYYY>` (sb-go's
    title; the date as written when it doesn't parse), the message
    through `DisplayText`.
- **Migrations:** entries with `migration.required` and a non-empty tag,
  across both repositories, by date (stable: Saltbox's first on equal
  dates). The line `The following tags need to be run to handle
  migrations:`, a table `#`, `Repo`, `Tag`,
  `Announcement Date` (term-ui's table), then the confirm `Run all
  migration tags?` (default no), or `--migrations`. All or none
  (saltbox/announcements.go:460-571).
  - Run: info `Starting migration execution`, for each `Running migration
    '<tag>' for <Repo> repository` and ansible's `run` (its playbook run
    by tag, as `sb install`'s) with that tag in that repository (Saltbox, Sandbox, saltbox_mod by
    path), then `All migrations completed successfully`. A failure:
    `failed to execute migration '<tag>' for <Repo> repository (to run
    by hand: sb install <tag>, sb install sandbox-<tag>, ...): ...`,
    exit 1, the list naming it and the migrations after it. A playbook
    that ran and failed has no `details:` line under it: its output is
    above (cli-core, **Errors**; the maintainer, 2026-11-18); one that
    couldn't start keeps it. Ctrl-C
    during one reaches Ansible, which exits 99: that failure, with its
    list. sb's own interrupt (SIGTERM, a hangup, Ctrl-C before a
    playbook starts or one it dies of): warning `Migrations interrupted;
    to run by hand: ...` with the same list, then sb ends by the signal
    (difference 43).
  - Skipped (declined or `--migrations skip`): warning `Skipped
    migrations: <repo> <tag>, ...`, so the screen and the log keep them
    (decision 22).

#### What moved, and the closing hint

The maintainer's decisions of 2026-11-18 (layout "B" and hint "H2" of
the mock-ups; decisions 36 to 38). sb-go printed none of this.

- **Where:** after the last step tree (validation's and the completion
  refresh's), a blank line, the block, then, when a checkout moved, a
  blank line and the hint. Both on stderr through `lines`, as sb's other
  lines in `update` (the migration lines, the warnings), so they follow
  the tree on the same stream.
- **The block,** one line per checkout whose pull finished, Saltbox's
  first, the name padded to 9 columns (`Saltbox  `, `Sandbox  `); each
  id is the commit's first 7 characters:
  - moved: `Saltbox  a1b2c3d → e4f5a6b`;
  - not moved: `Saltbox  already up to date (e4f5a6b)`;
  - moved, the tags compared and none added or removed:
    `Sandbox  c6b5a49 → d7e8f90, no tag changes`;
  - moved, the tags compared, some added or removed: the range line,
    then `  New tags:      <tags>` and `  Removed tags:  <tags>`, each
    only when its list isn't empty, the tags starting at column 18;
  - moved, the tags not compared: the range line only.
- **The tags** as the user types them: Saltbox's as listed, Sandbox's
  with `sandbox-`; the new ones in the new listing's order, the removed
  ones in the old one's; each escaped (cli-core's `escaped`: a tag is
  Ansible's output).
- **Each list** is the tags joined with `, `, wrapped at words to 80
  columns under its label (63 columns of tags; term-ui's `wrap_line`,
  tabled's `Wrap::wrap`, as the tree and help wrap; a tag wider than
  that breaks anywhere), and capped at two lines: when the whole list
  needs more, as many tags as fit in two lines, then ` and <n> more (sb
  list)`. 80 whatever the terminal's width: one layout to read and paste
  (as bench's piped tables).
  ```
  Sandbox  0f9e8d7 → 9a8b7c6
    New tags:      sandbox-a, sandbox-b, sandbox-c, sandbox-d, sandbox-e,
                   sandbox-f, sandbox-g, sandbox-h and 9 more (sb list)
  ```
- **When the tags are compared** (decision 37): only when the
  checkout's old cache entry was valid for the commit before the pull,
  and the rebuild after the pull finished. Valid is ansible's
  `TagCache::fresh(repo, <HEAD before the fetch>)`: the file present,
  readable and parsed, the entry of sb's shape (`commit` a string,
  `tags` strings), `tags` not empty, and `commit` the HEAD read before
  the fetch. sb-go writes the same shape (its ansible/cache.go and
  `RunAndCacheAnsibleTags`), so an entry it wrote counts as any other:
  the rule is the entry's, not its writer's. The entry is read before
  the rebuild writes over it: the cache is loaded once before
  Saltbox's step, and Saltbox's rebuild changes only Saltbox's entry. In
  every other case (no file, no entry, an unreadable or corrupt file,
  an entry of another shape, an entry for another commit, empty tags, a
  rebuild that failed or didn't run) the line shows the range only,
  never every tag as new: the first update on a host with no cache shows
  `Saltbox  a1b2c3d → e4f5a6b` and nothing more.
- **Colours,** the theme's roles: the ids and the arrow in Muted, the
  new tags in Ok, the removed ones in Warn (a removed tag needs a look,
  but isn't a failure, which is Problem's ✗), `sb list` and the hint's
  commands in Code (the mock-ups' value colour); the rest plain.
  `lines` is anstream's stderr, so it is plain under `NO_COLOR`,
  `--color=never` or when stderr isn't a terminal.
- **The run log:** after each pull `checkout pulled` with `repo`, `old`
  and `new` (the full ids); after a rebuild `tags compared` with `repo`,
  `added` and `removed` (`, `-joined, every tag), or `tags not compared`
  with `repo` and the reason (`no valid cache entry for the old
  commit`).
- **The hint** (decision 36), the commands in Code:
  - when Saltbox moved (whether or not Sandbox did), hint H2:
    ```
    Saltbox's files are updated; your server and apps are not yet. To apply it:
      sb install saltbox         your main tag (or mediabox, feederbox); includes core
      sb install core,<apps>     the minimum: core plus the apps you run
    ```
  - when only Sandbox moved, Sandbox's own, in the same layout and
    without core (nothing in core changed):
    ```
    Sandbox's files are updated; your Sandbox apps are not yet. To apply it:
      sb install sandbox-<app>   each Sandbox app you run
      sb install sandbox-roles   the apps you list in sandbox_roles
    ```
  - when nothing moved: no hint.
- **A failed or interrupted run** (decision 38): the block shows the
  checkouts whose pull finished (their HEAD read after the fetch and
  reset), each by the rules above (its tags only if its rebuild
  finished), printed once the runner that failed is done, then the
  error as before. No hint: it is the next step after a finished
  update, and after a failure the next step is the error's. Nothing is
  printed when no pull finished (a refusal before any step, a failed
  self-update or apt step, a replaced sb).

### `sb branch <name>` and `sb branch-sandbox <name>`

1. repo's `check_branch_name` (before anything changes).
2. The Saltbox user (config's `saltbox_user()`).
3. repo's `fetch_and_reset` to `<name>` (it widens the refspec, checks
   the branch exists on the remote, and sets ownership), under sb-go's
   labels: "Switching <Repo> repository to <name>" / "<Repo> repository
   switched to <name>" / "<Repo> branch switch", with the sub-step
   "Updating <Repo> repository" / "<Repo> repository updated (<name>)".
4. Saltbox only: python-env's `reconcile_runtime`, the same call as
   `update`'s step 7 (decision 21).
5. "Updating <Repo> tags cache" (ansible's `TagCache::refresh`), always,
   as sb-go.

No prompt: the argument is the answer (decision 20).

### `sb announcements`

A reader for the repositories' announcements (the maintainer,
2026-11-15: it shows announcements only, and migrations run during updates
only; decision 27). It takes no flags of its
own and always reads Saltbox's and Sandbox's (the maintainer,
2026-11-17; decision 30, difference 45): each repository's own file,
`<repo>/announcements.yml` at its place in assumption 7. A missing file
counts as "no file", so a server without Sandbox shows Saltbox's alone. A
checkout that is there without its own file (one older than the file: a
Sandbox checkout behind the commit that added it, the maintainer's
server on 2026-11-17) is named in an info line, `Sandbox has no
announcements file: <path>`, and the run log records each file's path,
whether it was loaded, missing or failed to load, and how many entries
are shown. The rules above decide which entries apply; every one that
applies is shown, by date. Nothing is called new, on a terminal or piped:
"new" is `update`'s word, which compares the files before and after its
pull in-process (decision 31; the maintainer, 2026-11-17: the command
calls nothing new).
One runner step, "Reading announcements" / "Read announcements", loads
them; its info line counts what will be shown (`12 announcements`, `1
announcement`, `No announcements`), the same set on a terminal and
piped. Then, each entry labelled with its repository (the maintainer,
2026-11-17):

- stdout a terminal: tui's announcement viewer (tui spec, "The
  announcement viewer") with the entries of both repositories in one
  list, none marked, each row naming its repository, the pane's title
  the selected entry's, when at least one is shown;
- otherwise: the entries inline, as `update` prints them, each headed
  `# <Repo> Announcement - <date>`, Saltbox's first.

No migration table, no confirm and no `--migrations`: migrations run in
`update` only (M86). A file that can't be loaded warns, `<Repo>
announcements skipped: error loading announcement file '<path>':
<issue>`, and the other repository's announcements show, exit 0, as in
`update` and for a missing checkout (decision 31).

`sb announcements` keeps its help: its two subcommands below are hidden
(clap's `hide`; cli-core's help lists only shown subcommands), so its
usage line, flags and text stay as they are, and a word after it that
isn't one of them is clap's error, exit 2.

#### `sb announcements preview` (hidden)

A developer's view of what the next `sb update` would show (the
maintainer, 2026-11-18; decision 34, difference 60). "Before" is each
checkout's `announcements.yml` at the git ref `--since` names (default
`HEAD`, so uncommitted edits count as new); "after" is the file in the
working tree. `--since origin/master` shows everything not yet on master
as new. Everything after that is `update`'s own code:

1. One runner step per checkout, `Reading <Repo> announcements at
   <ref>` / `Read <Repo> announcements at <ref>`: repo's
   `file_at(ref, "announcements.yml")` (git's `rev-parse --verify` and
   `cat-file`, through sb-exec as repo's other reads, within 10 s): the
   file's text, or none when the ref has no such file (then every entry
   is new, as `update` with no before file). Its text is held to
   cli-core's 1 MiB and UTF-8 rules, as `read_small`, and loaded by the
   same loader. A ref git doesn't know fails the step: `error reading
   <Repo> announcements at '<ref>': '<ref>' isn't a commit in <path>`,
   exit 1. Without `--repo` both checkouts, as `update`; a checkout that
   isn't there is skipped silently (a server without Sandbox), unless
   `--repo` names it (repo's `inspect required directory` error, exit
   1).
2. `update`'s step "Checking for new announcements" / "Checked for new
   announcements" (the same function): the diff, the gate and the sort;
   a file that can't be loaded at the ref or in the tree is `update`'s
   warning and skips that checkout; `Displaying new announcements` when
   there are some. The step adds one info line per checkout, `<Repo>: <n>
   new since <ref>`.
3. Shown as `update` shows them: on a terminal, tui's announcement
   viewer, every entry that applies listed and the new ones marked;
   piped, the new ones inline. Nothing new: nothing is shown, exit 0.
   Ctrl-C on the viewer ends with 130; nothing was going to run, so no
   warning names the migrations.
4. The migrations, `update`'s table and its confirm `Run all migration
   tags?` (default no):
   - without `--run-migrations` (a dry run): yes prints `Would run: sb
     install <tag>` for each (Saltbox's tag as it is, Sandbox's as
     `sandbox-<tag>`, the lines `update` names for running by hand), then
     `Dry run: nothing was run; --run-migrations runs them.`; no prints
     `update`'s warning `Skipped migrations: <repo> <tag>, ...`;
   - with `--run-migrations`: yes runs them through `update`'s migration
     runner, its lines, failure (exit 1, the `sb install` lines of the
     ones left) and interrupt handling (decision 22, difference 43); no
     is `update`'s warning.
   - without a terminal no question is asked (term-ui's `can_ask`):
     without `--run-migrations` the would-run lines print as for yes,
     then the dry-run line; with it they run, the flag being the answer,
     as `update`'s `--migrations run` is.

sb's lines (the would-run lines, the dry-run line, the warnings) go to
stderr as `update`'s do, the table and the inline form to stdout. The run
log records the ref, each file's outcome, the counts and the migrations
decided, as `update` records them.

#### `sb announcements check` (hidden)

A linter for announcement files, for developers and Saltbox's CI (the
maintainer, 2026-11-18; decision 35). With a file, that file; without,
both checkouts' own files (a checkout without its file: `<Repo> has no
announcements file: <path>`, as `sb announcements` says it; neither file
there: the error `no announcements file to check`, exit 1). Each file is
loaded by the loader above; a file that doesn't load is its problems, in
the loader's words (`error reading announcement file: ...`, or one
`error parsing announcement YAML: <issue>` per issue, with its
position), and no entry is checked. Otherwise every entry, numbered
from 1:

- `date`, `title` and `message` are set: `` `<field>` is missing ``;
- `date` is a real date as `YYYY-MM-DD` (the heading's rule): `` `date`
  '<text>' isn't a date as YYYY-MM-DD ``;
- `required_folder` and `required_file`, when set, are absolute paths
  without a `..` component: `` `<field>` '<path>' isn't an absolute path
  ``, `` `<field>` '<path>' has a '..' component ``;
- the message renders: no HTML (tui's Markdown shows it as text: ``
  `message` has HTML, which sb shows as text: '<html>' ``, one for an
  HTML block) and no reference link (`[text][label]`, `[label][]`)
  without its definition (`` `message` links to '[<label>]', which it
  doesn't define ``), as pulldown-cmark's events and its broken-link
  callback find them; a shortcut (`[label]` alone) isn't flagged, as it
  is prose as often as a link;
- a `migration.required` entry has a tag: `` `migration.required` is
  true and `migration.tag` is empty ``;
- a migration tag is one of the repository's tags (sb-ansible's install
  validation over the tags cache, its hints): `` `migration.tag` '<tag>'
  isn't one of <Repo>'s tags ``, then, in install's order, `; it is
  <Other>'s tag` when the other repository has it, `; did you mean
  '<near>'?` for one of its own within two edits, or `; did you mean
  <Other>'s '<near>'?` for one of the other's. The cache is read, never rebuilt: when the
  repository's entry is missing, empty, or at another commit than its
  checkout's HEAD, the check is skipped with `note: migration tags not
  checked: no cached tags for <Repo> at its checkout's commit (sb list
  caches them)`;
- no two entries share `date` and `title` (`update` tells entries apart
  by them): `same date and title as entry <n>`.

The repository of a given file is `--repo`'s; without it, Sandbox when
the file is Sandbox's own file, else Saltbox. Without a file, `--repo`
checks that repository's own file alone. Like every ops command it runs
as root (assumption 2): run by a user, sb relaunches itself through
sudo, as on Saltbox's CI runners. Output on stdout: each file
as a line `<path>:`, then each problem as `  entry <n> (<date>, "<title>"):
<problem>` (`no date` and `no title` for a missing one), the notes as
`  note: ...`, then `  <n> problems` or `  no problems`. Any problem:
sb's error `announcements check found <n> problems`, exit 1; none, exit 0.
Every text from the file is escaped as sb's other output (cli-core's
`escaped`).

## Differences from sb-go (the complete list)

`setup`:
1. `--verbose/-v` is accepted, hidden and ignored, with a run-log line
   (decision 1). sb has no verbose mode (term-ui decision 9).
2. The branch prompt reads the terminal, not stdin (term-ui, prompts).
   Under `curl | bash`, stdin is the script.
3. The output is sb's runner tree; the apt, git and uv output is each
   step's tail and the run log, not sb-go's verbose stream.
4. The sources.list.d cleanup refuses a symlinked, foreign-owned or
   group- or world-writable directory, works through a directory
   handle, and logs each name (decision 5; a host amendment). sb-go
   removed by path without these checks.
5. The cleared sources.list.d entries are moved into a root-only
   `/var/backups/sb/apt-sources/<run id>/`, not deleted (decision 28; a
   host amendment). sb-go deleted them, on `setup --force` and `gha`
   over existing servers too.
6. setup takes `--keep-branch` and `--reset-branch`, as `update` does;
   with no terminal and neither flag, a branch question fails naming
   them (decision 29). sb-go kept the current branch silently
   (git.go:108).
7. The host spec's listed differences apply (the desktop check's
   installed state, the apt lock wait, atomic source writes).

`gha`:
8. No forced verbose mode: the runner is already plain under CI and
   when redirected (term-ui), so the CI log shows sb's plain tree.
   Differences 4 and 5 apply to gha's cleanup too.

`restore`:
9. On a terminal, a full-screen form and then a live list of the nine
   files, each row's outcome and reason as it comes, then a viewer of
   the restored files with secrets masked and shown on demand, closed
   by Continue; the final list printed after the screen closes. sb-go's
   full-screen form gave way to its lines (decision 32). With both flags
   given, the lines only, as sb-go's.
10. Without a terminal, the flags answer: no prompt at all. Esc in the
    form keeps sb-go's `Restore cancelled.`, exit 0; Ctrl-C ends with
    130.
11. `--user` and `--password-file` answer the prompts (decision 8); an
    empty user name is refused (sb-go accepted it and fetched under the
    empty name's key); sb-go's 64-character limit on the inputs goes.
12. No `-v`: the user key, URLs and salt are never printed (sb-go's `-v`
    printed them).
13. A `User-Agent` is sent and redirects aren't followed (sb-go: none,
    and Go's ten).
14. Decryption happens in memory and each file is persisted from a
    temporary file in the root (sb-go: an encrypted and a decrypted file
    in `/tmp/saltbox-restore-*`, then a move with an EXDEV copy
    fallback). The files, modes and owners are the same.

`bench`:
15. A new implementation and output format (decision 11): sb's runner
    and tables, no `clear`, no banner, binary size units, Mbps, the help
    text above. Its numbers aren't comparable with bench.sh's.
16. Virtualization comes from `systemd-detect-virt` only: bench.sh's
    dmidecode, dmesg and cgroup fallbacks go (a server without systemd
    isn't supported).
17. IPv4 and IPv6 are checked with an HTTPS request on each family, not
    a ping to Google with an icanhazip fallback; ipinfo.io is asked once
    for JSON, not four times.
18. The disk test needs 2 GiB free (bench.sh: 1 GiB), so it can't fill
    the disk to the last byte, and it writes through `sync_data` in
    process rather than `dd conv=fdatasync`.
19. *(Dropped 2026-11-16: Total Disk adds the swap, as
    bench.sh did.)*
20. Speedtest runs on aarch64 too, with its own pinned asset; bench.sh
    in sb-go was x86_64-only.
21. The vendored bench.sh, its LICENSE and NOTICE aren't shipped.

`update`, `branch`, `branch-sandbox`, `announcements`:
22. `update` runs without a terminal when flags answer every question;
   sb-go refused (decided.md D-Q4).
23. Without a terminal, `update` needs `--migrations`, and a branch flag
   when a checkout is off `master`; sb-go had no migrations flag
   (decision 19).
24. Announcements print inline when stdout isn't a terminal; sb-go always
   used the full-screen viewer (decided.md D-Q4).
25. Prompts read the terminal through term-ui, accept `y` and `yes`, and
   fail naming the flag without one; sb-go read stdin, its branch prompt
   took only `y`, and kept the current branch on EOF or with no TTY
   (term-ui "Prompts").
26. Both branch questions come before the first step; sb-go asked
   Sandbox's in the middle of the run.
27. `sb branch <name>` always switches; sb-go asked whether to "reset" to
   the named branch and silently kept the current one on `n` or with no
   TTY, then reported `switched to <current>` (decision 20).
28. `branch` uses `update`'s runtime step (deadsnakes, venv, fact); sb-go
   ran fact then venv, without deadsnakes (decision 21).
29. An unreadable `announcements.yml` in `update` is a warning that skips
   that repository's announcements; sb-go failed the update, so a broken
   file in the checkout kept `sb update` from fetching its fix
   (decision 23).
30. Skipped migrations are listed in a warning (decision 22).
31. `announcements` is shown in `sb --help` and only reads announcements:
    sb-go's was a hidden test harness that also listed the migrations,
    asked about them and printed what it would run. There is no migration
    table, confirm or `--migrations` on it (the maintainer, 2026-11-15;
    decision 27).
32. No `-v` on `update` or `announcements` (decision 18). *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*
33. Parse errors (both branch flags) exit 2 (cli-core's parse errors);
    sb-go exited 1.
34. The repo and release specs' differences apply (branch-name checks,
    refspec widening, pinned downloads and the rest).
35. `announcements` needs no flag: without `--before` every entry is
    new, without `--after` it reads the repository's own file; sb-go
    required both (`MarkPersistentFlagRequired`; decision 27).
    *Amended 2026-11-17:* it has no `--before` or `--after` at all
    (difference 45).
36. On a terminal, `announcements` (and `update`, M87) lists every
    applicable announcement in tui's viewer, the new ones marked and the
    older ones faint; sb-go's viewer showed only the new ones (tui spec,
    "The announcement viewer"; proto-markdown's reviewed design).
    *Amended 2026-11-17:* only `update` marks new ones; `announcements`
    lists every one unmarked, each row naming its repository.
37. A single line break inside a paragraph of a message is a space, as
    CommonMark has it (tui-markdown); sb-go's glamour kept it as a break
    (`WithPreservedNewLines`, saltbox/announcements.go:410-414). Saltbox's
    and Sandbox's public files of 2026-11-15 don't rely on it (M74's
    review).
38. `announcements` without `-r` shows Saltbox's and Sandbox's
    announcements; sb-go's harness read one repository, Saltbox by
    default (decision 30). *Amended 2026-11-17:* always both; there is
    no `-r` (difference 45).
39. `announcements` without `-r` warns about an invalid repository file
    and shows the other's, and its step line counts the announcements
    shown; sb-go's harness failed on any invalid file and printed no
    count (decision 31). *Amended 2026-11-17:* always so; there is no
    `-r` or named file that fails.

`setup` and `gha`, the default configs (module build M80):
40. A `defaults/` directory that can't be read is an error (`error
    listing default config files: <cause>`); sb-go's `filepath.Glob`
    ignores the error and finds nothing, so it secured nothing silently.
41. A default named only `.default` stands for no file and is skipped;
    sb-go's glob took it and failed on the Saltbox root itself (`... is
    not a regular file`).

`update`, the announcements (module build M85):
42. The comparison of each checkout's announcements before and after its
    pull is a step of its own, "Checking for new announcements" /
    "Checked for new announcements", on every update; sb-go printed only
    `Displaying new announcements`, when there were some. The step holds
    that line and decision 23's warnings: term-ui's runner has no notices
    outside a step.
43. No migration is silently lost: a failed migration's error names it
    and the migrations after it, which didn't run (Ctrl-C reaches
    Ansible, which exits 99, so it ends here too), and one sb itself is
    interrupted in (SIGTERM, a hangup) gives a warning naming it and
    those after it before sb ends by the signal; each as the `sb
    install` line that runs it by hand (`sb install <tag>`, `sb install
    sandbox-<tag>` for Sandbox's).
    The next update won't offer them again, as their announcements are
    no longer new. sb-go named only the failed one, and nothing after an
    interrupt (the maintainer, 2026-11-17).

`restore` (module build M76):
44. `restore` refuses a decrypted file that isn't UTF-8 text as a wrong
    password: nothing is placed and the password line is printed. sb-go
    placed whatever passed the padding check, so a wrong password
    overwrote about one file in 256 with garbage (the maintainer,
    2026-11-16).

`announcements` (the maintainer, 2026-11-17):
45. `sb announcements` takes no flags of its own: sb-go's `-b/--before`
    and `-a/--after` are gone, as "new" is `update`'s alone, which
    compares its files in-process (sb-go's update never called the
    command either, and nothing in Saltbox or Sandbox does, checked on
    public GitHub), and `-r/--repo` is gone, as both repositories are
    always shown. Each entry is labelled with its repository: in the
    viewer's list rows and its pane's title, and in the inline headings
    (sb-go's harness showed one repository's, unlabelled in its viewer).
    The flags are clap's unknown-argument error, exit 2.

`bench`, 2026-11-16 (decision 33):
46. System Uptime's units are singular or plural as their numbers need,
    with a comma between each (`1 day, 1 hour, 1 min`); bench.sh printed
    `%d days, %d hour %d min` whatever the numbers (the maintainer,
    2026-11-16).
47. The disk test's average is the mean of the measured rates, in MB/s
    throughout; bench.sh averaged dd's rounded text and counted a
    `GB/s` as 1024 `MB/s` where dd's GB is 1000 MB. A failed run is
    `Test failed` and left out; bench.sh showed dd's error text.
48. An unwritable working directory is the Disk write section's row and
    the other sections still run; bench.sh stopped before any section.
49. On ARM, CPU Model is sysinfo's name for the CPU part, CPU Cores the
    `processor` entries (online CPUs), and the frequency the largest
    `cpuinfo_max_freq`; bench.sh asked lscpu, which names parts from its
    own table and counts offline CPUs too.
50. The timestamp is in `/etc/localtime`'s zone, as sb-tui's; `date`
    followed `TZ` too.
51. Total RAM's used follows the release's procps (decision 33), so it
    matches `free` there; one formula would differ from `free` on
    22.04 or on 24.04 and later. Only 22.04 gets procps 3.3's formula
    (the supported releases'; another system with procps 3.3 would get
    procps 4's).
52. Total Disk counts every zfs pool; bench.sh passed `zpool list`'s
    whole output to `calc_sum` as one word, so with two pools or more it
    added nothing for zfs.
53. Total Disk's filesystems are read within 5 s, the value blank past
    that or when the mounts can't be read; bench.sh's df had no limit (a
    hung filesystem held the script). OS falls back to redhat-release's
    text as the script did, which no supported release has.

`bench`, at M78 (2026-11-16; decision 33):
54. IPv4/IPv6 is one cell in one colour (Ok, Warn or Problem by the two
    families); bench.sh coloured each half. bench.sh's warning when
    neither family answered went: the script printed it before its
    `clear`, which wiped it.
55. Speedtest's values come from the CLI's JSON (`--format=json`), the
    numbers its text form prints, with its units; bench.sh read the
    text with awk. The CLI's licence file stays in sb's private
    directory (`HOME`); bench.sh's went to root's home.
56. A Speedtest that can't be prepared is its section's row and the
    other sections print; sb-go prepared it before the script ran and
    failed the whole command without any output.
57. A `Preparing Speedtest` step shows the download; bench.sh printed
    nothing while sb-go downloaded.
58. Hardening at M78's security review: the Speedtest's private
    directory is made in `/tmp` whatever `$TMPDIR` says (sb-go's
    `os.MkdirTemp("")` followed it; a non-sticky one another user can
    write would let them swap the directory before it runs), the CLI
    runs without `XDG_CONFIG_HOME` (its licence file would go there
    before `HOME`), and icanhazip's and ipinfo's answers are read up to
    64 KiB (curl had no limit).
59. `update` and `setup` hold completion's "Refreshing shell
    completions" in a step of their own, "Checking shell completions"
    (success "Shell completions checked"): sb's runner shows a step's
    warning under a step, and the refresh's failure is one. sb-go ran
    the refresh at the runner's top level (cmd/completion.go:182).

`announcements`, its hidden subcommands (the maintainer, 2026-11-18):
60. sb-go's harness for trying announcements (`--before`, `--after`,
    gone with difference 45) is replaced by `sb announcements preview`:
    the before file is the checkout's own at a git ref, not a file given
    by path, and what follows is `update`'s own diff, viewer and
    migration list, the migrations a dry run unless `--run-migrations`.
61. `sb announcements check` is new: sb-go had no check of an
    announcement file but its loader.

`update`'s end (the maintainer, 2026-11-18; decisions 36 to 38):
62. `update` ends with what moved: each checkout's commit range or
    `already up to date`, and the tags the pull added or removed when
    the old cache entry was valid for the commit before it; the commits
    and the tag diff in the run log. sb-go printed nothing after its
    last step.
63. `update` ends with a closing hint when a checkout moved: how to
    apply the update (`sb install saltbox` or `core,<apps>`; Sandbox's
    tags). sb-go gave none.

## Scope

**In:**
- `setup`, `gha`, `restore`, `bench`, `update`, `branch`,
  `branch-sandbox` and `announcements` as above;
- the shared pieces: `prerequisites`, `locale`, `copy_default_configs`,
  `secure_existing_configs`, `git_hooks`;
- the install script in this repository (below) and its tests.

**Update and branches** are in this spec: "`update`, `branch`,
`branch-sandbox` and `announcements`" above, sharing
`secure_existing_configs` and repo's branch resolution with setup.
`update`'s full form is built: the announcement viewer (M87, the new
ones marked, every one that applies listed) and the completion refresh
(M87, after completion's M60).

**Not here:** `diag` and `reinstall-*` (their own specs); pointing
Saltbox's docs at `sb restore` instead of `restore.saltbox.dev` (a
consumer change for the maintainer to raise); publishing the install
script at install.saltbox.dev (a separate, explicitly authorised step);
a `--json` for bench.

### The install script

The map's open item "Install script" (correction under Amendments). It
moves into this repository as `install/install.sh` and starts from
sb-go's `install.sh` plus `-b` (decided.md G-Q5):
- sb-go's checks: root, an existing Saltbox root refused (except
  `--repair`), x86_64, setup's release list (24.04, 26.04) or the
  runtime list for `--repair`;
- the latest tag through the svm proxy, then GitHub; the download from
  `https://github.com/saltyorg/<repo>/releases/download/<tag>/<asset>`
  only (decided.md D-Q2), retried, then the ELF and size check and `sb
  version` matching the tag; the repository is saltyorg/sb-rust, the
  asset `sb_linux_amd64` (x86_64 only: the arm64 assets wait for Saltbox
  to support arm64), and `--beta` installs the `_beta` asset of the
  highest release, prereleases included (maintainer, 2026-11-18; release
  spec, "Release channels");
- options: sb-go's `-v/--verbose`, `--force-curl`, `--force-wget`,
  `--repair`, `-h/--help`, plus `-b <branch>` (from the served
  `sb_install.sh`), passed to `sb setup -b <branch>` as one argument;
- `-v` makes the script's own lines verbose and is **not** passed to `sb
  setup` (sb has no verbose mode; the served script's `-v` is why setup
  keeps it hidden);
- `sb setup` runs attached to the terminal, never captured; nothing
  reads stdin; the script exits non-zero on any failure, so `&& cd
  /srv/git/saltbox` only runs after success;
- shellcheck-clean.

## Tech Stack

New crates (each API confirmed at docs.rs in its task; the RustCrypto
set shares `cipher` 0.5 and `digest` 0.11, which sha2 0.11.1 already
brings in):

| Need | Crate | Why | Docs |
|---|---|---|---|
| AES-256 | `aes` 0.9.4 | stack table (RustCrypto), pure Rust, AES-NI detected at run time | https://docs.rs/aes/0.9.4/aes/ |
| CBC, PKCS#7 | `cbc` 0.2.1 | `cbc::Decryptor<aes::Aes256>` with `Pkcs7` padding; a padding error is the wrong-password signal | https://docs.rs/cbc/0.2.1/cbc/ |
| PBKDF2 | `pbkdf2` 0.13.0 (`hmac`) | `pbkdf2_hmac::<Sha256>` into 48 bytes | https://docs.rs/pbkdf2/0.13.0/pbkdf2/ |
| SHA-1 | `sha1` 0.11.0 | the user key | https://docs.rs/sha1/0.11.0/sha1/ |
| tar | `tar` 0.4.46 | the Speedtest archive's one entry, size-capped (python-env reads uv's archive with the same crate and version) | https://docs.rs/tar/0.4.46/tar/ |

Already in Cargo.lock: `sha2` 0.11.1 (PBKDF2's hash, the Speedtest
digest through release), `zeroize` 1.9.1 (RustCrypto's own; key, IV,
password and plaintext buffers; https://docs.rs/zeroize/1.9.1/zeroize/),
`flate2` 1.1.10 (gzip, the `rust_backend` on miniz_oxide 0.9.1;
https://docs.rs/flate2/1.1.10/flate2/read/struct.GzDecoder.html; python-env uses it too), `reqwest` 0.13.5, `tempfile`
3.27.0 (`NamedTempFile::persist`,
https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist),
`sysinfo` 0.40.0 (`system` only: bench's mount list is procfs's, so the
`disk` feature isn't needed, decision 33), `procfs`
0.18.0 (bench's `/proc/cpuinfo`, `/proc/meminfo` and mounts, as motd's;
https://docs.rs/procfs-core/0.18.0/procfs_core/struct.CpuInfo.html), `rustix`
(`openat`, `statat`, `mkdirat`, `renameat`, `unlinkat` for the host
amendment, and bench's `stat` and `statvfs`; the `fs` feature is already on), `serde_json` (Speedtest and
ipinfo JSON). `dialoguer` 0.12.0 is new to Cargo.lock: term-ui's confirm
comes with `release` (self-update, the first user in the build order).
restore's form is tui's (decision 32), on ratatui-textarea, already in
the tree.

`aws-lc-rs` (in the tree through rustls) could do all the crypto with no
new crate, but it is C and the stack table names RustCrypto; switching
would amend the stack table, so it isn't proposed. The binary-size cost
of the new crates is measured in their task against the ratchet.

## Commands

```
Check:   scripts/check.sh fast | task | full
Test:    cargo test -p sb-ops -p sb
Matrix:  SB_LOG_DIR=<scratch> systemd-run --scope -p MemoryMax=4G -p CPUQuota=200% crates/ops/tests/gha-matrix.sh
Install: SBI_SB=<sb built with SB_VERSION, SB_GIT_COMMIT and SB_CHANNEL=release> install/tests/containers.sh (policy.sh runs inside)
Lint:    shellcheck install/install.sh install/tests/*.sh crates/ops/tests/*.sh
Try:     the production build of sb, run as root with SB_LOG_DIR set
```

The matrix and install tests run in `ubuntu:<release>` containers (the
Docker the size check already uses), by hand at checkpoints, like the
journal matrix. The install script and its tests never run on a host:
it is a root installer that replaces `/usr/local/bin/sb`.

## Project Structure

```
crates/ops/                 sb-ops
  src/lib.rs
  src/setup.rs              guard, phases, branch
  src/completions.rs        the completion refresh, for update and setup
  src/gha.rs                gha's sequence and ownership
  src/prereq.rs             prerequisites (apt sequence)
  src/locale.rs             locale-gen, update-locale, dpkg-reconfigure
  src/configs.rs            copy_default_configs, secure_existing_configs
  src/hooks.rs              git_hooks
  src/update.rs             update: the up-front flag check and its steps
  src/branch.rs             branch, branch-sandbox
  src/announcements.rs      load, diff, gate, sort, the migration list (pure
                            functions over paths and parsed files), and the
                            command
  src/announcements/preview.rs  the hidden preview: the file at a ref, then
                            update's step, viewer and migrations
  src/announcements/check.rs    the hidden check
  src/restore/              key, crypto, fetch, place, answers
  src/bench/                system, disk, speedtest, network
  src/testing.rs            fakes: restore service, ipinfo/icanhazip (`testing` feature)
  tests/                    fixtures (openssl-made ciphertexts, how made), gha-matrix.sh
crates/sb/src/main.rs       the eight commands wired in
crates/sb/tests/            end-to-end tests through the binary
install/install.sh          the install script
install/tests/              its container tests (sb-go's test-install cases)
docs/platform-notes/speedtest.md   Ookla CLI JSON and HOME, version, method
```

deny.toml: `sb-ops` gets its own entry allowing only `sb`, and is added
to the wrapper lists of the crates it uses.

`update` and the branch commands add no crate of their own beyond the
modules': `jiff` (workspace) for the announcement heading's date,
term-ui's confirm and table, tui's viewer and inline Markdown.

## Code Style

Pure functions for the rules, tested first; the commands wire effects
to them.

```rust
/// OpenSSL's `enc -aes-256-cbc -pbkdf2` format, as Saltbox's backup role
/// writes it: `Salted__`, an 8-byte salt, then the ciphertext.
pub fn decrypt(data: &[u8], password: &[u8]) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
    let rest = data.strip_prefix(b"Salted__").ok_or(DecryptError::NoHeader)?;
    let (salt, ciphertext) = rest.split_at_checked(8).ok_or(DecryptError::Short)?;
    let mut key_iv = Zeroizing::new([0u8; 48]);
    pbkdf2_hmac::<Sha256>(password, salt, 10_000, key_iv.as_mut());
    let (key, iv) = key_iv.split_at(32);
    cbc::Decryptor::<Aes256>::new_from_slices(key, iv)
        .map_err(|_| DecryptError::KeyLength)?
        .decrypt_padded_vec::<Pkcs7>(ciphertext)
        .map(Zeroizing::new)
        .map_err(|_| DecryptError::Padding)
}
```

(The cipher 0.5 method names are confirmed at docs.rs in the task.)

## Testing Strategy

Nothing touches the real host's apt, sources, locale, Saltbox root or
the real services.

- **restore, unit:** fixtures made once with `openssl enc -aes-256-cbc
  -pbkdf2 -k <synthetic password>` over synthetic content, committed
  with the command that made them; a round trip; a wrong password gives
  `Padding`; a short, unaligned or headerless input; the key of known
  names (`sha1("")`, ASCII, UTF-8); the password file's newline rule,
  the 4 KiB cap and an empty file.
- **restore, placement:** on a temp root via `SB_SALTBOX_DIR`: 0600 and
  exclusive creation; a destination symlink replaced, its target
  untouched; an existing file overwritten; no temp file left after a
  failure or an interrupt; a missing root created 0755.
- **restore, end to end:** the built `sb restore --user ...
  --password-file -` as root (`SB_TEST_ROOT`, `SB_LOG_DIR` in the
  test's directory) against the fake restore service on loopback
  through `SB_RESTORE_URL` (axum, the `testing` feature): `[DONE]`,
  `[IGNORED]` (404), `[FAIL]` (a 17 MiB body, a bad header, a wrong
  password with its line, a dropped connection), a redirect not
  followed, 0 restored exits 1, partial exits 0; the fake records the
  User-Agent. The log test feeds a known user name and password and
  asserts that neither, nor the key, is in the run log, stdout or
  stderr.
- **restore, the screen:** tui's unit tests on ratatui's `TestBackend`
  (the fields, the checks, the masks, the list's updates, the viewer's
  menu, content, cursor, `s` and `a`, failures not opened, Continue, the
  mouse); sb-config's tests of the shared rule and `loggable_url`; on a
  PTY (portable-pty and vt100): the form, the mismatch text, the list,
  the viewer and Continue, the final list after it, Esc's cancel, Ctrl-C
  in the form and mid-run ending 130, both flags giving the line form;
  without a terminal and without flags, the error naming `--user`, exit
  1, no hang.
- **setup guard:** sb-go's setup_test.go cases on a temp root (missing,
  symlink with and without `--force`, a file, a directory with and
  without `--force`).
- **Sequences:** setup's and gha's order checked through PATH stubs, as
  cli-core's fake-sudo tests do: stand-in `apt-get`, `lsb_release`,
  `locale`, `locale-gen`, `update-locale`, `dpkg-reconfigure`, `git`,
  `bash` and `systemd-detect-virt` that record their arguments, in a
  private mount namespace with tmpfs over `/etc/apt`, `/etc/default`,
  `/srv` (so nothing reaches the host); the recorded calls must equal
  each command's order; gha with a symlinked root and `SUDO_USER` set,
  the six files chowned to it; a failing stub per phase gives that
  phase's error.
- **locale:** the decision flow on stubs: installed or not, update-locale
  failing (info, not error), the file missing then fixed, still missing
  (warnings, exit 0).
- **default configs:** sb-go's setup_security_test.go cases: 0600 new,
  existing chmodded, a symlinked or non-regular destination refused,
  content untouched.
- **sources cleanup** (host's tests, on a given root and a given backup
  root): a symlinked directory, a group-writable or non-root-owned one
  refused with nothing moved; the same for a symlinked or foreign-owned
  backup directory; a symlink entry moved as a link and its target kept;
  `ubuntu.sources` kept on DEB822; a subdirectory fails; regular files
  arrive with their bytes and mode; the `EXDEV` path through a seam that
  fails the rename (copy, sync, unlink); no backup directory when
  nothing moves; the moved names logged.
- **setup's branch flags:** on stubs with a checkout on another branch:
  `--reset-branch` and `--keep-branch` give the recorded git calls of
  each answer; both together are clap's error, exit 2; no terminal and
  neither flag: the error naming both, no stub called.
- **bench:** the system rows from fixture `/proc/cpuinfo`,
  `/proc/meminfo` and os-release text (x86_64 and aarch64, procps 3.3
  and 4), df's sums from fixed `statvfs` counts, zpool's from a stand-in,
  values formatted (sizes, uptime, load); the
  disk test with a small size on a temp directory, the 2 GiB rule from a
  fixed free-space value, an unwritable directory, Ctrl-C between
  blocks removing the file; the Speedtest step with a stand-in
  `speedtest` script printing fixture JSON (success, error JSON, missing
  fields, a timeout), the address fields absent from output and log; the
  archive extraction (one entry, an oversized entry, a symlink entry,
  a missing entry); a digest mismatch from a fake download refusing to
  run anything; icanhazip and ipinfo from the fake server; the whole
  command through the binary in a namespace with a tmpfs working
  directory and the network fakes.
- **gha matrix (by hand, capped):** `sb gha` in `ubuntu:22.04`, `24.04`
  and `26.04` containers with a synthetic Saltbox checkout symlinked at
  `/srv/git/saltbox`, run as a `runner` user with a stand-in sudo
  policy; asserts sb-go's test-gha.yml checks: `runner:runner:600` on
  accounts.yml and settings.yml, `/srv/ansible/venv/bin/ansible
  --version`, `/usr/local/bin/uv`, `saltbox.fact`. *Run 2026-11-16
  (module build M81, `crates/ops/tests/gha-matrix.sh`): passed on 22.04,
  24.04 and 26.04, all six configs `runner:runner:600`, the checks and
  `uv pip freeze` run as `runner`, with uv 0.13.4,
  ansible-core 2.21.2 and saltbox.fact 1.1.0, Saltbox master's pins.*
- **setup matrix (by hand, capped):** `sb setup -b master` as root with
  no terminal in fresh `ubuntu:24.04` and `26.04` containers (systemd
  installed for `systemd-detect-virt`), cloning Saltbox's public
  repository; then a second `sb setup`, which the guard refuses, and
  `sb setup --force`, which fetches and resets the checkout. Checks: the
  checkout on master, accounts.yml and settings.yml `root:root:600`,
  `/srv/ansible/venv/bin/ansible --version`, `/usr/local/bin/uv`,
  saltbox.fact, `LANG` in `/etc/default/locale`. *Run 2026-11-16
  (module build M82, `crates/ops/tests/setup-matrix.sh`): passed on
  24.04 and 26.04, with uv 0.13.4, ansible-core 2.21.2 and saltbox.fact
  1.1.0.*
- **install script (by hand, capped):** sb-go's test-install-os-policy
  cases in containers (release lists, root, existing root, `--repair`,
  `-b` passed as one argument, stdin never read: the script piped into
  bash with trailing lines that must still run), plus the todo's
  acceptance checks: both documented forms, with and without `-v`, a
  forced failure exiting non-zero. *Run 2026-11-16 (module build M83,
  `install/tests/containers.sh` with `policy.sh` inside): passed on
  22.04, 24.04 and 26.04. `--repair` piped with curl and wget installed
  the latest released sb (sb-go 0.1.3) from GitHub; a fresh install of
  the sb under test, piped with `-b master` and no terminal, ran `sb
  setup` through to ansible-core 2.21.2; a second run, a fresh install
  on 22.04 and a run as another user were refused with `&&` not run;
  truncated scripts exited non-zero.*
- **Against sb-go (by hand, once):** `sb restore` and sb-go's restore
  against the same fake service in a mount namespace; files, modes and
  `[DONE]/[IGNORED]/[FAIL]` lines the same, differences only as listed.
- **Gates:** CONSTRAINTS.md as always.
- **update, branch and announcements:** no test runs a real update,
  apt, ansible or the network.
  - **Unit:** the diff rules (each row above, `date|title` identity,
    equal dates across repos), the gating (folder vs file, both set, a
    symlink to a directory), the migration list and its order, the
    up-front flag check (every combination of terminal, branch state and
    flags), the heading date (valid, invalid).
  - **Commands end to end** (`crates/sb/tests`, root tests with
    `SB_TEST_ROOT`, `SB_LOG_DIR` and `SB_SALTBOX_DIR` in temp dirs): local
    checkouts of temp bare repositories (repo's test pattern), stand-in
    programs on `PATH` for `apt-get`, `ansible-playbook` and the
    python-env children (exec's pattern), release's fake server for
    self-update. Cases:
    - `branch develop` and `branch-sandbox develop`: switched, tags step
      reached, no prompt even on a terminal; `branch -x`: the name error,
      nothing changed;
    - `update` piped with all flags: every step in order, exit 0;
      without `--migrations`: the error naming it, nothing changed; a
      checkout off master without a branch flag: the error naming both;
    - `update` on a PTY (rexpect): the branch questions before the first
      step; the migration confirm after the viewer;
    - a new announcement with a migration upstream: listed, run with
      `--migrations run` (the stand-in records `--tags <tag>` and the
      working directory), skipped with `skip` (the warning);
    - an invalid `announcements.yml` before and after: the warnings, the
      update completes;
    - self-update replaced: the warning and exit 0, no apt step started;
    - SIGINT during the Saltbox fetch: "interrupted", death by the signal;
    - a failed migration (and Ansible's own Ctrl-C exit 99): the error
      naming it and those after it as `sb install` lines, exit 1; sb
      interrupted (SIGTERM) during one: the warning with those lines,
      death by the signal;
    - what moved (decisions 36 to 38): piped, the block and the hint
      on stderr, plain, and the run log's `checkout pulled` and `tags
      compared` records.
  - **What moved, in process** (`crates/ops/tests/update.rs`, the
    stand-in `ansible-playbook` listing a file the remote commits): a
    tag added and one removed in Sandbox (the lines, the H2 hint);
    nothing moved (both `already up to date`, no hint); only Sandbox
    moved (Sandbox's hint); moved with the same tags (`, no tag
    changes`); and the range only, never the tags, for each case of
    decision 37: no cache file (the first update), no entry, an
    unreadable file, a corrupt file, an entry of another shape, an entry
    for another commit, empty tags, a failed rebuild (with the run's
    error, and no hint); Saltbox's runtime failing after its pull (the
    range, no hint); a failure after Saltbox's pull (the Saltbox line,
    no Sandbox line, no hint).
  - **Units** for the block: the layout of each line kind, the 80-column
    wrap, the two-line cap and its `and <n> more (sb list)`, a list that
    fits exactly two lines, the colours by role and plain with the plain
    theme, each hint's text.
  - **`announcements`:** the root's own file and Sandbox's (under the
    test's own appdata path, an inventory in the temporary root, never
    the host's), piped: every entry that applies inline, each headed with
    its repository, no migration table for an entry with a migration, no
    child started, nothing called new; both repositories, Saltbox's
    first, and Sandbox's alone still labelled; a missing Sandbox checkout
    (Saltbox's alone, no notice); a checkout without its file (the notice
    and the run log's `file="missing"` with its path); an invalid file (a
    warning naming it, the other repository's shown, exit 0; both
    invalid, nothing shown, exit 0); `--before`, `--after`, `--repo` and
    their short forms, and `--migrations`: clap's error, exit 2; the
    step line's count, piped and on a terminal; on a PTY the viewer opens
    with both repositories in one list, each row naming its repository,
    `q` closes it (exit 0) and Ctrl-C ends with 130, the terminal
    restored; tui's snapshots of the list and pane with both
    repositories, marked (`update`) and not.
  - **`announcements preview`** (2026-11-18): temp git checkouts made
    with the host's git (a commit holding the before file, then the
    working tree's edit): piped, the new entries inline and the step's
    `<Repo>: <n> new since <ref>` line; `--since` a branch, a ref
    without the file (every entry new), and a bad ref (the error naming
    it, exit 1); `--repo` each, and a missing Sandbox checkout skipped
    without it; an invalid file at the ref (`update`'s warning); the
    migrations piped: the table, the would-run lines and the dry-run
    line, the stand-in `ansible-playbook` not run; `--run-migrations`
    piped: the stand-in records `--tags <tag>` in each checkout, and a
    failing one gives `update`'s error with its `sb install` lines; on a
    PTY: the viewer with the new entries marked, `q`, then the table and
    the question, `y` giving the would-run lines and nothing run, and
    with `--run-migrations` `y` running them; Ctrl-C on the viewer, 130.
    Units: repo's `file_at` (a file, no file at the ref, a bad ref, a
    ref starting with `-` refused, a file over 1 MiB, not UTF-8).
  - **`announcements check`:** a good file (`no problems`, exit 0); a
    bad one with each problem above once (exit 1, each line with its
    entry, date and title); a file that doesn't parse (the loader's
    issues); a near tag's `did you mean` and the other repository's tag
    from a tags cache at the checkout's commit, and the skip note with
    no cache and with a stale one; both checkouts without a file
    argument, one without its file; `--repo`; text from the file
    escaped. Units in sb-ops for each rule, and in sb-tui for the
    Markdown checks (HTML block and inline, a broken reference link, a
    defined one, a code span holding `<b>`).

## Boundaries

- **Always:** fixed paths for every destructive step; lstat before
  chmod, replace or remove; secrets in `Zeroizing` and out of logs;
  stubs, namespaces and containers for host effects; synthetic
  fixtures; run sb as root only with `SB_LOG_DIR` in `target/` or a
  temporary directory; cap container runs with systemd-run.
- **Ask first:** any request to the real crs.saltbox.dev beyond the one
  read-only check in the restore task (a GET under a random key,
  expecting 404 and no redirect); a real Speedtest run (the platform
  note's one run); any change to the node list or the pinned asset;
  dependencies beyond the table; any difference not listed; publishing
  the install script.
- **Never:** run setup, gha or the cleanup on this host; fetch and run
  bench.sh or any script at run time; put a password, user name or key
  in argv, output or logs; follow a symlink in a destructive step;
  commit real backups, hashes, Speedtest output, addresses or host
  details; touch Saltbox's repositories or docs.

## Success Criteria

1. `sb setup`'s guard gives each of sb-go's five outcomes (tests), and
   `--force` allows only a real directory.
2. setup's and gha's recorded calls equal their sequences above,
   including gha's Python-before-fact order, on stubs in a namespace; a
   failing stub in each phase gives that phase's error text and exit 1.
3. gha works with a symlinked root and leaves the six files owned by
   `SUDO_USER`, mode 0600 (namespace test and the container matrix on
   22.04, 24.04 and 26.04).
4. The sources cleanup refuses a symlinked, foreign-owned or
   group-writable directory with nothing moved, never moves through a
   link, keeps `ubuntu.sources` on DEB822, moves every other entry into
   a new 0700 `/var/backups/sb/apt-sources/<run id>/` with its bytes and
   mode intact (the `EXDEV` copy too), creates no backup directory when
   there is nothing to move, and logs every moved name.
5. `sb setup -v` and `--verbose` run as without them and log one line;
   neither shows in `--help`; the contract test passes with the
   listed extensions.
6. The branch prompt appears on a PTY when the branch differs and reads
   `/dev/tty`; piped stdin is left unread; `--keep-branch` and
   `--reset-branch` answer it without asking; with no terminal and
   neither flag, setup fails naming both before any step, exit 1.
7. `sb restore` decrypts the openssl-made fixtures, places them 0600
   root-owned at the root, replaces a symlink without touching its
   target, and prints sb-go's lines; 0 restored exits 1, partial 0.
8. With a known user name and password, none of the name, the key, the
   password or a URL with the key appears in stdout, stderr or the run
   log, including on transport errors.
9. Without a terminal, `sb restore --user u --password-file -` works
   from piped stdin; with an answer missing, it fails naming the flag,
   exit 1, within a second.
10. `sb bench` prints the three sections and their rows on a namespace
    run with fakes; the disk test's file and the temporary directory are
    gone after success, a failure and Ctrl-C; a digest mismatch runs
    nothing.
11. No bench.sh, LICENSE or NOTICE file is in the repository or binary.
12. The install script passes shellcheck and its container tests; `-b`
    reaches `sb setup` as one argument; `-v` doesn't reach it.
13. Startup stays within the 10 ms budget and the `--help` ratchet; the
    binary-size change of the crypto and tar crates is measured and, if
    it crosses the tolerance, recorded in an ADR.
14. `sb update` with every flag runs with stdin, stdout and stderr all
   piped and no controlling terminal, and finishes with exit 0 on the
   fixtures.
15. Without a terminal and a needed flag, one error names each missing
   flag before any step, exit 1, and nothing on disk changed.
16. The step order and labels are sb-go's (snapshot of the plain
   output), apart from the listed differences.
17. The announcement diff, gating and order match sb-go's rules on every
   unit case; sb-go's own fixture files from Saltbox and Sandbox
   (date, title, message; `required_folder`) give the same new entries.
18. Migrations run all or none, each with its tag in its repository; a
   skip leaves the warning in the output and the run log.
19. `sb branch <name>` ends on `<name>` with no prompt; an invalid name
   changes nothing.
20. A successful self-update ends `update` with exit 0 and the "Re-run"
   warning, before apt.
21. Inline announcements go to stdout, sanitized, when stdout isn't a
   terminal.
22. `update` ends with the block and the hint of "What moved, and the
   closing hint", its tags only from an old cache entry valid for the
   commit before the pull, every case of decision 37 tested.

## Decisions

1. **setup accepts `-v/--verbose` hidden and ignored** until the served
   installer is replaced; removal is tied to that switch-over.
   decided.md G-Q1.
2. **setup, restore, bench and gha keep their names and hiddenness, and
   setup its `--branch/-b` and `--force`.** Map decisions 1 and 2;
   Saltbox's CI calls `sb gha`, the installer `sb setup -b`.
3. **gha and restore have no `-v`.** Term-ui decision 9, config decision
   1, docker decision 6. *Amended 2026-11-18: restore, which sb-go gave
   `-v`, accepts it hidden with a notice (the maintainer, 2026-11-18);
   gha, which sb-go didn't, still refuses it.*
4. **setup's phase order and texts, `--force`'s meaning and gha's order
   are sb-go's.** sb-go is the reference (map); host spec for the
   checks.
5. **The sources.list.d cleanup is fenced** as a destructive operation:
   a fixed root, the directory checked and opened without following,
   each entry handled through its handle one level below, names logged
   first.
   The maintainer's request of 2026-11-13 (security-and-hardening's
   rules for destructive operations); a host amendment.
6. **Prompts read `/dev/tty`** and every prompt has a flag or fails
   naming it. term-ui spec, "Prompts" and criterion 10 (decided.md cites
   this rule as "term-ui decision 2").
7. **restore specs today's protocol:** crs.saltbox.dev
   `/load/<sha1(user)>/<file>`, OpenSSL `Salted__`, PBKDF2-SHA256 at
   10000 iterations, AES-256-CBC, with the RustCrypto crates. The
   maintainer's answer of 2026-11-13 (G-Q4); stack table.
8. **restore's answers are `--user` and `--password-file <path|->`,
   never a password flag.** decided.md G-Q2.
9. **restore's placement and modes are sb-go's** (root level, 0600,
   root-owned, a symlink replaced), through `tempfile`'s persist. sb-go
   restore.go; stack table (atomic saves).
10. **The user key and the user name are secrets**, as the password:
    out of output, errors and logs. decided.md G (the hash is treated as
    a secret); map decision 11 (never logged: secrets).
11. **bench is rewritten in Rust:** in-process reads for the system part
    (bench.sh's logic: decision 33), an
    in-process fsync'd disk test, Speedtest as the sha256-pinned Ookla
    binary; a new output format, a listed difference; the vendored
    script and its licence go. The maintainer's answer of 2026-11-13
    (G-Q3).
12. **The disk test writes in the working directory**, as bench.sh, and
    needs 2 GiB free (host's disk-space floor). Writing in `/tmp` could
    measure a tmpfs.
13. **Speedtest is pinned per architecture:** sb-go's
    `ookla-speedtest-1.2.0-linux-x86_64.tgz` (size 1106829, sha256
    `5690596c54ff9bed63fa3732f818a05dbc2db19ad36ed68f21ca5f64d5cfeeb7`,
    host/benchmark.go:20-22) and Ookla's 1.2.0 `linux-aarch64` asset
    (size 1082287, sha256
    `3953d231da3783e2bf8904b6dd72767c5c6e533e163d3742fd0437affa431bd3`,
    taken from Ookla's download at M78; docs/platform-notes/speedtest.md).
    arm64 sb binaries are published (map decision 6).
14. **No disk-space check is added to setup or gha.** sb-go has none;
    adding one would be an unrequested difference.
15. **restore keeps creating a missing Saltbox root** (sb-go). The docs
    order (setup first) avoids the conflict with setup's guard.
16. **The install script starts from sb-go's install.sh plus `-b`,**
    runs `sb setup` attached, never reads stdin, and is published only
    as a separate, authorised step. decided.md G-Q5; the map's open
    item; tasks/prototypes/todo.md, "Install script".
17. **Download URLs are pinned** to GitHub's releases path in the
    script; saltyorg/sb-rust's `sb_linux_<arch>` and `_beta` assets
    (decided.md D-Q2; D-Q3 settled 2026-11-18, release spec, "Release
    channels").
18. **No `-v`** on `update` and `announcements` (config decision 1,
    docker decision 6; research D §6.1). *Amended 2026-11-18 by the maintainer's decision: `-v` and `--verbose` are accepted, hidden from help, and change nothing; the run prints one notice on stderr before its steps, naming this run in the run log (`-v isn't needed: sb logs full detail to <file> (run <id>)`), once however often the flag is given (crates/sb/src/verbose.rs).*
19. **Every prompt has a flag:** `--keep-branch`/`--reset-branch`
    (sb-go's), `--migrations run|skip` (new), self-update answered inside
    `update` (term-ui "Prompts"; research D §6.2, §6.14). Without a
    terminal `--migrations` is required up front, because its question
    can't be asked again later (decided.md D-Q4's condition, "flags answer
    every prompt").
20. **`sb branch <name>` doesn't prompt:** the argument is the given
    answer (term-ui "Prompts"; research D §6.3).
21. **One runtime step** (python-env's `reconcile_runtime`, as `update` and
    `update-venv` use) for `branch` too (research D §8: one operation in
    one order).
22. **Skipped migrations are named** in a warning (the run log answers
    "what did each step print"; capability map decision 11).
23. **An unreadable announcements file warns in `update`,** and skips
    that repository's announcements: a broken file would otherwise block
    the fetch that fixes it.
24. **`update` without a terminal; announcements inline then**
    (decided.md D-Q4; the inline fallback rule of decided.md A-Q4: stdout
    not a terminal).
25. **Self-update inside `update` ends the run with exit 0** and the
    "Re-run" warning (research D §6.13; sb-go 6108017).
26. **The announcement viewer and inline Markdown are tui's** (module
    table; research D §6.16); `update`'s full form is built after tui.
27. **`announcements` is shown and only reads** (the maintainer,
    2026-11-15, replacing "the hidden `announcements`
    stays as sb-go's test harness, with `--migrations`"): migrations run
    in `update` only. A reader needs no flag, so none is required:
    without `--before` every entry is new (sb-go's rule for no before
    file, proto-markdown's viewer), without `--after` the repository's
    own file (assumption 7). Decided from these specs. Its
    "`-r` keeps sb-go's default, Saltbox" is replaced by decision 30.
    *Amended 2026-11-17 (the maintainer):* no `--before` or `--after`:
    "new" belongs to `update` alone, which compares the files
    in-process; the command shows the current announcements that apply,
    calling none new (difference 45).
28. **setup moves the cleared apt sources aside** into a root-only
    `/var/backups/sb/apt-sources/<run id>/` instead of deleting them
    (difference 5). Reason: the hardening rules for destructive
    operations (security-and-hardening): `setup --force` and `gha` run
    over existing servers, and a removal that can't be undone has no
    reason to stay that way. Decided from recorded
    rules, 2026-11-13.
29. **setup takes `update`'s `--keep-branch` and `--reset-branch`;**
    with no terminal and neither flag, the branch question fails naming
    them (difference 6). Reason: term-ui's rule that every prompt has a
    flag or fails naming it (term-ui spec, "Prompts" and criterion 10;
    decided.md cites it as term-ui decision 2). Decided
    from recorded rules, 2026-11-13.
30. **`announcements` shows both repositories by default** (the
    maintainer, 2026-11-15, at M74b's look): without `-r`, Saltbox's and
    Sandbox's announcements in one list (the viewer's pane names each
    entry's repository), Saltbox's first in the inline form; `-r` narrows
    to one. A missing Sandbox checkout is no file: Saltbox's alone, no
    error. `--before` and `--after` name one repository's files, so
    either without `-r` means Saltbox's (sb-go's default for them). Replaces decision 27's "`-r` keeps sb-go's
    default, Saltbox". Root stays required (assumption 2; the
    maintainer, the same day). *Amended 2026-11-17 (the maintainer):* no
    `-r`: both repositories, always, each entry labelled with its
    repository in the viewer and the inline form (difference 45).
31. **`announcements` reads leniently without `-r`, and counts what it
    shows** (the maintainer, 2026-11-16). With both repositories and no
    file named, a repository's file that can't be loaded warns, naming
    the file, and the other's announcements show with exit 0, as `update`
    does (decision 23) and as a missing checkout already did; with `-r`,
    or a file named with `--before` or `--after`, it fails with exit 1.
    The step's info line is a count of what will be shown (`12
    announcements`), not of new ones: "new" stays `update`'s word. The
    rule covers Saltbox's own file too (decided: neither
    file is named then); with both invalid, each warns and nothing shows,
    exit 0, as `update` skips each. *Amended 2026-11-17:* with no flags,
    always leniently; nothing it shows is called new.

32. **`sb restore` on a terminal is a form, a live list and a viewer of
    the restored files** (the maintainer, 2026-11-16, replacing M76's
    line prompts; amended 2026-11-17): tui's restore screen, on sb-tui's
    screen pieces (the `Tui`/`App` loop, ratatui-textarea for the
    fields, the announcement viewer's menu beside a tui-scrollview pane).
    Once the run ends, the viewer asks the user to check each restored
    file, then Continue closes it (a later step of a flow would follow).
    Secrets: "Logging should mask but not the screen, unless you can
    show it optionally on demand in the TUI": masked on screen with `s`
    and `a` to show them, one rule shared with the fact editor, which
    gains `apprise`, `auth`, `credential`, `hook` and URLs with
    credentials. Both `--user` and `--password-file` given: no screen,
    the line form. Decided: "a terminal" is stdout's, as the
    log viewer's screen; Esc in the form keeps sb-go's cancel; the final
    list prints after the screen closes in the line form.

33. **bench's System rows and disk test follow bench.sh's logic** (the
    maintainer, 2026-11-16: bench.sh's tested logic is
    followed). Each row is found
    as sb-go's `cmd/bench_script.sh` finds it, in process where the
    script ran a program: df's types, its used (`f_blocks - f_bfree`)
    and its one-entry-per-device rule, the swap and zfs pools in Total
    Disk, free's figures, the last `model name` and `cache size`, the
    first `cpu MHz`, the whole-file greps for AES and virtualization,
    `PRETTY_NAME`, `uname -m (LONG_BIT Bit)`. A difference stays only
    where this spec requires it (its output format, decision 11 and
    difference 15; systemd-detect-virt only, difference 16; the 2 GiB
    floor, difference 18), the script's way can't be had in process
    (difference 49), the maintainer asked (difference 46), the script's
    arithmetic was wrong (differences 47 and 52), a hang has to end
    (difference 53), or sb's other screens already decide (difference
    50: sb-tui's zone); difference 51 keeps `free`'s figure on each
    release. Each is listed (differences 46 to 58; at M78, 54 to 58 for
    the network rows and Speedtest, differences 17 and 20 already
    listed).

34. **A hidden `sb announcements preview`** replaces sb-go's
    before/after harness (the maintainer, 2026-11-18, who didn't like
    it): the file at a git ref against the working tree, through
    `update`'s code, so what a developer sees is what `update` will
    show; migrations a dry run unless `--run-migrations`. Without a
    terminal no question is asked and nothing runs without the flag
    (the coordinator's suggestion, taken: `update`'s rule that an
    unanswered question needs its flag).
35. **A hidden `sb announcements check [file]`** for developers and
    Saltbox's CI (the maintainer, 2026-11-18): every problem with its
    entry, exit 1 on any; the tag check from the tags cache only,
    skipped when it isn't fresh, rather than guessed; the loader's own
    words where they overlap. `--repo` names a given file's repository
    for the tag check (an addition: a Sandbox file outside its
    checkout can't be told by its path).
36. **`update` says what moved and what to do next** (the maintainer,
    2026-11-18, the report's "sb update should say what changed and
    what to do next"): a short block after the tree (layout B, chosen
    over the tags inside the tree, which can't wrap a long list, and
    over counts only, which `sb list` can't point at), then hint H2, or
    Sandbox's own when only Sandbox moved, and none when nothing moved.
    No list of new roles: sb can't tell which roles a user cares about;
    the tags are what the user types. sb can't tell which main tag a host
    was installed with (Saltbox keeps no install record), so the hint
    names the options. Decided: H2's second line keeps its
    description in the first line's column (the mock-up had it one
    column to the left, its `<apps>` written as HTML entities), and
    Sandbox's hint takes H2's layout so both read alike; the block and
    the hint go to stderr with the tree; removed tags are in Warn.
37. **Tags only from a valid old entry** (the maintainer, 2026-11-18):
    never every tag as new because the old cache is missing or doesn't
    fit the commit before the pull; each of those cases is tested,
    the first update on a host with no cache among them.
38. **A partial run shows what moved, without the hint** (the
    least surprising option, chosen at the maintainer's request):
    a checkout whose pull finished is on disk at its new commit whatever
    failed after it, so the block names it; a step that didn't finish
    shows nothing; the hint, which assumes a finished update, is left
    out, so the last thing shown is the error and what it asks for.
    The other options: nothing at all (a moved checkout then goes
    unmentioned, and the next run says `already up to date`), or the
    hint too (advice to run `sb install` while, say, the config doesn't
    validate).

## Known limits

- The viewer reads the placed file again, as root, following a link,
  with no type or size check: someone else able to write the Saltbox
  root could point it elsewhere. The root is the operator's (Saltbox's
  seed user's), so this is left as it is. Its copies of the file (the
  lines, the frames) aren't cleared; the file is on disk anyway.
- The viewer masks by the key's name and a URL's credentials (the
  shared rule): a secret under another name shows, such as a token in a
  URL's path (an Apprise URL under a key that isn't `apprise`), a
  flow mapping's values, or the lines of a block scalar under a secret
  key. A file past 2,000 wrapped rows is cut there in the pane, as the
  announcement viewer's.
- The form's fields are ratatui-textarea `String`s, not cleared on drop:
  sb takes the answers into `Zeroizing` buffers and empties the fields
  at once, with no undo history, but the fields' freed memory and the
  textarea's yank buffer (Ctrl-K, Ctrl-W) aren't cleared.
- Clearing secrets is best effort: sb's own buffers, AES's key schedule,
  CBC's IV and the hash cores are cleared on drop, but hmac 0.13's
  padded key block and PBKDF2's intermediate blocks are stack temporaries
  the crates don't clear (module build M75's security audit).
- A forced exit (a second signal) or SIGKILL while a file is written can
  leave one `.sb-restore-*` file (0600, root's) in the Saltbox root.
- `--user` puts the user name in the process list and in sudo's command
  log (module build M76's security audit); the form keeps it out.
- Copies the crates keep aren't cleared: reqwest's request URL (it
  holds the key), std's stdin buffer for `--password-file -`.
- The backup format is unauthenticated (CBC, no MAC, decision 7): a
  changed ciphertext decrypts to changed plaintext without the password.
  A wrong password that passes the padding check (about once in 256
  files) is caught by the text check (difference 44) unless its garbage
  happens to be UTF-8, which is rare. Integrity rests on HTTPS to the
  service and refused redirects (difference 13).
- `--password-file <path>` is read after the sudo relaunch, as root. A
  user with sudo can read the file anyway; the content is never shown.
- bench's Speedtest nodes are fixed IDs from Ookla's list; a node Ookla
  retires shows `Test failed` until the list is changed.
- The cleanup's directory checks can't stop root itself from planting
  a link between the check and the open; the open refuses one anyway.
- Migrations declined in one run aren't offered again (the diff is
  empty next time); the warning lists them, and `sb install <tag>`
  (`sb install sandbox-<tag>` for Sandbox's) runs one by hand. sb-go is
  the same. After a failed or interrupted migration, its error or warning
  gives those lines for it and the ones left (difference 43).
- A Sandbox checkout that doesn't exist fails `update` with repo's
  `inspect required directory` error, as sb-go.

## Open questions

None. The two raised in research (the removed apt sources; setup's branch prompt
with no terminal) were decided on 2026-11-13 from recorded rules:
decisions 28 and 29.

## Amendments to other specs

**Repo spec (2026-11-18, `announcements preview`):** `Repo::file_at(ref,
path)`, a local read: `git rev-parse --verify --quiet <ref>^{commit}`
(a ref starting with `-` refused first, as branch names are), `git
ls-tree <commit> -- <path>`, then `git cat-file -s` and `git cat-file
blob <commit>:<path>`, each within 10 s; the file's text, `None` when
the commit has no blob at the path, held to 1 MiB and UTF-8 (exec's
lossy capture: a U+FFFD counts as not UTF-8); `RepoError::Ref` (`'<ref>'
isn't a commit in <path>`) when `rev-parse` exits 1, and git's own error
for any other failure (no repository, its dubious-ownership refusal).

**term-ui spec (2026-11-18, `update`'s end):** `wrap_line(line, width,
words)`, the tree's wrap (tabled's `Wrap::wrap` by display width, a
colour closed and reopened at each break), becomes public, for
`update`'s tag lists.

**Tui spec (2026-11-18, `announcements check`):** `markdown::problems(message)`,
what the announcement's Markdown can't show: HTML (block and inline,
pulldown-cmark's `Html` and `InlineHtml` events) and reference links
without a definition (its broken-link callback). pulldown-cmark becomes
a direct dependency of sb-tui at tui-markdown's version (0.13.6, already
in the tree through it), default features off.

**Capability map, "Open items carried into module specs", the "Install
script" item** (stale: the served installer no longer captures output).
Replace it with:

> - **Install script:** moves into this repository (ops spec, "The
>   install script"), starting from sb-go's `install.sh` plus `-b`. The
>   served installer (`sb_install.sh` in saltyorg/sb, behind
>   install.saltbox.dev) no longer captures `sb setup`'s output (checked
>   2026-11-13); it passes `-v` and `-b`, so `sb setup` accepts `-v`
>   hidden and ignored until the served script is replaced. Publishing
>   the new script is a separate, explicitly authorised step.

**Capability map, "Commands" table:**
- `bench` row, planning note: `rewritten in Rust (ops spec decision 11):
  sysinfo, an in-process disk test, the pinned Ookla Speedtest; no
  vendored script`.
- `setup` row, first column: `setup` (hidden; `--branch`, `--force`,
  `--keep-branch`, `--reset-branch`, `-v` hidden and ignored).
- `restore` row, planning note: `RustCrypto stack (stack table);
  --user and --password-file, no -v (ops spec)`.
- `update` row, first column: `update` (`--keep-branch`,
  `--reset-branch`, `--skip-self-update`, `--migrations`).
- `branch` and `branch-sandbox` rows: `branch <name>`,
  `branch-sandbox <name>` (required).
- `announcements` row, first column: `announcements` (no flags);
  planning note: `reads Saltbox's and Sandbox's announcements, every one
  that applies, each labelled with its repository: tui's viewer on a
  terminal, inline otherwise; "new" and migrations stay with update (ops
  spec, decisions 27 and 30, difference 45)`. *Amended 2026-11-17.*

**Capability map, stack table:** a row for archives, shared with the
python-env spec: `Archives (uv, Speedtest) | tar + flate2 (rust_backend)
| 0.4.46 / 1.1.10 | — | archive/tar, compress/gzip | untested (python-env
and ops specs)`; the `Host OS release` row's sysinfo note gained `disk`
(bench's Total Disk), withdrawn 2026-11-16 (decision 33: bench lists
the mounts through procfs).

**tasks/prototypes/todo.md, "Later: Install script lives in this
repository", "Why":** the capture premise is stale for the same reason;
the section points at the ops spec.

**host spec, "apt", `add_repositories`, the "Cleanup" bullet.** Replace
with:

> - **Cleanup:** clears every entry in `sources.list.d`; on DEB822
>   releases it keeps `ubuntu.sources`. The entries are moved into a
>   root-only `/var/backups/sb/apt-sources/<run id>/`, not deleted. Both
>   directories are fixed paths (the function's root arguments exist for
>   tests only). `sources.list.d` is opened with `O_DIRECTORY |
>   O_NOFOLLOW`; a symlink, or a directory not owned by uid 0 or
>   writable by group or others, fails before anything is moved
>   (`refusing to clean <dir>: <reason>`), and so do `/var/backups/sb`
>   and `apt-sources` (created 0700 when missing); the run's directory is
>   created 0700, exclusively, only when there is something to move.
>   Each entry is `lstat`ed through the handle and moved with `renameat`
>   between the two handles, so a link is moved, never followed; across
>   filesystems (`EXDEV`) it is copied, synced, then unlinked through the
>   handle. A subdirectory fails, as in sb-go (`error removing <p>:
>   <cause>`). The names are logged before they move, and the step
>   reports their count and the backup directory (ops spec decisions 5
>   and 28; difference 13).

and add to the host spec's differences: `13. The sources.list.d cleanup
refuses a symlinked, foreign-owned or group- or world-writable
directory, works through directory handles, and moves the entries into
/var/backups/sb/apt-sources/<run id>/ instead of deleting them (sb-go
deleted by path).` The Boundaries' "anything touching `/etc/apt` outside
`add_repositories`" gains `/var/backups/sb` as that function's backup
target.

**host spec, "Disk space":** add `free_space(path) -> u64`, the
`f_bavail × f_frsize` of the path's nearest existing ancestor, which
`check_disk_space` uses and bench calls.

**term-ui spec, "Out of scope":** drop "restore prompts (deferred with
the restore backend)"; the text and password prompts' first user is `sb
restore` (ops spec; the confirm comes earlier with `release`). Add under "Prompts": `dialoguer's text and password prompts have
no cancel key; Ctrl-C is the way out (cli-core's interrupted ending).`

**cli-core spec, "Command tree", deliberate extensions:** add `sb restore
--user` and `--password-file`, `sb setup`'s hidden `--verbose`, and `sb
bench`'s help text (ops spec), with the contract test's `EXTENSIONS`
entries `flag\tsb restore\tlocal\tuser`, `flag\tsb
restore\tlocal\tpassword-file`, `flag\tsb setup\tglobal\tverbose` and
`command\tsb bench` (taken at M77 as the stricter `REWORDED` entry for
sb-go's `command\tsb bench\tfalse\tRuns bench.sh benchmark` line, as only
the help text differs; the maintainer, 2026-11-16), plus `sb setup`'s `--keep-branch` and
`--reset-branch` (decision 29; entries `flag\tsb setup\tglobal\tkeep-branch`
and `flag\tsb setup\tglobal\treset-branch`), and the new `--migrations`
on `sb update` (`flag\tsb update\tlocal\tmigrations`); `sb announcements`
is shown where sb-go hid it, the contract test's `REWORDED` entry for its
command line (decision 27), and its four sb-go flags are missing on
purpose, each with its reason in the contract test (difference 45).

**config spec, Scope:** the `saltbox_user()` amendment, one function for
repo's callers, python-env and ops (its wording is in the repo spec's
amendments).

**tui spec:** the announcement viewer's input and its inline form are
in the tui spec's Scope (this spec's decision 26).

## Suggested slices for the plan

1. **restore core, the first vertical slice (S):** `user_key`,
   `decrypt`, the openssl-made fixtures, and the placement layer on a
   temp root. Needs nothing but the new crates.
2. **`sb restore` (M):** term-ui's text and password prompts (first user), the password
   file, the fetch against the fake service, the output lines, the
   secrets log test, the one read-only check of the real service's 404.
3. **bench system and disk (M):** sysinfo rows, cpuinfo parsing,
   host's `free_space`, the in-process disk test, the tables.
4. **bench network (M):** release's verified download of both pinned
   assets, the tar extraction, the Speedtest JSON (platform note from
   one run), icanhazip and ipinfo.
5. **Shared pieces (M):** `prerequisites`, `locale`,
   `copy_default_configs`, `secure_existing_configs`, `git_hooks`, the
   host cleanup amendment; the stub-and-namespace harness.
6. **`sb gha` (S):** the sequence, `SUDO_USER` ownership, the container
   matrix.
7. **`sb setup` (M):** guard, phases, branch prompt and its flags,
   hidden `-v`, completion refresh.
8. **The install script (M):** `install/install.sh` from sb-go's plus
   `-b`, its container tests, the acceptance checks.
9. **The branch commands (M), the update part's first vertical slice:** `sb branch` and `sb branch-sandbox` on
   repo (no runtime or tags until those modules land; each added when
   it does).
10. **Announcement rules (S):** load, diff, gate, sort, migration list,
   with `sb announcements` printing inline (the viewer on a terminal
   follows with tui's viewer).
11. **`update` skeleton (M):** the up-front flag check, disk space,
   self-update, branch questions, apt, both repositories, secure files,
   validation; announcements inline.
12. **Migrations (S):** the table, the confirm, ansible's `run`.
13. **Viewer and completion (S):** tui's viewer on a terminal and the
   completion refresh, when those modules exist.
