# Log source picker

## Problem Statement

How might we get someone from "I want to see what X is doing" to the right
log in as few steps as possible, while making anything broken easy to spot?

## Recommended Direction

One picker for both kinds of source (Saltbox systemd services and Docker
containers), grouped into sections with readable, coloured status:

1. **Needs attention:** failed services, containers that stopped with an
   error, containers that keep restarting, failing health checks.
2. **Services:** `saltbox_managed_*` units plus `docker`, matching sb-go's
   filter. Units whose file no longer exists are hidden.
3. **Containers:** running and stopped. A container that exited cleanly is
   shown as stopped, not as a problem.

Each row shows a status symbol and words: `✓ running · 2d`,
`✗ failed 2h ago`, `↻ restarting (3×)`, `✓ scheduled · next run 3h`.
Colours follow the meaning (green fine, red failed, yellow restarting, blue
stopped),
and the words carry the meaning on their own for `NO_COLOR` and
colour-blind users. `/` filters across all sections. The list updates
live while open.

sb-go's split into `sb logs` and `sb docker logs` goes away. The old
command names can open the same picker pre-filtered to one kind.

## Key Assumptions to Validate

- [ ] "Needs attention" can be decided from state alone (unit state, exit
      code, restart count, health). Check against real sources.
- [ ] Fetching status is fast. One inspect per container, in parallel. If
      the picker takes more than about 150 ms to open, show the list first
      and fill status in.
- [ ] Docker's event stream is enough for live updates without polling.
      Services may still need a slow refresh.
- [ ] A section header that the cursor skips is clear, with mouse and keys.

## MVP Scope

In: the three sections, status and colour, stopped containers, filter,
live container updates, mouse and keyboard.

Out: everything in Not Doing.

## Not Doing (and Why)

- A Recent section: tried and removed at the maintainer's request.
- Sidebar, search-launcher and dashboard layouts: shelved, not dropped.
  The grouped list was the maintainer's first pick.
- Opening a source by name on the command line: not picked for now.
- Last error line per problem: it would mean reading every log just to draw
  the list.
- Merged multi-source view: a different tool.

## Open Questions

- How many problems before the attention section needs a "+N more" cap?
- Should a source that recovers drop out of Needs attention at once, or
  stay marked for a while?
