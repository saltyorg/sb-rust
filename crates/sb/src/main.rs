//! The Saltbox CLI: wires the modules into one command tree
//! (capability map, `sb`; spec docs/specs/cli-core.md).

// Output goes through `Write` and `exit::output_error`: `print!`/`eprint!`
// panic when the reader goes away (exit 101) instead of the quiet 141.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::colour;
use sb_cli_core::config;
use sb_cli_core::exit::{self, ExitCode};
use sb_cli_core::help;
use sb_cli_core::log::{self, RunLog};
use sb_cli_core::root;
use sb_cli_core::theme;

/// Saltbox CLI
#[derive(Parser)]
#[command(name = "sb", about = "Saltbox CLI")]
struct Cli {
    /// `--color=auto|always|never` (spec: startup step 5).
    #[command(flatten)]
    color: colour::Color,
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
    /// Manage Docker containers managed by Saltbox
    // Alone: help on stdout, exit 0, as sb-go (`arg_required_else_help`
    // makes clap's missing-subcommand error the kind `parse` prints as
    // help). No `help` subcommand: sb-go's docker group has none (cobra
    // adds one at the root only).
    #[command(
        long_about = "Manage Docker containers managed by Saltbox",
        arg_required_else_help = true,
        disable_help_subcommand = true
    )]
    Docker {
        #[command(subcommand)]
        command: DockerCommand,
    },
    /// Validate Saltbox configuration files
    // No `--verbose/-v`: the run log has the detail (config decision 1).
    #[command(long_about = "Validate Saltbox configuration files")]
    ValidateConfig,
}

/// `sb docker`'s subcommands (docker spec); start, stop, restart and logs
/// come with their own plans.
#[derive(Subcommand)]
enum DockerCommand {
    /// List Docker containers with port mappings
    #[command(
        long_about = "List all Docker containers and their status, displaying their internal\nports (as potentially exposed by Traefik labels) and their external port bindings."
    )]
    Ps {
        /// Print the containers as JSON
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
            Command::Docker {
                command: DockerCommand::Ps { .. },
            } => "docker ps",
            Command::ValidateConfig => "validate-config",
        }
    }

    /// Whether the command needs root (spec: startup step 3). Every command
    /// decides here, with no wildcard arm, so a new one can't skip it.
    fn needs_root(&self) -> bool {
        match self {
            Command::Version { .. } => false,
            // The Docker socket, and sb's rule that only version and
            // completion run unprivileged (cli-core decision 3).
            Command::Docker { .. } => true,
            // Saltbox's config is root's to read (config spec).
            Command::ValidateConfig => true,
        }
    }
}

