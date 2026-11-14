# ADR-006: A reviewed 16-colour pick per role

## Status

Accepted (maintainer, 2026-11-03, term-ui Task 12); amends
[ADR-002](ADR-002-colour-fallback.md) decision 3 for the `ansi` profile

## Date

2026-11-03

## Context

ADR-002 resolves every palette role with anstyle-lossy, and under `ansi`
(`SB_COLOR_PROFILE=ansi`, 16 colours) matches each RGB colour against
anstyle-lossy's VGA palette. For the built-in palette that nearest match
puts five roles on the same plain white (SGR 37): muted, ok, idle,
container and accent_2. A finished step's ✓, help's flag names and a
running container then look like muted text. The cause is the palette:
its greens, purples and blue-greys are soft, and the nearest VGA colour
to each is grey. term-ui's runner and tables make OK the most common
colour on screen.

The maintainer compared today's mapping with a proposed pick per role in
a preview on a real terminal and chose the picks.

## Decision

Under `ansi`, a role whose colour is still the built-in palette's takes a
fixed pick from the 16 colours; a role a theme file sets to another
colour keeps the nearest match, as before. The other profiles are
unchanged. The picks (cli-core's `theme.rs`, `SIXTEEN`) keep every
other role's nearest match (faint and border share bright black, text and
selected_fg bright white, as before) and change only these:

| Role | Nearest match | Pick |
|---|---|---|
| ok | white | bright green |
| idle | white | cyan |
| container, accent_2 | white | bright magenta |
| container_rule | bright black | magenta |
| service_rule | bright black | blue |
| attention_rule | bright black | red |
| search_hit | bright black | yellow |
| selected_bg | bright black | blue |

Guard tests pin every role's pick by name beside its built-in colour's
nearest match, and check that the other profiles resolve every role
directly, that the status and list roles (muted, ok, problem, warn,
idle, service, container) never share a colour, and that an overridden
role keeps the nearest match.

## Alternatives considered

- **Keep the nearest match:** ✓ and muted text stay indistinguishable for
  16-colour users. Rejected by the maintainer.
- **Match against another palette:** anstyle-lossy's other palette
  (WIN10_CONSOLE) moves the collisions (ok, idle and service on bright
  cyan) rather than removing them, and the terminal's real palette is
  unknown anyway. A hand-built anstyle-lossy `Palette` tuned so the
  nearest match lands on each pick would encode the same table less
  readably and less exactly.
- **16-colour picks in the theme file too:** theme authors would write a
  second set of colours, which ADR-002 avoided; not needed until someone
  asks for it.

## Consequences

- Nothing else converts colours: the picks live beside the palette in
  cli-core's theme, which every adapter already reads.
- Changing a built-in colour later means reviewing its pick as well: the
  palette-value test fails on any change, and the pinned test, which
  records each built-in colour's nearest match beside its pick, fails
  when that nearest match moves.
- The picks belong to the built-in `default` palette only; a later
  built-in theme (a light one, say) gets nearest matching until it has
  reviewed picks of its own.
- 16-colour output still depends on the terminal's own palette, as
  ADR-002 accepted.

## Amendments

- **2026-11-14 (the maintainer; tui spec decision 23):** the viewer's
  search matches are drawn in reverse video under `ansi`, so the
  `search_hit` pick above (yellow) is no longer drawn: it turned a yellow
  span inside a match yellow on yellow. The pick and its guard test stay
  in the table; a theme file's `search_hit` still counts under truecolor
  and `ansi256` only.
