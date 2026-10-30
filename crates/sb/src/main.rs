//! The Saltbox CLI: wires the modules into one command tree
//! (capability map, `sb`; spec docs/specs/cli-core.md).

use std::io::Write;

use clap::{Parser, Subcommand};
use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, ExitCode};

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

fn main() -> ExitCode {
    // Every command, help and version included, refuses an unsupported
    // host first, as sb-go does (cli-core spec, startup step 1).
    if let Err(unsupported) = sb_host::check_support() {
        return exit::report(&unsupported, None, &mut std::io::stderr());
    }
    let cli = Cli::parse();
    let shutdown = Shutdown::new();
    let result = shutdown
        .listen_for_signals()
        .map_err(anyhow::Error::from)
        .and_then(|()| run(cli, &shutdown));
    let error = result.as_ref().err().map(|e| e.as_ref() as _);
    exit::finish_run(&shutdown, error, None, &mut std::io::stderr())
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