fn main() -> ExitCode {
    // Colour and theme first (from the arguments, the environment and the
    // theme file), so every message below follows `--color` and the
    // profile. The colour
    // choice must be anstream's global one before the real parse, which
    // prints help (spec: startup steps 5 and 6).
    colour::early_choice(Cli::command(), std::env::args_os()).write_global();
    let (choice, mut warnings) = theme_choice();
    let (profile, warning) = theme::Profile::choose(
        std::env::var("SB_COLOR_PROFILE").ok().as_deref(),
        choice.color_profile,
    );
    warnings.extend(warning);
    theme::install(theme::Theme::resolve(&choice.palette, profile));
    // Every command, help and version included, refuses an unsupported
    // host first, as sb-go does (cli-core spec, startup step 1); the real
    // parse runs too, printing nothing yet. Together they say whether this
    // run hands over to a root run through sudo, which then reads its own
    // settings and warns about those: a run about to relaunch leaves the
    // startup warnings to it.
    let support = sb_host::check_support();
    let parsed = parse(profile);
    let relaunching = support.is_ok()
        && parsed
            .as_ref()
            .is_ok_and(|cli| root::needs_relaunch(cli.command.needs_root(), root::effective_uid()));
    // sb's own messages go through anstream, so `--color` and NO_COLOR win.
    // Each write makes its stream then: anstream reads the colour choice
    // when a stream is made, and the parsed `--color` is applied below.
    if !relaunching {
        for warning in &warnings {
            warn(warning);
        }
    }
    if let Err(unsupported) = support {
        return exit::report(&unsupported, None, &mut anstream::stderr());
    }
    let cli = parsed.unwrap_or_else(|err| parse_failed(err));
    // The real parse's choice is the one that counts for the run, for
    // anstream and console alike (term-ui spec: colour on every path).
    cli.color.write_global();
    sb_term_ui::follow_colour_choice(profile);
    let relaunch = root::ensure_root(
        cli.command.needs_root(),
        root::effective_uid(),
        root::relaunch_as_root,
    );
    if let Err(err) = relaunch {
        // No root run follows: the warnings held back for it show here.
        for warning in &warnings {
            warn(warning);
        }
        return exit::report(&err, None, &mut anstream::stderr());
    }
    // Root runs log on the host (spec: startup step 4); the log directory
    // is root-only, so unprivileged runs (help, version) don't.
    let log = (root::effective_uid() == 0)
        .then(|| open_log(&log::log_dir(std::env::var_os("SB_LOG_DIR"))))
        .flatten();
    let _run = log.as_ref().map(RunLog::enter);
    if let Some(log) = &log {
        // Startup warnings go to the log too (shown on screen only once,
        // before any log was open).
        for warning in &warnings {
            tracing::warn!("{warning}");
        }
        log.started(
            cli.command.name(),
            &BuildInfo::current(env!("CARGO_PKG_VERSION")).version,
        );
    }
    let shutdown = Shutdown::new();
    let result = shutdown
        .listen_for_signals()
        .map_err(anyhow::Error::from)
        .and_then(|()| run(cli, &shutdown, log.as_ref()));
    let error = result.as_ref().err().map(|e| e.as_ref() as _);
    // One frozen view of how the run ended, so the log and the screen agree
    // even if a signal arrives between them.
    let ending = shutdown.snapshot();
    if let Some(log) = &log {
        log.finished(&log::outcome(&ending, error), error);
    }
    let pointer = log.as_ref().map(RunLog::pointer);
    exit::finish_run(&ending, error, pointer.as_deref(), &mut anstream::stderr())
}

/// The theme file's choice (`SB_CONFIG`, else
/// `<server_appdata_path>/sb/config.toml`, read from the inventory under
/// `SB_SALTBOX_DIR`'s root), or the built-in theme and a
/// warning saying why the file wasn't used.
fn theme_choice() -> (config::ThemeChoice, Vec<String>) {
    let (path, loaded) = match config_override(std::env::var_os("SB_CONFIG")) {
        Some(path) => {
            let loaded = config::load_theme(&path);
            (path, loaded)
        }
        None => {
            let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
            let appdata = config::appdata_path(&config::inventory_path(&root));
            (
                config::config_path(&appdata),
                config::load_theme_in(&appdata),
            )
        }
    };
    match loaded {
        Ok(choice) => (choice, Vec::new()),
        Err(err) => (
            config::ThemeChoice::default(),
            vec![format!(
                "theme config {} {err}; using the built-in theme",
                config::escape_controls(&path.display().to_string())
            )],
        ),
    }
}

