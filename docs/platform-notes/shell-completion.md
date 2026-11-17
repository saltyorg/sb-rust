# Shell completion through clap_complete's stubs

Verified 2026-11-17 for the `completion` module (module build M57;
completion spec, "Verification items").

**Software:** sb's debug build with `clap_complete` 4.6.11
(`unstable-dynamic`), in the `ubuntu:<release>` images, amd64, with the
shells from apt:

| Release | bash | bash-completion | zsh | fish | tmux |
|---|---|---|---|---|---|
| 22.04 | 5.1.16 | 2.11 | 5.8.1 | 3.3.1 | 3.2a |
| 24.04 | 5.2.21 | 2.11 | 5.9 | 3.7.0 | 3.4 |
| 26.04 | 5.3.9 | 2.16.0 | 5.9 | 4.2.1 | 3.6a |

**Method:** `crates/completion/tests/shells.sh`, one throwaway container
per release. `sb completion install` runs as root in it, with a second
name, `/usr/local/bin/saltbox`, a symlink to sb; it writes
`/usr/local/share/bash-completion/completions/<name>.bash`,
`/usr/local/share/zsh/site-functions/_<name>` and
`$__fish_sysconf_dir/completions/<name>.fish` (`/etc/fish/completions`)
for both names (module build M58; before it, M57 put
`sb completion generate`'s stub in the same places). A tag
cache at the default Saltbox root holds Saltbox's `plex`, `radarr`,
`sonarr` and Sandbox's `plex`. A new user's login shell (bash, zsh, fish)
runs in tmux; each case is typed, Tab pressed once, and a key bound in the
user's rc file copies the edited line to a file. That key is the only
setup in the user's files (with an empty `~/.zshrc`, so zsh's new-user
menu doesn't start). 18 cases per shell, the first Tab of each shell's
session included, and a second session whose first Tab completes a tag.

## Results

The same on all three releases.

1. **zsh autoload (item 1): confirmed, and fixed.** Debian's
   `/etc/zsh/zshrc` runs `compinit`, and the clean `fpath` holds
   `/usr/local/share/zsh/site-functions`. With clap's stub alone as `_sb`,
   the first Tab in a new zsh completed nothing on 24.04, the first
   release run (`sb vers` stayed, and so did `sb install so` when it came
   first); later Tabs worked. compinit
   autoloads the file as the body of `_sb`, which only defines and
   registers clap's completer. With the `funcstack[1]` line sb adds after
   the stub (`ZSH_FIRST_LOAD`, crates/completion/src/lib.rs), the first
   Tab completes, on all three releases. Upstream clap issue to propose: the zsh stub is written
   for `source`, not for an autoloaded `fpath` file.
2. **bash loader (item 2):** bash-completion's loader finds the file as
   `sb.bash` and as `sb` in `/usr/local/share/bash-completion/completions`
   on 2.11 and 2.16; sb writes `sb.bash`, as sb-go. A login shell loads
   bash-completion through the user's `~/.bashrc` from `/etc/skel`. The
   symlinked name completes in each shell from its own file, its first
   Tab included (`saltbox vers`, `saltbox install plex,so`).
3. **fish (item 3):** fish 3.3.1, 3.7.0 and 4.2.1 load the file from
   `/etc/fish/completions`; the stub's quoting works; an open quote
   (`sb install 'plex, so`) reaches sb with its quote, gets nothing, and
   the line is left as typed (sb-go's rule).
4. **Earlier words (item 4):** every shell passes them. fish passes them
   unquoted; bash and zsh pass them as typed, so `sb install "plex" p`
   offered `plex` until the completer unquoted them (shlex). bash breaks
   `--extra-vars=so` at the `=` (`COMP_WORDBREAKS`), so clap's engine saw
   `so` as a tag after a value `=`; the completer gives no tags after a
   lone `=`, and joins `--skip-tags`, `=`, `plex` again before reading
   the earlier tags (as bash-completion's `-n =`). bash's readline completes inside an open quote and closes it
   (`'plex, so` becomes `'plex, sonarr'`); zsh passes the quote on and
   gets nothing. No fallback to file names: a file `sofile` in the
   user's home was never offered for `--extra-vars so`, `--skip-tags so`
   or a tag.
5. **The completer path (item 5):** each name's zsh and fish file calls
   that name's own path (`/usr/local/bin/saltbox` for the symlink, not
   the installing run's `argv[0]`); each bash file registers every name,
   each calling its own path. Every file is 0644 and carries its marker.
6. **An upgrade from sb-go (module build M59):** with sb-go's own files
   in place first (its scripts before 0.1.2 in `/etc/bash_completion.d`
   and `/usr/share/zsh/vendor-completions`, which call `sb __complete`,
   and its current files in the new directories;
   crates/completion/tests/fixtures/sb-go), `sb completion install`
   replaced the current ones in place and removed the old scripts, and
   every Tab case above passed after it, on all three releases.
7. **Startup budget (item 6):** a request for `sb install tag2` against a
   cache of 300 Saltbox and 100 Sandbox tags, release build,
   `hyperfine -N --warmup 5 --runs 50`: about 4 ms mean, as `sb version`
   on the same machine; the budget is 10 ms (CONSTRAINTS.md, Startup).
