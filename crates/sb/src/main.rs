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
use sb_cli_core::exit::{self, RunEnd};
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
/// prints `<name> <version>` (`clap_builder` 4.6.7 `Command::_render_version`);
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
    /// Change the branch used by Sandbox
    #[command(long_about = "Change the branch used by Sandbox")]
    BranchSandbox {
        /// The branch to switch to
        name: String,
    },
    /// Edit Saltbox configuration files
    // Alone: help on stdout, exit 0, until the picker comes (M53). The
    // help is sb-go's without "Editor wrappers should exec the foreground
    // editor so cancellation can stop it" (tui spec, difference 27) and
    // without its GUI-client sentence (difference 34).
    #[command(
        long_about = EDIT_ABOUT,
        arg_required_else_help = true,
        disable_help_subcommand = true
    )]
    Edit {
        /// Don't offer to edit the file again when it has issues
        // On `edit` and each subcommand (tui spec, editors): a global
        // flag reaches the subcommands
        // (<https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.global>).
        #[arg(long, global = true)]
        no_edit_again: bool,
        #[command(subcommand)]
        command: EditCommand,
    },
    /// Display system information
    // No `--verbose/-v` (motd decision 9).
    #[command(long_about = motd::ABOUT)]
    Motd(motd::Args),
    /// Display logs of managed systemd services
    #[command(
        long_about = "Shows Saltbox's systemd services and Docker containers, and the log of the one you choose."
    )]
    Logs,
    /// Update Saltbox CLI
    // Hidden, as sb-go's. No `--verbose/-v` (release spec, decision 13).
    // `--force-update` exists only in builds with self-update off, raw
    // builds included: `parse` adds it then (`force_update_arg`).
    #[command(long_about = "Update Saltbox CLI", hide = true)]
    SelfUpdate {
        /// Automatically accept update without confirmation
        #[arg(short, long)]
        yes: bool,
        /// Set from `--force-update` when this build has it.
        #[arg(skip)]
        force_update: bool,
    },
    /// Runs Saltbox diagnostics role
    #[command(long_about = "Runs Saltbox diagnostics role")]
    Diag,
    /// Runs Ansible playbooks with specified tags
    #[command(long_about = "Runs Ansible playbooks with specified tags")]
    Install(sb_ansible::InstallArgs),
    /// Update the Ansible virtual environment and saltbox.fact
    // Hidden, as sb-go's. No `--verbose/-v` (python-env decision 9).
    #[command(long_about = UPDATE_VENV_ABOUT, hide = true)]
    UpdateVenv,
    /// List available Saltbox, Sandbox or Saltbox-mod tags
    #[command(long_about = LIST_LONG)]
    List(sb_ansible::ListArgs),
    /// Reinstall the Ansible virtual environment
    // No `--verbose/-v` (python-env decision 9).
    #[command(long_about = "Reinstall the Ansible virtual environment")]
    ReinstallVenv,
    /// Reinstall Saltbox's Python and its Ansible virtual environment using uv
    // No `--verbose/-v` (python-env decision 9). The short help is
    // reworded to fit fang's 120 columns (python-env difference 15); the
    // command's own help keeps sb-go's.
    #[command(
        long_about = "Reinstall the Python version used by Saltbox and related Ansible virtual environment using uv"
    )]
    ReinstallPython,
    /// Reinstall the Rust saltbox.fact file
    // No `--verbose/-v` (python-env decision 9).
    #[command(long_about = "Reinstall the Rust saltbox.fact file")]
    ReinstallFacts,
    /// Display announcements from announcement files
    // Shown, not hidden as sb-go's test harness was: it reads
    // announcements now, and migrations stay with `update` (ops decision
    // 27, the maintainer, 2026-11-15). No `--verbose/-v` (ops decision 18).
    #[command(long_about = ANNOUNCEMENTS_LONG)]
    Announcements(sb_ops::announcements::AnnouncementsArgs),
}

