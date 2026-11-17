//! `sb bench` (ops spec, "`sb bench`"): this server's system, disk and
//! network, measured on sb's current-thread runtime, the disk test in the
//! working directory (ops decision 12).

use std::path::Path;

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::log::RunLog;

/// `sb bench`. The runtime is built here, after parsing, as for the other
/// commands; its requests carry `User-Agent: sb/<version>`, as
/// self-update's.
///
/// # Errors
///
/// sb-ops' command's: the system can't be read, stdout can't be written,
/// or the run was interrupted.
pub fn bench(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    let user_agent = format!("sb/{}", build.version);
    super::block_on(sb_ops::bench::bench(
        Path::new("."),
        &user_agent,
        shutdown,
        log,
    ))?
}
