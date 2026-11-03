# cloudflare-go's retries, and why sb-config retries by hand

Verified 2026-11-03 for the `config` module (config plan Task 9).

**Software:** cloudflare-go v7.13.1 (sb-go's version), read:
`internal/requestconfig/requestconfig.go` (`shouldRetry`,
`parseRetryAfterHeader`, `retryDelay`, `Execute`) and
`option/requestoption.go` (`WithBaseURL`, the auth headers); reqwest
0.13.5 (`src/retry.rs`), read; Go 1.27.

**Method:** reading the sources above; sb-config's Cloudflare tests
script each case against a local fake.

## What cloudflare-go does

- **Attempts:** the first and up to two retries (`MaxRetries: 2`).
- **When it retries:** on a connection error (no response; a timed-out
  attempt counts, since the client's timeout ends the attempt, not the
  request's context), else when `x-should-retry` says `true` (never when
  it says `false`), else on 408, 409, 429 and 5xx.
- **How long it waits:** `Retry-After-Ms` (milliseconds, a float), else
  `Retry-After` (seconds as a float, or an RFC 1123 date as the time until
  then), never negative and with no upper limit; else 0.5 s doubling per
  retry, at most 8 s, less a random amount up to a quarter. A value Go's
  `ParseFloat` refuses (`1e400`) is skipped; one past Go's `Duration`
  (`inf`) converts to a negative duration on amd64, hence no wait.
  sb-config follows these rules but for two small differences: a
  far-future date (Go's `time.Until` saturates to ~292 years) doesn't wait,
  and a hex-float value (`0x1p3`, which Go reads) takes the backoff.
- **Base URL:** `WithBaseURL` adds a trailing `/` to a path that lacks one,
  so `http://host/client/v4` keeps its `v4`.
- **Auth:** `authorization: Bearer <token>` for a scoped token,
  `X-Auth-Key` and `X-Auth-Email` for a global key.

## Why not reqwest's own retries

reqwest 0.13.5's `retry` module (https://docs.rs/reqwest/0.13.5/reqwest/retry/index.html)
can't express these rules: its tower policy's future is
`std::future::Ready<()>` (no backoff, no `Retry-After`), its classifier
(`classify::ReqRep`) sees the method, URI, status and error but no
headers (no `x-should-retry`), and its policies carry a retry budget
unless told otherwise. No retry middleware crate (reqwest-retry, backon,
tokio-retry) is in the tree. sb-config's loop is three attempts around
`send`, with the delay rules above as a pure function its tests pin.
