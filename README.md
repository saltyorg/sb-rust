# sb

`sb` is the [Saltbox](https://github.com/saltyorg/Saltbox) command-line tool. It installs and
updates Saltbox, runs its playbooks by tag, validates and edits its
configuration, shows the status screen at login, and browses service and
container logs. It is a rewrite in Rust of
[sb-go](https://github.com/saltyorg/sb-go), with the same commands; see
[MIGRATING.md](MIGRATING.md) for what changed.

sb runs on x86_64 Ubuntu 22.04, 24.04 and 26.04; a new Saltbox install
needs 24.04 or 26.04. arm64 binaries are published too, ready for when
Saltbox supports arm64. Until then the install script and Saltbox are
x86_64 only; a hand-installed arm64 sb updates itself on its channel.

## Install

Once this installer is published at install.saltbox.dev, which today still
serves sb-go's, a fresh Ubuntu host installs with:

```sh
curl -sL https://install.saltbox.dev | sudo -H bash && cd /srv/git/saltbox
```

The script is `install/install.sh` in this repository. It downloads sb from
this repository's releases and checks it: the
download URL is pinned, the size and type are checked, and the binary must
report the release's version and channel. It installs sb as `/usr/local/bin/sb` and runs
`sb setup`.

Options, passed after `bash -s --`:

| Option | What it does |
|---|---|
| `-b <branch>` | Install that Saltbox branch (default `master`) |
| `--beta` | Install the beta channel, which includes prereleases |
| `--repair` | Put the sb binary back on an existing install, nothing else |
| `-v`, `--verbose` | Show the installer's own detail lines |
| `--force-curl`, `--force-wget` | Choose the download tool |
| `-h`, `--help` | Show the options |

For example, `curl -sL https://install.saltbox.dev | sudo -H bash -s -- --beta`.

## Release channels

Each sb binary follows one channel, fixed when it was built:

- **release**, the default, follows stable releases only.
- **beta** follows every release, prereleases (`1.1.0-rc.1`) included. When
  a stable release comes out, beta hosts get it too and stay on beta.

`sb version` shows the channel. `sb update` updates sb on its own channel
first; `sb self-update`, hidden from `sb --help`, does the same on its own.

To switch once, run `sb self-update --channel beta`, or
`--channel release` to go back. Going back from a newer prerelease can
install a lower version; sb says so before it asks. The binary you switch
to keeps following its own channel from then on. Nothing is stored on the
host.

Builds made from source have self-update turned off (`--force-update`
overrides it).

## Commands

Run `sb --help` for the full list, and `sb <command> --help` for each one.
The main ones:

| Command | What it does |
|---|---|
| `sb install <tags>` | Run Saltbox, Sandbox (`sandbox-<tag>`) or mod playbooks by tag |
| `sb list [query]` | List the available tags |
| `sb update` | Update Saltbox and Sandbox, sb itself, and show announcements |
| `sb branch <name>` | Switch the branch Saltbox uses (`branch-sandbox` for Sandbox) |
| `sb validate-config` | Check Saltbox's config files against their schemas |
| `sb edit <file>` | Edit a config file, then validate it |
| `sb fact` | Browse and edit the saltbox facts |
| `sb motd` | The status screen: system, media servers, downloaders, certificates |
| `sb logs`, `sb docker logs` | Browse service and container logs, live, with search |
| `sb docker ps`, `start`, `stop`, `restart` | List and control Saltbox's containers |
| `sb announcements` | Saltbox's and Sandbox's news |
| `sb bench` | A system and network benchmark |
| `sb restore` | Fetch and decrypt a user's backed-up config files |
| `sb completion install` | Tab completion for bash, zsh and fish |
| `sb version` | The version, commit and release channel (`--json` for scripts) |

Every command but `version` and Tab completion runs as root: started as
another user, sb reruns itself through `sudo`. Commands that change the
host (`install`, `update`, `setup`, `branch`, `restore`, `self-update` and
the like) run one at a time. A second one waits for the first, and says
so.

Questions are asked on the terminal. Each one has a flag that answers it,
so `sb update --keep-branch --migrations run` runs without a terminal.

## The run log

Every root run is logged to `/var/log/sb/sb.log`, rotated by logrotate.
Each record carries the run's id. When a command fails, sb prints where the
full output is:

```
details: /var/log/sb/sb.log (run 4f2a91c0d3b5e817)
```

A failed Ansible playbook is the exception: its own output, on the screen
above, says what failed (`Scroll up to the failed task to review.`), and
sb's log doesn't have it.

Secrets are masked in the log, as on screen. One exception: values given
to `sb install -e` are shown and logged as typed (`-e @file` keeps them
out). The log is readable by root only.

## The theme

sb's colours can be set in `config.toml` in sb's directory under Saltbox's
appdata path, by default `/opt/sb/config.toml`. Saltbox's backups include
it. It starts from the built-in theme, `default`, and can change the
colour depth and any role's colour:

```toml
[theme]
color_profile = "ansi256"   # truecolor, ansi256, ansi or ascii
[theme.colors]
accent = "#5CCFE6"          # #RRGGBB for a role
warn = "#FFCC66"
```

The roles are `text`, `muted`, `faint`, `border`, `accent`, `accent_2`,
`on_accent`, `ok`, `problem`, `warn`, `idle`, `service`, `service_rule`,
`container`, `container_rule`, `attention_rule`, `highlight`, `link`,
`code` and `code_bg`. A file sb can't read or use gives a warning, and the
built-in theme. `NO_COLOR` and `--color=never` turn colour off, and
`SB_COLOR_PROFILE` picks the colour depth for one run.

## Reporting issues

Open an issue at https://github.com/saltyorg/sb-rust/issues. Include:

- `sb version`;
- the command you ran;
- the run's records from the log, found by the run id on the `details:`
  line: `sudo grep 4f2a91c0d3b5e817 /var/log/sb/sb.log`. Look over them
  before you share them.

For a log viewer screen, `sb logs` can save what it shows to a file.

## Building from source

sb builds with the Rust toolchain that `rust-toolchain.toml` pins:

```sh
cargo build --release -p sb
```

Such a build has self-update off. The project's quality checks are in
[CONSTRAINTS.md](CONSTRAINTS.md), run by `scripts/check.sh`.

## Licence

GPL-3.0-only; see [LICENSE](LICENSE).
