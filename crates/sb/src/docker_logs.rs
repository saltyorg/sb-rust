//! `sb docker logs` (tui spec): the picker filtered to Saltbox's Docker
//! containers, and the log of the one chosen in the viewer; and the
//! container list both logs commands load before the screen.
//!
//! Before the screen (tui decision 20): the container list (an
//! unreachable daemon ends the run as `sb docker ps` does, exit 1; an
//! empty list prints `No containers found.`, exit 0), then the terminal
//! check, then the services `Tab` shows and their journal (a failure is
//! the Services section's one row). Ctrl-C on the screen ends the run
//! with 130, quietly (tui decision 13).

use std::io::{IsTerminal as _, Write as _};
use std::sync::Arc;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit;
use sb_cli_core::theme;
use sb_docker::connect::{self, Daemon};
use sb_docker::ps::Row;
use sb_host::systemd::{Systemd, default_filters};

use crate::block_on;

/// The message for an empty list (tui spec, "First load before the
/// screen"; sb-go: `No running containers found.`).
pub const NO_CONTAINERS: &str = "No containers found.";

/// The error with no terminal to draw the picker on (tui spec, "The
/// inline fallback").
pub const NO_TERMINAL: &str = "sb docker logs needs a terminal to choose a log";

/// `sb docker logs`. The runtime is built here, after parsing, as for the
/// other commands.
///
/// # Errors
///
/// An unreachable daemon or a failed list, no terminal, a screen that
/// fails; `Exit(130)` after Ctrl-C.
pub fn docker_logs(shutdown: &Shutdown) -> anyhow::Result<()> {
    block_on(run(shutdown))?
}

async fn run(shutdown: &Shutdown) -> anyhow::Result<()> {
    let address = address()?;
    let Some(listed) = shutdown
        .token()
        .run_until_cancelled(containers(&address))
        .await
    else {
        anyhow::bail!("interrupted");
    };
    let (daemon, rows) = listed?;
    if rows.is_empty() {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{NO_CONTAINERS}").map_err(exit::output_error)?;
        out.flush().map_err(exit::output_error)?;
        return Ok(());
    }
    // The inline fallback comes with the tui spec's slice 5.
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(NO_TERMINAL);
    }
    let services = services(shutdown).await?;
    let ending = sb_tui::logs(
        shutdown,
        sb_tui::Logs {
            services,
            docker: sb_tui::Docker::Listed { daemon, rows },
            filter: sb_tui::Filter::Containers,
            theme: theme::current().clone(),
            zone: sb_tui::line::local_zone(),
        },
    )
    .await?;
    crate::logs::ended(ending)
}

/// The services `Tab` shows, with their journal, or why they couldn't be
/// listed: the Services section's one row, as an unreachable daemon is
/// `sb logs`' Containers row. This command's own kind is the containers,
/// so a failure here doesn't end it.
///
/// # Errors
///
/// A signal during the list.
async fn services(shutdown: &Shutdown) -> anyhow::Result<Result<sb_tui::Services, String>> {
    let token = shutdown.token();
    let listed = Systemd::new().services(&default_filters(), &token).await;
    if token.is_cancelled() {
        anyhow::bail!("interrupted");
    }
    let services = match listed {
        Ok(list) => crate::logs::with_journal(list).await,
        Err(err) => Err(err.into()),
    };
    Ok(services.map_err(|err| {
        let cause = format!("{err:#}");
        tracing::warn!(error = %config::escape_controls(&cause), "services unavailable");
        cause
    }))
}

/// The daemon's address from `DOCKER_HOST`, `DOCKER_TLS_VERIFY` and
/// `DOCKER_CERT_PATH`, for `sb docker ps` and the logs commands.
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
