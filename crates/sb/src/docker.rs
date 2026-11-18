//! The Docker daemon sb talks to, for `sb docker ps` and `sb logs`: its
//! address from the environment, and the container list `sb logs` loads
//! before the screen (tui spec, "First load before the screen").

use std::sync::Arc;

use sb_docker::connect::{self, Daemon};
use sb_docker::ps::Row;

/// The daemon's address from `DOCKER_HOST`, `DOCKER_TLS_VERIFY` and
/// `DOCKER_CERT_PATH`, for `sb docker ps` and `sb logs`.
///
/// # Errors
///
/// A TLS or ssh setting, or a scheme sb doesn't speak (sb-docker's
/// `connect::address`).
pub fn address() -> anyhow::Result<String> {
    let env = |name| std::env::var(name).ok();
    connect::address(
        env("DOCKER_HOST").as_deref(),
        env("DOCKER_TLS_VERIFY").as_deref(),
        env("DOCKER_CERT_PATH").as_deref(),
    )
}

/// The daemon at `address` and its containers, sorted by name. A failed
/// inspect leaves its container out (`connect::list` logs it): the picker
/// shows the others.
///
/// # Errors
///
/// An unreachable daemon or a failed list, as `sb docker ps` reports
/// them.
pub async fn containers(address: &str) -> anyhow::Result<(Arc<Daemon>, Vec<Row>)> {
    let daemon = connect::connect(address).await?;
    let listing = connect::list(&daemon).await?;
    Ok((Arc::new(daemon), listing.rows))
}
