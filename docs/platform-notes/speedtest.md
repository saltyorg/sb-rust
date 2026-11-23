# Ookla's Speedtest CLI: archives, JSON and HOME

Verified 2026-11-16 for `sb bench`'s Speedtest (modules plan Task M78;
ops spec, "`sb bench`", 3., decision 13).

**Software:** Ookla Speedtest CLI 1.2.0 (its `--version` reports a 1.2.0
build, a static musl binary), from Ookla's
`ookla-speedtest-1.2.0-linux-x86_64.tgz` and
`ookla-speedtest-1.2.0-linux-aarch64.tgz`, run in the `ubuntu:24.04`
image, amd64.

**Method:** a throwaway container (`docker run --rm`, downloads allowed
there; never on the host). Inside it, as root: both archives fetched from
`install.speedtest.net/app/cli/`, their sizes and SHA-256 taken, their
entries listed; then the amd64 `speedtest` run with `HOME` set to an empty
directory, once with `--format=json --progress=no --accept-license
--accept-gdpr` (Speedtest.net's server), once the same with
`--server-id=7190` (then live; retired by 2026-11-23), once with an
unknown `--server-id`, and once without
`--format=json` (bench.sh's text form). Only the field names, exit codes
and file locations were kept: no output, address or result.

## The archives

| Archive | Size (bytes) | SHA-256 | Entries |
|---|---|---|---|
| `linux-x86_64` | 1106829 | `5690596c54ff9bed63fa3732f818a05dbc2db19ad36ed68f21ca5f64d5cfeeb7` | `speedtest` (0755), `speedtest.md`, `speedtest.5` |
| `linux-aarch64` | 1082287 | `3953d231da3783e2bf8904b6dd72767c5c6e533e163d3742fd0437affa431bd3` | the same three |

The x86_64 figures equal sb-go's pin (host/benchmark.go:20-22). Each
archive has one regular entry named `speedtest` at its root.

## `--format=json`

- **Success:** exit 0, one JSON object on stdout, the licence notice on
  stderr. Its top-level keys: `type` (`"result"`), `timestamp`, `ping`,
  `download`, `upload`, `packetLoss`, `isp`, `interface`, `server`,
  `result`.
  - `ping.latency` (ms, a number) is the text form's `Idle Latency`.
  - `download.bandwidth` and `upload.bandwidth` are bytes per second
    (numbers); × 8 / 10^6 gives the text form's Mbps.
  - `interface` holds `internalIp`, `externalIp`, `macAddr`, `name` and
    `isVpn`; `server` holds `id`, `host`, `port`, `name`, `location`,
    `country` and `ip`; `result` holds `id`, `url` and `persisted`. sb
    reads none of these.
- **Failure (a server id not in Ookla's list):** exit 2, nothing on
  stdout, a JSON log line on stderr (`{"type":"log",...,"level":"error"}`).
  Checked again 2026-11-23 (the same CLI in a throwaway
  `curlimages/curl` container) with a retired id, `--server-id=7190`
  (Ookla's former Los Angeles node, which its server search no longer
  lists), and with an id that never existed (`999999999`): both give
  the same one line on stderr, its `timestamp` aside,
  `{"type":"log",...,"message":"Configuration - No servers defined
  (NoServersException)","level":"error"}`. sb reads that log object's
  `type`, `level` and `message` fields to show `Server gone` for a
  node with a server id.

## The text form (bench.sh's)

Lines `Idle Latency:   <n> ms   (jitter ...)`, `Download:  <n> Mbps (data
used: ...)` and `Upload:   <n> Mbps (data used: ...)`; bench.sh's
`awk '/Download/{print $3" "$4}'` (and the same for Upload and Latency)
gives `<n> Mbps` and `<n> ms`, the values the JSON's fields give.

## HOME

With `--accept-license --accept-gdpr` the CLI writes its acceptance to
`$HOME/.config/ookla/speedtest-cli.json`, and nothing under root's own
home: `HOME` pointed at sb's private directory keeps it there, and it
goes with the directory.
