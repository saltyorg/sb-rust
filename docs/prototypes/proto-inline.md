# Prototype: proto-inline

> The prototype's code was removed once its module was built
> (maintainer, 2026-11-06); it is in git history
> (`git log -- prototypes/proto-inline`). These findings stay as the record.

**Question:** Do comfy-table, anstyle and dialoguer give the non-TUI look for
`docker ps`-style tables and restore-style prompts, on a TTY and when piped?

**Crates tried:** comfy-table@8.0.1 (default `tty` feature, crossterm 0.29 for
styling, the same version ratatui uses), anstyle@1.0.14, anstream@1.0.0,
dialoguer@0.12.0 (console 0.16, the same family as indicatif),
bollard@0.21.1.

**What was built:** `ps` renders a NAME / STATE / STATUS / PORTS table from
bollard with state colours and a styled summary line. `prompt` asks for a
username (rejects empty or whitespace), a password (minimum length, masked),
and a confirmation (default no). Checked against two throwaway containers (one
running with port mappings, one exited), which were removed afterwards.

## Result

- **Table:** comfy-table's dynamic arrangement fits the terminal width. At
  narrow widths it wraps cells inside their columns instead of overflowing.
  Multi-value cells (several port mappings) wrap cleanly. Wrapping and width
  maths are comfy-table's job.
- **Colour policy needs one decision point.**
  - anstream honours `NO_COLOR`, `CLICOLOR_FORCE` and non-terminal stdout.
  - comfy-table only checks whether stdout is a terminal and **ignores
    `NO_COLOR`**.
  - Fix: decide once with `anstream::AutoStream::choice(&stdout)` and call
    `Table::force_no_tty()` when colour is off.
  - Result: `NO_COLOR=1` on a terminal emitted zero colour codes; piped
    output had none either.
- **Piped output** keeps Unicode box-drawing borders and the natural column
  widths. For scripts, a plainer style (or a `--plain`/JSON output) is worth
  deciding on in the `docker` spec.
- **Prompts:** dialoguer's `ColorfulTheme` gives clear `?` / `✔` / `✘`
  feedback. Validation errors show inline and re-prompt. The password echo is
  a fixed `********`, which doesn't reveal the length. The confirm defaults to
  no.
- **No terminal:** dialoguer would fail with an I/O error, so the prompt path
  checks stdin and stderr up front and fails with a clear message and a
  non-zero exit. The module needs a non-interactive alternative (flags or
  environment variables) for every prompt.
- **API churn note:** comfy-table 8 removed `load_preset` and string presets
  in favour of `load_style` and `TableStyle` constants. That's fine at a
  pinned version, but expect breaking major releases.

## Palette and `--color` (before the maintainer's review)

- **Truecolor palette.** The table's state and header colours, the summary
  lines and dialoguer's prompt theme now use proto-theme's RGB colours in
  place of named ANSI ones. comfy-table takes `Color::Rgb`, anstyle
  `RgbColor`, and dialoguer's `ColorfulTheme` has public fields that take
  console styles (`Style::true_color`). Guard tests fail on any named
  colour.
- **State colours** follow the palette roles: running OK, restarting and
  dead PROBLEM, exited, created and paused IDLE. The list endpoint has no
  exit code, so a failed exit looks like a clean stop here; the real table
  would reuse proto-logs' classification, which inspects each container.
- **`--color=auto|always|never`** (decision 10) comes from
  `colorchoice-clap`, the anstream project's clap mixin (the library-reviewer
  pointed it out), which sets anstream's global choice. No library passes
  that choice on to console (the prompts) or crossterm, so `always` and
  `never` are applied to both by hand.
- **crossterm reads `NO_COLOR` itself** and drops every colour it writes
  (crossterm 0.29 `style/types/colored.rs`). comfy-table styles through
  crossterm, so `--color=always` lost the table's colours under `NO_COLOR`
  until `Colored::set_ansi_color_disabled(false)` was added. ratatui's
  crossterm backend goes through the same code, so the TUI screens already
  honour `NO_COLOR`; the colour-fallback ADR should build on that.
