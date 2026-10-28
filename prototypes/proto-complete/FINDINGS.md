# Prototype: proto-complete

**Question:** Can clap_complete's runtime completion supply Ansible tags
dynamically in bash, zsh and fish, replacing sb-go's tag_completion?

**Crates tried:** clap@4.6.7, clap_complete@4.6.11 with the
**`unstable-dynamic`** feature (`CompleteEnv` and `ArgValueCompleter`).

**What was built:** an `install <tags>` command whose argument uses an
`ArgValueCompleter`. It completes the last element of a comma-separated list,
keeps the prefix, skips tags already chosen, and offers Sandbox tags under a
`sandbox-` prefix, as sb-go does. Tags come from a static list; the module
reads Saltbox's tag cache. `CompleteEnv::with_factory(Cli::command).complete()`
answers the shell's requests, and the shell is registered with
`source <(COMPLETE=<shell> proto-complete)`.

**How it was checked:** an interactive bash (`--norc`) and zsh (`-f`, with
`compinit`) driven in a PTY with real Tab presses.

## Result

- **bash:** subcommands, tag filtering (`s<TAB>` lists the six matching tags),
  a unique comma completion (`plex,so` → `plex,sonarr`), and Sandbox-prefixed
  tags after a comma all work.
- **zsh:** the same results, plus each subcommand's description from clap's
  doc comments.
- **fish:** not tested. The 1-line registration is generated, but the
  behaviour is unverified.
- **No per-shell code is ours.** `COMPLETE=<shell>` prints a small
  clap-generated registration stub (34 lines for bash, 36 for zsh, 1 for
  fish) that calls the binary at Tab time. The completion logic is a single
  Rust function shared by all shells. An `sb completion install` only needs to
  write that stub to each shell's completion directory.
- **Cosmetic:** with comma lists, the candidate menu shows the whole string
  (`plex,sonarr,sandbox-komga`), not just the last tag. That's acceptable, and
  it matches what the shell will insert.
- **Stability risk:** the runtime engine is still behind `unstable-dynamic`
  in clap_complete 4.6, so its API may change in a minor release.
  Mitigations:
  - Pin the exact `clap_complete` version.
  - Keep the completer function small and isolated.
  - Fallback if it breaks: static scripts from `clap_complete::generate`,
    plus a hidden subcommand for tags. That brings back a small amount of
    per-shell glue.

## Verdict

**Adopt with changes:** use the dynamic engine, with an exact version pin and
an ADR recording the unstable-feature dependency.

## Map impact

- Stack table: `clap_complete` with `unstable-dynamic`, pinned exactly; ADR
  needed.
- `completion` module: one tag completer reading the tag cache. Install
  writes the generated registration stub per shell. Test fish before release.
