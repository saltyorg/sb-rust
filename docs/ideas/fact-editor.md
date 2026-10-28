# Fact editor: settings-page redesign

## Problem Statement

How might we let Saltbox users of any skill level find, copy, change and
delete the settings their apps generated, without hunting through a tree one
item at a time?

## Recommended Direction

A single page that reads like a phone's settings screen:
- Each app is a heading, with its settings listed beneath it and their values
  visible (secrets shown as dots).
- Apps that run several copies get a small heading per copy; all others show
  their settings directly under the app.
- A search box at the top narrows the page as you type.
- Buttons along the bottom do everything: **Edit**, **Copy**, **Show**,
  **Delete**, **Save changes**. They work by click or key, so nobody has to
  memorise shortcuts.
- A small panel always lists unsaved changes, each with an undo, so saving
  never surprises anyone.

**Behaviour:**
- **Copy** puts a secret on the clipboard without showing it. **Show** reveals
  only the selected value.
- **Delete** works on a setting, a copy heading or an app heading. A short note
  explains that Saltbox creates a new value on its next run.
- Small installs open fully expanded. Large ones open folded to app names, and
  searching unfolds the matching groups.

## Key Assumptions to Validate

- [ ] Most apps have one copy named after the app. Check against real
      installs.
- [ ] Users understand that deleting means "Saltbox regenerates it". Check
      that the note is enough.
- [ ] Clipboard copy works in common terminals, including over SSH. Where it
      can't, the editor says so and offers **Show** instead.
- [ ] The "folded when many" threshold feels right. Try it with a large
      synthetic set of apps.

## MVP Scope

The page with app and copy headings, search, the bottom buttons, the changes
panel with undo, copy and per-value show, and delete at every level. Mouse and
keyboard both work.

## Not Doing (and Why)

- **Tree navigation and a separate details pane:** this is what made sb-go
  slow to scan.
- **A review that only appears at the end:** the changes panel replaces it.
- **Revealing all secrets at once by default:** it stays an option, not the
  norm.
- **Adding brand-new settings or renaming them:** sb-go doesn't do this
  either. Saltbox's apps create settings, and the editor maintains them.

## Open Questions

- None blocking.