/// `sb update-venv`'s help (sb-go cmd/updateVenv.go, `Long`).
const UPDATE_VENV_ABOUT: &str = "Update the Ansible virtual environment from the current Saltbox checkout and refresh saltbox.fact without updating Git repositories";

/// `sb announcements`' long help: what it reads now that it is shown
/// (ops spec, "`sb announcements`").
const ANNOUNCEMENTS_LONG: &str = "Display Saltbox's announcements, or Sandbox's with --repo Sandbox

Reads the repository's announcements.yml, or the file --after names, and
shows the announcements missing from the --before file; without --before,
every announcement is new. Migrations run in sb update.";

/// `sb list`'s long help, sb-go's (cmd/list.go).
const LIST_LONG: &str = "List available Saltbox, Sandbox or Saltbox-mod tags

Without arguments, displays all available tags.
With a query argument, performs fuzzy search across all tags.

Examples:
  sb list                # List all tags
  sb list plex           # Search for tags matching \"plex\"
  sb list arr            # Search for tags matching \"arr\"";

/// `--force-update`, sb-go's flag in builds with self-update disabled
/// (cmd/selfUpdate.go): added to the parsed tree only in such builds, so
/// the tree of a release build is sb-go's. Why not a clap `hide`: a
/// hidden flag would still parse in builds where it must not exist.
fn force_update_arg(cmd: clap::Command, build: &BuildInfo) -> clap::Command {
    if build.self_update_off() {
        cmd.mut_subcommand("self-update", |sub| {
            sub.arg(
                clap::Arg::new("force-update")
                    .long("force-update")
                    .action(clap::ArgAction::SetTrue)
                    .help("Force update even when self-update is disabled"),
            )
        })
    } else {
        cmd
    }
}

/// `sb edit`'s help (sb-go cmd/edit.go, `editCmd.Long`).
const EDIT_ABOUT: &str = "Edit existing Saltbox configuration files using your preferred editor.

Set VISUAL or EDITOR to an executable editor, with optional quoted arguments.
VISUAL takes precedence over EDITOR. If neither is set, sb tries sensible-editor,
editor, nano, then vi, using the first executable it finds.

Export your preference, for example:
  export EDITOR=\"/usr/bin/vim.basic\"

Editors run as root. Sudo may change HOME and PATH. Use an absolute executable
path for editors installed outside sudo's PATH. If you invoke sudo yourself, pass the
preference explicitly:
  sudo EDITOR=/usr/bin/vim.basic sb edit settings

Quotes and backslash escapes are supported. Shell aliases, functions, variable
expansion, substitutions, and ~ expansion are not supported.

Configuration targets must already exist and be regular files. Symlinks to
regular files are supported.

After the editor exits, the file is checked as sb validate-config checks it,
without the credential checks. If it has issues, sb offers to edit it again;
--no-edit-again answers no.";

/// `sb edit`'s subcommands, one per file (tui spec, editors; sb-go's
/// names, underscores kept).
#[derive(Subcommand, Clone, Copy)]
enum EditCommand {
    /// Accounts
    Accounts,
    /// Advanced Settings
    #[command(name = "adv_settings")]
    AdvSettings,
    /// Backup Settings
    #[command(name = "backup_config")]
    BackupConfig,
    /// Hetzner VLAN Settings
    #[command(name = "hetzner_vlan")]
    HetznerVlan,
    /// Settings
    Settings,
    /// Inventory Settings
    Inventory,
}

impl EditCommand {
    /// The file it opens.
    fn file(self) -> edit::File {
        match self {
            Self::Accounts => edit::File::Accounts,
            Self::AdvSettings => edit::File::AdvSettings,
            Self::BackupConfig => edit::File::BackupConfig,
            Self::HetznerVlan => edit::File::HetznerVlan,
            Self::Settings => edit::File::Settings,
            Self::Inventory => edit::File::Inventory,
        }
    }
}

