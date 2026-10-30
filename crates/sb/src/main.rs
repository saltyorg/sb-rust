//! The Saltbox CLI: wires the modules into one command tree
//! (capability map, `sb`; spec docs/specs/cli-core.md).

use std::io::Write;

use clap::{Parser, Subcommand};
use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, ExitCode};
use sb_cli_core::log::{self, RunLog};
use sb_cli_core::root;

/// Saltbox CLI
#[derive(Parser)]
#[command(name = "sb", about = "Saltbox CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Why not clap's `#[command(version)]`: it adds a `--version` flag that
/// prints `<name> <version>` (clap_builder 4.6.7 `Command::_render_version`);
/// sb-go has a `version` subcommand with its own line and `--json`.
#[derive(Subcommand)]
enum Command {
    /// Print Saltbox CLI version
    Version {
        /// Print machine-readable version information
        #[arg(long)]
        json: bool,
    },
}

impl Command {
    /// The command's name for the run log: never its arguments, which can
    /// carry secrets.
    fn name(&self) -> &'static str {
        match self {
            Command::Version { .. } => "version",
        }
    }

    /// Whether the command needs root (spec: startup step 3). Every command
    /// decides here, with no wildcard arm, so a new one can't skip it.
    fn needs_root(&self) -> bool {
        match self {
            Command::Version { .. } => false,
        }
    }
}

fn main() -> ExitCode {
    // Every command, help and version included, refuses an unsupported
    // host first, as sb-go does (cli-core spec, startup step 1).
    if let Err(unsupported) = sb_host::check_support() {
        return exit::report(&unsupported, None, &mut std::io::stderr());
    }
    let cli = Cli::parse();
    let relaunch = root::ensure_root(
        cli.command.needs_root(),
        root::effective_uid(),
        std::env::var_os(root::RELAUNCHED).is_some(),
        root::relaunch_as_root,
    );
    if let Err(err) = relaunch {
        return exit::report(&err, None, &mut std::io::stderr());
    }
    // Root runs log on the host (spec: startup step 4); the log directory
    // is root-only, so unprivileged runs (help, version) don't.
    let log = (root::effective_uid() == 0)
        .then(|| open_log(&log::log_dir(std::env::var_os("SB_LOG_DIR"))))
        .flatten();
    let _run = log.as_ref().map(RunLog::enter);
    if let Some(log) = &log {
        log.started(cli.command.name());
    }
    let shutdown = Shutdown::new();
    let result = shutdown
        .listen_for_signals()
        .map_err(anyhow::Error::from)
        .and_then(|()| run(cli, &shutdown));
    let error = result.as_ref().err().map(|e| e.as_ref() as _);
    if let Some(log) = &log {
        log.finished(&log::outcome(&shutdown, error), error);
    }
    let pointer = log.as_ref().map(RunLog::pointer);
    exit::finish_run(&shutdown, error, pointer.as_deref(), &mut std::io::stderr())
}

/// Open the run log and make it the process's subscriber; a log that can't
/// be opened is a warning, never a failed run.
fn open_log(dir: &std::path::Path) -> Option<RunLog> {
    match RunLog::open(dir) {
        Ok((log, subscriber)) => tracing::subscriber::set_global_default(subscriber)
            .is_ok()
            .then_some(log),
        Err(err) => {
            eprintln!("warning: no run log this time ({}: {err})", dir.display());
            None
        }
    }
}

fn run(cli: Cli, _shutdown: &Shutdown) -> anyhow::Result<()> {
    let info = BuildInfo::current();
    let mut out = std::io::stdout().lock();
    match cli.command {
        Command::Version { json } => {
            let text = if json {
                info.version_json()
            } else {
                info.version_line()
            };
            out.write_all(text.as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `version` runs as the calling user, with no sudo (spec success
    /// criterion 1).
    #[test]
    fn the_log_gets_the_command_name_not_its_arguments() {
        assert_eq!(Command::Version { json: true }.name(), "version");
    }

    #[test]
    fn version_does_not_need_root() {
        assert!(!Command::Version { json: false }.needs_root());
    }
}
