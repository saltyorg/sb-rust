# Prototypes

Throwaway spikes that test the proposed stack in
`docs/specs/capability-map.md`. Each prototype has a `FINDINGS.md` with its
question, result and a **technical** verdict. The look-and-feel verdict is the
maintainer's, after trying them.

## How to try them

From the repository root:

```sh
scripts/try-prototype.sh            # list prototypes
scripts/try-prototype.sh facts      # build one, explain it, then run it
```

Each run builds the prototype, prepares demo data, prints what to look at and
the controls, then waits for Enter before starting. Nothing touches real
Saltbox files.

| Name | What you see | Demo data | Needs |
|---|---|---|---|
| `facts` | Full-screen fact editor: tree, inspector, edit popup, review/apply, mouse | Copy of `proto-facts/fixtures` in a temp dir; a diff is printed after quitting | — |
| `logs` | Source picker, then a streaming log viewer with follow, search and switching | A demo container and a demo systemd unit, both removed on exit | Docker, systemd, root |
| `markdown` | Announcement rendered by tui-markdown (full-screen, then inline), then by termimad for comparison | `proto-markdown/fixtures/announcements.yml` | — |
| `runner` | Spinners with a live output tail, a failing step, a Ctrl-C run, then piped output | Simulated steps; uses `ansible-playbook --version` if the Saltbox venv exists | — |
| `inline` | `docker ps` style table (with and without `NO_COLOR`), then restore-style prompts | Your containers (read-only); prompts restore nothing | Docker |
| `complete` | A bash subshell with dynamic tag completion loaded | Static tag list | bash |

## What to look for

- Whether each screen is pleasant and clear to use, and what you'd change.
- Mouse support and Shift-drag text selection in `facts`, in your usual
  terminal emulators.
- Wide characters (CJK and emoji) in the `markdown` inline output.

Report reactions per prototype; verdicts and the capability map are updated
only after that review.
