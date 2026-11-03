# Docker Engine API: versions, bollard's paths, failed starts

Verified 2026-11-03 for the `docker` module (docker plan, Task 1).

**Software:** Docker Engine 29.7.2 (API 1.55, minimum 1.40), measured;
bollard 0.21.1; moby v28.5.2 source (`api/swagger.yaml`,
`api/server/middleware/version.go`, `daemon/start.go`,
`daemon/errors.go`, `container/state.go`), read (the failed-start
finding comes from that source, not from a 29.x daemon); the Engine API
overview at https://docs.docker.com/reference/api/engine/.

**Method:** read-only requests with curl over the daemon's Unix socket
(`/_ping`, `/version`, `/vX.Y/version`); a scratch bollard client pointed
at a listener that prints each request line; the same client listing and
inspecting containers on a real daemon (read-only); reading moby's source.
No container was created, started or changed.

## Versioned and unversioned paths

- **The daemon** accepts `/vX.Y/...` only for versions between its minimum
  and its maximum. Outside that range it answers `400` with `client
  version X.Y is too new. Maximum supported API version is …` or `… is too
  old. Minimum supported API version is …` (`version.go`). Every response
  carries an `Api-Version` header with the daemon's maximum.
- **Unversioned paths** (`/_ping`, `/version`, `/containers/json`) are
  served at the daemon's current API version. moby's API description:
  "If you omit the version-prefix, the current version of the API … is
  used. … Using the API without a version-prefix is deprecated and will be
  removed in a future release." (`api/swagger.yaml`).
- **Minimum versions rise:** Docker 29.7.2 refuses anything below 1.40 by
  default (operators can lower it with `DOCKER_MIN_API_VERSION`)
  (`/v1.24/version`: 400, too old). The Engine API overview: "API versions
  before v1.40 are deprecated and no longer supported by current versions
  of the Docker Engine and CLI." A single low "floor" version therefore
  doesn't work across releases.
- **bollard 0.21.1 sends every request unversioned:** `/_ping`,
  `/version`, `/containers/json?all=true&size=false`,
  `/containers/<id>/json`. `uri.rs` formats `…/v{major}.{minor}{path}`
  and then calls `Url::join(path)` with the absolute path, which replaces
  the versioned path. The client version is used nowhere else, so
  `negotiate_version` (which reads `/version` and lowers the client
  version) and a pinned `ClientVersion` change nothing on the wire in this
  release. Against the real daemon (API 1.55) bollard's 1.53 models
  listed and inspected every container without an error.

## A failed start

`daemon/start.go`: when a start fails, the daemon records the error
(`SetError`) and, if no exit code was set, sets 128 (`exitUnknown`); a
container that never started reports state `created`
(`container/state.go`: `StartedAt` zero). So a failed first start is
`created` with `State.Error` set and a non-zero exit code; a later failed
restart of a stopped container stays `exited` with its error.

## Dependencies

`cargo deny check` with sb's `deny.toml` on a scratch crate depending on
bollard 0.21.1 (default features): advisories, bans and sources ok;
licences ok apart from the scratch crate's own missing licence field.
axum's tree (for the test-only fake) was not checked here; Task 5 adds
it under cargo-deny.

## Fake Engine API for tests

axum 0.8.9 serves on a `tokio::net::UnixListener` (its `serve::Listener`
trait is implemented for it on Unix,
https://docs.rs/axum/0.8.9/axum/serve/trait.Listener.html) and routes
paths such as `/containers/{id}/json`; hyper alone would need hand-written
routing.
