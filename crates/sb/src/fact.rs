//! `sb fact` (tui spec, "Editors", `sb fact`; module build M54): the
//! terminal check, the facts session opened on Saltbox's facts directory,
//! then sb-tui's fact editor over it (M54, M55). The session's editor
//! lock is held until the editor closes.
//!
//! `sb fact get` and `sb fact list` (tui spec, "`sb fact get` and `sb
//! fact list`"): sb-config's read-only `facts::read`, no lock of any kind,
//! the value alone on stdout. The run log gets the names and the outcome,
//! never the value.

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::bail;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::escape_controls;
use sb_cli_core::{exit, tail};
use sb_config::facts::read::{self, Store};
use sb_config::facts::session::Session;
use sb_term_ui::DisplayText;

use crate::{block_on, screen};

/// The refusal without a terminal on stdin and stdout (sb-go cmd/fact.go).
pub const NO_TERMINAL: &str = "sb fact requires an interactive terminal for input and output";

/// `sb fact`'s help (sb-go cmd/fact.go's `Long`, with the tui spec's
/// difference 28: the changes panel instead of a review at the end).
pub const ABOUT: &str = "Interactively browse and edit existing Saltbox configuration facts.
Changes are listed in the changes panel until you save. Requires an interactive terminal.
Mouse: click to select, use the wheel to navigate, and right-click for actions. Hold Shift while dragging for native terminal text selection.";

/// `sb fact get`'s help (tui spec, "`sb fact get` and `sb fact list`").
pub const GET_ABOUT: &str = "Print one fact's value: sb fact get <app> <instance> <key>.
When the app has one instance, sb fact get <app> <key> works too.
The value alone goes to standard output, so VALUE=$(sb fact get ...) captures it;
to a terminal, control characters are left out. Exit 1 when it isn't there or is empty.";

/// `sb fact list`'s help.
pub const LIST_ABOUT: &str =
    "List the facts' names as <app> <instance> <key> lines, sorted, never their values.
Give an app, or an app and an instance, to list only theirs.";

/// The facts directory under `SB_SALTBOX_DIR`'s root (sb-config's
/// `facts::directory`).
fn facts_dir() -> PathBuf {
    sb_config::facts::directory(std::env::var_os("SB_SALTBOX_DIR"))
}

/// An error of the facts layer as one message. Why not `?` or anyhow's
/// `context`: a `SessionError` already shows its cause in its own text,
/// and cli-core's error block prints every cause in the chain on its own
/// line, so the cause would show twice (as [`fact`] does).
fn flat(err: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

/// `text` for stdout: as it is to a pipe or a file, through term-ui's
/// `DisplayText` (colour off) to a terminal, so a value can't move the
/// cursor or retitle it, as the editor shows values.
fn for_stdout(text: &str, terminal: bool) -> String {
    if terminal {
        DisplayText::from_bytes(text.as_bytes(), false)
            .as_str()
            .to_owned()
    } else {
        text.to_owned()
    }
}

/// `text` written to stdout through cli-core's output-error path: a
/// reader gone away ends sb quietly with 141, as `version` does.
fn print(text: &str) -> anyhow::Result<()> {
    let mut out = tail::Tracked::stdout(std::io::stdout().lock());
    out.write_all(text.as_bytes()).map_err(exit::output_error)?;
    out.flush().map_err(exit::output_error)?;
    Ok(())
}

/// `sb fact get <app> <instance> <key>`, or `<app> <key>` (`third`
/// `None`): the value and `\n`, or with `json` the names and the value as
/// one line of JSON. No lock is taken.
///
/// # Errors
///
/// The store's error, or the miss (sb-config's `LookupError`): exit 1,
/// names only.
pub fn get(app: &str, second: &str, third: Option<&str>, json: bool) -> anyhow::Result<()> {
    let (instance, key) = match third {
        Some(key) => (Some(second), key),
        None => (None, second),
    };
    tracing::info!(
        app = %escape_controls(app),
        instance = instance.map(escape_controls).map(tracing::field::display),
        key = %escape_controls(key),
        "fact lookup"
    );
    let store = Store::open(&facts_dir()).map_err(flat)?;
    let found = read::get(&store, app, instance, key).map_err(flat)?;
    tracing::info!(instance = %escape_controls(&found.instance), "fact found");
    let text = if json {
        found.json()
    } else {
        let mut text = for_stdout(&found.value, std::io::stdout().is_terminal());
        text.push('\n');
        text
    };
    print(&text)
}

/// `sb fact list [<app> [<instance>]]`: `<app> <instance> <key>` lines,
/// sorted, or with `json` an array of the names. Never a value.
///
/// # Errors
///
/// The store's error, or an unknown app or instance: exit 1.
pub fn list(app: Option<&str>, instance: Option<&str>, json: bool) -> anyhow::Result<()> {
    tracing::info!(
        app = app.map(escape_controls).map(tracing::field::display),
        instance = instance.map(escape_controls).map(tracing::field::display),
        "fact list"
    );
    let store = Store::open(&facts_dir()).map_err(flat)?;
    let names = read::list(&store, app, instance).map_err(flat)?;
    tracing::info!(facts = names.len(), "facts listed");
    let text = if json {
        read::names_json(&names)
    } else {
        // Line by line: `DisplayText` drops a text's final newline.
        let terminal = std::io::stdout().is_terminal();
        names.iter().fold(String::new(), |mut lines, name| {
            let line = format!("{} {} {}", name.app, name.instance, name.key);
            lines.push_str(&for_stdout(&line, terminal));
            lines.push('\n');
            lines
        })
    };
    print(&text)
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
    tracing::info!(dir = %escape_controls(&dir.display().to_string()), "facts directory");
    // Why not anyhow's `context`: each `SessionError` already shows its
    // cause in its own text, and cli-core's error block prints every cause
    // in the chain on its own line, so the cause would show twice.
    let session = match Session::open(&dir) {
        Ok(session) => session,
        Err(err) => bail!("open fact editor: {err}"),
    };
    // The page keeps the session, and its editor lock, until it closes.
    let ending = block_on(sb_tui::fact::browse(shutdown, session, screen::theme()))??;
    screen::ended(ending)
}
