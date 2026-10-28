# Prototype: proto-inline

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

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt.** comfy-table, anstyle/anstream and dialoguer cover the inline look,
provided colour is decided in one place.

## Map impact

- Stack table: confirm `comfy-table`, `anstyle` + `anstream`, and `dialoguer`.
  No new rendering families: comfy-table uses ratatui's crossterm, and
  dialoguer uses indicatif's console.
- `term-ui` module: a single `colours_on()` decision feeds anstream,
  comfy-table and indicatif. Every prompt has a non-interactive flag. A
  `--plain` or JSON output option is to be decided per command.
