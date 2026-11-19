# Moving from sb-go to sb

sb has sb-go's commands, flags and output wherever they made sense. This page lists the
changes you could trip over, what is new, and where things are now. The
full list of differences is in each module's spec under `docs/specs/`
(the "Differences from sb-go" sections).

## The upgrade

Once sb is declared ready for production, a final sb-go release will
update itself to sb's newest release from
[saltyorg/sb-rust](https://github.com/saltyorg/sb-rust), so `sb update`
moves you across, and replaces sb-go's shell-completion files in place.

To switch before then, run this repository's `install/install.sh` with
`--repair` (install.saltbox.dev still serves sb-go's installer until the
new one is published), then `sudo sb completion install`: sb-go's
completion files call a command sb doesn't have.

## Changes that could trip you up

### Scripts and automation

- **Prompts read the terminal, not stdin.** Piping `y` into sb no longer
  answers anything. Each question has a flag instead:
  - `update --reset-branch` or `--keep-branch`, and `--migrations run|skip`;
  - `setup --keep-branch` or `--reset-branch`;
  - `restore --user` and `--password-file`;
  - `self-update --yes`;
  - `edit --no-edit-again`.

  With no terminal and no flag, sb stops and names the flag it needs. It
  never guesses an answer and never hangs.
- **Parse errors exit 2,** not 1: an unknown command or flag, or a bad
  value. `sb docker` with only a global flag is an error, not help.
- **A failed playbook exits with Ansible's own code,** not 1, and Ctrl-C
  during one exits 99, Ansible's, not 130: `sb install`, and the
  migrations `sb update` runs. A script testing `$? -eq 1` needs `-ne 0`.
- **`-v`/`--verbose`** is accepted on the commands that had it and ignored,
  with a note: the run log has the detail now. `install` keeps `-v` to
  `-vvvv`, which it passes to Ansible.
- **`sb docker logs` is gone** (exit 2): use `sb logs`, which shows
  Saltbox's services and Docker containers in one picker (`Tab` twice
  shows the containers alone). The error names it too.
- **`sb announcements`' `-b`, `-a` and `-r` are gone** (exit 2).
- **`sb branch` switches without asking.**
- **`sb edit` exits 1** when the file you edited has issues.
- **Host-changing commands run one at a time:** `install`, `update`,
  `setup`, `gha`, `branch`, `branch-sandbox`, `self-update`,
  `update-venv`, the reinstall commands and `restore`. A second one waits,
  printing `another sb command is running (...); waiting… (Ctrl-C to give
  up)`. Read-only commands such as `motd`, `list`, `logs` and
  `validate-config` never wait.

### Stricter checks

- **validate-config fails where sb-go passed** on:
  - the schemas' `pattern` and `additionalProperties` rules;
  - YAML that Ansible's libyaml refuses: a second document, a duplicate
    anchor, a key-less `:`, a control character, an unknown `!tag`;
  - merge keys (`<<`), which aren't followed;
  - very deep nesting.

  Each issue now comes with its line and column.
- **Redirects aren't followed** by motd's providers, the credential checks,
  the Docker controller or restore. A motd.yml URL that redirects from HTTP
  to HTTPS shows a `status code 3xx` line; use the HTTPS address.
- **Docker:** a `DOCKER_HOST` using TLS or ssh is refused, and
  `DOCKER_API_VERSION` isn't read.
- **Size limits:** config files, schemas, fact files and a motd
  `--banner-file` over 1 MiB are refused.
- **Self-update** takes downloads only from the release's own URL, and
  runs the new binary before swapping it in.

### Different output

- **New layouts:** `sb list` (no `(cached)` marker), `sb docker ps`
  (status texts and colours), `sb fact`, and `sb bench`. Bench numbers
  can't be compared with bench.sh's.
- **motd:** idle containers are no longer listed, queue states read "with
  a warning", and redaction is `[REDACTED]`. A colour motd.yml doesn't
  set comes from the GitHub Dark Colorblind palette instead of sb-go's
  charmtone defaults.
- **Announcements** join line breaks inside a paragraph, as Markdown does.
- **`sb update` and `sb self-update` draw one tree,** printed when the
  run ends. Their questions (the branch questions, `Run all migration
  tags?`, `Do you want to update`) open on the alternate screen, with
  what they are about, and each answer is a `›` line in the tree.
  Skipped migrations are a `!` line there too, not a `warning:` line.
- **Errors** go to stderr as a `✗` block, with a `details:` line pointing
  at the run log. A failed playbook's block has none: Ansible's output
  above it says what failed, as with sb-go.

## What's new

- **Release channels:** `sb version` shows yours, and
  `sb self-update --channel beta|release` switches once. See the
  [README](README.md#release-channels).
- **arm64 binaries** are published, for when Saltbox supports arm64. The
  install script and Saltbox are x86_64 only until then.
- **The run log** at `/var/log/sb/sb.log`.
- **A log viewer** for `sb logs`, services and containers together: a
  live picker, search, wrap, mouse support and saving to a file.
- **Editors:** `sb edit` validates after editing, and `sb fact` masks
  credentials, with Show and Copy.
- **motd** gains a Certificates line (with `--traefik`) and a safer
  `--share`.
- **A theme file** for sb's colours.
- **`sb docker ps --json`.**
- **`sb update` ends with what moved:** each checkout's commits
  (`Saltbox  a1b2c3d → e4f5a6b`, or `already up to date`), the tags the
  pull added or removed (when the tag cache held the old commit's
  tags), and how to apply the update: `sb install saltbox` (or your main
  tag), or at least `sb install core` plus your apps; Sandbox's apps by
  their `sandbox-` tags. sb-go printed none of this.
- **Restore** shows a live per-file list and a viewer of the restored
  files with secrets masked, and decrypts in memory.
- **`version` and `self-update` run on an unsupported Ubuntu release,**
  with a warning, so such a host can still reach a newer sb.

## Where things are

| What | sb-go | sb |
|---|---|---|
| The binary | `/usr/local/bin/sb` | the same |
| Releases | saltyorg/sb-go | saltyorg/sb-rust, assets `sb_linux_<arch>` and `sb_linux_<arch>_beta` |
| Logs of a run | the terminal only | `/var/log/sb/sb.log`, rotated |
| Theme | none | `<appdata>/sb/config.toml`, by default `/opt/sb/config.toml` |
| Tag cache | `/srv/git/saltbox/cache.json` | the same |
| Shell completion | sb-go's files | the same paths, replaced by `sb completion install` and by `sb update` |
| Config and schemas | Saltbox's files | the same files, checked more strictly |
| Moved-aside apt sources | deleted | `/var/backups/sb/` |
