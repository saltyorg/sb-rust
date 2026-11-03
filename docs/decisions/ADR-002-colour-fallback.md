# ADR-002: Colour profile fallback

## Status

Accepted (maintainer, 2026-10-30); amended by
[ADR-006](ADR-006-sixteen-colour-picks.md)

## Date

2026-10-30

## Context

sb-rust draws for truecolor by default (capability map, decision 10): one
role-named RGB palette, set by the theme (cli-core spec), used by every
screen and inline output. Some terminals can't show truecolor, and sb-go
lets users step down with `SB_COLOR_PROFILE` (`truecolor`, `ansi256`,
`ansi`, `ascii`), forcing truecolor otherwise.

The colours reach the terminal through several libraries: ratatui with
crossterm (screens), anstream/anstyle (styled lines), console (indicatif
templates and dialoguer prompts) and tabled (tables). A fallback has to
reach all of them, and the library-first rule says the colour maths come
from a library.

Facts checked at source (2026-10-30):

- `anstyle-lossy` 1.1.5 (rust-cli, the anstream project; about 2M recent
  downloads) converts `RgbColor` to the xterm 256 palette (`rgb_to_xterm`)
  and to the 16 ANSI colours for a given palette (`rgb_to_ansi`).
- `anstyle-query` 1.1.5 detects truecolor only from `COLORTERM`
  (`truecolor()` checks for `truecolor` or `24bit`). Many truecolor
  terminals leave it unset, notably over SSH and inside tmux, so detection
  would downgrade terminals that work.
- `termprofile` 0.2.5 does detection and conversion with a ratatui
  feature, but has about 7k recent downloads.
- crossterm drops colours under `NO_COLOR` on its own; anstream, console
  and clap honour `NO_COLOR` too; `--color` overrides all of them
  (cli-core spec).

## Decision

1. **Default stays truecolor, forced**, as sb-go: no detection.
2. **`SB_COLOR_PROFILE`** keeps sb-go's values: `truecolor`, `ansi256`,
   `ansi` (16 colours), `ascii` (no colour). An unknown value is a warning
   and means `truecolor`, as in sb-go (see Amendments). The theme config
   may also set `color_profile` with the same values; the environment
   variable wins.
3. **Conversion with `anstyle-lossy`, once at startup.** cli-core resolves
   every palette role for the chosen profile into an `anstyle::Color`
   (`Rgb`, `Ansi256` via `rgb_to_xterm`, or `Ansi` via `rgb_to_ansi`), and
   each output library takes its colours from that resolved palette
   through a small adapter: ratatui through ratatui-core's `anstyle`
   feature, console by index or name, tabled and anstream directly.
   Nothing else converts colours.
4. **`NO_COLOR` and `--color=never`** mean no colour at all, whatever the
   profile, as today.
5. **Tests:** for each profile, the truecolor guard tests become profile
   guards: `ansi256` output uses only 256-colour codes, `ansi` only the 16,
   `ascii` none.

## Alternatives considered

### Detect the profile (anstyle-query or termprofile)

- Pros: no setting needed on terminals that report themselves.
- Cons: `COLORTERM` is unset on many truecolor setups, so users would get
  a worse picture by default; termprofile's adoption is low.
- Rejected: forced truecolor plus an explicit way down is sb-go's working
  behaviour.

### Each library converts on its own

- Pros: no resolved palette.
- Cons: four libraries with four conversion rules (or none) would show the
  same role in different colours.
- Rejected: one conversion keeps every surface consistent.

### Our own RGB to 256/16 maths

- Rejected: the library-first rule; anstyle-lossy is maintained by the
  project we already use for colour.

## Consequences

- One new dependency: `anstyle-lossy`, plus ratatui-core's `anstyle`
  feature as a direct dependency (the ratatui facade doesn't forward it).
- Themes stay RGB-only; the fallback derives the rest, so theme authors
  don't write 256 or 16-colour variants.
- 16-colour output depends on the terminal's own palette, so roles can look
  different between terminals; that is accepted for a fallback.

## Amendments

- 2026-10-30, cli-core Task 9 (theme config): an unknown
  `SB_COLOR_PROFILE` still warns, but falls back to the theme file's
  `color_profile` when the file sets one, and to `truecolor` otherwise.
  An unusable environment value shouldn't discard a valid choice in the
  file; with no file profile this is decision 2 unchanged.
- 2026-11-03, term-ui Task 12: under `ansi`, the built-in palette's
  colours take a reviewed pick per role instead of the nearest VGA match;
  colours a theme file sets are still matched. See
  [ADR-006](ADR-006-sixteen-colour-picks.md).
