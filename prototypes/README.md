# Prototypes

Throwaway spikes that test the proposed stack in
`docs/specs/capability-map.md`. Each prototype has a `FINDINGS.md` with its
question, result and a **technical** verdict. The look-and-feel verdict is the
maintainer's, after trying them. A prototype is removed once its module is
built (maintainer, 2026-11-06); its findings move to `docs/prototypes/`
(runner, inline, help, yaml, musl and journal so far). `proto-theme` stays while the
others here build on it.

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
| `facts` | Fact editor as a settings page: search, visible buttons, unsaved-changes panel, copy and show, mouse | Copy of `proto-facts/fixtures` in a temp dir; a diff is printed after quitting | — |
| `facts-large` | The same with 60 apps, to see folding and search | Generated in a temp dir | — |
| `logs` | Grouped source picker (Needs attention, Services, Containers) with live status, then a streaming log viewer with follow, search and switching | Your real containers and Saltbox services (read-only) | Docker, systemd, root |
| `logs-demo` | The same on predictable output, including a crash-looping container | Two demo containers and a demo systemd unit, all removed on exit | Docker, systemd, root |
| `markdown` | Announcement rendered by tui-markdown (full-screen, then inline), then by termimad for comparison | `proto-markdown/fixtures/announcements.yml` | — |
| `complete` | A bash subshell with dynamic tag completion loaded | Static tag list | bash |

### Every picker state

`scripts/demo-log-sources.sh up` creates demo containers and `sbdemo-*`
systemd units covering every status the log picker shows: crashed, out of
memory, restarting, unhealthy, paused, stopped, waiting on a timer and more.
`scripts/demo-log-sources.sh view` opens the picker on them only, and
`scripts/demo-log-sources.sh down` removes them. Needs Docker, systemd and
root. Real Saltbox sources are not touched.

## Production-style build

`scripts/try-prototype.sh` uses ordinary release builds. To judge speed as a
shipped binary would have it (static musl, fat LTO, mimalloc):

```sh
CARGO_TARGET_DIR=target/cross cross build --profile production \
  --target x86_64-unknown-linux-musl -p proto-logs
target/cross/x86_64-unknown-linux-musl/production/proto-logs
```

## What to look for

- Whether each screen is pleasant and clear to use, and what you'd change.
- Mouse support and Shift-drag text selection in `facts`, in your usual
  terminal emulators.

Report reactions per prototype; verdicts and the capability map are updated
only after that review.
