//! `sb logs` (tui spec): Saltbox's systemd services and Docker containers
//! in the picker, and the log of the one chosen in the viewer.
//!
//! Before the screen (tui decision 20): the service list (a failed one is
//! host's error, exit 1) and the container list (an unreachable daemon is
//! the Containers section's one row; none of either is sb-go's message
//! and exit 0), the terminal check, then the run's one journal and the
//! coredump user's uid, both under `spawn_blocking` (sb-host's journal
//! docs). Ctrl-C on the screen ends the run with 130, quietly (tui
//! decision 13).
//!
//! When stdout isn't a terminal, the inline fallback (tui spec, "The
//! inline fallback"): the picker on `/dev/tty`, then the chosen log on
//! stdout; with no terminal at all, the error.

use std::fs::File;
use std::io::{IsTerminal as _, Write as _};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit::{self, Exit};
use sb_cli_core::log;
use sb_cli_core::theme;
use sb_host::journal::{self, Journal, Source};
use sb_host::systemd::{Service, Systemd, default_filters};
use sb_tui::Ending;

use crate::{block_on, docker_logs, set_override};

/// sb-go's message for an empty list, extended to both kinds (tui spec,
/// "First load before the screen").
pub const NOTHING_FOUND: &str = "No matching systemd services or Docker containers found.";

/// The error with no terminal to draw the picker on (tui spec, "The
/// inline fallback").
pub const NO_TERMINAL: &str = "sb logs needs a terminal to choose a log";

/// `sb logs`. The runtime is built here, after parsing, as for the other
/// commands.
///
/// # Errors
///
/// The service list's error, no terminal, an unknown release, a journal
/// that can't be opened, a screen that fails; `Exit(130)` after Ctrl-C.
pub fn logs(shutdown: &Shutdown) -> anyhow::Result<()> {
    // The journal's files and the follow's inotify instance, as
    // journalctl's (tui spec, "RLIMIT_NOFILE"), before anything is open.
    sb_tui::raise_open_files();
    block_on(run(shutdown))?
}

async fn run(shutdown: &Shutdown) -> anyhow::Result<()> {
    let token = shutdown.token();
    let services = Systemd::new().services(&default_filters(), &token).await?;
    // A signal during the list: finish_run shows "interrupted".
    if token.is_cancelled() {
        anyhow::bail!("interrupted");
    }
    // An unreachable daemon isn't an error here: the Containers section
    // says so (tui spec, "First load before the screen").
    let listed = match docker_logs::address() {
        Ok(address) => token
            .run_until_cancelled(docker_logs::containers(&address))
            .await
            .ok_or_else(|| anyhow::anyhow!("interrupted"))?,
        Err(err) => Err(err),
    };
    let docker = match listed {
        Ok((daemon, rows)) => sb_tui::Docker::Listed { daemon, rows },
        Err(err) => {
            let cause = format!("{err:#}");
            tracing::warn!(error = %config::escape_controls(&cause), "Docker unreachable");
            sb_tui::Docker::Unreachable(cause)
        }
    };
    let no_containers = matches!(&docker, sb_tui::Docker::Listed { rows, .. } if rows.is_empty());
    if services.is_empty() && no_containers {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{NOTHING_FOUND}").map_err(exit::output_error)?;
        out.flush().map_err(exit::output_error)?;
        return Ok(());
    }
    let terminal = terminal(NO_TERMINAL)?;
    let services = with_journal(services).await?;
    show(
        shutdown,
        terminal,
        Ok(services),
        docker,
        sb_tui::Filter::All,
    )
    .await
}

/// Where the picker draws.
#[derive(Debug)]
pub enum Terminal {
    /// stdout is the terminal: the screens.
    Screen,
    /// stdout isn't: the picker on this tty, then the log on stdout.
    Tty(File),
}

/// The terminal to choose a log on: stdout's, else `/dev/tty`, opened for
/// reading and writing as crossterm opens it (its `tty_fd`). Why not a
/// crossterm call: it opens `/dev/tty` only inside, with no public API.
///
/// # Errors
///
/// `no_terminal` when stdout isn't a terminal and there is no `/dev/tty`
/// (no controlling terminal); the cause goes to the run log.
pub fn terminal(no_terminal: &'static str) -> anyhow::Result<Terminal> {
    if std::io::stdout().is_terminal() {
        return Ok(Terminal::Screen);
    }
    match File::options().read(true).write(true).open("/dev/tty") {
        Ok(tty) => Ok(Terminal::Tty(tty)),
        Err(err) => {
            tracing::info!(error = %err, "no terminal: /dev/tty not opened");
            anyhow::bail!(no_terminal)
        }
    }
}