/// The `SB_CONFIG` path; empty means unset.
fn config_override(value: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

/// The real parse, with every command's help in sb-go's format, in the
/// theme's colours (`help::apply`), and parse errors in sb-go's ERROR-badge layout
/// (`help::parse_error_block`). Under `ascii` what clap still draws itself
/// (`sb help help`) gets clap's plain styles
/// (https://docs.rs/clap/4.6.7/clap/builder/struct.Styles.html#method.plain).
/// Nothing is printed here: help, version and errors come back as `Err`
/// for [`parse_failed`].
fn parse(profile: theme::Profile) -> Result<Cli, clap::Error> {
    let cmd = match profile {
        theme::Profile::Ascii => Cli::command().styles(clap::builder::Styles::plain()),
        _ => Cli::command(),
    };
    // Help goes to stdout; whether it shows in colour decides the shaded
    // usage block's padding rows.
    let colour =
        profile != theme::Profile::Ascii && sb_term_ui::colour_on(sb_term_ui::Stream::Stdout);
    let cmd = help::apply(cmd, theme::current(), colour);
    cmd.try_get_matches()
        .and_then(|matches| Cli::from_arg_matches(&matches))
}

/// What the real parse didn't turn into a command: help or version
/// (printed, exit 0), else a parse error (sb-go's layout, exit 2).
fn parse_failed(err: clap::Error) -> ! {
    match err.kind() {
        // Bare `sb`: sb-go prints the help on stdout and succeeds; clap would
        // print it as an error, on stderr with exit 2
        // (https://docs.rs/clap/4.6.7/clap/error/enum.ErrorKind.html#variant.DisplayHelpOnMissingArgumentOrSubcommand).
        clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            // Write errors are ignored, as clap ignores them for `--help`.
            let mut out = anstream::stdout();
            let _ = write!(out, "{}", err.render().ansi()).and_then(|()| out.flush());
            std::process::exit(0)
        }
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => err.exit(),
        _ => parse_error(err),
    }
}

/// A parse error in sb-go's ERROR-badge layout, on stderr, ending the run with
/// clap's exit code (2, the usage-error convention; cli-core spec).
fn parse_error(err: clap::Error) -> ! {
    let code = err.exit_code();
    let block = help::parse_error_block(err, theme::current());
    let _ = write!(anstream::stderr(), "{block}");
    std::process::exit(code)
}

/// Open the run log and make it the process's subscriber; a log that can't
/// be opened is a warning, never a failed run.
fn open_log(dir: &std::path::Path) -> Option<RunLog> {
    match RunLog::open(dir) {
        Ok((log, subscriber)) => tracing::subscriber::set_global_default(subscriber)
            .is_ok()
            .then_some(log),
        Err(err) => {
            warn(format_args!(
                "no run log this time ({}: {err})",
                dir.display()
            ));
            None
        }
    }
}

/// sb's `warning:` line, in the theme's warn colour, through anstream so
/// `--color` and NO_COLOR are honoured.
fn warn(message: impl std::fmt::Display) {
    let style = theme::current().style(theme::Role::Warn);
    let _ = writeln!(anstream::stderr(), "{style}warning:{style:#} {message}");
}

fn run(cli: Cli, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    match cli.command {
        Command::ValidateConfig => validate_config(shutdown, log)?,
        Command::Docker {
            command: DockerCommand::Ps { json },
        } => docker_ps(json, shutdown)?,
        Command::Version { json } => {
            let info = BuildInfo::current(env!("CARGO_PKG_VERSION"));
            let mut out = std::io::stdout().lock();
            let text = if json {
                info.version_json()
            } else {
                info.version_line()
            };
            out.write_all(text.as_bytes()).map_err(exit::output_error)?;
            // Flush through the output-error path. Today's outputs end in a
            // newline and go out on write; this covers output that doesn't,
            // whose broken pipe would otherwise hit the implicit flush at
            // exit and be ignored (exit 0, not 141).
            out.flush().map_err(exit::output_error)?;
        }
    }
    Ok(())
}

