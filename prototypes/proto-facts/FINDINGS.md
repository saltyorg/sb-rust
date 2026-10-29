# Prototype: proto-facts

**Question:** Do ratatui and ratatui-textarea give a good fact editor, with
keyboard and mouse, on Saltbox facts data?

**Crates tried:** ratatui@0.30.2 (crossterm 0.29 via `ratatui::crossterm`),
ratatui-textarea@0.9.3, tui-popup@0.7.7 (dialogs), crossterm `osc52`
(clipboard).

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

- **Library audit (after review asked whether proper libraries were used):**
  - Clipboard copy now uses crossterm's own `CopyToClipboard` (feature
    `osc52`), replacing hand-written escape codes and the base64 crate.
    Verified by decoding the emitted OSC 52 payload: it carried exactly the
    selected value, wide characters included.
  - The edit popup and the save/quit dialogs now use tui-popup (ratatui
    org). A fresh state each frame keeps dialogs centred and sized; button
    clicks are hit-tested against the popup's reported area.
  - The right-click menu stays our own drawing. tui-popup's `move_to` only
    works after a first render and its size is frozen after that (see
    `popup_state.rs` and `popup_area` in its source), which doesn't suit a
    menu that opens at the cursor with changing labels.
  - The INI writer stays our own. The library review corrected the
    evidence for this (checked in rust-ini 0.21.3's source):
    - **rust-ini:** the earlier round trip mis-read the indented
      continuation lines only because `enabled_indented_mutiline_value` is
      off by default. With it on, they parse. The writer is what rules it
      out: it regenerates the whole file (blank lines between sections),
      drops comments while parsing, writes a newline as an escaped `\n`
      instead of indented continuation lines, and unquotes and unescapes
      values by default, which can change passwords.
    - **configparser 3.3.0:** multi-line values parse only when asked
      (`IniDefault::multiline`, off by default), the same opt-in mistake as
      above. The writer rules it out: `write` and `pretty_write` serialise the
      parsed configuration and overwrite the file, and `;`/`#` lines are read
      as comments and dropped.
    - **ini-roundtrip:** very low adoption.

    Line preservation and continuation lines are Saltbox-specific
    constraints. `model.rs` now says this in its module doc.
  - The event loop uses crossterm's documented `read()`/`poll()` on one
    thread, cited in the code.
  - A `TestBackend` render test now checks the save dialog's title and
    buttons in memory: the module-level testing approach.

- **Library review of the whole crate** (the `library-reviewer` agent,
  after the truecolor change). Each fix was test-first: the test failed
  against the old code, or for refactors, it pinned the behaviour first.
  - **Security:** saving rewrote a fact file through a fixed temp name with
    default permissions, so a 0600 file holding secrets became 0644.
    - It now matches sb-go: tempfile creates the temp file (unpredictable
      name, exclusive, 0600, named `.<app>-XXXXXX.tmp`).
    - The original owner, then mode, are copied before writing.
    - Data and directory are synced.
    - A symlinked fact file is refused rather than replaced.
    - A second review of the fix found nothing to replace, but corrected
      the doc comment, which had overclaimed sb-go parity.
  - **Dialog buttons could still be cut off.** The wrapped-line estimate
    (text width / dialog width) undercounts when whole words wrap. The
    dialog height now comes from `Paragraph::line_count`. The real dialog
    texts happened not to trigger it; a test with text built to trigger it
    failed before the fix.
  - **Click areas and widths moved onto ratatui layout:**
    - Bottom buttons: `Layout::horizontal` with spacing, widths from
      `Span::width`, each drawn into the rectangle that is its click area.
    - Undo links: `Rect::rows` plus `Rect::layout`.
    - Dialog buttons: drawn into rectangles from the dialog's inner rows.
    - The menu is placed with `Rect::clamp`.
  - **List movement** uses `ListState::select_next`/`select_previous`/
    `scroll_down_by`/`scroll_up_by`, clamped at once, because several
    events can be handled before the next draw and `ListState` only clamps
    while rendering.
  - **Smaller fixes:**
    - Ctrl+M can no longer add a line to the search box. This follows
      ratatui-textarea's single-line example.
    - A panic now also turns mouse capture off. ratatui's hook doesn't, so
      a hook is installed before `ratatui::init`, as its docs say.
    - The reader and writer now share one rule for where a multi-line value
      ends, so changing a value no longer deletes an indented comment below
      it.
  - **Context menu:** still our own popup (tui-popup reasons above), but its
    rows now come from `Rect::rows` plus `Rect::layout`, the same rows map
    clicks back to items, and shortcut widths use `Span::width`. A second
    review flagged the first decline ("the current drawing works") as not
    a valid reason under the library rule.
  - **Stays custom, with comments:** click-to-row mapping in the list
    (`List` has no hit-testing).
  - **Up with nothing selected** goes to the first row again. The library
    `select_previous` would wrap to the last; a test caught the change.

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

_Reviewed by the maintainer (2026-10-29): fine. The review shaped the
settings-page design, the right-click menu (no Fold, every action on the
keyboard too), the search box and the truecolor palette. Still open: the
manual check in two real terminal emulators (mouse, Shift-drag selection,
wide characters after overwrites)._

**Adopt** ratatui, ratatui-textarea and tui-popup, with tempfile for saves.
The settings-page design goes into the `tui` spec.

## Map impact

- `tui` module: the fact editor is a settings page. It has search, visible
  buttons, an always-visible changes panel with undo, per-value show and
  copy, and delete at app, copy and setting level.
- `config` (facts) module: deletes at three levels (setting line, section,
  file). Line-preserving writer with round-trip tests. Keep sb-go's locking
  and drift detection.
