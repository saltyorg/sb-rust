//! `sb announcements` (ops spec, "`sb announcements`"): a repository's
//! announcements, read on sb's current-thread runtime.

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_ops::announcements::AnnouncementsArgs;

/// `sb announcements`. The runtime is built here, after parsing, as for
/// the other commands; the Saltbox root is `SB_SALTBOX_DIR`'s (ops
/// assumption 5).
///
/// # Errors
///
/// sb-ops' command's: an unknown repository, a file that can't be
/// loaded, a failed write.
pub fn announcements(
    args: &AnnouncementsArgs,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    super::block_on(sb_ops::announcements::announcements(
        args, &root, shutdown, log,
    ))?
}