/// `sb docker ps [--json]` (docker spec): the table or the JSON on stdout,
/// then, if inspects failed, their error (exit 1). The runtime is built
/// here, after parsing, so help and version never pay for it (term-ui):
/// current-thread, with I/O and time (bollard's sockets and its request
/// timeout). A signal drops the requests
/// (https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled);
/// cli-core's `finish_run` then shows "interrupted" with the signal's code
/// and nothing reaches stdout.
fn docker_ps(json: bool, shutdown: &Shutdown) -> anyhow::Result<()> {
    use sb_docker::connect;
    let env = |name| std::env::var(name).ok();
    let address = connect::address(
        env("DOCKER_HOST").as_deref(),
        env("DOCKER_TLS_VERIFY").as_deref(),
        env("DOCKER_CERT_PATH").as_deref(),
    )?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listing = runtime.block_on(shutdown.token().run_until_cancelled(async {
        let daemon = connect::connect(&address).await?;
        connect::list(&daemon).await
    }));
    let Some(listing) = listing else {
        anyhow::bail!("interrupted");
    };
    let listing = listing?;
    // A signal between the answers and the output: nothing reaches stdout
    // (it narrows the window; finish_run still decides the ending).
    if shutdown.token().is_cancelled() {
        anyhow::bail!("interrupted");
    }
    if json {
        let mut out = std::io::stdout().lock();
        out.write_all(sb_docker::json::json(&listing.rows).as_bytes())
            .map_err(exit::output_error)?;
        out.flush().map_err(exit::output_error)?;
    } else {
        sb_docker::ps::table(&listing.rows).print()?;
    }
    match connect::failures_error(&listing.failures) {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// `sb validate-config` (config spec): sb-config's run under term-ui's
/// runner, titled as sb-go's, on the Saltbox root that also holds the
/// theme's inventory. The runtime is built here, after parsing, so help
/// and version never pay for it: current-thread, with I/O and time
/// (reqwest's sockets, the checks' timeouts and backoff). Exit 1 when
/// anything failed: the run has shown every block, so it returns a quiet
/// exit. An HTTP client that can't be built (TLS or proxy setup) fails
/// before the runner starts, as sb's plain error block.
fn validate_config(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    use sb_config::{cloudflare, dockerhub, http, run, validators};
    // Empty means unset, as for `SB_SALTBOX_DIR`. A test or debugging URL
    // in place of a service's is logged, without any credentials in it: a
    // leftover export would otherwise send real credentials elsewhere
    // unseen.
    let service_url = |name| {
        let value = std::env::var(name).ok().filter(|value| !value.is_empty());
        if let Some(url) = &value {
            tracing::info!(
                variable = name,
                url = http::loggable_url(url),
                "service URL overridden"
            );
        }
        value
    };
    let settings = run::Settings {
        root: config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR")),
        zoneinfo: validators::Saltbox::zoneinfo_places(std::env::var_os("ZONEINFO")),
        http: http::Http::new(&http::Proxy::System, http::TIMEOUT).map_err(anyhow::Error::msg)?,
        cloudflare_url: service_url("CLOUDFLARE_BASE_URL")
            .unwrap_or_else(|| cloudflare::BASE_URL.to_string()),
        dockerhub_url: service_url("SB_DOCKERHUB_URL")
            .unwrap_or_else(|| dockerhub::LOGIN_URL.to_string()),
        backoff: cloudflare::backoff,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let options = sb_term_ui::Options::default();
        let mut runner =
            sb_term_ui::Runner::start("sb validate-config", shutdown, log, options).await;
        let result = run::run(&mut runner, &settings).await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })
}

#[cfg(test)]
mod contract;

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty `SB_CONFIG` is unset, as an empty `SB_COLOR_PROFILE` is.
    #[test]
    fn an_empty_sb_config_means_the_real_file() {
        assert_eq!(config_override(None), None);
        assert_eq!(config_override(Some("".into())), None);
        assert_eq!(
            config_override(Some("/x/c.toml".into())),
            Some(std::path::PathBuf::from("/x/c.toml"))
        );
    }

    #[test]
    fn the_log_gets_the_command_name_not_its_arguments() {
        assert_eq!(Command::Version { json: true }.name(), "version");
        let ps = Command::Docker {
            command: DockerCommand::Ps { json: true },
        };
        assert_eq!(ps.name(), "docker ps");
    }

    /// `validate-config` reads root's config and logs under its name.
    #[test]
    fn validate_config_needs_root() {
        assert!(Command::ValidateConfig.needs_root());
        assert_eq!(Command::ValidateConfig.name(), "validate-config");
    }

    /// `docker ps` needs root (cli-core decision 3).
    #[test]
    fn docker_needs_root() {
        let ps = Command::Docker {
            command: DockerCommand::Ps { json: false },
        };
        assert!(ps.needs_root());
    }

    /// `version` runs as the calling user, with no sudo (spec success
    /// criterion 1).
    #[test]
    fn version_does_not_need_root() {
        assert!(!Command::Version { json: false }.needs_root());
    }
}
