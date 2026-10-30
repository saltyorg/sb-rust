# Prototype: proto-help

**Question:** What do Rust's options for `--help` look like on sb's
command tree next to sb-go's (fang) help, and what does copying sb-go's
format cost?

**Crates tried:** clap@4.6.7 (`Styles`, `help_template`), clap-cargo@0.19.0
(`style::CLAP_STYLING`), color-print@0.3.7 (`cstr!`), clap-help@1.7.0
(termimad tables), anstyle@1.0.14, anstream@1.0.0, sb-cli-core's theme (the
real palette) and colorchoice-clap (sb's real `--color`).

**What was built:** sb-go 0.1.3's whole command tree (`src/commands.tsv`:
the contract-test fixture plus each command's cobra `Use` string, from
which the positionals are modelled), with sb's real `--color` and required
subcommands, drawn six ways. The clap looks print exactly what clap prints
for `sb <command> --help` (the parse's help error, rendered).

1. **clap's defaults:** what sb shows today.
2. **clap `Styles` from sb's palette:** colours only, clap's layout.
3. **cargo's look:** clap-cargo's style preset plus a help template with
   color-print markup, as cargo does it (rust-lang/cargo#12578).
4. **clap template:** sb-go's sections (USAGE, COMMANDS, FLAGS as palette
   badges) placed around clap's `{usage}`, `{subcommands}` and `{options}`.
5. **clap-help:** termimad tables.
6. **sb-go's format copied:** fang's layout (bold headings, the usage line
   in a shaded block, commands with usage hints, flags as `-s --long`, one
   column), drawn by our own code from clap's command data, in sb's
   palette (decision 10: sb's palette, not sb-go's colours).

## sb-go's format (captured through a pty)

fang draws: the about in the text colour; headings in bold purple
(`#6B50FF`); the usage line in a code block (`#2F2E36` background) with a
padding row above and below, the program in blue and the hints muted;
command names pink (`#FF79D0`), hints muted (`#605F6B`), descriptions near
white; flags green (`#12C78F`) as `-e --extra-vars` with no value
placeholder, sorted by name, help included, aligned in the same column as
the commands. Hints follow fang v2.0.1 `help.go`: `[command]` for a command
with subcommands, then its arguments, then `[--flags]` when it has flags.
fang also queries the terminal's background colour (OSC 11) at start.

## Result (technical, before the maintainer's review)

- **clap's own API (looks 2 to 4)** changes colours and wraps sections,
  but keeps clap's layout:
  - The command list shows names (plus subcommand flags and visible
    aliases), never usage hints (`help_template.rs`). `display_name` isn't
    used there; `before_help` is free text.
  - Usage says `[OPTIONS]`, hard-coded (`output/usage.rs`); only
    `override_usage` gives `[--flags]`. `subcommand_value_name("command")`
    would give `sb [command]`.
  - With `{all-args}` the heading text is configurable
    (`subcommand_help_heading`), the trailing colon isn't
    (`help_template.rs`); look 4 uses the separate blocks to avoid it.
  - Lists are indented two spaces (`TAB` in `output/mod.rs`).
  - `Styles::header` takes a full style, background included, so a badge
    heading with a colon is possible in pure clap.
- **cargo's look** is cargo's named ANSI colours, not sb's palette;
  color-print markup is compile-time, so it can't take the theme at run
  time. color-print's last release is 0.3.7 (crates.io API: updated
  2024-11-08).
- **clap-help** draws a clear options table, but its README says it has no
  subcommand support, so the root's command list is missing. It brings
  termimad (rejected earlier as a second rendering stack, proto-markdown)
  and terminal-light, which asks the terminal for its background colour.
- **Copying sb-go's format (look 6)** is about 120 lines of our own layout.
  Checked against the installed sb-go's output (escapes stripped) for
  `sb`, `docker`, `edit`, `motd` and `completion generate`: identical but
  for the `--color` row. It copies:
  - the about from `Long`, else `Short` (fang `cmp.Or`), and flag defaults
    as ` (value)` unless empty, false, 0 or `[]`;
  - `[--flags]` always on the usage line (cobra's own `-h` counts there),
    but in the command list only for commands with flags of their own;
  - the help row as `Help for <name>`;
  - the program and the command path in two colours, and no padding rows
    without colour (fang drops them for ASCII or a pipe).
  - No library draws fang's format, and clap exposes no width helper
    (`StyledStr::display_width` is crate-private). Widths here count
    characters after stripping styles (anstream), fine for sb's ASCII
    names; `unicode-width` would make that general.
  - The hints are derived from clap's own data (positionals' value names,
    subcommands, flags), so the help can't drift from the parser.
  - `--color` is listed in every command's flags (sb-rust has it; sb-go
    doesn't) and counts towards `[--flags]` only on the root, since every
    command inherits it.
  - Not copied yet: fang wraps the about at min(terminal, 120) columns, and
    errors (clap's "unrecognized subcommand", sb's own) would need the same
    treatment to match.

## Verdict

_Reviewed by the maintainer (2026-10-30): sb's help follows sb-go's format
(look 6), in sb's palette._ clap stays the parser; the help is drawn by sb
from clap's command data, so it can't drift from the arguments. For the
real version: width with `unicode-width` (clap already pulls it in), the
120-column wrap, the same look for errors, and the cobra `Long` texts move
into the commands' doc comments as they are ported.
