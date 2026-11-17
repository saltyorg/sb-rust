//! `sb announcements` (ops spec, "`sb announcements`"): Saltbox's and
//! Sandbox's announcements, read on sb's current-thread runtime.

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;

/// `sb announcements`. The runtime is built here, after parsing, as for
/// the other commands; the Saltbox root is `SB_SALTBOX_DIR`'s (ops
/// assumption 5).
///
/// # Errors
///
/// sb-ops' command's: a failed write, the viewer's.
pub fn announcements(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    super::block_on(sb_ops::announcements::announcements(&root, shutdown, log))?
}
