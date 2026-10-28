# Prototype: proto-facts

## Task 4: keyboard

**Question:** Do ratatui and ratatui-textarea give a good keyboard-driven fact
editor on real facts data?

**Crates tried:** ratatui@0.30.2 (crossterm 0.29 backend via
`ratatui::crossterm`), ratatui-textarea@0.9.3. Both resolve to one crossterm
version, so key events pass straight through to the textarea.

**What was built:** a role → instance → fact tree with an inspector pane. It
has inline staging (edit `●`, delete `✗`, undo), a review popup, and apply via
line-based rewrites of only the affected lines. Secret-looking keys
(pass/secret/token/key/api) are masked by default, with `r` to reveal: a
redesign over sb-go. The layout is side by side at 96 or more columns and
stacked below that. A guard refuses the standard Saltbox facts directory. About 535 lines
of UI plus 155 lines of model, against sb-go factui's roughly 1,900 non-test
lines, though the prototype has no mouse support, locking, or drift handling.

**How it was checked:** a pexpect + pyte harness drove the binary in a PTY
with real keystrokes and printed screen snapshots, against synthetic fixtures
(committed) and a copy of a real facts directory (parse check only).

### Result

- **Works on real data.** A copy of a real facts directory parses to the
  same role and fact counts as a plain text scan.
- The full flow works end to end: expand, browse, reveal, stage a delete,
  edit a value, review, apply. The diff of the copy shows exactly the two
  intended line changes, and the comment line and other keys are untouched.
- **ratatui-textarea** covers the editor without extra code: cursor movement,
  editing, multi-line values with Alt+Enter, and a block title. Feeding it
  crossterm `KeyEvent`s directly works.
- **Wide text:** CJK and emoji values render and truncate correctly. ratatui
  owns the width maths.
- **Resize and small terminals:** a resize redraws correctly with no extra
  code. At 28×7 it degrades without panicking.
- **Problems found and fixed. All were our own code, not stack limits:**
  1. ratatui `Span`s don't break on `\n`, so multi-line values must become one
     `Line` per value line.
  2. An inline editor squeezed into the inspector got no height in stacked
     layouts. A **modal editor popup** sized to the screen works better.
  3. `CursorMove::End` goes to the end of the current line. Use `Bottom` then
     `End` for the end of the value.
  4. **The writer dropped continuation-line indentation**, which would turn
     multi-line values into broken keys. The real module needs a
     round-trip test suite for this, as sb-go has.
- **Harness limitation:** pyte doesn't blank the second half of a wide
  character when its first half is overwritten, so a value shown after a CJK
  value can look like it has missing spaces. Real terminals blank both
  halves. To be confirmed in the Task 5 checks in real terminals.
- **Testing approach for the real module:** ratatui's `TestBackend` gives
  in-process snapshot tests (with `insta`), which needs no pexpect harness.
  Keep a PTY-driven smoke test for terminal-level behaviour.

### Verdict (keyboard)

**Adopt.** ratatui plus ratatui-textarea covers the fact editor. Every
problem found was in our own code.

### Map impact

- `tui` module: modal editor popup; masked secrets with reveal; per-line value
  rendering; snapshot tests via `TestBackend` and `insta`.
- `config` (facts) module: line-preserving writer with continuation-line
  round-trip tests. Keep sb-go's locking and drift detection; the prototype
  skipped them on purpose.