/// `sb docker`'s subcommands (docker spec; `logs`, the tui spec).
#[derive(Subcommand)]
enum DockerCommand {
    /// Display logs of Docker containers
    #[command(
        long_about = "Shows Saltbox's systemd services and Docker containers, and the log of the one you choose, starting with the Docker containers."
    )]
    Logs,
    /// List Docker containers with port mappings
    #[command(
        long_about = "List all Docker containers and their status, displaying their internal\nports (as potentially exposed by Traefik labels) and their external port bindings."
    )]
    Ps {
        /// Print the containers as JSON
        #[arg(long)]
        json: bool,
    },
    /// Start Docker containers managed by Saltbox
    // No `--verbose/-v` (docker decision 6).
    #[command(long_about = "Start Docker containers managed by Saltbox in dependency order.")]
    Start,
    /// Stop Docker containers managed by Saltbox
    #[command(long_about = "Stop Docker containers managed by Saltbox in dependency order.")]
    Stop {
        /// Containers to ignore during stop operation (can be specified multiple times)
        // Repeatable and comma-separated, as cobra's `StringSlice` reads it
        // for any name Docker allows (docker spec, "Commands and help").
        #[arg(long, value_delimiter = ',', value_name = "ignore")]
        ignore: Vec<String>,
    },
    /// Restart Docker containers managed by Saltbox
    #[command(long_about = "Restart Docker containers managed by Saltbox in dependency order.")]
    Restart {
        /// Containers to ignore during restart operation (can be specified multiple times)
        #[arg(long, value_delimiter = ',', value_name = "ignore")]
        ignore: Vec<String>,
    },
}

impl Command {
    /// The command's name for the run log: never its arguments, which can
    /// carry secrets.
    fn name(&self) -> &'static str {
        match self {
            Command::Version { .. } => "version",
            Command::Docker { command } => match command {
                DockerCommand::Logs => "docker logs",
                DockerCommand::Ps { .. } => "docker ps",
                DockerCommand::Start => "docker start",
                DockerCommand::Stop { .. } => "docker stop",
                DockerCommand::Restart { .. } => "docker restart",
            },
            Command::ValidateConfig => "validate-config",
            Command::BranchSandbox { .. } => "branch-sandbox",
            Command::Edit { command, .. } => match command {
                EditCommand::Accounts => "edit accounts",
                EditCommand::AdvSettings => "edit adv_settings",
                EditCommand::BackupConfig => "edit backup_config",
                EditCommand::HetznerVlan => "edit hetzner_vlan",
                EditCommand::Settings => "edit settings",
                EditCommand::Inventory => "edit inventory",
            },
            Command::Motd(_) => "motd",
            Command::SelfUpdate { .. } => "self-update",
            Command::Logs => "logs",
            Command::Diag => "diag",
            Command::Install(_) => "install",
            Command::UpdateVenv => "update-venv",
            Command::List(_) => "list",
            Command::ReinstallVenv => "reinstall-venv",
            Command::ReinstallPython => "reinstall-python",
            Command::ReinstallFacts => "reinstall-facts",
            Command::Announcements(_) => "announcements",
        }
    }

    /// Whether the command needs root (spec: startup step 3). Every command
    /// decides here, with no wildcard arm, so a new one can't skip it.
    fn needs_root(&self) -> bool {
        match self {
            Command::Version { .. } => false,
            // The Docker socket, and sb's rule that only version and
            // completion run unprivileged (cli-core decision 3); Saltbox's
            // config is root's to read (config spec); the Sandbox checkout
            // and its ownership are root's to change; so are the files
            // `edit` opens (tui spec, editors, assumption 2); the journal
            // files are root's to read (tui assumption 2); motd as every
            // command (motd decision 20: pam_motd already runs it as root);
            // sb's own file, which `self-update` replaces (release spec,
            // assumption 5); playbooks run with `--become` from root's
            // Saltbox checkout (ansible spec, success criterion 9); list
            // writes that checkout's tag cache; the venv, its wrappers and
            // apt, which `update-venv`, `reinstall-venv` and
            // `reinstall-python` change, and the checkout's fact, which
            // `reinstall-facts` replaces (python-env assumption 5);
            // `announcements`, as every ops command (ops assumption 2).
            Command::Docker { .. }
            | Command::ValidateConfig
            | Command::BranchSandbox { .. }
            | Command::Edit { .. }
            | Command::Motd(_)
            | Command::SelfUpdate { .. }
            | Command::Logs
            | Command::Diag
            | Command::Install(_)
            | Command::UpdateVenv
            | Command::List(_)
            | Command::ReinstallVenv
            | Command::ReinstallPython
            | Command::ReinstallFacts
            | Command::Announcements(_) => true,
        }
    }
}