- **Fixed: `NO_COLOR` stopped the table wrapping.** The old code called
  comfy-table's `force_no_tty` to drop colours, which also switches off its
  terminal-width lookup. Measured at 60 columns with `NO_COLOR=1`: 172
  columns wide before, 60 after. Colour is now decided per cell, and
  `enforce_styling` keeps colours for `--color=always` when piped.

The comfy-table notes above are kept as history; the table below replaced
it.

## sb-go's `docker ps` layout on tabled (maintainer review)

The maintainer chose sb-go's `sb docker ps` layout over the first
table (NAME / STATE / STATUS / PORTS). Compared at 120 columns:

- **sb-go's columns:** Container, Status, **Traefik Port** (from Saltbox's
  `traefik.http.services.*.loadbalancer.server.port` labels, skipping the
  `-http` redirect services) and Port Bindings, one binding per line with
  the ports padded so the arrows line up and IPv6 as `[::]`.
- **sb-go's look:** rounded corners, a rule between rows (chosen again in
  review), bold centred headings, port columns right-aligned, a coloured
  border, sorted by name.
- **The first table** repeated the state in two columns, showed Docker's
  "Up 30 hours (healthy)" text, joined bindings with commas so they wrapped
  mid-list, showed IPv6 as `:::` and kept Docker's order.

Rebuilt with all of that, plus:

- **Status from inspect** in sb-go's wording, but an exit code decides
  whether a stopped container is a problem (`exited (code 137)` PROBLEM, a
  clean `exited` IDLE, out of memory named), as in proto-logs. sb-go shows
  every exited container red. Health: healthy OK, starting WARN, unhealthy
  PROBLEM.
- **Parallel inspect** (futures `join_all`, as proto-logs): 9 ms against
  sb-go's 40 ms for the same containers (one run each, release build).
- **Stable order:** Traefik ports and bindings are sorted. sb-go iterates Go
  maps, so their order could change between runs.
- **Palette and width:** BORDER for the lines, status colours from the
  palette, and the table fitted to the terminal (checked at 120, 70 and 50
  columns: 92, 70 and 50 characters wide). At narrow widths the widest
  column wraps first, so "running (healthy)" can wrap at 70 columns.
- **Library switch, comfy-table → tabled 0.22.** comfy-table has no way to
  colour border lines (its `TableStyle` sets characters only). tabled has
  `BorderColor` (truecolor via `Color::rgb_fg`), `Style::modern_rounded()`
  (sb-go's layout exactly), `Width::wrap(..).priority(..)` for the whole
  table and ANSI-aware widths (`ansi` feature). It doesn't read the terminal
  width, so that comes from console's `Term::size_checked` (None when stdout
  isn't a terminal). tabled doesn't read NO_COLOR either; the table follows the one
  colour decision. Both crates are maintained and widely used (crates.io,
  2026-10: tabled 13M recent downloads, comfy-table 23M).
- **Tests** cover the truecolor guard, no codes with colour off, the rule
  between rows, fitting a width, the status wording, the Traefik rule and
  the binding format, each confirmed by breaking the code.

## Restore prompts: deferred

The restore backend is being rewritten, so the restore CLI takes shape
after that. The `prompt` subcommand stays as the dialoguer evidence
(palette theme, validation, no-terminal failure) but is out of the review
and the try script.

## Verdict

_Reviewed by the maintainer (2026-10-29): the `docker ps` table is liked
as it is. The review moved it to sb-go's columns and layout on tabled, and
asked for theme configuration in the real CLI (capability map, open
items). The restore prompts are deferred until the restore backend
rewrite._

**Adopt** tabled, anstyle/anstream and dialoguer for the inline look,
provided colour is decided in one place. tabled replaced comfy-table for
the coloured border.

## Map impact

- Stack table: `tabled` (replacing `comfy-table`), `anstyle` + `anstream`,
  and `dialoguer`. tabled renders on its own (papergrid) and takes the
  terminal width from console; dialoguer uses indicatif's
  console.
- `docker` module: the `ps` columns, status rules and binding format above.
- `term-ui` module: a single `colours_on()` decision feeds anstream,
  tabled and indicatif. Every prompt has a non-interactive flag. A
  `--plain` or JSON output option is to be decided per command.
