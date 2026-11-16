//! `sb fact` (tui spec, "Editors", `sb fact`; module build M54): the
//! terminal check, the facts session opened on Saltbox's facts directory,
//! then sb-tui's page over its catalog. The session's editor lock is held
//! until the page closes.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use anyhow::bail;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_config::facts::session::Session;

use crate::{block_on, screen};

/// The refusal without a terminal on stdin and stdout (sb-go cmd/fact.go).
pub const NO_TERMINAL: &str = "sb fact requires an interactive terminal for input and output";

/// `sb fact`'s help (sb-go cmd/fact.go's `Long`, with the tui spec's
/// difference 28: the changes panel instead of a review at the end).
pub const ABOUT: &str = "Interactively browse and edit existing Saltbox configuration facts.
Changes are listed in the changes panel until you save. Requires an interactive terminal.
Mouse: click to select, use the wheel to navigate, and right-click for actions. Hold Shift while dragging for native terminal text selection.";

/// The facts directory: `<server_appdata_path>/saltbox` from the
/// inventory under `SB_SALTBOX_DIR`'s root (cli-core's `saltbox_dir`),
/// `/opt/saltbox` by default (tui spec, editors, assumption 3).
fn facts_dir() -> PathBuf {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    config::appdata_path(&config::inventory_path(&root)).join("saltbox")
}

/// `sb fact`: refuses before anything is opened without a terminal on
/// stdin and stdout; opens the session (its errors are sb's plain error
/// block, before the screen draws); then the page.
///
/// # Errors
///
/// [`NO_TERMINAL`]; `open fact editor: <cause>` (another editor, a
/// symlinked or missing directory, a file that can't be read or parsed,
/// naming it); the screen's I/O error; `Exit(130)` after Ctrl-C
/// ("interrupted" is `finish_run`'s after a signal).
pub fn fact(shutdown: &Shutdown) -> anyhow::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!(NO_TERMINAL);
    }
    let dir = facts_dir();
    tracing::info!(dir = %config::escape_controls(&dir.display().to_string()), "facts directory");
    // Why not anyhow's `context`: each `SessionError` already shows its
    // cause in its own text, and cli-core's error block prints every cause
    // in the chain on its own line, so the cause would show twice.
    let session = match Session::open(&dir) {
        Ok(session) => session,
        Err(err) => bail!("open fact editor: {err}"),
    };
    let ending = block_on(sb_tui::fact::browse(
        shutdown,
        session.catalog(),
        screen::theme(),
    ))??;
    // The editor lock is released only now the page has closed.
    drop(session);
    screen::ended(ending)
}