fn main() -> RunEnd {
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
        return exit::report(&unsupported, None, &mut anstream::stderr()).into();
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
        return exit::report(&err, None, &mut anstream::stderr()).into();
    }
    // A root run holds secrets: no core dump of sb (cli-core spec, startup
    // step 3); for help and version it costs nothing. Failing both ways
    // leaves the host's default.
    if let Err(err) = root::no_core_dumps() {
        warn(format!("could not turn off core dumps: {err}"));
    }
    // Root runs log on the host (spec: startup step 4); the log directory
    // is root-only, so unprivileged runs (help, version) don't.
    let log = (root::effective_uid() == 0)
        .then(|| open_log(&log::log_dir(std::env::var_os("SB_LOG_DIR"))))
        .flatten();
    let _run = log.as_ref().map(RunLog::enter);
    if let Some(log) = &log {
        // A panic from here on leaves a record in the run (sb-M2).
        log.record_panics();
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
        .and_then(|()| run(&cli.command, &shutdown, log.as_ref()));
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
    let (path, loaded) = if let Some(path) = config_override(std::env::var_os("SB_CONFIG")) {
        let loaded = config::load_theme(&path);
        (path, loaded)
    } else {
        let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
        let appdata = config::appdata_path(&config::inventory_path(&root));
        (
            config::config_path(&appdata),
            config::load_theme_in(&appdata),
        )
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
/// (<https://docs.rs/clap/4.6.7/clap/builder/struct.Styles.html#method.plain>).
/// Nothing is printed here: help, version and errors come back as `Err`
/// for [`parse_failed`].
fn parse(profile: theme::Profile) -> Result<Cli, clap::Error> {
    let cmd = match profile {
        theme::Profile::Ascii => Cli::command().styles(clap::builder::Styles::plain()),
        _ => Cli::command(),
    };
    // Help goes to stdout; whether it shows in colour decides the shaded
    // usage block's padding rows, and its width where its lines wrap.
    let colour =
        profile != theme::Profile::Ascii && sb_term_ui::colour_on(sb_term_ui::Stream::Stdout);
    // Before the help's layout, which lists each command's flags.
    let cmd = force_update_arg(cmd, &BuildInfo::current(env!("CARGO_PKG_VERSION")));
    let cmd = help::apply(cmd, theme::current(), colour, help::width());
    cmd.try_get_matches().and_then(|matches| {
        let mut cli = Cli::from_arg_matches(&matches)?;
        if let Command::SelfUpdate { force_update, .. } = &mut cli.command {
            *force_update = forced(&matches);
        }
        Ok(cli)
    })
}

/// Whether `--force-update` was given (only builds that have it can).
fn forced(matches: &clap::ArgMatches) -> bool {
    matches
        .subcommand_matches("self-update")
        .and_then(|sub| sub.try_get_one::<bool>("force-update").ok().flatten())
        .copied()
        .unwrap_or(false)
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
/// `--color` and `NO_COLOR` are honoured. The message is escaped here, the
/// one place every warning passes (`SB_LOG_DIR` and the io error naming
/// it can hold anything).
fn warn(message: impl std::fmt::Display) {
    let style = theme::current().style(theme::Role::Warn);
    let message = exit::escaped(&message.to_string());
    let _ = writeln!(anstream::stderr(), "{style}warning:{style:#} {message}");
}

fn run(command: &Command, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    match *command {
        Command::ValidateConfig => validate_config(shutdown, log)?,
        Command::BranchSandbox { ref name } => branch::branch_sandbox(name, shutdown, log)?,
        Command::Edit {
            command,
            no_edit_again,
        } => edit::edit(command.file(), no_edit_again, shutdown, log)?,
        Command::Motd(ref args) => motd::motd(args, shutdown)?,
        Command::UpdateVenv => python_env::update_venv(shutdown, log)?,
        Command::ReinstallVenv => python_env::reinstall_venv(shutdown, log)?,
        Command::ReinstallPython => python_env::reinstall_python(shutdown, log)?,
        Command::ReinstallFacts => python_env::reinstall_facts(shutdown, log)?,
        Command::Announcements(ref args) => announcements::announcements(args, shutdown, log)?,
        Command::SelfUpdate { yes, force_update } => {
            self_update::self_update(yes, force_update, shutdown, log)?;
        }
        Command::Logs => logs::logs(shutdown)?,
        Command::Docker {
            command: DockerCommand::Logs,
        } => docker_logs::docker_logs(shutdown)?,
        Command::Diag => ansible::diag(shutdown)?,
        Command::Install(ref args) => ansible::install(args, shutdown, log)?,
        Command::List(ref args) => ansible::list(args, shutdown, log)?,
        Command::Docker {
            command: DockerCommand::Ps { json },
        } => docker_ps(json, shutdown)?,
        Command::Docker {
            command: DockerCommand::Start,
        } => docker_control(command.name(), &Control::Start, shutdown, log)?,
        Command::Docker {
            command: DockerCommand::Stop { ref ignore },
        } => docker_control(command.name(), &Control::Stop(ignore), shutdown, log)?,
        Command::Docker {
            command: DockerCommand::Restart { ref ignore },
        } => docker_control(command.name(), &Control::Restart(ignore), shutdown, log)?,
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

/// Which controller command runs, with its ignored names.
enum Control<'a> {
    Start,
    Stop(&'a [String]),
    Restart(&'a [String]),
}

/// An override variable's value; empty means unset, as for `SB_CONFIG`.
fn set_override(value: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    value.filter(|v| !v.is_empty())
}

/// `sb docker start`, `stop` and `restart` (docker spec, "Start, stop and
/// restart"): the Saltbox Docker controller's steps under term-ui's
/// runner, titled with the command's name. `SB_DOCKER_CONTROLLER_URL` and
/// `SB_DOCKER_CONTROLLER_UNIT` replace the controller's address and unit
/// file (tests and debugging; empty means unset; each is logged, the URL
/// without any credentials in it, as validate-config's service URLs). A
/// URL that doesn't parse, or isn't UTF-8, is sb-go's error before the
/// runner starts: falling back to the real controller would start or stop
/// the host's containers. Why not clap's `env`: it needs a flag sb-go
/// doesn't have, and passes an empty value on. exec's forced-exit hook is
/// registered here, the first command that runs children: a second signal
/// SIGKILLs a `systemctl` still running. The runtime is built here, after
/// parsing, as for the other commands.
fn docker_control(
    title: &str,
    command: &Control<'_>,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    use sb_docker::control::{self, ControlError, Controller};
    let mut controller = Controller::saltbox()?;
    if let Some(url) = set_override(std::env::var_os("SB_DOCKER_CONTROLLER_URL")) {
        let url = url.into_string().map_err(|_| {
            ControlError::ParseUrl("SB_DOCKER_CONTROLLER_URL isn't UTF-8".to_string())
        })?;
        tracing::info!(
            variable = "SB_DOCKER_CONTROLLER_URL",
            url = sb_config::http::loggable_url(&url),
            "controller URL overridden"
        );
        controller = controller.with_base(&url)?;
    }
    if let Some(unit) = set_override(std::env::var_os("SB_DOCKER_CONTROLLER_UNIT")) {
        let unit = std::path::PathBuf::from(unit);
        tracing::info!(
            variable = "SB_DOCKER_CONTROLLER_UNIT",
            unit = %config::escape_controls(&unit.display().to_string()),
            "controller unit file overridden"
        );
        controller = controller.with_unit_file(unit);
    }
    let title = format!("sb {title}");
    let _children = control::on_forced_exit(shutdown);
    // The runtime's error first (`?`), then the run's, as the result.
    block_on(async {
        let mut runner =
            sb_term_ui::Runner::start(&title, shutdown, log, sb_term_ui::Options::default());
        let result = match *command {
            Control::Start => control::start(&mut runner, &controller).await,
            Control::Stop(ignore) => control::stop(&mut runner, &controller, ignore).await,
            Control::Restart(ignore) => control::restart(&mut runner, &controller, ignore).await,
        };
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}

/// `sb docker ps [--json]` (docker spec): the table or the JSON on stdout,
/// then, if inspects failed, their error (exit 1). The runtime is built
/// here, after parsing, so help and version never pay for it (term-ui):
/// current-thread, with I/O and time (bollard's sockets and its request
/// timeout). A signal drops the requests
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>);
/// cli-core's `finish_run` then shows "interrupted" and ends sb by the
/// signal, and nothing reaches stdout.
fn docker_ps(json: bool, shutdown: &Shutdown) -> anyhow::Result<()> {
    use sb_docker::connect;
    let address = docker_logs::address()?;
    let listing = block_on(shutdown.token().run_until_cancelled(async {
        let daemon = connect::connect(&address).await?;
        connect::list(&daemon).await
    }))?;
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
/// before the runner starts, as sb's plain error block. An old-format
/// motd.yml is migrated first, its original copied to `SB_BACKUPS_DIR`,
/// or sb-config's default, `/opt/saltbox/backups` (motd decision 4; a
/// temporary folder in sb's test build).
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
    // The runtime's error first (`?`), then the run's, as the result.
    block_on(async {
        let options = sb_term_ui::Options::default();
        let mut runner = sb_term_ui::Runner::start("sb validate-config", shutdown, log, options);
        let backups = std::env::var_os("SB_BACKUPS_DIR")
            .filter(|dir| !dir.is_empty())
            .map_or_else(
                sb_config::migrate::default_backups,
                std::path::PathBuf::from,
            );
        let result = run::run(&mut runner, &settings, &backups).await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}

/// `future` run on a current-thread runtime that isn't waited for after.
///
/// The runtime has I/O and time, and is built here, after parsing, so
/// help and version never pay for it (term-ui). A host name lookup runs on a blocking thread (hyper-util's
/// `GaiResolver` calls getaddrinfo under `spawn_blocking`, for reqwest's
/// clients and bollard's `tcp://` connector alike), and the runtime's drop
/// waits for every blocking task: after Ctrl-C with DNS dead, sb would
/// hang until the resolver gave up (config-L11; `docker ps` with
/// `DOCKER_HOST` naming a host, Task 60). The run is over when the future
/// is, so nothing left on the pool is waited for; the process exits with
/// it. tokio's documented way not to wait:
/// <https://docs.rs/tokio/1.54.1/tokio/runtime/struct.Runtime.html#shutdown>,
/// <https://docs.rs/tokio/1.54.1/tokio/runtime/struct.Runtime.html#method.shutdown_background>,
/// <https://docs.rs/hyper-util/0.1.21/src/hyper_util/client/legacy/connect/dns.rs.html>,
/// <https://docs.rs/bollard/0.21.1/src/bollard/docker.rs.html> (`connect_with_http`).
///
/// # Errors
///
/// When the runtime can't be built.
fn block_on<F: std::future::Future>(future: F) -> std::io::Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let output = runtime.block_on(future);
    runtime.shutdown_background();
    Ok(output)
}

mod announcements;
mod ansible;
mod branch;
mod docker_logs;
mod edit;
mod logs;
mod motd;
mod python_env;
mod self_update;

#[cfg(test)]
mod contract;

#[cfg(test)]
mod tests {
    use super::*;

    /// No help text sb gives clap holds `{n}`: clap's help template would
    /// print it as a new line (cli-core's help module, "What the template
    /// way costs"). Checked on the texts `help::apply` sets, every flag and
    /// default included, for a build with `--force-update`.
    #[test]
    fn no_help_text_holds_claps_new_line_variable() {
        fn walk(cmd: &clap::Command, texts: &mut Vec<String>) {
            texts.extend(
                [cmd.get_before_help(), cmd.get_before_long_help()]
                    .into_iter()
                    .flatten()
                    .map(ToString::to_string),
            );
            for sub in cmd.get_subcommands() {
                walk(sub, texts);
            }
        }
        let raw = BuildInfo::from_env("0.1.0", None, None, None);
        let cmd = help::apply(
            force_update_arg(Cli::command(), &raw),
            &sb_cli_core::theme::Theme::plain(),
            false,
            help::MAX_WIDTH,
        );
        let mut texts = Vec::new();
        walk(&cmd, &mut texts);
        assert!(texts.len() > 10, "the walk sees the tree: {}", texts.len());
        assert!(
            texts.iter().any(|t| t.contains("--force-update")),
            "and the flags added after the derive"
        );
        let found: Vec<&String> = texts.iter().filter(|t| t.contains("{n}")).collect();
        assert!(found.is_empty(), "{found:?}");
    }

    /// `--force-update` is in the tree only of builds with self-update
    /// off: raw builds and those built with it disabled, never a plain
    /// release build (sb-go's tree); `self-update` is hidden and needs root.
    #[test]
    fn force_update_exists_only_where_self_update_is_off() {
        let has_it = |build: &BuildInfo| {
            force_update_arg(Cli::command(), build)
                .find_subcommand("self-update")
                .expect("self-update")
                .get_arguments()
                .any(|arg| arg.get_long() == Some("force-update"))
        };
        let release = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, None);
        let disabled = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Some("true"));
        let raw = BuildInfo::from_env("0.1.0", None, None, None);
        assert!(!has_it(&release));
        assert!(has_it(&disabled));
        assert!(has_it(&raw));
        let cmd = Cli::command();
        assert!(
            cmd.find_subcommand("self-update")
                .expect("self-update")
                .is_hide_set()
        );
        let update = Command::SelfUpdate {
            yes: false,
            force_update: false,
        };
        assert!(update.needs_root());
        assert_eq!(update.name(), "self-update");
    }

    /// `--force-update` given is read into the command; not given, or in a
    /// build without it, it is false.
    #[test]
    fn a_given_force_update_is_read() {
        let cmd = force_update_arg(
            Cli::command(),
            &BuildInfo::from_env("0.1.0", None, None, None),
        );
        let matches = cmd
            .clone()
            .try_get_matches_from(["sb", "self-update", "--force-update"])
            .expect("parsed");
        assert!(forced(&matches));
        let matches = cmd
            .try_get_matches_from(["sb", "self-update", "-y"])
            .expect("parsed");
        assert!(!forced(&matches));
        let matches = Cli::command()
            .try_get_matches_from(["sb", "self-update"])
            .expect("parsed");
        assert!(!forced(&matches));
    }

    /// `--ignore` is repeatable and comma-separated (cobra's
    /// `StringSlice` for any Docker name); names are kept as given, empty
    /// ones too (the request drops them), and `start` has none.
    #[test]
    fn ignore_splits_on_commas_and_repeats() {
        let ignored = |args: &[&str]| match Cli::try_parse_from(args).expect("parses").command {
            Command::Docker {
                command: DockerCommand::Stop { ignore } | DockerCommand::Restart { ignore },
            } => ignore,
            _ => panic!("not stop or restart: {args:?}"),
        };
        assert_eq!(
            ignored(&["sb", "docker", "stop", "--ignore", "a,b", "--ignore", "c"]),
            ["a", "b", "c"]
        );
        assert_eq!(
            ignored(&[
                "sb", "docker", "restart", "--ignore", "a,,b", "--ignore", " c"
            ]),
            ["a", "", "b", " c"]
        );
        assert_eq!(ignored(&["sb", "docker", "stop"]), Vec::<String>::new());
        assert!(Cli::try_parse_from(["sb", "docker", "start", "--ignore", "a"]).is_err());
    }

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
        // The ignored names never reach the log's name.
        let secret = || vec!["secret-name".to_string()];
        let stop = Command::Docker {
            command: DockerCommand::Stop { ignore: secret() },
        };
        assert_eq!(stop.name(), "docker stop");
        let restart = Command::Docker {
            command: DockerCommand::Restart { ignore: secret() },
        };
        assert_eq!(restart.name(), "docker restart");
        let start = Command::Docker {
            command: DockerCommand::Start,
        };
        assert_eq!(start.name(), "docker start");
    }

    /// An empty override is unset: an empty `SB_DOCKER_CONTROLLER_URL`
    /// can't stand for a controller (nor turn into the real one unseen).
    #[test]
    fn an_empty_override_is_unset() {
        assert_eq!(set_override(None), None);
        assert_eq!(set_override(Some("".into())), None);
        assert_eq!(
            set_override(Some("http://127.0.0.1:9".into())),
            Some("http://127.0.0.1:9".into())
        );
    }

    /// `validate-config` reads root's config and logs under its name.
    #[test]
    fn validate_config_needs_root() {
        assert!(Command::ValidateConfig.needs_root());
        assert_eq!(Command::ValidateConfig.name(), "validate-config");
    }

    /// `edit` needs root and logs each subcommand under its sb-go name.
    #[test]
    fn edit_needs_root_and_logs_its_file() {
        let edit = |command| Command::Edit {
            command,
            no_edit_again: false,
        };
        assert!(edit(EditCommand::Settings).needs_root());
        assert_eq!(edit(EditCommand::AdvSettings).name(), "edit adv_settings");
        assert_eq!(edit(EditCommand::Inventory).name(), "edit inventory");
        assert_eq!(EditCommand::Inventory.file(), edit::File::Inventory);
    }

    /// `motd` needs root and logs under its name (motd decision 20).
    #[test]
    fn motd_needs_root_and_logs_its_name() {
        let motd = Command::Motd(motd::Args::default());
        assert!(motd.needs_root());
        assert_eq!(motd.name(), "motd");
    }

    /// `logs` reads root's journal and logs under its name.
    #[test]
    fn logs_needs_root() {
        assert!(Command::Logs.needs_root());
        assert_eq!(Command::Logs.name(), "logs");
    }

    /// `diag` runs a playbook as root and logs under its name.
    #[test]
    fn diag_needs_root() {
        assert!(Command::Diag.needs_root());
        assert_eq!(Command::Diag.name(), "diag");
    }

    /// `install` runs playbooks as root, and its log name is the
    /// command's alone (the run log itself: tests/ansible.rs).
    #[test]
    fn install_needs_root_and_is_named_without_its_arguments() {
        let install = Cli::try_parse_from(["sb", "install", "plex", "-e", "token=x"])
            .expect("parses")
            .command;
        assert!(install.needs_root());
        assert_eq!(install.name(), "install");
    }

    #[test]
    fn python_env_commands_need_root_and_log_their_names() {
        for (command, name) in [
            (Command::UpdateVenv, "update-venv"),
            (Command::ReinstallVenv, "reinstall-venv"),
            (Command::ReinstallPython, "reinstall-python"),
            (Command::ReinstallFacts, "reinstall-facts"),
        ] {
            assert!(command.needs_root(), "{name}");
            assert_eq!(command.name(), name);
        }
    }

    /// `list` writes the Saltbox checkout's tag cache, as root (ansible
    /// spec, success criterion 9).
    #[test]
    fn list_needs_root() {
        let list = Cli::try_parse_from(["sb", "list", "plex", "-m"])
            .expect("parses")
            .command;
        assert!(list.needs_root());
        assert_eq!(list.name(), "list");
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
