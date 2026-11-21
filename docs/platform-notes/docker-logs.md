# Docker container logs: tail, since, until, equal times, long lines

Verified 2026-11-13 for the log viewer (module build M63; tui spec,
"The daemon probe"; docker spec, `logs.rs`).

**Software:** Docker Engine 29.6.2 (API 1.55), measured, with the
`json-file` log driver (Saltbox's, 10 MiB × 3 per container); bollard
0.21.1's `NewlineLogOutputDecoder` (`src/read.rs`), read.

**Method:** two throwaway containers made for the probe from an image
already on the daemon, with `--log-driver json-file` and no network, and
removed after it: one without a TTY and one with `-t`. Each wrote known
lines: twenty lines 50 ms apart, three lines in one `write`, one line of
40,000 bytes, a line on stderr between stdout lines. The log was read
through the Engine API over the daemon's Unix socket with curl
(`/containers/{id}/logs?stdout=1&stderr=1&timestamps=1` with `tail`,
`since` and `until`), and the multiplexed frames were split by a small
script (8-byte header: stream, then the big-endian length). Times passed
as `since`/`until` were Unix seconds with nine decimals, taken from the
entries' own timestamps.

## Findings

- **`tail` applies before `since` and `until`.** `tail=3&until=<the 10th
  line's time>` returns nothing: the last three entries are taken first,
  then cut by `until`. `tail=3&since=<the 5th line's time>` returns the
  last three entries. So a page back in time can't be read with `tail`
  plus `until`, as sb-go did; it needs a time window without `tail`.
- **`since` and `until` are inclusive,** to the nanosecond: a window
  from the 5th line's exact time to the 10th line's returns lines 5 to 10.
- **Both filters are one-sided.** The daemon skips entries until the
  first one at or after `since` and returns everything after it, and
  stops at the first entry after `until`. An entry stored later with an
  earlier time (below) passes `since` and is lost to `until`:
  `until=<the long line's time>` returned the long line's first part
  only, because the stderr entry stored after it is newer. A reader must
  filter the window itself and can only be one-sided too.
- **Equal times.** Lines written in one `write` get distinct times (ns
  apart). The parts of a long line share one time. Five reads of the
  whole log came back byte for byte the same: the order of entries,
  equal times included, is the stored order and stable across reads.
- **Lines over 16 KiB** are stored as parts of 16,384 bytes, each part
  its own entry with the same timestamp; only the last part ends in a
  newline.
  - **Without a TTY,** each part is its own frame. Entries of the other
    stream can come between the parts: the 40,000-byte line came as
    16,384 bytes, the stderr line (a later time), 16,384 bytes and 7,233
    bytes (the rest and its newline). So the stream isn't in time order
    around a long line, and parts can't be joined by "the next frame".
  - **With a TTY,** the stream has no frames: each entry is its
    timestamp, a space and its bytes, lines ending in `\r\n`. A part
    without a newline is followed at once by the next part's timestamp,
    in the middle of the line: `<time> <16,384 bytes><time> <rest>\r\n`.
    bollard's `Docker::logs` splits a TTY stream at newlines (one
    `LogOutput::Console` per line, a last line without a newline at the
    end: `NewlineLogOutputDecoder::new(false)` in `src/read.rs`), so one
    message can hold several parts, each to be split at its 16,384th
    byte.
- **Times** are RFC 3339 with nanoseconds, in UTC (`Z`). The inspect's
  `Created` and `State.StartedAt` have the same form, with trailing zeros
  dropped from the fraction.

## What sb does with it

sb-docker's `logs.rs` (docker spec): positions are a nanosecond time plus
an ordinal among entries of that time, counted in the order the daemon
sends them (a tail that begins inside entries of one time reads that
second again, to number them from the first); older pages are windows of
whole seconds without `tail`, widened from 60 s, filtered by sb itself;
newer pages and the follow start at the newest position's second and drop
entries at or before it. A part of a long line is a row of its own (the
tui spec's known limit), with the ordinal telling the parts apart. Pages
are cut in position order; an entry stored out of time order next to a
page boundary can still be missed or shown twice there (the tui spec's
known limit on equal timestamps, which this widens).

## Container events

Verified 2026-11-13 (module build M64) on the same daemon (Docker Engine
29.6.2, API 1.55): one throwaway container made for the probe from an
image already on the daemon, with no network and a health check of
`true` every second, started, left to exit, renamed and removed. Its
events were read through the Engine API with curl
(`/events?since=..&until=..&filters={"type":["container"],"container":[..]}`),
one JSON object a line.

- **Actions** are plain words (`create`, `start`, `die`, `rename`,
  `destroy`, `exec_die`), except two families that carry a value after
  `: `: `health_status: healthy` (the state) and `exec_create: <command>`,
  `exec_start: <command>`. A health check runs as an exec, so a container
  with one sends three exec events per check.
- **The container** is `Actor.ID` (the full ID) and
  `Actor.Attributes.name`, the name without a leading `/`. After a
  rename, `name` is the new name and `oldName` the old one with its `/`;
  `die` adds `exitCode`. Each event also has `scope`, `time` and
  `timeNano`.
- **The open stream** (no `since` or `until`, nothing happening): the
  daemon sends its headers at once (`200`, `Content-Type:
  application/jsonl`, chunked) and then nothing until an event, so a
  client's timeout on the response head doesn't fire on a quiet daemon.

- **Filters** (verified 2026-11-21 on Docker Engine 29.10.0, API 1.57,
  in docker-in-docker: two throwaway containers, one with a health
  check every 2 s, run, one restarted, their events read with curl over
  `since`/`until` under three `filters`): `event` takes a list of
  actions, and with `health_status` among them every value matches as a
  prefix, so `health_status` takes `health_status: healthy`, while the
  exec events stay out unless named; without one of `health_status`,
  `exec_create`, `exec_start` the match is exact. `container` matches a
  prefix of the name or the ID: `container=hc` also took the container
  `hc2`'s events. This is moby's `matchEvent` and `fuzzyMatchName`
  (https://github.com/moby/moby/blob/docker-v29.0.0/daemon/events/filter.go)
  over `FuzzyMatch`
  (https://github.com/moby/moby/blob/docker-v29.0.0/daemon/internal/filters/parse.go).

sb-docker's `events.rs` maps `create`, `start`, `die`, `destroy`,
`rename` and `health_status: <state>`, and passes every other action
through as written.