/// The logs commands' screens on `terminal`, then the run's result
/// ([`ended`]).
///
/// # Errors
///
/// A screen that fails, a write to stdout that fails (a broken pipe is
/// cli-core's quiet 141), a follow the stream can't start; `Exit(130)`
/// after Ctrl-C on a screen.
pub async fn show(
    shutdown: &Shutdown,
    terminal: Terminal,
    services: Result<sb_tui::Services, String>,
    docker: sb_tui::Docker,
    filter: sb_tui::Filter,
) -> anyhow::Result<()> {
    let logs = |theme| sb_tui::Logs {
        services,
        docker,
        filter,
        theme,
        zone: sb_tui::line::local_zone(),
        save_dir: log::log_dir(std::env::var_os("SB_LOG_DIR")),
    };
    let ending = match terminal {
        Terminal::Screen => sb_tui::logs(shutdown, logs(screen_theme())).await?,
        Terminal::Tty(tty) => {
            // The picker's colours are the tty's choice (`--color`,
            // `NO_COLOR`, a terminal), the stream's stdout's (off when
            // piped, unless `--color=always`).
            let picker = if anstream::AutoStream::choice(&tty) == anstream::ColorChoice::Never {
                theme::Theme::plain()
            } else {
                theme::current().clone()
            };
            sb_tui::inline(shutdown, logs(picker), tty, screen_theme()).await?
        }
    };
    ended(ending)
}

/// The theme the logs screens draw with: the run's, or none when stdout
/// gets no colour (`NO_COLOR`, `--color=never`; ADR-002 decision 4, the
/// tui spec's "Text path"), read as sb's other output reads it
/// (`colour_on`, anstream's choice). crossterm drops only colours under
/// `NO_COLOR` and doesn't know `--color`, so without this the viewer
/// lit a match with a background that isn't drawn; the plain theme's
/// `ascii` lights it in reverse video (tui decision 23). Left as it is:
/// `NO_COLOR` with `--color=always` gets the run's theme, whose colours
/// crossterm still drops (crossterm's `force_color_output` would follow
/// the choice; no Saltbox use sets both).
pub(crate) fn screen_theme() -> theme::Theme {
    if sb_term_ui::colour_on(sb_term_ui::Stream::Stdout) {
        theme::current().clone()
    } else {
        theme::Theme::plain()
    }
}

/// `list` with the journal their logs are read from (the run's one,
/// opened under `spawn_blocking`, as sb-host's journal docs ask) and the
/// systemd-coredump user's uid.
///
/// # Errors
///
/// An unknown release, or a journal that can't be opened.
pub async fn with_journal(list: Vec<Service>) -> anyhow::Result<sb_tui::Services> {
    let release = sb_host::ubuntu_release()?;
    let source = journal_source(set_override(std::env::var_os("SB_JOURNAL_DIR")));
    // The run's span goes along to each thread (cli-core's `spawn`).
    let journal = sb_cli_core::spawn::blocking(move || Journal::open(source, &release)).await??;
    let coredump_uid = sb_cli_core::spawn::blocking(journal::coredump_uid)
        .await?
        .unwrap_or_else(|err| {
            // The unit's own entries still show; only 26.04's coredump
            // term needs the uid.
            tracing::warn!(error = %err, "systemd-coredump's uid not looked up");
            None
        });
    Ok(sb_tui::Services {
        list,
        journal,
        coredump_uid,
    })
}

/// The run's result after the screens: 130 after Ctrl-C, quietly (tui
/// decision 13).
///
/// # Errors
///
/// `Exit(130)` after Ctrl-C.
pub fn ended(ending: Ending) -> anyhow::Result<()> {
    match ending {
        Ending::Interrupted => Err(Exit(130).into()),
        // A signal's ending is finish_run's to show.
        Ending::Quit | Ending::Cancelled => Ok(()),
    }
}

/// The journal to read: `SB_JOURNAL_DIR`'s directory (tests and
/// debugging, tui decision 19; logged), else the host's own. sudo's
/// relaunch keeps only the variables `root::relaunch_as_root` names, so
/// an unprivileged caller's value never reaches the root run.
fn journal_source(dir: Option<std::ffi::OsString>) -> Source {
    match dir {
        Some(dir) => {
            let dir = std::path::PathBuf::from(dir);
            tracing::info!(
                variable = "SB_JOURNAL_DIR",
                dir = %config::escape_controls(&dir.display().to_string()),
                "journal directory overridden"
            );
            Source::Directory(dir)
        }
        None => Source::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sb_journal_dir_replaces_the_local_journal() {
        assert_eq!(journal_source(None), Source::Local);
        assert_eq!(
            journal_source(Some("/x/journal".into())),
            Source::Directory("/x/journal".into())
        );
    }
}
