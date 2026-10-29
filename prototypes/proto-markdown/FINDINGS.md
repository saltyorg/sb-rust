# Prototype: proto-markdown

**Question:** Do tui-markdown or termimad render Saltbox announcements well
enough to replace glamour?

**Crates tried:** tui-markdown@0.3.12 (ratatui maintainer, built on
pulldown-cmark), termimad@0.35.5, ratatui@0.30.2 (with the unstable
`rendered-line-info` feature), serde-saphyr@2.0.0 for `announcements.yml`.

**What was built:** a loader for the announcements file (date, title,
message) with four renderer/mode combinations:
- tui-markdown inline: ratatui inline viewport plus `insert_before`, so output
  goes to normal scrollback.
- tui-markdown scrollable view: full-screen ratatui with scroll and
  previous/next announcement.
- termimad inline.
- termimad's own scrollable view (`MadView`) was **not** built. See below.

Checked against a synthetic fixture (headings, emphasis, inline code, links,
nested and ordered lists, a fenced code block, a long quote, wide CJK/emoji
text) and a real Saltbox announcements file, at 80 and 140 columns.

## Result

| Feature | tui-markdown (default) | tui-markdown + 10-line `StyleSheet` | termimad |
|---|---|---|---|
| Headings | `## Title`, cyan/bold | `Title`, cyan/bold | plain text, no marker |
| Bold / italic | styled | styled | styled (SGR 1 / 3) |
| Inline code | white on black | same | grey background |
| Links | `label (url)`, blue underlined | same | raw `[label](url)` |
| Lists, nested, ordered | correct | correct | correct (`•` bullets) |
| Code block | fences shown | fences hidden, styled | plain, no fences |
| Quote wrap | prefix on first line only | same | `▐` on every wrapped line |
| Wide text | correct in full-screen view | same | correct |
| Wrap and rewrap on resize | yes (view) | yes | n/a (inline print) |

- **tui-markdown is the better fit.** It produces ratatui `Text`, so the same
  output drops into any screen (announcements viewer, update summaries) with
  ratatui owning wrapping and width. Its `StyleSheet` trait makes the look
  configurable: ten lines removed the literal `##` markers and code fences
  for a glamour-like result.
- **termimad is a separate rendering stack.** It renders well inline, but it
  has its own crossterm area and view types (`MadView`), and it pulls in
  `unicode-width` 0.1 next to ratatui's 0.2: two width implementations for
  the same screen. Building its scrollable view would have meant a second TUI
  stack beside ratatui, which goes against the "don't own rendering" policy,
  so that combination was deliberately left out.
- **Inline output via ratatui works** (`Viewport::Inline` plus
  `insert_before`), but:
  1. Measuring wrapped height needs `Paragraph::line_count`, which is behind
     ratatui's **unstable** `rendered-line-info` feature.
  2. The inline viewport queries the cursor position at start-up and fails
     when nothing answers (pipes, some CI). The module needs a plain-text
     fallback when stdout isn't a terminal.
  3. Wide characters looked spaced out on the inline path in the emulated
     terminal, but rendered correctly in the full-screen view. Pending a
     check in a real terminal.
- **Quote wrapping** loses the `>` prefix after the first line. This is
  cosmetic, and a candidate for a `StyleSheet` tweak or an upstream issue.

## Truecolor styling (before maintainer review)

The truecolor decision (capability map, decision 10) came after this
prototype was built, so it was restyled before review.
- tui-markdown takes every colour from its `StyleSheet` trait (tui-markdown
  0.3.12 `style_sheet.rs`). Its own tests are the only other place colours
  appear. `SaltboxSheet` now overrides every styled element with roles from
  the shared palette (`prototypes/proto-theme`): headings, code, links,
  quotes, alerts, tables, list markers, footnotes and the rest.
- The title line, border and footer use the same palette and key-hint
  helper as the log viewer.
- The fixture gained a synthetic announcement with an alert, a table and a
  footnote, so every styled element is drawn. Real Saltbox announcements
  currently use one heading and no tables, alerts, code or quotes.
- A `TestBackend` test fails if any cell of any fixture announcement uses a
  named ANSI colour. It failed against the default stylesheet first.
- On the real binary with `TERM=xterm-256color`, the full-screen view sent
  132 24-bit colour codes and the inline path 68, with no 16-colour codes.

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt tui-markdown** (with a small Saltbox `StyleSheet`); **reject
termimad**. It's a second rendering stack with its own width maths.

## Map impact

- Stack table, Markdown row: `tui-markdown` with a custom `StyleSheet`.
  Enable ratatui's `unstable-rendered-line-info` if inline rendering is
  kept, and record that pin.
- `tui` module: the announcements viewer is a ratatui screen. Inline
  announcement output (e.g. after `sb update`) uses the inline viewport, with
  a plain-text fallback when stdout isn't a terminal.
- Pending a real-terminal check: wide characters on the inline path.
