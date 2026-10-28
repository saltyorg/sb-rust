# Prototype: proto-facts

**Question:** Do ratatui and ratatui-textarea give a good fact editor, with
keyboard and mouse, on Saltbox facts data?

**Crates tried:** ratatui@0.30.2 (crossterm 0.29 via `ratatui::crossterm`),
ratatui-textarea@0.9.3, base64 (clipboard copy).

## History

The first version copied sb-go's layout (a tree plus a separate details pane)
without questioning it. It worked technically but kept sb-go's main usability
problem: values were hidden until you selected each setting. It has been
replaced by the settings-page design in `docs/ideas/fact-editor.md`, chosen
for users of mixed skill levels. The technical lessons from the first version
are kept below.

## What was built (settings page)

- Each app is a heading with its settings beneath it and values visible.
  Secrets show as dots.
- Apps with several copies get a heading per copy; single-copy apps don't.
- A search box narrows the page as you type and opens matching apps.
- Buttons along the bottom: Edit, Copy, Show/Hide, Delete/Keep,
  Save changes, Quit. Each shows its key and can be clicked.
- An "Unsaved changes" panel, always visible, lists each change with a
  clickable undo. It sits on the right at 100+ columns and at the bottom
  below that.
- Delete works on a setting, a copy or a whole app. The status line and the
  save dialog explain that Saltbox creates a new value on its next run.
- Copy sends the value to the terminal clipboard without showing it. Show
  reveals only the selected value.
- The page opens fully expanded when everything fits on screen, and folded to
  app names otherwise.
- The save dialog has clickable Save and Cancel buttons.
- **Right-click menu** on any setting, copy or app heading, with that item's
  actions:
  - settings: Show/Hide, Copy, Edit, Undo change, Delete/Keep;
  - copy headings: Delete copy;
  - app headings: Delete app (folding stays on click or Enter, not in the menu).

  The item under the mouse is highlighted. Every menu action is also on the
  keyboard: `m` (or the Menu key) opens the same menu on the selected row,
  each item shows its direct shortcut, and the shortcut works with or without
  the menu open. Deleting an app or copy replaces any pending changes inside
  it, so the changes panel stays readable.

**How it was checked:** the PTY harness drove search, show, copy, edit,
delete (setting, copy, app), undo by clicking in the changes panel, the
right-click menu (open, hover, click an item) and the keyboard menu, and save
by key and by click. It ran at 110 and 80 columns, against the synthetic
fixtures and a generated 60-app set. The resulting files matched the intended
changes exactly: an edited value, a removed section, a removed app file.

## Result

- ratatui covers the whole design with stock widgets (list, paragraphs,
  blocks). Click targets come from the areas recorded during drawing, so
  there's no hand-maintained hit-map.
- **Problems found in our own code, all fixed:**
  - The save dialog cut off its buttons when a line wrapped. Its height now
    counts wrapped lines.
  - Values jumped sideways while searching. The value column is now sized
    from all settings, not just visible ones.
  - A search with no results showed a blank page. It now says nothing
    matches and how to clear the search.
  - Backspace typed an "h" in the search box in terminals that send Ctrl+H.
    The box was hand-rolled and treated any key with a letter as text. It now
    uses ratatui-textarea, which handles Backspace, Ctrl+H, word delete and
    cursor movement. Regression tests in `src/main.rs` cover it. Lesson: every
    text input, however small, uses the library widget.
- **Clipboard copy can't confirm success.** Terminals that block clipboard
  access ignore the request silently, so the status message says "if your
  terminal allows it; otherwise use Show".
- **Folding threshold:** "fold when it doesn't fit" folds even the four
  demo apps in a 20-row window. A small window may deserve a higher
  threshold. To be decided in review.

- **Same two input problems as proto-logs, same fixes:** input events are
  handled in batches with one redraw per batch, and input is drained after
  mouse capture is turned off. Before the fix, mouse codes arriving 10–100 ms
  after `q` reached the shell (550 bytes in the test); after, none.

**Lessons kept from the first version:**
- Multi-line values must become one ratatui `Line` per value line.
- `CursorMove::Bottom` then `End` puts the editor cursor at the end of the
  value.
- The writer must keep continuation-line indentation, so the real module
  needs round-trip tests, as sb-go has.
- Resize and tiny terminals are handled by ratatui without extra code.
- Pending a real-terminal check: Shift-drag text selection while mouse
  capture is on, and wide characters after overwrites (the PTY harness can't
  emulate either).

## Verdict

_Technical verdict only: it builds, works and fits the library policy. Pending the maintainer's own review of the look and feel._

**Adopt** ratatui plus ratatui-textarea. The settings-page design is the
candidate for the `tui` spec, subject to review.

## Map impact

- `tui` module: the fact editor is a settings page. It has search, visible
  buttons, an always-visible changes panel with undo, per-value show and
  copy, and delete at app, copy and setting level.
- `config` (facts) module: deletes at three levels (setting line, section,
  file). Line-preserving writer with round-trip tests. Keep sb-go's locking
  and drift detection.
